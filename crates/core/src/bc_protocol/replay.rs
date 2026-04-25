//! SD card replay: day records, file list, seek, start/stop playback (MSG 142, 14, 15, 13, 123, 5, 7).
//!
//! Desktop app (FUN_180177b80) sends CMD 0x17d with a 0x944-byte payload: 20-byte inner header
//! then 0x930 body (channel, 32 zeros, path 0x3ff). Use MSG_ID_REPLAY_DESKTOP when MSG 8 returns 400.
//!
//! Response codes (header offset 0x10): 200 = accept, 300 = end (by-name), 331 = end (by-time),
//! 400 = reject. App only writes binary when response_code matches stored expected (e.g. Android +0x1c).
//! 32-byte metadata skip: app does it only for msg_id 5 (not 0x17d); see docs/BCMEDIA_REPLAY_FORMAT.md.

use super::stream::StreamData;
use super::{BcCamera, Error, Result};
use std::collections::HashSet;
use std::convert::{TryFrom, TryInto};
use crate::bc::{
    model::*,
    xml::{
        xml_ver, BcPayloads, BcXml, DayRecord, DayRecordList, DayRecords, EventAlarmType,
        FileInfo, FileInfoList, FindAlarmVideo, ReplayDateTime, ReplaySeek,
    },
};
use crate::bcmedia::codex::BcMediaCodex;
use crate::bcmedia::model::BcMedia;
use bytes::BytesMut;
use tokio_util::codec::Decoder;
use log::debug;
use std::time::SystemTime;
use time::{Date, Month, PrimitiveDateTime, Time};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::channel;
use tokio::task;
use tokio_util::sync::CancellationToken;

/// Desktop replay payload size: 20-byte inner header + 0x930 body (channel, 32 zeros, path).
pub const DESKTOP_REPLAY_PAYLOAD_LEN: usize = 0x944;
const DESKTOP_INNER_HEADER_LEN: usize = 20;
const DESKTOP_PATH_MAX: usize = 0x3ff;

/// E1 (Extension 1.1) per-packet envelope: optional 32-byte prefix then Extension XML, then decrypted payload.
const E1_REPLAY_PREFIX_LEN: usize = 32;

/// Returns true if `data` begins with a known BcMedia frame magic (INFO, IFRAME, PFRAME, AAC).
/// Used to distinguish BcMedia streams (non-E1 cameras) from container formats when detecting replay mode.
fn is_bcmedia_start(data: &[u8]) -> bool {
    if data.len() < 4 {
        return false;
    }
    let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
    matches!(magic,
        0x31303031 | // INFO_V1
        0x32303031 | // INFO_V2
        0x63643030..=0x63643039 | // IFRAME range
        0x63643130..=0x63643139 | // PFRAME range
        0x62773530   // AAC
    )
}

/// Strip E1 replay envelope from a packet body.
/// Layouts: (1) 32 bytes + `<?xml ...></Extension>[\r]\n` + payload, or (2) `<?xml ...></Extension>...` + payload.
/// Also supports any closing after `</Extension>` (e.g. `</xml>`); uses first `00dc` as fallback for media start.
/// Special case: if packet ends with `</Extension>\n`, return empty (payload starts in next packet).
/// Returns the payload slice (decrypted media); if the body doesn't match, returns the whole slice.
fn strip_e1_replay_envelope(data: &[u8]) -> &[u8] {
    // Check if packet ends with </Extension>\n or </Extension>\r\n (XML spans entire packet, payload in next packet)
    if data.len() >= 14 && (data.ends_with(b"</Extension>\n") || data.ends_with(b"</Extension>\r\n")) {
        if data.starts_with(b"<?xml") || (data.len() > E1_REPLAY_PREFIX_LEN && data[E1_REPLAY_PREFIX_LEN..].starts_with(b"<?xml")) {
            return &[];
        }
    }
    // Try layout (1): 32-byte prefix then XML
    if data.len() > E1_REPLAY_PREFIX_LEN && data[E1_REPLAY_PREFIX_LEN..].starts_with(b"<?xml") {
        if let Some(payload_start) = crate::bc::e1::e1_xml_end_offset(data, E1_REPLAY_PREFIX_LEN) {
            if payload_start <= data.len() {
                return &data[payload_start..];
            }
        }
    }
    // Try layout (2): body starts with XML (no 32-byte prefix)
    if data.len() >= 12 && data.starts_with(b"<?xml") {
        if let Some(payload_start) = crate::bc::e1::e1_xml_end_offset(data, 0) {
            if payload_start <= data.len() {
                return &data[payload_start..];
            }
        }
    }
    data
}

/// Build the 0x944-byte payload for desktop-style replay (CMD 0x17d).
/// Layout: 20-byte inner header (2, 0x82f, 8, 500 LE) then 0x930 body: channel (4), 32 zeros, path (1023), padding.
pub fn build_desktop_replay_payload(channel_id: u8, path: &str) -> Vec<u8> {
    let mut out = vec![0u8; DESKTOP_REPLAY_PAYLOAD_LEN];
    // Inner 20-byte header (from Ghidra FUN_180177b80 local_b08)
    out[0..8].copy_from_slice(&2u64.to_le_bytes());
    out[8..12].copy_from_slice(&0x82fu32.to_le_bytes());
    out[12..16].copy_from_slice(&8u32.to_le_bytes());
    out[16..20].copy_from_slice(&500u32.to_le_bytes());
    // 0x930 body: offset 0 = channel (u32 LE)
    out[DESKTOP_INNER_HEADER_LEN..DESKTOP_INNER_HEADER_LEN + 4]
        .copy_from_slice(&(channel_id as u32).to_le_bytes());
    // offset 4..36 = 32 zeros (already zeroed)
    // offset 36..36+1023 = path, null-padded
    let path_start = DESKTOP_INNER_HEADER_LEN + 4 + 32;
    let path_len = path.as_bytes().len().min(DESKTOP_PATH_MAX);
    out[path_start..path_start + path_len].copy_from_slice(&path.as_bytes()[..path_len]);
    out
}

/// BC_DOWNLOAD_BY_TIME_INFO size (0x480 = 1152 bytes). Layout from APK JNA + native path at +0x60.
pub const BC_DOWNLOAD_BY_TIME_INFO_SIZE: usize = 0x480;
const SAVE_PATH_MAX: usize = 1024; // cSaveFileName at +0x60, max 1024 bytes

/// Build the 0x480-byte BC_DOWNLOAD_BY_TIME_INFO payload for download-by-time (MSG 143).
/// Layout: iChannel (4), cUID (32), logicChnBitmap (8), startTime (6×i32), endTime (6×i32),
/// padding (4), cSaveFileName (1024), fileSize/curSize/processed (8 each), streamType (4), padding (4).
pub fn build_download_by_time_payload(
    channel_id: u8,
    uid: &[u8],
    logic_chn_bitmap: u64,
    start_time: &ReplayDateTime,
    end_time: &ReplayDateTime,
    save_path: &str,
    stream_type: u32,
) -> Vec<u8> {
    let mut out = vec![0u8; BC_DOWNLOAD_BY_TIME_INFO_SIZE];
    let mut off = 0;
    out[off..off + 4].copy_from_slice(&(channel_id as u32).to_le_bytes());
    off += 4;
    let uid_len = uid.len().min(32);
    out[off..off + uid_len].copy_from_slice(&uid[..uid_len]);
    off += 32;
    out[off..off + 8].copy_from_slice(&logic_chn_bitmap.to_le_bytes());
    off += 8;
    out[off..off + 4].copy_from_slice(&(start_time.year as i32).to_le_bytes());
    out[off + 4..off + 8].copy_from_slice(&(start_time.month as i32).to_le_bytes());
    out[off + 8..off + 12].copy_from_slice(&(start_time.day as i32).to_le_bytes());
    out[off + 12..off + 16].copy_from_slice(&(start_time.hour as i32).to_le_bytes());
    out[off + 16..off + 20].copy_from_slice(&(start_time.minute as i32).to_le_bytes());
    out[off + 20..off + 24].copy_from_slice(&(start_time.second as i32).to_le_bytes());
    off += 24;
    out[off..off + 4].copy_from_slice(&(end_time.year as i32).to_le_bytes());
    out[off + 4..off + 8].copy_from_slice(&(end_time.month as i32).to_le_bytes());
    out[off + 8..off + 12].copy_from_slice(&(end_time.day as i32).to_le_bytes());
    out[off + 12..off + 16].copy_from_slice(&(end_time.hour as i32).to_le_bytes());
    out[off + 16..off + 20].copy_from_slice(&(end_time.minute as i32).to_le_bytes());
    out[off + 20..off + 24].copy_from_slice(&(end_time.second as i32).to_le_bytes());
    off += 24;
    off += 4; // +0x5c: 4 bytes padding (already zeroed)
    let path_len = save_path.as_bytes().len().min(SAVE_PATH_MAX);
    out[off..off + path_len].copy_from_slice(&save_path.as_bytes()[..path_len]);
    // +0x478: streamType (4 bytes)
    out[0x478..0x47c].copy_from_slice(&stream_type.to_le_bytes());
    out
}

/// BC_DOWNLOAD_BY_NAME_INFO size (0xD48 = 3400 bytes). Layout from Ghidra analysis of
/// BaichuanDownloader::downloadFileByName in libBCSDKWrapper.so (Android SDK).
pub const BC_DOWNLOAD_BY_NAME_INFO_SIZE: usize = 0xD48;
const FILE_NAME_OFFSET: usize = 0x24; // cFileName at +0x24 (SDK_LOG confirms: "download file with name: %s", param_2 + 0x24)
const FILE_NAME_MAX: usize = 0x100; // 256 bytes (conservative; 0x500 bytes available between 0x24 and 0x524)

/// Build the 0xD48-byte BC_DOWNLOAD_BY_NAME_INFO payload for download-file-by-name (MSG 8).
/// Layout (from Ghidra): iChannel (4), cUID (32, at 0x04), cFileName (256, at 0x24), rest zeroed.
/// The camera reads channel + filename and streams the raw file back. Output path at 0x524 is
/// SDK-internal (local filesystem path) and not meaningful to send; left zeroed.
pub fn build_download_by_name_payload(channel_id: u8, uid: &str, file_name: &str) -> Vec<u8> {
    let mut out = vec![0u8; BC_DOWNLOAD_BY_NAME_INFO_SIZE];
    out[0..4].copy_from_slice(&(channel_id as u32).to_le_bytes());
    let uid_bytes = uid.as_bytes();
    let uid_len = uid_bytes.len().min(32);
    out[4..4 + uid_len].copy_from_slice(&uid_bytes[..uid_len]);
    let name_bytes = file_name.as_bytes();
    let name_len = name_bytes.len().min(FILE_NAME_MAX - 1); // reserve 1 byte for null terminator
    out[FILE_NAME_OFFSET..FILE_NAME_OFFSET + name_len].copy_from_slice(&name_bytes[..name_len]);
    out
}

/// Duration in seconds between two ReplayDateTimes (end − start). Returns None if invalid or negative.
pub fn replay_datetime_duration_secs(start: &ReplayDateTime, end: &ReplayDateTime) -> Option<u64> {
    let sm = Month::try_from(start.month).ok()?;
    let em = Month::try_from(end.month).ok()?;
    let sd = Date::from_calendar_date(start.year, sm, start.day).ok()?;
    let ed = Date::from_calendar_date(end.year, em, end.day).ok()?;
    let st = Time::from_hms(start.hour, start.minute, start.second).ok()?;
    let et = Time::from_hms(end.hour, end.minute, end.second).ok()?;
    let start_dt = PrimitiveDateTime::new(sd, st).assume_utc();
    let end_dt = PrimitiveDateTime::new(ed, et).assume_utc();
    let secs = (end_dt - start_dt).whole_seconds();
    if secs < 0 {
        None
    } else {
        Some(secs as u64)
    }
}

/// Parse ReplayDateTime from a filename like `01_20260204120000` (channel_YYYYMMDDHHmmss).
fn parse_seek_time_from_name(name: &str) -> Option<ReplayDateTime> {
    let digits: Vec<u8> = name
        .bytes()
        .filter(|b| b.is_ascii_digit())
        .map(|b| b - b'0')
        .collect();
    if digits.len() < 14 {
        return None;
    }
    let d = &digits[digits.len() - 14..];
    let year = (d[0] as i32) * 1000 + (d[1] as i32) * 100 + (d[2] as i32) * 10 + (d[3] as i32);
    let month = d[4] * 10 + d[5];
    let day = d[6] * 10 + d[7];
    let hour = d[8] * 10 + d[9];
    let minute = d[10] * 10 + d[11];
    let second = d[12] * 10 + d[13];
    if month == 0 || month > 12 || day == 0 || day > 31 || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some(ReplayDateTime {
        year,
        month,
        day,
        hour,
        minute,
        second,
    })
}

impl BcCamera {
    /// Get day records in a time range (MSG 142). Returns which days have recordings.
    pub async fn get_day_records(
        &self,
        start_time: ReplayDateTime,
        end_time: ReplayDateTime,
    ) -> Result<DayRecords> {
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub = connection
            .subscribe(MSG_ID_DAY_RECORDS, msg_num)
            .await?;

        let day_records = DayRecords {
            version: xml_ver(),
            start_time: start_time.clone(),
            end_time: end_time.clone(),
            day_record_list: DayRecordList {
                day_record: vec![DayRecord {
                    index: 0,
                    channel_id: self.channel_id,
                }],
            },
            day_type_list: None,
        };

        let msg = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_DAY_RECORDS,
                channel_id: self.channel_id,
                msg_num,
                stream_type: 0,
                response_code: 0,
                class: 0x6414,
            },
            body: BcBody::ModernMsg(ModernMsg {
                extension: None,
                payload: Some(BcPayloads::BcXml(BcXml {
                    day_records: Some(day_records),
                    ..Default::default()
                })),
            }),
        };

        sub.send(msg).await?;
        let msg = sub.recv().await?;

        if msg.meta.response_code != 200 {
            return Err(Error::CameraServiceUnavailable {
                id: msg.meta.msg_id,
                code: msg.meta.response_code,
            });
        }

        if let BcBody::ModernMsg(ModernMsg {
            payload:
                Some(BcPayloads::BcXml(BcXml {
                    day_records: Some(ref data),
                    ..
                })),
            ..
        }) = msg.body
        {
            debug!(
                "DayRecords response: day_type_list present = {}, entries = {}; full response: {:?}",
                data.day_type_list.is_some(),
                data.day_type_list
                    .as_ref()
                    .map(|l| l.day_type.len())
                    .unwrap_or(0),
                data
            );
            Ok(data.clone())
        } else {
            Err(Error::UnintelligibleReply {
                _reply: std::sync::Arc::new(Box::new(msg)),
                why: "Expected DayRecords xml in response",
            })
        }
    }

    /// Get file list handle for a day (MSG 14). Returns a handle to use with get_file_list_by_handle.
    pub async fn get_file_list_handle(
        &self,
        stream_type: &str,
        record_type: &str,
        start_time: ReplayDateTime,
        end_time: ReplayDateTime,
    ) -> Result<FileInfo> {
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub = connection
            .subscribe(MSG_ID_REPLAY_FILE_LIST_HANDLE, msg_num)
            .await?;

        let file_info = FileInfo {
            channel_id: Some(self.channel_id),
            record_type: Some(record_type.to_string()),
            support_sub: Some(0),
            start_time: Some(start_time.clone()),
            end_time: Some(end_time.clone()),
            stream_type: Some(stream_type.to_string()),
            ..Default::default()
        };

        let xml = BcXml {
            file_info_list: Some(FileInfoList {
                version: Some(xml_ver()),
                file_info: vec![file_info],
            }),
            ..Default::default()
        };

        let msg = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_REPLAY_FILE_LIST_HANDLE,
                channel_id: self.channel_id,
                msg_num,
                stream_type: 0,
                response_code: 0,
                class: 0x6414,
            },
            body: BcBody::ModernMsg(ModernMsg {
                extension: None,
                payload: Some(BcPayloads::BcXml(xml)),
            }),
        };

        sub.send(msg).await?;
        let msg = sub.recv().await?;

        if msg.meta.response_code != 200 {
            return Err(Error::CameraServiceUnavailable {
                id: msg.meta.msg_id,
                code: msg.meta.response_code,
            });
        }

        if let BcBody::ModernMsg(ModernMsg {
            payload:
                Some(BcPayloads::BcXml(BcXml {
                    file_info_list: Some(ref list),
                    ..
                })),
            ..
        }) = msg.body
        {
            if list.file_info.is_empty() {
                return Err(Error::UnintelligibleReply {
                    _reply: std::sync::Arc::new(Box::new(msg)),
                    why: "FileInfoList response had no FileInfo",
                });
            }
            Ok(list.file_info[0].clone())
        } else {
            Err(Error::UnintelligibleReply {
                _reply: std::sync::Arc::new(Box::new(msg)),
                why: "Expected FileInfoList xml in response",
            })
        }
    }

    /// Fetch one page of files by handle (MSG 15). Returns the file list for this page.
    /// An empty vec means no more pages.
    async fn get_file_list_page(&self, handle: u32) -> Result<Vec<FileInfo>> {
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub = connection
            .subscribe(MSG_ID_REPLAY_FILE_LIST, msg_num)
            .await?;

        let file_info = FileInfo {
            channel_id: Some(self.channel_id),
            handle: Some(handle),
            ..Default::default()
        };

        let xml = BcXml {
            file_info_list: Some(FileInfoList {
                version: Some(xml_ver()),
                file_info: vec![file_info],
            }),
            ..Default::default()
        };

        let msg = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_REPLAY_FILE_LIST,
                channel_id: self.channel_id,
                msg_num,
                stream_type: 0,
                response_code: 0,
                class: 0x6414,
            },
            body: BcBody::ModernMsg(ModernMsg {
                extension: None,
                payload: Some(BcPayloads::BcXml(xml)),
            }),
        };

        sub.send(msg).await?;
        let msg = sub.recv().await?;

        if msg.meta.response_code != 200 {
            // Non-200 signals end of results (same pattern as alarm_video_search_next)
            log::debug!(
                "get_file_list_page: response {} — treating as end of list",
                msg.meta.response_code
            );
            return Ok(vec![]);
        }

        if let BcBody::ModernMsg(ModernMsg {
            payload:
                Some(BcPayloads::BcXml(BcXml {
                    file_info_list: Some(ref list),
                    ..
                })),
            ..
        }) = msg.body
        {
            Ok(list.file_info.clone())
        } else {
            Err(Error::UnintelligibleReply {
                _reply: std::sync::Arc::new(Box::new(msg)),
                why: "Expected FileInfoList xml in response",
            })
        }
    }

    /// List all files by handle from get_file_list_handle (MSG 15), paginating until empty.
    pub async fn get_file_list_by_handle(&self, handle: u32) -> Result<Vec<FileInfo>> {
        let mut all_files = Vec::new();
        let mut page = 0usize;
        loop {
            let batch = self.get_file_list_page(handle).await?;
            if batch.is_empty() {
                log::debug!("get_file_list_by_handle: {} total files in {} pages", all_files.len(), page);
                break;
            }
            page += 1;
            log::debug!("get_file_list_by_handle: page {} returned {} files", page, batch.len());
            all_files.extend(batch);
        }
        Ok(all_files)
    }

    /// Alarm video search: START (MSG 175). Returns the response containing a fileHandle.
    /// `alarm_types` is a list of alarm/AI tags (e.g. ["md", "people", "vehicle"]).
    /// `stream_type_num` is 0 for mainStream, 1 for subStream.
    pub async fn alarm_video_search_start(
        &self,
        stream_type_num: u8,
        alarm_types: &[&str],
        start_time: ReplayDateTime,
        end_time: ReplayDateTime,
    ) -> Result<FindAlarmVideo> {
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub = connection
            .subscribe(MSG_ID_ALARM_VIDEO_SEARCH, msg_num)
            .await?;

        let alarm_type_str = alarm_types.join(", ");
        let xml = BcXml {
            find_alarm_video: Some(FindAlarmVideo {
                version: Some(xml_ver()),
                channel_id: Some(self.channel_id),
                file_handle: None,
                stream_type: Some(stream_type_num),
                not_search_video: Some(0),
                start_time: Some(start_time),
                end_time: Some(end_time),
                alarm_type: Some(alarm_type_str),
                event_alarm_type: Some(EventAlarmType {
                    items: alarm_types.iter().map(|s| s.to_string()).collect(),
                }),
            }),
            ..Default::default()
        };

        let msg = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_ALARM_VIDEO_SEARCH,
                channel_id: self.channel_id,
                msg_num,
                stream_type: 0,
                response_code: 0,
                class: 0x6414,
            },
            body: BcBody::ModernMsg(ModernMsg {
                extension: None,
                payload: Some(BcPayloads::BcXml(xml)),
            }),
        };

        sub.send(msg).await?;
        let msg = sub.recv().await?;

        if msg.meta.response_code != 200 {
            return Err(Error::CameraServiceUnavailable {
                id: msg.meta.msg_id,
                code: msg.meta.response_code,
            });
        }

        if let BcBody::ModernMsg(ModernMsg {
            payload:
                Some(BcPayloads::BcXml(BcXml {
                    find_alarm_video: Some(ref fav),
                    ..
                })),
            ..
        }) = msg.body
        {
            Ok(fav.clone())
        } else {
            Err(Error::UnintelligibleReply {
                _reply: std::sync::Arc::new(Box::new(msg)),
                why: "Expected findAlarmVideo xml in response",
            })
        }
    }

    /// Alarm video search: DO/paginate (MSG 175). Send a fileHandle to get the next batch of events.
    /// Returns the AlarmEventList from the response.
    pub async fn alarm_video_search_next(
        &self,
        file_handle: i32,
    ) -> Result<FindAlarmVideo> {
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub = connection
            .subscribe(MSG_ID_ALARM_VIDEO_SEARCH, msg_num)
            .await?;

        let xml = BcXml {
            find_alarm_video: Some(FindAlarmVideo {
                version: Some(xml_ver()),
                file_handle: Some(file_handle),
                ..Default::default()
            }),
            ..Default::default()
        };

        let msg = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_ALARM_VIDEO_SEARCH,
                channel_id: self.channel_id,
                msg_num,
                stream_type: 0,
                response_code: 0,
                class: 0x6414,
            },
            body: BcBody::ModernMsg(ModernMsg {
                extension: None,
                payload: Some(BcPayloads::BcXml(xml)),
            }),
        };

        sub.send(msg).await?;
        let msg = sub.recv().await?;

        if msg.meta.response_code != 200 {
            return Err(Error::CameraServiceUnavailable {
                id: msg.meta.msg_id,
                code: msg.meta.response_code,
            });
        }

        if let BcBody::ModernMsg(ModernMsg {
            payload:
                Some(BcPayloads::BcXml(BcXml {
                    find_alarm_video: Some(ref fav),
                    ..
                })),
            ..
        }) = msg.body
        {
            Ok(fav.clone())
        } else {
            Err(Error::UnintelligibleReply {
                _reply: std::sync::Arc::new(Box::new(msg)),
                why: "Expected findAlarmVideo xml in response",
            })
        }
    }

    /// Get file details by name (MSG 13). Use name from get_file_list_by_handle.
    /// App (FUN_1801740c0) sends supportSub=1 for subStream and only includes playSpeed when in [1,32].
    pub async fn get_file_by_name(
        &self,
        name: &str,
        support_sub: u8,
        play_speed: u32,
        stream_type: &str,
    ) -> Result<FileInfo> {
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub = connection
            .subscribe(MSG_ID_REPLAY_FILE_BY_NAME, msg_num)
            .await?;

        let support_sub = if stream_type == "subStream" { 1 } else { support_sub };
        let file_info = FileInfo {
            channel_id: Some(self.channel_id),
            name: Some(name.to_string()),
            support_sub: Some(support_sub),
            play_speed: (1..=32).contains(&play_speed).then_some(play_speed),
            stream_type: Some(stream_type.to_string()),
            ..Default::default()
        };

        let xml = BcXml {
            file_info_list: Some(FileInfoList {
                version: Some(xml_ver()),
                file_info: vec![file_info],
            }),
            ..Default::default()
        };

        let msg = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_REPLAY_FILE_BY_NAME,
                channel_id: self.channel_id,
                msg_num,
                stream_type: 0,
                response_code: 0,
                class: 0x6414,
            },
            body: BcBody::ModernMsg(ModernMsg {
                extension: None,
                payload: Some(BcPayloads::BcXml(xml)),
            }),
        };

        sub.send(msg).await?;
        let msg = sub.recv().await?;

        if msg.meta.response_code != 200 {
            return Err(Error::CameraServiceUnavailable {
                id: msg.meta.msg_id,
                code: msg.meta.response_code,
            });
        }

        if let BcBody::ModernMsg(ModernMsg {
            payload:
                Some(BcPayloads::BcXml(BcXml {
                    file_info_list: Some(ref list),
                    ..
                })),
            ..
        }) = msg.body
        {
            if list.file_info.is_empty() {
                return Err(Error::UnintelligibleReply {
                    _reply: std::sync::Arc::new(Box::new(msg)),
                    why: "FileInfoList response had no FileInfo",
                });
            }
            Ok(list.file_info[0].clone())
        } else {
            Err(Error::UnintelligibleReply {
                _reply: std::sync::Arc::new(Box::new(msg)),
                why: "Expected FileInfoList xml in response",
            })
        }
    }

    /// Get file metadata (duration, size) from the file list for a given replay file name.
    /// Does seek for the file's day, then fetches the file list and finds the matching entry.
    /// Returns None if the name cannot be parsed (no date) or the file is not in the list.
    pub async fn get_replay_file_metadata(
        &self,
        name: &str,
        stream_type: &str,
        record_type: &str,
    ) -> Result<Option<FileInfo>> {
        let seek_time = match parse_seek_time_from_name(name) {
            Some(t) => t,
            None => return Ok(None),
        };
        let day_start = ReplayDateTime {
            hour: 0,
            minute: 0,
            second: 0,
            ..seek_time.clone()
        };
        let day_end = ReplayDateTime {
            hour: 23,
            minute: 59,
            second: 59,
            ..seek_time
        };
        let seq = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs() as u32)
            .unwrap_or(0);
        self.replay_seek(seq, day_start.clone()).await?;
        let handle_info = self
            .get_file_list_handle(stream_type, record_type, day_start, day_end)
            .await?;
        let handle = match handle_info.handle {
            Some(h) => h,
            None => return Ok(None),
        };
        let files = self.get_file_list_by_handle(handle).await?;
        let found = files
            .into_iter()
            .find(|f| f.name.as_deref() == Some(name));
        Ok(found)
    }

    /// Get replay file duration in seconds from the file list (start_time..end_time).
    /// Returns None if metadata cannot be obtained or start/end time are missing.
    pub async fn get_replay_file_duration_secs(
        &self,
        name: &str,
        stream_type: &str,
        record_type: &str,
    ) -> Result<Option<u64>> {
        let meta = self
            .get_replay_file_metadata(name, stream_type, record_type)
            .await?;
        let duration = meta.and_then(|info| {
            let start = info.start_time.as_ref()?;
            let end = info.end_time.as_ref()?;
            replay_datetime_duration_secs(start, end)
        });
        Ok(duration)
    }

    /// Replay seek: prepare playback position (MSG 123). seq is epoch seconds; seek_time from file list.
    pub async fn replay_seek(
        &self,
        seq: u32,
        seek_time: ReplayDateTime,
    ) -> Result<()> {
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub = connection
            .subscribe(MSG_ID_REPLAY_SEEK, msg_num)
            .await?;

        let replay_seek = ReplaySeek {
            version: xml_ver(),
            channel_id: self.channel_id,
            seq,
            seek_time,
        };

        let msg = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_REPLAY_SEEK,
                channel_id: self.channel_id,
                msg_num,
                stream_type: 0,
                response_code: 0,
                class: 0x6414,
            },
            body: BcBody::ModernMsg(ModernMsg {
                extension: None,
                payload: Some(BcPayloads::BcXml(BcXml {
                    replay_seek: Some(replay_seek),
                    ..Default::default()
                })),
            }),
        };

        sub.send(msg).await?;
        let msg = sub.recv().await?;

        if msg.meta.response_code == 200 {
            Ok(())
        } else {
            Err(Error::CameraServiceUnavailable {
                id: msg.meta.msg_id,
                code: msg.meta.response_code,
            })
        }
    }

    /// Start replay playback (MSG 5). Returns a stream of BCMedia; when dropped, sends stop (MSG 7).
    ///
    /// Caller should have obtained `name` from `get_file_list_by_handle`. The protocol requires
    /// ReplaySeek (MSG 123) before get_file_by_name (MSG 13); we parse seek time from the
    /// filename (e.g. `01_20260204120000` → 2026-02-04 12:00:00) when possible.
    ///
    /// If `expected_size` is set (e.g. from file list size_l/size_h), or the 32-byte replay
    /// header contains a plausible size at +0x10/+0x18 (per BaichuanDownloader RE), we stop when
    /// payload bytes received reach that size, since some cameras (e.g. E1) never send response 300.
    pub async fn start_replay(
        &self,
        name: &str,
        stream_type: &str,
        play_speed: u32,
        strict: bool,
        buffer_size: usize,
        dump_replay: Option<std::path::PathBuf>,
        dump_replay_limit: Option<usize>,
        expected_size: Option<u64>,
    ) -> Result<StreamData> {
        let seq = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs() as u32)
            .unwrap_or(0);

        // Protocol order: ReplaySeek (123) before get_file_by_name (13). Use seek time from
        // filename (e.g. 01_20260204120000 → 2026-02-04 12:00:00) or start of today.
        let seek_time = parse_seek_time_from_name(name).unwrap_or_else(|| {
            let now = time::OffsetDateTime::now_utc();
            ReplayDateTime {
                year: now.year(),
                month: now.month() as u8,
                day: now.day() as u8,
                hour: 0,
                minute: 0,
                second: 0,
            }
        });
        self.replay_seek(seq, seek_time.clone()).await?;
        log::info!("Replay: seek (MSG 123) done");

        // Use parsed time as file start_time for MSG 5 when possible, to avoid MSG 13 (get_file_by_name)
        // which some cameras (e.g. E1) reject with 400.
        let start_time = if let Some(parsed) = parse_seek_time_from_name(name) {
            parsed
        } else {
            let file_info = self
                .get_file_by_name(name, 0, play_speed, stream_type)
                .await?;
            file_info.start_time.clone().ok_or_else(|| {
                Error::UnintelligibleReply {
                    _reply: std::sync::Arc::new(Box::new(Bc::new_from_meta(BcMeta {
                        msg_id: 0,
                        channel_id: 0,
                        stream_type: 0,
                        response_code: 0,
                        msg_num: 0,
                        class: 0,
                    }))),
                    why: "FileInfo from camera had no startTime",
                }
            })?
        };

        log::info!("Replay: start_time {:04}-{:02}-{:02} {:02}:{:02}:{:02}", start_time.year, start_time.month, start_time.day, start_time.hour, start_time.minute, start_time.second);
        let connection = self.get_connection();
        let start_msg_num = self.new_message_num();
        let channel_id = self.channel_id;
        let name = name.to_string();
        let stream_type = stream_type.to_string();
        let dump_path = dump_replay;
        let expected_size = expected_size;

        let buffer_size = if buffer_size == 0 { 100 } else { buffer_size };
        const DEFAULT_DUMP_LIMIT: usize = 131072;
        let dump_limit = dump_replay_limit.unwrap_or(DEFAULT_DUMP_LIMIT);
        let (tx, rx) = channel(buffer_size);
        let abort_handle = CancellationToken::new();
        let abort_handle_thread = abort_handle.clone();

        let handle = task::spawn(async move {
            // Optional dump of replay stream (after 32-byte header). limit 0 = full stream.
            let mut dump_file = None::<(tokio::fs::File, Option<usize>)>;
            if let Some(ref p) = dump_path {
                match tokio::fs::File::create(p).await {
                    Ok(f) => {
                        let limit_str = if dump_limit == 0 {
                            "full stream".to_string()
                        } else {
                            format!("first {} bytes", dump_limit)
                        };
                        log::info!(
                            "Replay: will dump {} of raw replay data (after 32-byte header) to {}",
                            limit_str,
                            p.display()
                        );
                        dump_file = Some((f, if dump_limit == 0 { None } else { Some(dump_limit) }));
                    }
                    Err(e) => log::warn!("Replay: could not create dump file {}: {}", p.display(), e),
                }
            }

            // Try MSG 5 first (replay file download as in app pcap: 123 + 5). Some cameras (e.g. E1)
            // return 400 for MSG 5; then try MSG 8 (same FileInfoList body). If MSG 8 returns 400, try desktop (0x17d).
            // Body: FileInfoList version="1.1" → FileInfo with uid, name, channelId, supportSub, streamType, startTime.
            let mut msg_id = MSG_ID_REPLAY_START; // 5 = try first (pcap flow)
            let mut sub = connection
                .subscribe(msg_id, start_msg_num)
                .await?;

            let support_sub = if stream_type.as_str() == "subStream" { 1 } else { 0 };
            let file_info = FileInfo {
                uid: Some(0),
                id: None,
                name: Some(name.clone()),
                channel_id: Some(channel_id),
                support_sub: Some(support_sub),
                stream_type: Some(stream_type.clone()),
                start_time: Some(start_time),
                ..Default::default()
            };
            let xml = BcXml {
                file_info_list: Some(FileInfoList {
                    version: Some(xml_ver()),
                    file_info: vec![file_info.clone()],
                }),
                ..Default::default()
            };
            let msg = Bc {
                meta: BcMeta {
                    msg_id,
                    channel_id,
                    msg_num: start_msg_num,
                    stream_type: 0,
                    response_code: 0,
                    class: 0x6414,
                },
                body: BcBody::ModernMsg(ModernMsg {
                    extension: None,
                    payload: Some(BcPayloads::BcXml(xml)),
                }),
            };
            log::info!(
                "Replay: sending MSG {} start_replay name={} channel={} streamType={}",
                msg_id,
                name,
                channel_id,
                stream_type
            );
            sub.send(msg).await?;
            log::info!("Replay: MSG {} sent, waiting for first response...", msg_id);

            let mut first = true;
            let mut codec = BcMediaCodex::new(strict);
            let mut buf = BytesMut::new();
            // When set, camera sends MP4 (or other container) instead of BcMedia; forward raw bytes.
            let mut raw_replay_mode = false;
            // First packet (32 bytes) may be replay header or start of file; keep and send when entering raw mode so client has full stream.
            let mut pending_first_chunk: Option<Vec<u8>> = None;

            let mut packet_count = 0u32;
            let mut total_binary_bytes = 0usize;
            // App only writes binary when response_code matches expected. Some cameras (e.g. E1) use 200 for accept then 54778 (0xD5DA) for streaming; accept any code we've seen with binary.
            let mut accepted_stream_response_codes: HashSet<u16> = HashSet::new();
            // End when payload (excluding 32-byte header) reaches this size. From file list or 32-byte header (RE: BaichuanDownloader 0xe48 / header +0x10, +0x18).
            let mut expected_payload_size = expected_size;
            const RECV_TIMEOUT_SECS: u64 = 15;
            let recv_timeout = tokio::time::Duration::from_secs(RECV_TIMEOUT_SECS);
            'recv_loop: loop {
                tokio::select! {
                    _ = abort_handle_thread.cancelled() => break 'recv_loop,
                    msg_res = tokio::time::timeout(recv_timeout, sub.recv()) => {
                        let msg_res = match msg_res {
                            Err(_) => {
                                log::info!(
                                    "Replay: no data for {}s (camera may have stopped without 300), finishing ({} packets, {} bytes)",
                                    RECV_TIMEOUT_SECS, packet_count, total_binary_bytes
                                );
                                let _ = tx.send(Ok(BcMedia::StreamEnd)).await;
                                break 'recv_loop;
                            }
                            Ok(r) => r,
                        };
                        let msg = match msg_res {
                            Err(e) => {
                                log::info!(
                                    "Replay stream ended: recv error ({} packets, {} bytes binary): {:?}",
                                    packet_count, total_binary_bytes, e
                                );
                                log::info!(
                                    "Replay failed: verify protocol in Ghidra via MCP (see GHIDRA_CHECKLIST.md), do not guess"
                                );
                                return Err(e);
                            }
                            Ok(m) => m,
                        };
                        let body_type = match &msg.body {
                            BcBody::ModernMsg(ModernMsg { payload: Some(BcPayloads::Binary(d)), .. }) => {
                                format!("binary({} bytes)", d.len())
                            }
                            BcBody::ModernMsg(ModernMsg { payload: Some(BcPayloads::BcXml(_)), .. }) => "xml".to_string(),
                            _ => "other".to_string(),
                        };
                        log::debug!(
                            "Replay: recv msg_id={} msg_num={} response_code={} body={}",
                            msg.meta.msg_id,
                            msg.meta.msg_num,
                            msg.meta.response_code,
                            body_type
                        );
                        if first {
                            first = false;
                            if msg.meta.response_code == 400 && msg_id == MSG_ID_REPLAY_START {
                                // Camera rejected MSG 5; try MSG 8 (e.g. E1).
                                log::info!(
                                    "Replay: camera returned 400 for MSG 5, trying MSG 8..."
                                );
                                drop(sub);
                                msg_id = MSG_ID_REPLAY_START_ALT;
                                sub = connection.subscribe(msg_id, start_msg_num).await?;
                                let xml8 = BcXml {
                                    file_info_list: Some(FileInfoList {
                                        version: Some(xml_ver()),
                                        file_info: vec![file_info.clone()],
                                    }),
                                    ..Default::default()
                                };
                                let msg8 = Bc {
                                    meta: BcMeta {
                                        msg_id,
                                        channel_id,
                                        msg_num: start_msg_num,
                                        stream_type: 0,
                                        response_code: 0,
                                        class: 0x6414,
                                    },
                                    body: BcBody::ModernMsg(ModernMsg {
                                        extension: None,
                                        payload: Some(BcPayloads::BcXml(xml8)),
                                    }),
                                };
                                sub.send(msg8).await?;
                                log::info!("Replay: MSG 8 sent, waiting for first response...");
                                first = true;
                                continue 'recv_loop;
                            }
                            if msg.meta.response_code == 400 && msg_id == MSG_ID_REPLAY_START_ALT {
                                // Camera rejected MSG 8; try desktop binary replay (CMD 0x17d, 0x944 payload).
                                log::info!(
                                    "Replay: camera returned 400 for MSG 8, trying desktop replay (0x17d)..."
                                );
                                msg_id = MSG_ID_REPLAY_DESKTOP; // prevent re-entry if 0x17d also returns 400
                                drop(sub);
                                let sub_desktop = connection
                                    .subscribe(MSG_ID_REPLAY_DESKTOP, start_msg_num)
                                    .await?;
                                let payload = build_desktop_replay_payload(channel_id, &name);
                                let desktop_msg = Bc {
                                    meta: BcMeta {
                                        msg_id: MSG_ID_REPLAY_DESKTOP,
                                        channel_id,
                                        msg_num: start_msg_num,
                                        stream_type: 0,
                                        response_code: 0,
                                        class: 0x6414,
                                    },
                                    body: BcBody::ModernMsg(ModernMsg {
                                        extension: None,
                                        payload: Some(BcPayloads::Binary(payload)),
                                    }),
                                };
                                sub_desktop.send(desktop_msg).await?;
                                log::info!("Replay: desktop 0x17d sent, waiting for response...");
                                sub = sub_desktop;
                                first = true;
                                continue 'recv_loop;
                            }
                            if msg.meta.response_code != 200 {
                                log::info!(
                                    "Replay start rejected by camera: response_code={}",
                                    msg.meta.response_code
                                );
                                return Err(Error::UnintelligibleReply {
                                    _reply: std::sync::Arc::new(Box::new(msg)),
                                    why: "Camera did not accept replay start",
                                });
                            }
                            log::info!("Replay: camera accepted (200), streaming...");
                            accepted_stream_response_codes.insert(200);
                            // Tell consumer which msg_id was accepted so it can skip first 32 bytes only for MSG 5 (app parity).
                            let _ = tx.send(Ok(BcMedia::ReplayStarted(msg_id as u16))).await;
                        } else if msg.meta.response_code == 300 || msg.meta.response_code == 331 {
                            // Camera signals end of file (300 = by-name; 331 = by-time). Notify consumer then stop.
                            log::info!(
                                "Replay/Download: response {} (end of file), stopping ({} packets, {} bytes)",
                                msg.meta.response_code, packet_count, total_binary_bytes
                            );
                            let _ = tx.send(Ok(BcMedia::StreamEnd)).await;
                            break 'recv_loop;
                        }
                        if let BcBody::ModernMsg(ModernMsg {
                            payload: Some(BcPayloads::Binary(ref data)),
                            ..
                        }) = msg.body
                        {
                            // Accept binary when response_code is in accepted set (200 from accept). E1 and others use a different code (e.g. 54778) for streaming; add it when first seen.
                            let code = msg.meta.response_code;
                            if !accepted_stream_response_codes.contains(&code) {
                                if code == 300 || code == 331 {
                                    // End codes handled above
                                } else {
                                    accepted_stream_response_codes.insert(code);
                                    log::debug!("Replay: accepting streaming response_code {} (added to accepted set)", code);
                                }
                            }
                            if accepted_stream_response_codes.contains(&code) {
                            packet_count += 1;
                            total_binary_bytes += data.len();

                            // Dump stream (after 32-byte header). remaining None = unlimited.
                            if let Some((ref mut f, ref mut remaining)) = dump_file {
                                if packet_count >= 2 {
                                    let to_write = match *remaining {
                                        None => data.len(),
                                        Some(r) if r > 0 => data.len().min(r),
                                        _ => 0,
                                    };
                                    if to_write > 0 {
                                        if let Err(e) = f.write_all(&data[..to_write]).await {
                                            log::warn!("Replay: dump write error: {}", e);
                                            dump_file = None;
                                        } else if let Some(ref mut r) = remaining {
                                            *r -= to_write;
                                            if *r == 0 {
                                                let _ = f.flush().await;
                                                log::info!("Replay: reached dump limit, closed dump file");
                                                dump_file = None;
                                            }
                                        }
                                    }
                                }
                            }

                            if packet_count == 1 {
                                // Some cameras (e.g. E1) send a 32-byte replay header first; BcMedia
                                // stream starts in the next packet. Skip the header so the codec
                                // sees the real stream start. Keep it for raw mode so client gets full stream (may be ftyp).
                                const REPLAY_HEADER_LEN: usize = 32;
                                if data.len() == REPLAY_HEADER_LEN {
                                    log::info!(
                                        "Replay: skipping {} byte replay header (first packet)",
                                        REPLAY_HEADER_LEN
                                    );
                                    log::debug!(
                                        "Replay: header hex: {:02x?}",
                                        &data[..data.len().min(64)]
                                    );
                                    pending_first_chunk = Some(data.to_vec());
                                    // Parse possible file size from header (RE: 32-byte layout +0x10 uint64, +0x18 uint64; BaichuanDownloader uses fileSize at 0xe48).
                                    if expected_payload_size.is_none() && data.len() >= 32 {
                                        let size_at_10 = u64::from_le_bytes(data[16..24].try_into().unwrap());
                                        let size_at_18 = u64::from_le_bytes(data[24..32].try_into().unwrap());
                                        const MAX_PLAUSIBLE: u64 = 500_000_000;
                                        if size_at_10 > 0 && size_at_10 <= MAX_PLAUSIBLE {
                                            expected_payload_size = Some(size_at_10);
                                            log::info!("Replay: using expected size from header +0x10: {} bytes", size_at_10);
                                        } else if size_at_18 > 0 && size_at_18 <= MAX_PLAUSIBLE {
                                            expected_payload_size = Some(size_at_18);
                                            log::info!("Replay: using expected size from header +0x18: {} bytes", size_at_18);
                                        }
                                    }
                                } else {
                                    buf.extend_from_slice(&data);
                                }
                            } else {
                                // Detect MP4 (ISO Base Media): first payload after header has "ftyp" at 4..8.
                                if packet_count == 2 && data.len() >= 8 && data[4..8] == *b"ftyp" {
                                    raw_replay_mode = true;
                                    log::info!(
                                        "Replay: stream is MP4/container format, forwarding raw bytes"
                                    );
                                }
                                // Some cameras (e.g. E1 with MSG 5) send MP4 that doesn't start with ftyp after
                                // decrypt, or use a different container; continuation packets can have
                                // response_code != 200. Treat as raw replay to avoid codec Nom errors and connection loss.
                                // Exception: don't trigger for BcMedia streams (non-E1 cameras like Argus 2).
                                if packet_count == 2 && data.len() >= 8 && data[4..8] != *b"ftyp" && data.len() > 512
                                    && !is_bcmedia_start(data)
                                {
                                    raw_replay_mode = true;
                                    log::info!(
                                        "Replay: packet 2 not ftyp (decrypted: {:02x?}), treating as container/raw",
                                        &data[0..8.min(data.len())]
                                    );
                                }
                                if raw_replay_mode {
                                    if let Some(chunk) = pending_first_chunk.take() {
                                        if tx.send(Ok(BcMedia::RawReplayChunk(chunk))).await.is_err() {
                                            break 'recv_loop;
                                        }
                                    }
                                    let payload = strip_e1_replay_envelope(&data);
                                    // E1 fix: raw replay (raw_replay_mode=true) often lacks start codes for NAL units (SPS/PPS/IDR/SLICE)
                                    // if the camera sends them as individual messages (common for E1).
                                    // If payload looks like a NAL (starts with valid NAL type) but has no start code, prepend one.
                                    let mut chunk_data = Vec::with_capacity(payload.len() + 4);
                                    let nal_type = if !payload.is_empty() { payload[0] & 0x1f } else { 0 };
                                    // 1=Slice, 5=IDR, 6=SEI, 7=SPS, 8=PPS, 9=AUD
                                    let is_video_nal = matches!(nal_type, 1 | 5 | 6 | 7 | 8 | 9);
                                    let has_start_code = (payload.len() >= 4 && payload[0..4] == [0, 0, 0, 1])
                                        || (payload.len() >= 3 && payload[0..3] == [0, 0, 1]);

                                    if is_video_nal && !has_start_code {
                                        // Insert Annex B start code so the raw stream is playable
                                        chunk_data.extend_from_slice(&[0, 0, 0, 1]);
                                    }
                                    chunk_data.extend_from_slice(payload);

                                    if tx.send(Ok(BcMedia::RawReplayChunk(chunk_data))).await.is_err() {
                                        break 'recv_loop;
                                    }
                                } else {
                                    if packet_count == 2 && data.len() >= 8 {
                                        log::info!(
                                            "Replay: packet 2 first 8 bytes (decrypted): {:02x?}",
                                            &data[0..8]
                                        );
                                    }
                                    buf.extend_from_slice(&data);
                                    while let Some(bc_media) = codec.decode(&mut buf)? {
                                        if tx.send(Ok(bc_media)).await.is_err() {
                                            break 'recv_loop;
                                        }
                                    }
                                }
                            }
                            if packet_count <= 3 || packet_count % 200 == 0 {
                                log::info!(
                                    "Replay: {} packets, {} KB received",
                                    packet_count,
                                    total_binary_bytes / 1024
                                );
                            }
                            // Size-based end: some cameras never send 300; stop when we have expected payload (RE: BaichuanDownloader curSize >= fileSize).
                            let payload_bytes = total_binary_bytes.saturating_sub(32) as u64;
                            if let Some(exp) = expected_payload_size {
                                if payload_bytes >= exp {
                                    log::info!(
                                        "Replay: received {} bytes (expected {}), finishing",
                                        payload_bytes, exp
                                    );
                                    let _ = tx.send(Ok(BcMedia::StreamEnd)).await;
                                    break 'recv_loop;
                                }
                            }
                            }
                        } else {
                            log::info!(
                                "Replay: message has no binary payload (response_code={})",
                                msg.meta.response_code
                            );
                        }
                    }
                }
            }
            // Flush dump file so any written bytes are persisted (e.g. when stream ends before 128KB).
            if let Some((ref mut f, _)) = dump_file {
                let _ = f.flush().await;
            }
            log::info!(
                "Replay stream ended: {} packets, {} bytes binary",
                packet_count, total_binary_bytes
            );

            // Do not send MSG 7 (stop) here. The caller (replay play command) sends it via
            // replay_stop() after receiving StreamEnd. Sending it here would compete for the same
            // connection and can deadlock: main holds the connection in replay_stop (waiting for
            // response) while this task blocks on subscribe(), so this task never exits and the
            // process hangs when joining the stream handle.

            Ok(())
        });

        Ok(StreamData::from_parts(handle, rx, abort_handle))
    }

    /// Start download by time range (MSG 143). Returns a stream of BCMedia; when dropped, sends stop (MSG 144).
    /// Same stream behaviour as replay: first packet 32 bytes = stream info (skipped), then binary data; end on response 331 (or 300).
    pub async fn start_download_by_time(
        &self,
        start_time: ReplayDateTime,
        end_time: ReplayDateTime,
        save_path: &str,
        stream_type: u32,
        strict: bool,
        buffer_size: usize,
        dump_replay: Option<std::path::PathBuf>,
        dump_replay_limit: Option<usize>,
    ) -> Result<StreamData> {
        let connection = self.get_connection();
        let start_msg_num = self.new_message_num();
        let stop_msg_num = self.new_message_num();
        let channel_id = self.channel_id;
        let uid = [0u8; 32];
        let payload = build_download_by_time_payload(
            channel_id,
            &uid[..],
            0,
            &start_time,
            &end_time,
            save_path,
            stream_type,
        );
        let buffer_size = if buffer_size == 0 { 100 } else { buffer_size };
        const DEFAULT_DUMP_LIMIT: usize = 131072;
        let dump_limit = dump_replay_limit.unwrap_or(DEFAULT_DUMP_LIMIT);
        let (tx, rx) = channel(buffer_size);
        let abort_handle = CancellationToken::new();
        let abort_handle_thread = abort_handle.clone();

        let handle = task::spawn(async move {
            let mut dump_file = None::<(tokio::fs::File, Option<usize>)>;
            if let Some(ref p) = dump_replay {
                if let Ok(f) = tokio::fs::File::create(p).await {
                    let limit_str = if dump_limit == 0 {
                        "full stream".to_string()
                    } else {
                        format!("first {} bytes", dump_limit)
                    };
                    log::info!(
                        "DownloadByTime: will dump {} (after 32-byte header) to {}",
                        limit_str,
                        p.display()
                    );
                    dump_file = Some((f, if dump_limit == 0 { None } else { Some(dump_limit) }));
                }
            }
            let mut sub = connection
                .subscribe(MSG_ID_DOWNLOAD_BY_TIME, start_msg_num)
                .await?;
            let msg = Bc {
                meta: BcMeta {
                    msg_id: MSG_ID_DOWNLOAD_BY_TIME,
                    channel_id,
                    msg_num: start_msg_num,
                    stream_type: 0,
                    response_code: 0,
                    class: 0x6414,
                },
                body: BcBody::ModernMsg(ModernMsg {
                    extension: None,
                    payload: Some(BcPayloads::Binary(payload)),
                }),
            };
            log::info!(
                "DownloadByTime: sending MSG 143 start_time {:?} end_time {:?}",
                start_time, end_time
            );
            sub.send(msg).await?;

            let mut first = true;
            let mut codec = BcMediaCodex::new(strict);
            let mut buf = BytesMut::new();
            let mut raw_replay_mode = false;
            let mut packet_count = 0u32;
            let mut total_binary_bytes = 0usize;
            let mut accepted_stream_response_codes: HashSet<u16> = HashSet::new();
            'recv_loop: loop {
                tokio::select! {
                    _ = abort_handle_thread.cancelled() => break 'recv_loop,
                    msg_res = sub.recv() => {
                        let msg = match msg_res {
                            Err(e) => {
                                log::info!(
                                    "DownloadByTime stream ended: recv error ({} packets, {} bytes): {:?}",
                                    packet_count, total_binary_bytes, e
                                );
                                log::info!(
                                    "Download failed: verify protocol in Ghidra via MCP (see GHIDRA_CHECKLIST.md), do not guess"
                                );
                                return Err(e);
                            }
                            Ok(m) => m,
                        };
                        if first {
                            first = false;
                            if msg.meta.response_code != 200 {
                                log::info!(
                                    "DownloadByTime rejected: response_code={}",
                                    msg.meta.response_code
                                );
                                return Err(Error::UnintelligibleReply {
                                    _reply: std::sync::Arc::new(Box::new(msg)),
                                    why: "Camera did not accept download-by-time start",
                                });
                            }
                            accepted_stream_response_codes.insert(200);
                            log::info!("DownloadByTime: camera accepted (200), streaming...");
                        } else if msg.meta.response_code == 300 || msg.meta.response_code == 331 {
                            log::info!(
                                "DownloadByTime: response {} (end), stopping ({} packets, {} bytes)",
                                msg.meta.response_code, packet_count, total_binary_bytes
                            );
                            let _ = tx.send(Ok(BcMedia::StreamEnd)).await;
                            break 'recv_loop;
                        }
                        if let BcBody::ModernMsg(ModernMsg {
                            payload: Some(BcPayloads::Binary(ref data)),
                            ..
                        }) = msg.body
                        {
                            let code = msg.meta.response_code;
                            if !accepted_stream_response_codes.contains(&code) {
                                if code != 300 && code != 331 {
                                    accepted_stream_response_codes.insert(code);
                                    log::debug!("DownloadByTime: accepting streaming response_code {} (added to accepted set)", code);
                                }
                            }
                            if accepted_stream_response_codes.contains(&code) {
                            packet_count += 1;
                            total_binary_bytes += data.len();
                            if let Some((ref mut f, ref mut remaining)) = dump_file {
                                if packet_count >= 2 {
                                    let to_write = match *remaining {
                                        None => data.len(),
                                        Some(r) if r > 0 => data.len().min(r),
                                        _ => 0,
                                    };
                                    if to_write > 0 {
                                        let _ = f.write_all(&data[..to_write]).await;
                                        if let Some(ref mut r) = remaining {
                                            *r = r.saturating_sub(to_write);
                                            if *r == 0 {
                                                log::info!("DownloadByTime: reached dump limit, closed dump file");
                                                dump_file = None;
                                            }
                                        }
                                    }
                                }
                            }
                            if packet_count == 1 {
                                const REPLAY_HEADER_LEN: usize = 32;
                                if data.len() == REPLAY_HEADER_LEN {
                                    log::info!("DownloadByTime: skipping 32-byte stream info");
                                } else {
                                    buf.extend_from_slice(data);
                                }
                            } else {
                                if packet_count == 2 && data.len() >= 8 && data[4..8] == *b"ftyp" {
                                    raw_replay_mode = true;
                                    log::info!("DownloadByTime: stream is MP4, forwarding raw bytes");
                                }
                                if raw_replay_mode {
                                    let payload = strip_e1_replay_envelope(data);
                                    if packet_count <= 3 {
                                        log::debug!(
                                            "E1 strip (DownloadByTime): pkt={} data_len={} payload_len={} data_first_64={:02x?} payload_first_64={:02x?}",
                                            packet_count,
                                            data.len(),
                                            payload.len(),
                                            &data[..data.len().min(64)],
                                            &payload[..payload.len().min(64)]
                                        );
                                    }
                                    if tx.send(Ok(BcMedia::RawReplayChunk(payload.to_vec()))).await.is_err() {
                                        break 'recv_loop;
                                    }
                                } else {
                                    buf.extend_from_slice(data);
                                    while let Some(bc_media) = codec.decode(&mut buf)? {
                                        if tx.send(Ok(bc_media)).await.is_err() {
                                            break 'recv_loop;
                                        }
                                    }
                                }
                            }
                            }
                        }
                    }
                }
            }
            if let Some((ref mut f, _)) = dump_file {
                let _ = f.flush().await;
            }
            let sub_stop = connection.subscribe(MSG_ID_DOWNLOAD_STOP, stop_msg_num).await?;
            let stop_msg = Bc {
                meta: BcMeta {
                    msg_id: MSG_ID_DOWNLOAD_STOP,
                    channel_id,
                    msg_num: stop_msg_num,
                    stream_type: 0,
                    response_code: 0,
                    class: 0x6414,
                },
                body: BcBody::ModernMsg(ModernMsg {
                    extension: None,
                    payload: None,
                }),
            };
            let _ = sub_stop.send(stop_msg).await;
            Ok(())
        });

        Ok(StreamData::from_parts(handle, rx, abort_handle))
    }

    /// Start direct file download by name (MSG 8, NET_DOWNLOAD_V20).
    ///
    /// Sends a binary `BC_DOWNLOAD_BY_NAME_INFO` payload (0xD48 bytes) to the camera.
    /// The camera streams the raw file content back (typically MP4). Stream ends on
    /// response 300 or 331. Stop is sent as MSG 9 (NET_DOWNLOAD_STOP_V20) when stream drops.
    ///
    /// Layout confirmed from Android SDK Ghidra analysis:
    ///   offset 0x00: iChannel (uint32_t)
    ///   offset 0x04: cUID (32 bytes, camera UID as ASCII string)
    ///   offset 0x24: cFileName (null-terminated C string, file name from file list)
    ///   total: 0xD48 = 3400 bytes (remaining fields zeroed)
    pub async fn start_download_file_by_name(
        &self,
        file_name: &str,
        strict: bool,
        buffer_size: usize,
        dump_replay: Option<std::path::PathBuf>,
        dump_replay_limit: Option<usize>,
    ) -> Result<StreamData> {
        let connection = self.get_connection();
        let start_msg_num = self.new_message_num();
        let stop_msg_num = self.new_message_num();
        let channel_id = self.channel_id;
        let uid = self.uid().await.unwrap_or_default();
        let payload = build_download_by_name_payload(channel_id, &uid, file_name);
        let buffer_size = if buffer_size == 0 { 100 } else { buffer_size };
        const DEFAULT_DUMP_LIMIT: usize = 131072;
        let dump_limit = dump_replay_limit.unwrap_or(DEFAULT_DUMP_LIMIT);
        let file_name = file_name.to_string();
        let (tx, rx) = channel(buffer_size);
        let abort_handle = CancellationToken::new();
        let abort_handle_thread = abort_handle.clone();

        let handle = task::spawn(async move {
            let mut dump_file = None::<(tokio::fs::File, Option<usize>)>;
            if let Some(ref p) = dump_replay {
                if let Ok(f) = tokio::fs::File::create(p).await {
                    let limit_str = if dump_limit == 0 {
                        "full stream".to_string()
                    } else {
                        format!("first {} bytes", dump_limit)
                    };
                    log::info!(
                        "DownloadByName: will dump {} to {}",
                        limit_str,
                        p.display()
                    );
                    dump_file = Some((f, if dump_limit == 0 { None } else { Some(dump_limit) }));
                }
            }
            let mut sub = connection
                .subscribe(MSG_ID_DOWNLOAD_FILE_BY_NAME, start_msg_num)
                .await?;
            let msg = Bc {
                meta: BcMeta {
                    msg_id: MSG_ID_DOWNLOAD_FILE_BY_NAME,
                    channel_id,
                    msg_num: start_msg_num,
                    stream_type: 0,
                    response_code: 0,
                    class: 0x6414,
                },
                body: BcBody::ModernMsg(ModernMsg {
                    extension: None,
                    payload: Some(BcPayloads::Binary(payload)),
                }),
            };
            log::info!("DownloadByName: sending MSG 8 for file '{}'", file_name);
            sub.send(msg).await?;

            let mut first = true;
            let mut codec = BcMediaCodex::new(strict);
            let mut buf = BytesMut::new();
            let mut raw_replay_mode = false;
            let mut packet_count = 0u32;
            let mut total_binary_bytes = 0usize;
            let mut accepted_stream_response_codes: HashSet<u16> = HashSet::new();
            'recv_loop: loop {
                tokio::select! {
                    _ = abort_handle_thread.cancelled() => break 'recv_loop,
                    msg_res = sub.recv() => {
                        let msg = match msg_res {
                            Err(e) => {
                                log::info!(
                                    "DownloadByName stream ended: recv error ({} packets, {} bytes): {:?}",
                                    packet_count, total_binary_bytes, e
                                );
                                return Err(e);
                            }
                            Ok(m) => m,
                        };
                        if first {
                            first = false;
                            if msg.meta.response_code != 200 {
                                log::info!(
                                    "DownloadByName rejected: response_code={} (camera may not support MSG 8 download-by-name)",
                                    msg.meta.response_code
                                );
                                return Err(Error::UnintelligibleReply {
                                    _reply: std::sync::Arc::new(Box::new(msg)),
                                    why: "Camera did not accept download-file-by-name (MSG 8)",
                                });
                            }
                            accepted_stream_response_codes.insert(200);
                            log::info!("DownloadByName: camera accepted (200), streaming file...");
                        } else if msg.meta.response_code == 300 || msg.meta.response_code == 331 {
                            log::info!(
                                "DownloadByName: response {} (end), file complete ({} packets, {} bytes)",
                                msg.meta.response_code, packet_count, total_binary_bytes
                            );
                            let _ = tx.send(Ok(BcMedia::StreamEnd)).await;
                            break 'recv_loop;
                        }
                        if let BcBody::ModernMsg(ModernMsg { payload: Some(BcPayloads::Binary(data)), .. }) = &msg.body {
                            let code = msg.meta.response_code;
                            if !accepted_stream_response_codes.contains(&code) {
                                if code != 300 && code != 331 {
                                    accepted_stream_response_codes.insert(code);
                                    log::debug!("DownloadByName: accepting streaming response_code {} (added to accepted set)", code);
                                }
                            }
                            if accepted_stream_response_codes.contains(&code) {
                                packet_count += 1;
                                total_binary_bytes += data.len();
                                if let Some((ref mut f, ref mut remaining)) = dump_file {
                                    let to_write = if let Some(r) = remaining {
                                        data.len().min(*r)
                                    } else {
                                        data.len()
                                    };
                                    if to_write > 0 {
                                        let _ = f.write_all(&data[..to_write]).await;
                                        if let Some(ref mut r) = remaining {
                                            *r = r.saturating_sub(to_write);
                                            if *r == 0 {
                                                log::info!("DownloadByName: reached dump limit, closed dump file");
                                                dump_file = None;
                                            }
                                        }
                                    }
                                }
                                if packet_count == 1 {
                                    const REPLAY_HEADER_LEN: usize = 32;
                                    if data.len() == REPLAY_HEADER_LEN {
                                        log::info!("DownloadByName: skipping 32-byte stream info header");
                                        continue;
                                    }
                                }
                                if packet_count == 2 && data.len() >= 8 && data[4..8] == *b"ftyp" {
                                    raw_replay_mode = true;
                                    log::info!("DownloadByName: stream is raw MP4 (ftyp), forwarding bytes directly");
                                }
                                if raw_replay_mode {
                                    let _ = tx.send(Ok(BcMedia::RawReplayChunk(data.clone()))).await;
                                } else {
                                    buf.extend_from_slice(data);
                                    loop {
                                        match codec.decode(&mut buf) {
                                            Ok(Some(frame)) => {
                                                if tx.send(Ok(frame)).await.is_err() {
                                                    break 'recv_loop;
                                                }
                                            }
                                            Ok(None) => break,
                                            Err(_e) => break,
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if let Some((ref mut f, _)) = dump_file {
                let _ = f.flush().await;
            }
            let sub_stop = connection.subscribe(MSG_ID_DOWNLOAD_FILE_STOP, stop_msg_num).await?;
            let stop_msg = Bc {
                meta: BcMeta {
                    msg_id: MSG_ID_DOWNLOAD_FILE_STOP,
                    channel_id,
                    msg_num: stop_msg_num,
                    stream_type: 0,
                    response_code: 0,
                    class: 0x6414,
                },
                body: BcBody::ModernMsg(ModernMsg {
                    extension: None,
                    payload: None,
                }),
            };
            let _ = sub_stop.send(stop_msg).await;
            Ok(())
        });

        Ok(StreamData::from_parts(handle, rx, abort_handle))
    }

    /// Stop replay (MSG 7). Pass channel and file name from the playing file.
    pub async fn replay_stop(&self, name: &str) -> Result<()> {
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub = connection
            .subscribe(MSG_ID_REPLAY_STOP, msg_num)
            .await?;

        let file_info = FileInfo {
            channel_id: Some(self.channel_id),
            name: Some(name.to_string()),
            ..Default::default()
        };

        let xml = BcXml {
            file_info_list: Some(FileInfoList {
                version: Some(xml_ver()),
                file_info: vec![file_info],
            }),
            ..Default::default()
        };

        let msg = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_REPLAY_STOP,
                channel_id: self.channel_id,
                msg_num,
                stream_type: 0,
                response_code: 0,
                class: 0x6414,
            },
            body: BcBody::ModernMsg(ModernMsg {
                extension: None,
                payload: Some(BcPayloads::BcXml(xml)),
            }),
        };

        sub.send(msg).await?;
        let msg = sub.recv().await?;

        if msg.meta.response_code == 200 {
            Ok(())
        } else {
            Err(Error::CameraServiceUnavailable {
                id: msg.meta.msg_id,
                code: msg.meta.response_code,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::strip_e1_replay_envelope;

    #[test]
    fn strip_e1_replay_envelope_returns_payload_after_xml() {
        let prefix_32: Vec<u8> = (0..32).map(|i| i as u8).collect();
        let xml = b"<?xml version=\"1.0\" ?>\n<Extension version=\"1.1\">\n<encryptLen>1024</encryptLen>\n</Extension>\n";
        let payload = b"ftypmp42\x00\x00\x00\x00";
        let body: Vec<u8> = prefix_32.iter().chain(xml).chain(payload.iter()).copied().collect();
        let out = strip_e1_replay_envelope(&body);
        assert_eq!(out, payload as &[u8], "should return only the payload after </Extension>\\n");
    }

    #[test]
    fn strip_e1_replay_envelope_passthrough_when_no_xml() {
        let data = b"ftypmp42\x00\x00\x00\x00";
        assert_eq!(strip_e1_replay_envelope(data), data as &[u8]);
        assert_eq!(strip_e1_replay_envelope(&data[..4]), &data[..4]);
    }

    #[test]
    fn strip_e1_replay_envelope_xml_at_start_no_32_prefix() {
        let xml = b"<?xml version=\"1.0\" ?>\n<Extension version=\"1.1\">\n<encryptLen>1024</encryptLen>\n</Extension>\n";
        let payload = b"ftypmp42\x00\x00\x00\x00";
        let body: Vec<u8> = xml.iter().chain(payload.iter()).copied().collect();
        let out = strip_e1_replay_envelope(&body);
        assert_eq!(out, payload as &[u8], "when body starts with XML (no 32-byte prefix), should return only payload");
    }
}
