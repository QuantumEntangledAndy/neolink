//!
//! # Neolink Replay
//!
//! List recording days and files from SD card, start/stop playback (MSG 142, 14, 15, 5, 7).
//!
//! # Usage
//!
//! ```bash
//! neolink replay days --config=config.toml CameraName --start 2024-02-01 [--end 2024-02-07]
//! neolink replay files --config=config.toml CameraName --date 2024-02-04 [--stream subStream]
//! neolink replay play --config=config.toml CameraName --name "01_20240204120000" [--output out.h264]
//! neolink replay stop --config=config.toml CameraName --name "01_20240204120000"
//! ```
//!

use anyhow::{Context, Result};
use std::convert::TryInto;
use std::path::Path;

mod cmdline;

#[cfg(feature = "gstreamer")]
mod gst;

/// Annex B start code (4-byte) for H.264/HEVC NAL.
const ANNEX_B_START: &[u8] = &[0x00, 0x00, 0x00, 0x01];

/// Scan for the first BcMedia 4-byte LE magic (InfoV1/2, IFRAME, PFRAME, AAC). Returns offset or None.
fn find_bcmedia_magic_offset(stream: &[u8]) -> Option<usize> {
    const MAGIC_INFO_V1: u32 = 0x31303031;
    const MAGIC_INFO_V2: u32 = 0x32303031;
    const MAGIC_IFRAME: u32 = 0x63643030;
    const MAGIC_IFRAME_LAST: u32 = 0x63643039;
    const MAGIC_PFRAME: u32 = 0x63643130;
    const MAGIC_PFRAME_LAST: u32 = 0x63643139;
    const MAGIC_AAC: u32 = 0x62773530;
    for i in 0..stream.len().saturating_sub(4) {
        let magic = u32::from_le_bytes(stream[i..i + 4].try_into().unwrap());
        if magic == MAGIC_INFO_V1
            || magic == MAGIC_INFO_V2
            || (MAGIC_IFRAME..=MAGIC_IFRAME_LAST).contains(&magic)
            || (MAGIC_PFRAME..=MAGIC_PFRAME_LAST).contains(&magic)
            || magic == MAGIC_AAC
        {
            return Some(i);
        }
    }
    None
}

/// True if payload looks like a valid H.264 NAL (NAL type 1–9). Drops garbage from resync false positives.
fn is_likely_valid_h264_nal(payload: &[u8]) -> bool {
    if payload.is_empty() {
        return false;
    }
    let first = if payload.len() >= 5 && payload[0..4] == [0x00, 0x00, 0x00, 0x01] {
        payload[4]
    } else if payload.len() >= 4 && payload[0..3] == [0x00, 0x00, 0x01] {
        payload[3]
    } else {
        payload[0]
    };
    if first >> 7 != 0 { return false; } // forbidden_zero_bit must be 0
    (1..=9).contains(&(first & 0x1F))
}

/// True if payload looks like a valid H.265 NAL. H.265 NAL header is 2 bytes;
/// nal_unit_type = (byte0 >> 1) & 0x3F; valid types are 0–47.
fn is_likely_valid_h265_nal(payload: &[u8]) -> bool {
    let first = if payload.len() >= 5 && payload[0..4] == [0x00, 0x00, 0x00, 0x01] {
        if payload.len() < 6 { return false; }
        payload[4]
    } else if payload.len() >= 4 && payload[0..3] == [0x00, 0x00, 0x01] {
        if payload.len() < 5 { return false; }
        payload[3]
    } else {
        if payload.is_empty() { return false; }
        payload[0]
    };
    if first >> 7 != 0 { return false; } // forbidden_zero_bit must be 0
    let nal_type = (first >> 1) & 0x3F;
    nal_type <= 47
}

/// Inspect actual NAL bytes to determine whether the stream is H.264 or H.265,
/// regardless of what the BcMedia header's video_type field claims.
///
/// Some cameras (e.g. Argus PT) send H.265 data but label it "H264" in the BcMedia header.
/// Detection is based on NAL unit type after start codes:
/// - H.265 VPS(32)/SPS(33)/PPS(34): first byte = 0x40/0x42/0x44 — H.265-specific, cannot appear in H.264
/// - H.264 SPS(7)/PPS(8)/IDR(5): first byte = 0x67/0x68/0x65
///
/// Returns Some(codec) if a distinctive NAL is found, None if indeterminate.
fn detect_codec_from_nal_bytes(data: &[u8]) -> Option<neolink_core::bcmedia::model::VideoType> {
    use neolink_core::bcmedia::model::VideoType;
    let mut i = 0;
    while i < data.len() {
        let nal_start = if data[i..].starts_with(&[0, 0, 0, 1]) {
            i + 4
        } else if data[i..].starts_with(&[0, 0, 1]) {
            i + 3
        } else {
            i += 1;
            continue;
        };
        if nal_start >= data.len() {
            break;
        }
        let byte0 = data[nal_start];
        if byte0 >> 7 != 0 {
            i = nal_start + 1;
            continue;
        }
        let h265_type = (byte0 >> 1) & 0x3F;
        if matches!(h265_type, 32..=34) {
            // VPS/SPS/PPS — only possible in H.265
            return Some(VideoType::H265);
        }
        let h264_type = byte0 & 0x1F;
        if matches!(h264_type, 5 | 7 | 8) {
            // IDR/SPS/PPS — confirms H.264
            return Some(VideoType::H264);
        }
        i = nal_start + 1;
    }
    None
}

/// Max bytes to advance on Incomplete without finding any NAL. Prevents hanging on garbage/ciphertext (e.g. undecrypted replay).
const BCMEDIA_RESYNC_CAP: usize = 128 * 1024;

/// Decode a buffer (e.g. after stream_for_mp4_assembly) as BcMedia and collect video NAL payloads.
/// Returns Some((nals, fps)) if at least one IFRAME/PFRAME was decoded; None if stream is not BcMedia or has no video.
/// fps is extracted from the BcMediaInfoV1/V2 header if present, otherwise defaults to 25.
/// When the codec returns Incomplete (Ok(None)) we resync to the next frame and continue, so a full buffer is decoded.
/// Only payloads that look like valid H.264 NALs are collected to avoid garbage from resync.
/// See notes/REPLAY_VIDEO_FORMAT.md.
/// Result of decoding a BcMedia stream.
struct BcMediaDecoded {
    nals: Vec<Vec<u8>>,
    fps: u8,
    /// Per-frame microsecond timestamps from BcMedia headers (same length as `nals`).
    timestamps_us: Vec<u32>,
    /// Concatenated AAC ADTS frames (empty if no audio in stream).
    aac_data: Vec<u8>,
    /// H.264 or H.265 — detected from the first IFrame/PFrame in the stream.
    video_codec: neolink_core::bcmedia::model::VideoType,
}

fn try_decode_bcmedia_nals(stream: &[u8]) -> Option<BcMediaDecoded> {
    use neolink_core::bcmedia::model::VideoType;
    let mut codec = BcMediaCodex::new(false);
    let mut buf = BytesMut::from(stream);
    let mut nals: Vec<Vec<u8>> = Vec::new();
    let mut timestamps_us: Vec<u32> = Vec::new();
    let mut fps: u8 = 25; // default; overridden by InfoV1/V2 if present
    let mut aac_data: Vec<u8> = Vec::new();
    let mut detected_codec: Option<VideoType> = None;
    let start_len = buf.len();
    let mut bytes_advanced_without_nal: usize = 0;
    loop {
        match codec.decode(&mut buf) {
            Ok(Some(BcMedia::Iframe(BcMediaIframe { data, microseconds, video_type, .. }))) => {
                if detected_codec.is_none() {
                    // Override header claim with NAL-level detection — some cameras (e.g. Argus PT)
                    // label H.265 streams as "H264" in BcMedia headers.
                    let actual = detect_codec_from_nal_bytes(&data).unwrap_or(video_type);
                    if actual != video_type {
                        log::warn!(
                            "Replay: BcMedia header says {:?} but NAL bytes indicate {:?} — using detected codec",
                            video_type, actual
                        );
                    }
                    detected_codec = Some(actual);
                    log::info!("Replay: BcMedia stream codec = {:?} (from first IFrame)", actual);
                }
                let effective_codec = detected_codec.unwrap_or(video_type);
                let valid = match effective_codec {
                    VideoType::H265 => is_likely_valid_h265_nal(&data),
                    VideoType::H264 => is_likely_valid_h264_nal(&data),
                };
                if valid {
                    nals.push(data);
                    timestamps_us.push(microseconds);
                    bytes_advanced_without_nal = 0;
                }
            }
            Ok(Some(BcMedia::Pframe(BcMediaPframe { data, microseconds, video_type, .. }))) => {
                let effective_codec = detected_codec.unwrap_or(video_type);
                let valid = match effective_codec {
                    VideoType::H265 => is_likely_valid_h265_nal(&data),
                    VideoType::H264 => is_likely_valid_h264_nal(&data),
                };
                if valid {
                    nals.push(data);
                    timestamps_us.push(microseconds);
                    bytes_advanced_without_nal = 0;
                }
            }
            Ok(Some(BcMedia::InfoV1(BcMediaInfoV1 { fps: f, .. })))
            | Ok(Some(BcMedia::InfoV2(BcMediaInfoV2 { fps: f, .. }))) => {
                if f > 0 {
                    log::info!("Replay: BcMedia stream fps={} (from InfoV1/V2 header)", f);
                    fps = f;
                }
            }
            Ok(Some(BcMedia::Aac(BcMediaAac { data, .. }))) => {
                aac_data.extend_from_slice(&data);
            }
            Ok(Some(_)) => {}
            Err(_) => break,
            Ok(None) => {
                if buf.remaining() < 4 {
                    break;
                }
                buf.advance(1);
                bytes_advanced_without_nal += 1;
                if nals.is_empty() && bytes_advanced_without_nal >= BCMEDIA_RESYNC_CAP {
                    log::debug!(
                        "Replay: BcMedia decode advanced {} bytes without finding a frame, stopping (likely garbage/undecrypted)",
                        start_len - buf.len()
                    );
                    break;
                }
            }
        }
    }
    if !aac_data.is_empty() {
        log::info!("Replay: collected {} bytes of AAC audio from BcMedia stream", aac_data.len());
    }
    if nals.is_empty() { None } else {
        let video_codec = detected_codec.unwrap_or(neolink_core::bcmedia::model::VideoType::H264);
        Some(BcMediaDecoded { nals, fps, timestamps_us, aac_data, video_codec })
    }
}

/// Compute actual average fps from per-frame microsecond timestamps. Returns None if < 2 frames.
fn compute_actual_fps(timestamps_us: &[u32]) -> Option<f64> {
    if timestamps_us.len() < 2 {
        return None;
    }
    // Handle u32 wraparound: timestamps are relative to stream start and may wrap at ~4295s (~71 min).
    // Use deltas between consecutive frames to avoid wraparound issues.
    let mut total_delta_us: u64 = 0;
    let mut valid_deltas: u64 = 0;
    for pair in timestamps_us.windows(2) {
        let delta = pair[1].wrapping_sub(pair[0]);
        // Reject implausible deltas (> 2 seconds or 0) as likely wraparound artifacts or duplicates.
        if delta > 0 && delta < 2_000_000 {
            total_delta_us += delta as u64;
            valid_deltas += 1;
        }
    }
    if valid_deltas == 0 || total_delta_us == 0 {
        return None;
    }
    let avg_frame_duration_us = total_delta_us as f64 / valid_deltas as f64;
    let fps = 1_000_000.0 / avg_frame_duration_us;
    // Sanity: fps should be in 1..120 range
    if fps >= 1.0 && fps <= 120.0 {
        Some(fps)
    } else {
        None
    }
}

/// Recording metadata for MP4 embedding.
#[derive(Debug, Default, Clone)]
struct RecordingMeta {
    /// Camera recording type / AI detection tags (e.g. "manual,sched,md,people,vehicle").
    pub record_type: Option<String>,
    /// Recording start time as "YYYY-MM-DD HH:MM:SS".
    pub start_time: Option<String>,
    /// Recording end time as "YYYY-MM-DD HH:MM:SS".
    pub end_time: Option<String>,
    /// Camera name / channel.
    pub camera_name: Option<String>,
}

/// Mux decoded BcMedia NALs + optional AAC audio to MP4.
/// Uses GStreamer (with per-frame PTS from timestamps) when available; falls back to ffmpeg.
/// If `meta` is provided, AI detection tags and timing info are embedded in the MP4.
/// Returns Ok(true) on success.
async fn mux_to_mp4(
    nals: &[Vec<u8>],
    aac_data: &[u8],
    fps: u8,
    timestamps_us: &[u32],
    output: &Path,
    meta: &RecordingMeta,
    video_codec: neolink_core::bcmedia::model::VideoType,
) -> Result<bool> {
    use neolink_core::bcmedia::model::VideoType;
    if let Some(afps) = compute_actual_fps(timestamps_us) {
        log::info!(
            "Replay: actual avg fps from timestamps = {:.2} (declared fps = {})",
            afps, fps
        );
    }

    // Primary: GStreamer with per-frame PTS (handles VFR + audio natively, no external binaries)
    #[cfg(feature = "gstreamer")]
    {
        let nals = nals.to_vec();
        let timestamps_us = timestamps_us.to_vec();
        let aac_data = aac_data.to_vec();
        let path = output.to_path_buf();
        let gst_meta = gst::Mp4Metadata {
            record_type: meta.record_type.clone(),
            start_time: meta.start_time.clone(),
            end_time: meta.end_time.clone(),
            camera_name: meta.camera_name.clone(),
        };
        let is_h265 = matches!(video_codec, VideoType::H265);
        match tokio::task::spawn_blocking(move || {
            gst::mux_nals_to_mp4(&nals, &timestamps_us, &aac_data, &path, &gst_meta, is_h265)
        })
        .await
        {
            Ok(Ok(())) => return Ok(true),
            Ok(Err(e)) => log::warn!("Replay: GStreamer mux failed: {:?}, trying ffmpeg", e),
            Err(e) => log::warn!("Replay: GStreamer task panicked: {:?}, trying ffmpeg", e),
        }
    }

    // Fallback: ffmpeg with computed average fps
    let effective_fps = compute_actual_fps(timestamps_us).unwrap_or(fps as f64);
    let fps_str = format!("{:.3}", effective_fps);
    log::info!("Replay: muxing with ffmpeg at {} fps (fallback)", fps_str);

    let is_h265 = matches!(video_codec, VideoType::H265);
    let raw_ext = if is_h265 { "replay.h265" } else { "replay.h264" };
    let raw_fmt = if is_h265 { "hevc" } else { "h264" };
    let raw_path = output.with_extension(raw_ext);
    let annex_b = annex_b_from_nals(nals);
    tokio::fs::write(&raw_path, &annex_b).await.context("Write temp video")?;

    let aac_path = output.with_extension("replay.aac");
    if !aac_data.is_empty() {
        tokio::fs::write(&aac_path, aac_data).await.context("Write temp AAC")?;
    }

    let mut cmd = tokio::process::Command::new("ffmpeg");
    cmd.args(["-y", "-hide_banner", "-loglevel", "error",
              "-fflags", "+genpts", "-r", &fps_str, "-f", raw_fmt, "-i"]);
    cmd.arg(&raw_path);
    if !aac_data.is_empty() {
        cmd.args(["-f", "aac", "-i"]);
        cmd.arg(&aac_path);
    }
    cmd.args(["-c", "copy", "-fps_mode", "cfr", "-r", &fps_str, "-movflags", "+faststart"]);
    // Embed metadata
    if let Some(ref rt) = meta.record_type {
        cmd.args(["-metadata", &format!("comment=recordType: {}", rt)]);
    }
    if let Some(ref desc) = meta.start_time {
        cmd.args(["-metadata", &format!("description=Start: {}", desc)]);
    }
    cmd.arg(output);

    let status = cmd.status().await.context("Run ffmpeg for BcMedia mux")?;
    let _ = tokio::fs::remove_file(&raw_path).await;
    if !aac_data.is_empty() {
        let _ = tokio::fs::remove_file(&aac_path).await;
    }
    Ok(status.success())
}

/// Format a ReplayDateTime as "YYYY-MM-DD HH:MM:SS".
fn format_replay_datetime(dt: &ReplayDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second
    )
}

/// Write NAL payloads as H.264 Annex B (start code + NAL). Payloads that already start with 0x00 0x00 0x01 or 0x00 0x00 0x00 0x01 are written as-is.
fn annex_b_from_nals(nals: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for nal in nals {
        let has_start = (nal.len() >= 3 && nal[0..3] == [0x00, 0x00, 0x01])
            || (nal.len() >= 4 && nal[0..4] == [0x00, 0x00, 0x00, 0x01]);
        if !has_start {
            out.extend_from_slice(ANNEX_B_START);
        }
        out.extend_from_slice(nal);
    }
    out
}

/// H.264 Annex B start codes: 0x00 0x00 0x01 or 0x00 0x00 0x00 0x01.
/// Kept for potential use when scanning raw buffers for Annex B.
#[allow(dead_code)]
fn find_annexb_h264_start(buffer: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 3 <= buffer.len() {
        if buffer[i..i + 3] == [0x00, 0x00, 0x01] {
            return Some(i);
        }
        if i + 4 <= buffer.len() && buffer[i..i + 4] == [0x00, 0x00, 0x00, 0x01] {
            return Some(i);
        }
        i += 1;
    }
    None
}

use crate::common::NeoReactor;
use bytes::{Buf, BytesMut};
use neolink_core::bc::xml::{DayRecords, FileInfo, ReplayDateTime};
use neolink_core::bcmedia::codex::BcMediaCodex;
use neolink_core::bcmedia::model::{BcMedia, BcMediaAac, BcMediaIframe, BcMediaInfoV1, BcMediaInfoV2, BcMediaPframe};
use tokio::io::AsyncWriteExt;
use tokio::time::{sleep_until, Duration, Instant};
use tokio_util::codec::Decoder;

pub(crate) use cmdline::Opt;

/// E1 cameras sometimes write wrong values in the avcC box (e.g. config version 0, wrong lengthSizeMinusOne),
/// which breaks decoders. Patch the first bytes so the file can play when the rest (SPS/PPS) is present.
/// Provenance: empirical (no RE). App replay path does not handle raw MP4; download path builds MP4 from
/// DATA_FRAME_DESC, so we have no Ghidra trace of the app patching avcC. See notes § Replay: full end-to-end flow and provenance.
fn patch_e1_avcc_if_needed(path: &Path) -> Result<()> {
    const AVCC_TAG: &[u8; 4] = b"avcC";
    // avcC content: [0]=configurationVersion(1), [1-3]=profile/compat/level, [4]=lengthSizeMinusOne|reserved, [5]=numSPS|reserved
    const CONTENT_CONFIG_VERSION: usize = 0;
    const CONTENT_LENGTH_SIZE_BYTE: usize = 4;
    const CONFIG_VERSION_CORRECT: u8 = 0x01;
    const LENGTH_SIZE_BYTE_CORRECT: u8 = 0x43; // lengthSizeMinusOne=3 (4-byte NAL length)

    let mut buf = [0u8; 4096];
    let mut f = std::fs::File::open(path).context("Open MP4 for avcC patch")?;
    let n = std::io::Read::read(&mut f, &mut buf).context("Read MP4 head")?;
    let head = &buf[..n];
    if let Some(pos) = head.windows(4).position(|w| w == AVCC_TAG) {
        let content_start = pos + 4; // first byte of avcC content (after "avcC" tag)
        let mut patches: Vec<(usize, u8, u8)> = Vec::new();
        if content_start + CONTENT_LENGTH_SIZE_BYTE < head.len() {
            let b4 = head[content_start + CONTENT_LENGTH_SIZE_BYTE];
            if b4 != LENGTH_SIZE_BYTE_CORRECT {
                patches.push((content_start + CONTENT_LENGTH_SIZE_BYTE, b4, LENGTH_SIZE_BYTE_CORRECT));
            }
        }
        if content_start + CONTENT_CONFIG_VERSION < head.len() {
            let b0 = head[content_start + CONTENT_CONFIG_VERSION];
            if b0 != CONFIG_VERSION_CORRECT {
                patches.push((content_start + CONTENT_CONFIG_VERSION, b0, CONFIG_VERSION_CORRECT));
            }
        }
        if !patches.is_empty() {
            drop(f);
            use std::io::{Seek, SeekFrom, Write};
            let mut f = std::fs::OpenOptions::new().write(true).open(path)?;
            for (fix_pos, _was, correct) in &patches {
                f.seek(SeekFrom::Start(*fix_pos as u64))?;
                f.write_all(&[*correct])?;
            }
            f.sync_all().context("Sync patched MP4 to disk")?;
            for (_, w, c) in &patches {
                log::info!("Replay: patched E1 avcC (0x{:02x} -> 0x{:02x}) for playback", w, c);
            }
        }
    }
    Ok(())
}

/// Some cameras omit colour_primaries/transfer_characteristics/matrix_coefficients in the
/// avc1 sample entry, so ffmpeg reports "unspecified pixel format" and may not start decoding.
/// Write BT.709 (1,1,1) into the optional fields so players can infer yuv420p.
/// Only patch when avc1 box is >= 92 bytes so we don't overwrite the following avcC box.
/// Provenance: empirical (no RE). See notes § Replay: full end-to-end flow and provenance.
fn patch_avc1_colour_if_needed(path: &Path) -> Result<()> {
    const AVC1_TAG: &[u8; 4] = b"avc1";
    // Optional colour at 80..86; require box >= 92 so we don't overwrite avcC.
    const COLOUR_OFFSET_IN_AVC1_BOX: usize = 80;
    const MIN_AVC1_BOX_LEN: usize = 92;
    const COLOUR_BYTES: &[u8; 6] = &[0x00, 0x01, 0x00, 0x01, 0x00, 0x01]; // BT.709

    let mut buf = [0u8; 65536];
    let mut f = std::fs::File::open(path).context("Open MP4 for avc1 colour patch")?;
    let n = std::io::Read::read(&mut f, &mut buf).context("Read MP4 for avc1")?;
    let head = &buf[..n];

    let mut box_start = 0usize;
    while let Some(tag_pos) = head[box_start..].windows(4).position(|w| w == AVC1_TAG) {
        let i = box_start + tag_pos;
        if i < 4 {
            box_start = i + 1;
            continue;
        }
        let start = i - 4;
        let size_u32 = u32::from_be_bytes(head[start..start + 4].try_into().unwrap());
        let box_len = if size_u32 == 1 {
            if start + 16 > head.len() {
                box_start = i + 1;
                continue;
            }
            u64::from_be_bytes(head[start + 8..start + 16].try_into().unwrap()) as usize
        } else {
            size_u32 as usize
        };
        if box_len >= MIN_AVC1_BOX_LEN
            && start + COLOUR_OFFSET_IN_AVC1_BOX + COLOUR_BYTES.len() <= head.len()
        {
            let colour_pos = start + COLOUR_OFFSET_IN_AVC1_BOX;
            if head[colour_pos..colour_pos + COLOUR_BYTES.len()] != COLOUR_BYTES[..] {
                drop(f);
                let mut f = std::fs::OpenOptions::new().write(true).open(path)?;
                std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(colour_pos as u64))?;
                std::io::Write::write_all(&mut f, COLOUR_BYTES)?;
                f.sync_all().context("Sync avc1 colour patch")?;
                log::info!("Replay: patched avc1 colour description (BT.709) for playback");
                return Ok(());
            }
        }
        box_start = i + 1;
    }
    Ok(())
}

/// Parse box size at buffer[start]; supports 32-bit and size=1 (64-bit).
fn parse_box_size(buffer: &[u8], start: usize) -> Result<usize> {
    if buffer.len() < start + 8 {
        anyhow::bail!("Buffer too short for box header at {}", start);
    }
    let size_u32 = u32::from_be_bytes(buffer[start..start + 4].try_into().unwrap());
    if size_u32 == 1 {
        if buffer.len() < start + 16 {
            anyhow::bail!("Buffer too short for 64-bit box size");
        }
        let size64 = u64::from_be_bytes(buffer[start + 8..start + 16].try_into().unwrap());
        Ok(size64 as usize)
    } else {
        Ok(size_u32 as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal BcMedia IFRAME: magic 0x63643030, "H264", payload_size=5, additional_header_size=0,
    /// then 5-byte NAL (start code + SPS type) + 3-byte pad to 8-byte boundary.
    fn minimal_bcmedia_iframe() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&0x63643030u32.to_le_bytes()); // MAGIC IFRAME
        buf.extend_from_slice(b"H264");
        buf.extend_from_slice(&5u32.to_le_bytes());  // payload_size
        buf.extend_from_slice(&0u32.to_le_bytes()); // additional_header_size
        buf.extend_from_slice(&1000u32.to_le_bytes()); // microseconds = 1ms
        buf.extend_from_slice(&0u32.to_le_bytes()); // unknown_b
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x67]); // NAL: start code + SPS (type 7)
        buf.extend_from_slice(&[0u8; 3]); // padding to 8-byte boundary
        buf
    }

    #[test]
    fn test_try_decode_bcmedia_nals_decodes_iframe() {
        let stream = minimal_bcmedia_iframe();
        let result = try_decode_bcmedia_nals(&stream);
        assert!(result.is_some(), "BcMedia decode should return Some(BcMediaDecoded)");
        let decoded = result.unwrap();
        assert_eq!(decoded.nals.len(), 1);
        assert_eq!(decoded.nals[0], &[0x00, 0x00, 0x00, 0x01, 0x67]);
        assert_eq!(decoded.timestamps_us.len(), 1);
        assert_eq!(decoded.timestamps_us[0], 1000);
        assert_eq!(decoded.fps, 25, "Default fps should be 25 when no InfoV1/V2 header");
        assert!(decoded.aac_data.is_empty(), "No audio in video-only stream");
    }

    #[test]
    fn test_compute_actual_fps() {
        // 10 frames at exactly 100ms intervals = 10 fps
        let ts: Vec<u32> = (0..10).map(|i| i * 100_000).collect();
        let fps = compute_actual_fps(&ts).unwrap();
        assert!((fps - 10.0).abs() < 0.1, "Expected ~10 fps, got {}", fps);

        // 30 frames at ~33.3ms intervals = 30 fps
        let ts: Vec<u32> = (0..30).map(|i| i * 33_333).collect();
        let fps = compute_actual_fps(&ts).unwrap();
        assert!((fps - 30.0).abs() < 0.5, "Expected ~30 fps, got {}", fps);

        // < 2 frames → None
        assert!(compute_actual_fps(&[0]).is_none());
        assert!(compute_actual_fps(&[]).is_none());

        // All same timestamp → None (0 deltas rejected)
        assert!(compute_actual_fps(&[1000, 1000, 1000]).is_none());
    }

    #[test]
    fn test_stream_for_mp4_assembly_skips_32_when_no_ftyp() {
        let mut buf = vec![0u8; 64];
        buf[0..32].fill(0xab);
        buf[32..36].copy_from_slice(&0x63643030u32.to_le_bytes());
        buf[36..40].copy_from_slice(b"H264");
        let stream = stream_for_mp4_assembly(&buf, None);
        assert_eq!(stream.len(), 32);
        assert_eq!(stream[0..4], 0x63643030u32.to_le_bytes());
    }

    /// Decode a real replay dump if present. Generate with:
    ///   uv run python scripts/extract_replay_from_pcap_app.py --password PASSWORD --debug-dump /tmp/replay.bin PCAPdroid_*.pcap
    /// Then: REPLAY_DUMP=/tmp/replay.bin cargo test test_decode_real_replay_dump -- --ignored
    /// The script's dump is already reassembled with skip_first_32, so it starts with BcMedia (no leading 32-byte header).
    #[test]
    #[ignore]
    fn test_decode_real_replay_dump() {
        let path = std::env::var("REPLAY_DUMP").ok();
        let path = match path.as_deref() {
            Some(p) => std::path::Path::new(p),
            None => return,
        };
        let raw = std::fs::read(path).expect("read replay dump");
        // Script dump may have leading bytes before first BcMedia frame; find magic then decode.
        let stream = find_bcmedia_magic_offset(&raw)
            .map(|off| &raw[off..])
            .unwrap_or_else(|| stream_for_mp4_assembly(&raw, None));
        let decoded = try_decode_bcmedia_nals(stream).expect("BcMedia decode");
        assert!(!decoded.nals.is_empty(), "should get at least one video frame");
        eprintln!("Replay dump: {} NALs, fps={}, aac={} bytes", decoded.nals.len(), decoded.fps, decoded.aac_data.len());
        let annex_b = annex_b_from_nals(&decoded.nals);
        assert!(annex_b.len() > 4, "Annex B output non-empty");
    }
}

/// Replay stream used for MP4 assembly. If the buffer doesn't start with ftyp, we may skip
/// the first 32-byte replay header (app does this only for MSG 5; for 0x17d it writes the 32 bytes).
/// `skip_first_32`: Some(true) = skip 32 when no ftyp (MSG 5); Some(false) = never skip (MSG 8/0x17d); None = legacy (skip when no ftyp and len>32).
fn stream_for_mp4_assembly(buffer: &[u8], skip_first_32: Option<bool>) -> &[u8] {
    const REPLAY_HEADER_LEN: usize = 32;
    let starts_with_ftyp = buffer.len() >= 8 && buffer[4..8] == *b"ftyp";
    let should_skip = match skip_first_32 {
        Some(true) => buffer.len() > REPLAY_HEADER_LEN && !starts_with_ftyp,
        Some(false) => false,
        None => buffer.len() > REPLAY_HEADER_LEN && !starts_with_ftyp,
    };
    if should_skip {
        &buffer[REPLAY_HEADER_LEN..]
    } else {
        buffer
    }
}

/// ftyp box and return the slice from that box to the end. Accepts ftyp size 8..=1024 (ISO BMFF allows variable length).
/// Returns None if buffer already starts with ftyp or no valid ftyp found.
fn slice_from_ftyp(buffer: &[u8]) -> Option<&[u8]> {
    const FTYP_SIZE_MAX: u32 = 1024;
    if buffer.len() >= 8 && buffer[4..8] == *b"ftyp" {
        return None; // already starts with ftyp, caller uses buffer as-is
    }
    // Search for [size_be][b"ftyp"] so we can start MP4 from there
    let mut i = 0;
    let mut first_ftyp_at: Option<(usize, u32)> = None; // for diagnostic if no valid box found
    while i + 8 <= buffer.len() {
        if buffer[i + 4..i + 8] == *b"ftyp" {
            let size = u32::from_be_bytes(buffer[i..i + 4].try_into().unwrap());
            if first_ftyp_at.is_none() {
                first_ftyp_at = Some((i, size));
            }
            let sz = size as usize;
            if size >= 8 && size <= FTYP_SIZE_MAX && buffer.len() >= i + sz + 8 {
                return Some(&buffer[i..]);
            }
        }
        i += 1;
    }
    if let Some((off, size)) = first_ftyp_at {
        log::info!(
            "Replay: found \"ftyp\" at offset {} but size {} not in 8..{} or buffer too short; cannot use as MP4 start",
            off, size, FTYP_SIZE_MAX
        );
    } else if !buffer.is_empty() {
        log::debug!(
            "Replay: no ftyp in buffer (first 32 bytes): {:02x?}",
            &buffer[..buffer.len().min(32)]
        );
    }
    None
}

/// Given start of mdat box, return (content_start, header_len in bytes).
fn parse_mdat_header_range(buffer: &[u8], mdat_start: usize) -> Result<(usize, usize)> {
    if buffer.len() < mdat_start + 8 {
        anyhow::bail!("Buffer too short for mdat header");
    }
    let size_u32 = u32::from_be_bytes(buffer[mdat_start..mdat_start + 4].try_into().unwrap());
    if size_u32 == 1 {
        if buffer.len() < mdat_start + 16 {
            anyhow::bail!("Buffer too short for mdat 64-bit size");
        }
        Ok((mdat_start + 16, 16))
    } else {
        Ok((mdat_start + 8, 8))
    }
}

/// Build a playable MP4 by wrapping post-moov payload in an mdat box and fixing stco/co64.
/// Buffer is the raw stream after the 32-byte replay header (starts with ftyp).
/// Handles both box orders: ftyp+moov+mdat and ftyp+mdat+moov (E1 and others).
/// Provenance: reassembly requires updating stco/co64 when we relocate mdat (ISO BMFF).
/// E1 moov may lack stco/co64 (camera uses different structure); then we have a gap (see notes § Replay: full end-to-end flow and provenance).
fn assemble_mp4_with_mdat(buffer: &[u8]) -> Result<Vec<u8>> {
    if buffer.len() < 8 {
        anyhow::bail!("Replay buffer too short for ftyp");
    }
    if buffer[4..8] != *b"ftyp" {
        anyhow::bail!("Replay buffer does not start with ftyp");
    }
    let ftyp_size = u32::from_be_bytes(buffer[0..4].try_into().unwrap()) as usize;
    const FTYP_SIZE_MAX: usize = 1024;
    if ftyp_size < 8 || ftyp_size > FTYP_SIZE_MAX || buffer.len() < ftyp_size + 8 {
        anyhow::bail!(
            "Replay buffer: invalid ftyp box size {} (max {}) or buffer too short",
            ftyp_size, FTYP_SIZE_MAX
        );
    }

    let (moov_start, moov_end, payload, mdat_payload_offset) = {
        let first_box_type = &buffer[ftyp_size + 4..ftyp_size + 8];
        if first_box_type == b"moov" {
            // Layout: ftyp + moov + [mdat or raw payload]
            let moov_start = ftyp_size;
            let moov_size = parse_box_size(buffer, moov_start)?;
            let moov_end = moov_start + moov_size;
            if buffer.len() < moov_end {
                anyhow::bail!("Replay buffer truncated before end of moov (moov_size={})", moov_size);
            }
            let (payload, mdat_header_in_output) = if buffer.len() >= moov_end + 8 && buffer[moov_end + 4..moov_end + 8] == *b"mdat" {
                let (mdat_content_start, _) = parse_mdat_header_range(buffer, moov_end)?;
                (&buffer[mdat_content_start..], 8usize)
            } else {
                (&buffer[moov_end..], 8usize)
            };
            let mdat_payload_offset = moov_end + mdat_header_in_output;
            (moov_start, moov_end, payload, mdat_payload_offset)
        } else if first_box_type == b"mdat" {
            // Layout: ftyp + mdat + [optional boxes] + moov (E1 and some cameras)
            let mdat_start = ftyp_size;
            let mdat_box_len = parse_box_size(buffer, mdat_start)?;
            let mdat_end = mdat_start + mdat_box_len;
            if buffer.len() < mdat_end + 8 {
                anyhow::bail!("Replay buffer truncated before moov (mdat_size={})", mdat_box_len);
            }
            // Skip any boxes (free, wide, etc.) until we find moov
            let mut pos = mdat_end;
            let (moov_start, moov_size) = loop {
                if pos + 8 > buffer.len() {
                    anyhow::bail!("Replay buffer: no moov box found after mdat");
                }
                let box_size = parse_box_size(buffer, pos)?;
                let box_type = &buffer[pos + 4..pos + 8];
                if box_type == b"moov" {
                    break (pos, box_size);
                }
                pos += box_size;
            };
            let moov_end = moov_start + moov_size;
            if buffer.len() < moov_end {
                anyhow::bail!("Replay buffer truncated before end of moov (moov_size={})", moov_size);
            }
            // Output will be ftyp + moov + mdat; mdat content starts at ftyp + moov_size + 8
            let payload = if buffer[mdat_start + 4..mdat_start + 8] == *b"mdat" {
                let (content_start, _) = parse_mdat_header_range(buffer, mdat_start)?;
                &buffer[content_start..mdat_end]
            } else {
                &buffer[mdat_start + 8..mdat_end]
            };
            let mdat_payload_offset = ftyp_size + moov_size + 8;
            (moov_start, moov_end, payload, mdat_payload_offset)
        } else {
            anyhow::bail!("Replay buffer: expected moov or mdat after ftyp, got {:?}", first_box_type);
        }
    };

    let mut moov_copy = buffer[moov_start..moov_end].to_vec();
    let mut patched_count = 0u32;

    // Helper: for a chunk-offset box at tag_offset i (start of "stco" or "co64"), return (box_start, header_len to first entry, entry_count, entry_byte_len).
    let chunk_box_meta = |moov: &[u8], i: usize, entry_byte_len: usize| -> Option<(usize, usize, usize, usize)> {
        if i < 4 {
            return None;
        }
        let box_start = i - 4;
        let size_u32 = u32::from_be_bytes(moov[box_start..box_start + 4].try_into().unwrap());
        let (header_len, count_offset) = if size_u32 == 1 {
            // Extended size: size(4)=1, type(4), size64(8), ver+flags(4), entry_count(4)
            if i + 20 > moov.len() {
                return None;
            }
            (20, 16)
        } else {
            (12, 8)
        };
        let first_entry = i + header_len;
        let count_end = i + count_offset + 4;
        if count_end > moov.len() || first_entry + entry_byte_len > moov.len() {
            return None;
        }
        let entry_count = u32::from_be_bytes(moov[i + count_offset..count_end].try_into().unwrap()) as usize;
        let box_size = if size_u32 == 1 {
            if box_start + 16 > moov.len() {
                return None;
            }
            u64::from_be_bytes(moov[box_start + 8..box_start + 16].try_into().unwrap()) as usize
        } else {
            size_u32 as usize
        };
        if box_start + box_size > moov.len() {
            return None;
        }
        Some((box_start, first_entry, entry_count, box_size))
    };

    // Patch all stco boxes (32-bit chunk offsets).
    let mut search_from = 0usize;
    while let Some(i) = moov_copy[search_from..].windows(4).position(|w| w == b"stco") {
        let i = search_from + i;
        if let Some((box_start, first_entry, entry_count, box_size)) = chunk_box_meta(&moov_copy, i, 4) {
            let old_first = u32::from_be_bytes(moov_copy[first_entry..first_entry + 4].try_into().unwrap());
            let delta = (mdat_payload_offset as i64) - (old_first as i64);
            for j in 0..entry_count {
                let pos = first_entry + j * 4;
                if pos + 4 > moov_copy.len() {
                    break;
                }
                let old_val = u32::from_be_bytes(moov_copy[pos..pos + 4].try_into().unwrap());
                let new_val = ((old_val as i64) + delta) as u32;
                moov_copy[pos..pos + 4].copy_from_slice(&new_val.to_be_bytes());
            }
            patched_count += 1;
            search_from = box_start + box_size;
        } else {
            search_from = i + 1;
        }
    }

    // Patch all co64 boxes (64-bit chunk offsets).
    search_from = 0usize;
    while let Some(i) = moov_copy[search_from..].windows(4).position(|w| w == b"co64") {
        let i = search_from + i;
        if let Some((box_start, first_entry, entry_count, box_size)) = chunk_box_meta(&moov_copy, i, 8) {
            let old_first = u64::from_be_bytes(moov_copy[first_entry..first_entry + 8].try_into().unwrap());
            let delta = (mdat_payload_offset as i64) - (old_first as i64);
            for j in 0..entry_count {
                let pos = first_entry + j * 8;
                if pos + 8 > moov_copy.len() {
                    break;
                }
                let old_val = u64::from_be_bytes(moov_copy[pos..pos + 8].try_into().unwrap());
                let new_val = ((old_val as i64) + delta) as u64;
                moov_copy[pos..pos + 8].copy_from_slice(&new_val.to_be_bytes());
            }
            patched_count += 1;
            search_from = box_start + box_size;
        } else {
            search_from = i + 1;
        }
    }

    if patched_count == 0 {
        log::warn!("Replay: no stco/co64 found in moov, mdat offset may be wrong");
        // Diagnose: do the strings exist but chunk_box_meta failed?
        let moov_len = moov_copy.len();
        if let Some(pos) = moov_copy.windows(4).position(|w| w == b"stco") {
            let box_start = pos.saturating_sub(4);
            let size_u32 = if box_start + 4 <= moov_len {
                u32::from_be_bytes(moov_copy[box_start..box_start + 4].try_into().unwrap())
            } else {
                0
            };
            log::info!("Replay: moov size {} bytes; 'stco' at offset {} (box size field={}), chunk_box_meta would need valid box", moov_len, pos, size_u32);
        } else if let Some(pos) = moov_copy.windows(4).position(|w| w == b"co64") {
            let box_start = pos.saturating_sub(4);
            let size_u32 = if box_start + 4 <= moov_len {
                u32::from_be_bytes(moov_copy[box_start..box_start + 4].try_into().unwrap())
            } else {
                0
            };
            log::info!("Replay: moov size {} bytes; 'co64' at offset {} (box size field={}), chunk_box_meta would need valid box", moov_len, pos, size_u32);
        } else {
            log::info!("Replay: moov size {} bytes; no 'stco' or 'co64' substring in moov (camera may use different structure)", moov_len);
        }
    } else {
        log::info!("Replay: patched {} stco/co64 box(es) to mdat offset {}", patched_count, mdat_payload_offset);
    }

    let mdat_box_len = 8 + payload.len(); // 4 size + 4 tag + payload
    let mut out = Vec::with_capacity(ftyp_size + moov_copy.len() + mdat_box_len);
    out.extend_from_slice(&buffer[0..ftyp_size]);
    out.extend_from_slice(&moov_copy);
    if mdat_box_len <= 0xFF_FF_FF_FF {
        out.extend_from_slice(&(mdat_box_len as u32).to_be_bytes());
        out.extend_from_slice(b"mdat");
    } else {
        out.extend_from_slice(&1u32.to_be_bytes()); // size 1 = 64-bit size follows
        out.extend_from_slice(b"mdat");
        out.extend_from_slice(&((16 + payload.len()) as u64).to_be_bytes()); // 4+4+8+payload
    }
    out.extend_from_slice(payload);
    Ok(out)
}

/// Parse YYYY-MM-DD into (year, month, day).
fn parse_date(s: &str) -> Result<(i32, u8, u8)> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        anyhow::bail!("Date must be YYYY-MM-DD, got {:?}", s);
    }
    let year: i32 = parts[0].parse().context("Invalid year")?;
    let month: u8 = parts[1].parse().context("Invalid month")?;
    let day: u8 = parts[2].parse().context("Invalid day")?;
    if month == 0 || month > 12 || day == 0 || day > 31 {
        anyhow::bail!("Invalid month or day");
    }
    Ok((year, month, day))
}

fn date_to_replay_start(year: i32, month: u8, day: u8) -> ReplayDateTime {
    ReplayDateTime {
        year,
        month,
        day,
        hour: 0,
        minute: 0,
        second: 0,
    }
}

fn date_to_replay_end(year: i32, month: u8, day: u8) -> ReplayDateTime {
    ReplayDateTime {
        year,
        month,
        day,
        hour: 23,
        minute: 59,
        second: 59,
    }
}

async fn run_replay_or_download(
    camera: &crate::common::NeoInstance,
    name: &str,
    stream_type: &str,
    speed: u32,
    output: Option<std::path::PathBuf>,
    duration: Option<u64>,
    dump_replay: Option<std::path::PathBuf>,
    dump_replay_limit: Option<usize>,
    _is_download: bool,
) -> Result<()> {
    log::info!(
        "Replay play: name={} stream={} speed={}",
        name,
        stream_type,
        speed
    );
    // If writing to file and no --duration, try to get duration from file list (seek + list files by day).
    let mut duration = duration;
    if duration.is_none() && output.is_some() {
        let auto_secs = camera
            .run_task(|cam| {
                let name = name.to_string();
                let stream_type = stream_type.to_string();
                let record_type = cmdline::FILE_SEARCH_RECORD_TYPES.to_string();
                Box::pin(async move {
                    cam.get_replay_file_duration_secs(&name, &stream_type, &record_type)
                        .await
                        .map_err(anyhow::Error::msg)
                })
            })
            .await
            .ok()
            .flatten();
        if let Some(secs) = auto_secs {
            log::info!(
                "Replay: file duration {} s (from file list), will stop when complete",
                secs
            );
            duration = Some(secs);
        }
    }
    if duration.is_none() {
        log::info!("Replay: no --duration set; camera will stream until you stop or send response 300. Use --duration N to record N seconds then close.");
    }
    // Get file metadata (size, record_type with AI tags, timing) from file list.
    let file_meta: Option<FileInfo> = camera
        .run_task(|cam| {
            let name = name.to_string();
            let stream_type = stream_type.to_string();
            let record_type = cmdline::FILE_SEARCH_RECORD_TYPES.to_string();
            Box::pin(async move {
                let meta = cam.get_replay_file_metadata(&name, &stream_type, &record_type).await.ok().flatten();
                Ok(meta)
            })
        })
        .await
        .ok()
        .flatten();
    let expected_size = file_meta.as_ref().and_then(|info| {
        let l = info.size_l.unwrap_or(0) as u64;
        let h = info.size_h.unwrap_or(0) as u64;
        if l == 0 && h == 0 { None } else { Some(l + (h << 32)) }
    });
    if let Some(sz) = expected_size {
        log::info!("Replay: expected file size {} bytes (from file list), will stop when complete", sz);
    }
    // Build recording metadata for MP4 embedding
    let recording_meta = RecordingMeta {
        record_type: file_meta.as_ref().and_then(|f| f.record_type.clone()),
        start_time: file_meta.as_ref().and_then(|f| f.start_time.as_ref().map(format_replay_datetime)),
        end_time: file_meta.as_ref().and_then(|f| f.end_time.as_ref().map(format_replay_datetime)),
        camera_name: None,
    };
    if let Some(ref rt) = recording_meta.record_type {
        log::info!("Replay: file recordType = {}", rt);
    }
    let dump_limit = dump_replay.as_ref().and_then(|_| Some(dump_replay_limit.unwrap_or(131072)));
    let mut stream = camera
        .run_task(|cam| {
            let name = name.to_string();
            let stream_type = stream_type.to_string();
            let dump_path = dump_replay.clone();
            let expected_size = expected_size;
            let limit = dump_limit;
            Box::pin(async move {
                cam.start_replay(&name, &stream_type, speed, false, 100, dump_path, limit, expected_size)
                    .await
                    .context("Could not start replay on camera")
            })
        })
        .await?;

    // When no output path is given, use a sink so we don't flood the terminal with binary.
    let mut out: Box<dyn tokio::io::AsyncWrite + Unpin + Send> = match &output {
        Some(p) => Box::new(tokio::fs::File::create(p).await?),
        None => Box::new(tokio::io::sink()),
    };

    let mut frames: u64 = 0;
    let mut raw_bytes: u64 = 0;
    let mut raw_replay_buffer: Vec<u8> = Vec::new(); // when writing to file, buffer raw chunks to assemble with mdat
    // When output is .mp4, collect BcMedia NALs+timestamps for proper MP4 muxing at the end.
    let mux_to_mp4_output = output.as_ref().map(|p| p.extension().map(|e| e == "mp4").unwrap_or(false)).unwrap_or(false);
    let mut bcmedia_nals: Vec<Vec<u8>> = Vec::new();
    let mut bcmedia_timestamps: Vec<u32> = Vec::new();
    let mut bcmedia_aac: Vec<u8> = Vec::new();
    let mut bcmedia_codec: neolink_core::bcmedia::model::VideoType = neolink_core::bcmedia::model::VideoType::H264;
    // Skip first 32 bytes only when replay was started with MSG 5 (app parity; set when we receive ReplayStarted).
    let mut skip_first_32: Option<bool> = None;
    let deadline = duration.map(|secs| Instant::now() + Duration::from_secs(secs));

    loop {
        let res = if let Some(deadline) = deadline {
            tokio::select! {
                _ = sleep_until(deadline) => {
                    log::info!("Replay: {}s duration reached, sending replay stop (MSG 7) and closing file.", duration.unwrap());
                    break;
                }
                _ = tokio::signal::ctrl_c() => {
                    log::info!("Replay: Ctrl+C received, stopping and writing partial file.");
                    break;
                }
                r = stream.get_data() => r,
            }
        } else {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    log::info!("Replay: Ctrl+C received, stopping and writing partial file.");
                    break;
                }
                r = stream.get_data() => r,
            }
        };

        match res {
            Ok(Ok(BcMedia::Iframe(BcMediaIframe { data, microseconds, video_type, .. })))
            | Ok(Ok(BcMedia::Pframe(BcMediaPframe { data, microseconds, video_type, .. }))) => {
                if mux_to_mp4_output {
                    if frames == 0 {
                        // Override header claim with NAL-level detection (e.g. Argus PT labels H.265 as "H264")
                        let actual = detect_codec_from_nal_bytes(&data).unwrap_or(video_type);
                        if actual != video_type {
                            log::warn!(
                                "Replay: BcMedia header says {:?} but NAL bytes indicate {:?} — using detected codec",
                                video_type, actual
                            );
                        }
                        bcmedia_codec = actual;
                        log::info!("Replay: stream codec = {:?}", actual);
                    }
                    bcmedia_nals.push(data);
                    bcmedia_timestamps.push(microseconds);
                } else {
                    out.write_all(&data).await?;
                    out.flush().await?;
                }
                frames += 1;
                if frames <= 5 || frames % 30 == 0 {
                    log::info!("Replay: wrote frame {} to output", frames);
                }
            }
            Ok(Ok(BcMedia::Aac(BcMediaAac { data, .. }))) if mux_to_mp4_output => {
                bcmedia_aac.extend_from_slice(&data);
            }
            Ok(Ok(BcMedia::ReplayStarted(msg_id))) => {
                skip_first_32 = Some(msg_id == 5);
            }
            Ok(Ok(BcMedia::RawReplayChunk(data))) => {
                raw_bytes += data.len() as u64;
                if output.is_some() {
                    raw_replay_buffer.extend_from_slice(&data);
                }
                // Log progress; skip the initial "32 bytes" (replay header) to avoid confusion
                if (raw_bytes > 32 && raw_bytes <= 1024) || raw_bytes % 10240 == 0 {
                    log::info!("Replay: received {} bytes total (container/raw)", raw_bytes);
                }
            }
            Ok(Ok(BcMedia::StreamEnd)) => {
                log::info!("Replay: camera signalled end of file, finishing download.");
                break;
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) | Err(e) => return Err(e.into()),
        }
    }

    // Send MSG 7 (replay stop). Use a short timeout so we don't hang if the camera
    // already closed the stream (e.g. after "no data for 15s").
    log::info!("Replay: sending MSG 7 (stop) with {}s timeout", 5);
    const REPLAY_STOP_TIMEOUT_SECS: u64 = 5;
    let stop_start = std::time::Instant::now();
    match tokio::time::timeout(
        tokio::time::Duration::from_secs(REPLAY_STOP_TIMEOUT_SECS),
        camera.run_task(|cam| {
            let name = name.to_string();
            Box::pin(
                async move { cam.replay_stop(&name).await.context("Replay stop failed") },
            )
        }),
    )
    .await
    {
        Ok(Ok(())) => {
            log::info!("Replay: MSG 7 (stop) completed successfully in {:?}", stop_start.elapsed());
        }
        Ok(Err(e)) => {
            log::warn!("Replay: MSG 7 (stop) failed after {:?}: {:?}", stop_start.elapsed(), e);
        }
        Err(_) => {
            log::warn!(
                "Replay: MSG 7 (stop) timed out after {}s (camera may have already closed stream)",
                REPLAY_STOP_TIMEOUT_SECS
            );
        }
    }
    if let Some(p) = &output {
        if !raw_replay_buffer.is_empty() {
            let stream = stream_for_mp4_assembly(&raw_replay_buffer, skip_first_32);
            let to_try: &[u8] = slice_from_ftyp(stream).unwrap_or(stream);
            match assemble_mp4_with_mdat(to_try) {
                Ok(assembled) => {
                    tokio::fs::write(p, &assembled).await.context("Write assembled MP4")?;
                    patch_e1_avcc_if_needed(p).context("Patch E1 avcC for playback")?;
                    patch_avc1_colour_if_needed(p).context("Patch avc1 colour for playback")?;
                    println!(
                        "Wrote {} bytes (assembled MP4) to {}",
                        assembled.len(),
                        p.display()
                    );
                }
                Err(e) => {
                    log::warn!("Replay: could not assemble MP4: {:?}; trying BcMedia decode", e);
                    let stream_to_decode = find_bcmedia_magic_offset(stream)
                        .map(|off| &stream[off..])
                        .unwrap_or(stream);
                    let stream_to_decode_len = stream_to_decode.len();
                    // Save raw stream for offline analysis
                    let bcmedia_dump_path = p.with_extension("replay.bin");
                    if let Err(e) = tokio::fs::write(&bcmedia_dump_path, stream_to_decode).await {
                        log::warn!("Replay: could not write BcMedia dump: {}", e);
                    } else {
                        log::info!("Replay: BcMedia stream ({} bytes) saved to {}", stream_to_decode_len, bcmedia_dump_path.display());
                    }
                    // Decode BcMedia frames and mux to MP4
                    const DECODE_MUX_TIMEOUT_SECS: u64 = 60;
                    let stream_to_decode = stream_to_decode.to_vec();
                    let decoded_opt = match tokio::time::timeout(
                        tokio::time::Duration::from_secs(DECODE_MUX_TIMEOUT_SECS),
                        tokio::task::spawn_blocking(move || try_decode_bcmedia_nals(&stream_to_decode)),
                    )
                    .await
                    {
                        Ok(Ok(n)) => n,
                        Ok(Err(e)) => { log::warn!("Replay: BcMedia decode failed: {:?}", e); None }
                        Err(_) => { log::warn!("Replay: BcMedia decode timed out after {}s", DECODE_MUX_TIMEOUT_SECS); None }
                    };
                    if let Some(BcMediaDecoded { nals, fps, timestamps_us, aac_data, video_codec }) = decoded_opt {
                        let annex_b = annex_b_from_nals(&nals);
                        let is_mp4 = p.extension().map(|e| e == "mp4").unwrap_or(false);
                        if is_mp4 {
                            if mux_to_mp4(&nals, &aac_data, fps, &timestamps_us, p, &recording_meta, video_codec).await? {
                                println!("Parsed BcMedia replay and muxed to {}", p.display());
                            } else {
                                tokio::fs::write(p, &annex_b).await.context("Write video fallback")?;
                                println!("Wrote {} bytes (Annex B) to {} (mux failed)", annex_b.len(), p.display());
                            }
                        } else {
                            tokio::fs::write(p, &annex_b).await.context("Write video replay")?;
                            println!("Wrote {} bytes (Annex B) to {}", annex_b.len(), p.display());
                        }
                    } else {
                        let raw_path = if p.extension().map(|e| e == "mp4").unwrap_or(false) {
                            p.with_extension("replay.raw.bin")
                        } else {
                            p.clone()
                        };
                        tokio::fs::write(&raw_path, &raw_replay_buffer).await.context("Write raw replay")?;
                        if raw_path.as_path() != p.as_path() {
                            println!(
                                "Replay stream is not valid MP4 or BcMedia. Wrote {} bytes to {} for inspection.",
                                raw_replay_buffer.len(),
                                raw_path.display()
                            );
                        } else {
                            patch_e1_avcc_if_needed(&raw_path).context("Patch E1 avcC for playback")?;
                            patch_avc1_colour_if_needed(&raw_path).context("Patch avc1 colour for playback")?;
                            println!("Wrote {} bytes (raw container) to {}", raw_replay_buffer.len(), raw_path.display());
                        }
                    }
                }
            }
        } else if !bcmedia_nals.is_empty() {
            if let Some(p) = &output {
                if mux_to_mp4(
                    &bcmedia_nals,
                    &bcmedia_aac,
                    0,
                    &bcmedia_timestamps,
                    p,
                    &recording_meta,
                    bcmedia_codec,
                )
                .await?
                {
                    println!("Wrote {} frames to {} (MP4)", frames, p.display());
                } else {
                    let annex_b = annex_b_from_nals(&bcmedia_nals);
                    tokio::fs::write(p, &annex_b).await.context("Write video fallback")?;
                    println!("Wrote {} frames to {} (video, mux failed)", frames, p.display());
                }
            }
        } else if frames > 0 {
            out.flush().await?;
            out.shutdown().await?;
            println!("Wrote {} frames to {}", frames, p.display());
        } else {
            // No data received — delete the empty file that was created upfront.
            drop(out);
            let _ = tokio::fs::remove_file(p).await;
            return Err(anyhow::anyhow!(
                "No data received from camera (0 frames, 0 bytes). Output file removed. \
                 The camera may be busy or the file may no longer exist — try again."
            ));
        }
    } else {
        out.flush().await?;
        out.shutdown().await?;
        if raw_bytes > 0 || frames > 0 {
            log::info!("Replay: received {} bytes, {} frames (use --output <path> to save to file)", raw_bytes, frames);
        }
    }

    drop(stream);
    Ok(())
}

/// Download by time range (MSG 143). Stream ends on response 331; stop is MSG 144 (sent on drop).
async fn run_download_by_time(
    camera: &crate::common::NeoInstance,
    start_time: ReplayDateTime,
    end_time: ReplayDateTime,
    stream_type: &str,
    output: std::path::PathBuf,
    duration: Option<u64>,
    dump_replay: Option<std::path::PathBuf>,
    dump_replay_limit: Option<usize>,
) -> Result<()> {
    let stream_type_u32 = if stream_type == "subStream" { 1 } else { 0 };
    let save_path = output.display().to_string();
    const DEFAULT_DUMP_LIMIT: usize = 131072;
    let dump_limit = dump_replay.as_ref().map(|_| dump_replay_limit.unwrap_or(DEFAULT_DUMP_LIMIT));

    log::info!(
        "Replay download-by-time: {:?} to {:?} stream={}",
        start_time,
        end_time,
        stream_type
    );

    // Build metadata from the time range (no FileInfo available for download-by-time)
    let recording_meta = RecordingMeta {
        record_type: None,
        start_time: Some(format_replay_datetime(&start_time)),
        end_time: Some(format_replay_datetime(&end_time)),
        camera_name: None,
    };

    let mut stream = camera
        .run_task(|cam| {
            let start_time = start_time.clone();
            let end_time = end_time.clone();
            let save_path = save_path.clone();
            let dump_path = dump_replay.clone();
            let limit = dump_limit;
            Box::pin(async move {
                cam.start_download_by_time(
                    start_time,
                    end_time,
                    &save_path,
                    stream_type_u32,
                    false,
                    100,
                    dump_path,
                    limit,
                )
                .await
                .context("Could not start download-by-time on camera")
            })
        })
        .await?;

    let mut out: Box<dyn tokio::io::AsyncWrite + Unpin + Send> =
        Box::new(tokio::fs::File::create(&output).await?);
    let mut frames: u64 = 0;
    let mut raw_bytes: u64 = 0;
    let mut raw_replay_buffer: Vec<u8> = Vec::new();
    let deadline = duration.map(|secs| Instant::now() + Duration::from_secs(secs));

    loop {
        let res = if let Some(deadline) = deadline {
            tokio::select! {
                _ = sleep_until(deadline) => {
                    log::info!("DownloadByTime: {}s duration reached, closing (MSG 144 sent on drop).", duration.unwrap());
                    break;
                }
                _ = tokio::signal::ctrl_c() => {
                    log::info!("DownloadByTime: Ctrl+C received, writing partial file.");
                    break;
                }
                r = stream.get_data() => r,
            }
        } else {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    log::info!("DownloadByTime: Ctrl+C received, writing partial file.");
                    break;
                }
                r = stream.get_data() => r,
            }
        };

        match res {
            Ok(Ok(BcMedia::Iframe(BcMediaIframe { data, .. })))
            | Ok(Ok(BcMedia::Pframe(BcMediaPframe { data, .. }))) => {
                out.write_all(&data).await?;
                out.flush().await?;
                frames += 1;
                if frames <= 5 || frames % 30 == 0 {
                    log::info!("DownloadByTime: wrote frame {} to output", frames);
                }
            }
            Ok(Ok(BcMedia::RawReplayChunk(data))) => {
                raw_bytes += data.len() as u64;
                raw_replay_buffer.extend_from_slice(&data);
                if (raw_bytes > 32 && raw_bytes <= 1024) || raw_bytes % 10240 == 0 {
                    log::info!("DownloadByTime: received {} bytes total (container/raw)", raw_bytes);
                }
            }
            Ok(Ok(BcMedia::StreamEnd)) => {
                log::info!("DownloadByTime: camera signalled end of file, finishing download.");
                break;
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) | Err(e) => {
                if frames == 0 && raw_bytes == 0 {
                    return Err(anyhow::anyhow!(
                        "Camera rejected download-by-time (MSG 143). This camera may not support \
                         time-range downloads. Use 'replay download --name <file>' instead."
                    ));
                }
                return Err(e.into());
            }
        }
    }

    drop(stream); // sends MSG 144
    out.flush().await?;
    out.shutdown().await?;

    if !raw_replay_buffer.is_empty() {
        let stream = stream_for_mp4_assembly(&raw_replay_buffer, None);
        let to_try: &[u8] = slice_from_ftyp(stream).unwrap_or(stream);
        match assemble_mp4_with_mdat(to_try) {
            Ok(assembled) => {
                tokio::fs::write(&output, &assembled).await.context("Write assembled MP4")?;
                patch_e1_avcc_if_needed(&output).context("Patch E1 avcC for playback")?;
                patch_avc1_colour_if_needed(&output).context("Patch avc1 colour for playback")?;
                println!(
                    "Wrote {} bytes (assembled MP4 with mdat) to {}. If ffplay shows 'unspecified pixel format', try: ffplay -vf format=yuv420p {}",
                    assembled.len(),
                    output.display(),
                    output.display()
                );
            }
            Err(e) => {
                log::warn!(
                    "DownloadByTime: could not assemble MP4 with mdat: {:?}; trying BcMedia decode",
                    e
                );
                let stream_to_decode = find_bcmedia_magic_offset(stream)
                    .map(|off| &stream[off..])
                    .unwrap_or(stream);
                if let Some(BcMediaDecoded { nals, fps, timestamps_us, aac_data, video_codec }) = try_decode_bcmedia_nals(stream_to_decode) {
                    let annex_b = annex_b_from_nals(&nals);
                    let is_mp4 = output.extension().map(|e| e == "mp4").unwrap_or(false);
                    if is_mp4 {
                        if mux_to_mp4(&nals, &aac_data, fps, &timestamps_us, &output, &recording_meta, video_codec).await? {
                            println!("Parsed BcMedia replay and muxed to {}", output.display());
                        } else {
                            tokio::fs::write(&output, &annex_b).await.context("Write video fallback")?;
                            println!("Wrote {} bytes (Annex B) to {} (mux failed)", annex_b.len(), output.display());
                        }
                    } else {
                        tokio::fs::write(&output, &annex_b).await.context("Write video replay")?;
                        println!("Wrote {} bytes (Annex B) to {}", annex_b.len(), output.display());
                    }
                } else {
                    // Stream is not ftyp MP4 and BcMedia decode yielded no video; don't write to .mp4.
                    let raw_path = if output.extension().map(|e| e == "mp4").unwrap_or(false) {
                        output.with_extension("replay.bin")
                    } else {
                        output.clone()
                    };
                    tokio::fs::write(&raw_path, &raw_replay_buffer).await.context("Write raw replay")?;
                    if raw_path.as_path() != output.as_path() {
                        println!(
                            "Replay stream is not valid MP4 or BcMedia (e.g. camera sent XML or other format). Wrote {} bytes to {} for inspection.",
                            raw_replay_buffer.len(),
                            raw_path.display()
                        );
                    } else {
                        patch_e1_avcc_if_needed(&raw_path).context("Patch E1 avcC for playback")?;
                        patch_avc1_colour_if_needed(&raw_path).context("Patch avc1 colour for playback")?;
                        println!(
                            "Wrote {} bytes (raw container) to {}. If ffplay shows 'unspecified pixel format', try: ffplay -vf format=yuv420p {}",
                            raw_replay_buffer.len(),
                            raw_path.display(),
                            raw_path.display()
                        );
                    }
                }
            }
        }
    } else if frames > 0 {
        println!("Wrote {} frames to {}", frames, output.display());
    }
    Ok(())
}

/// Download a specific SD card file by name (MSG 8, NET_DOWNLOAD_V20).
/// Camera streams the raw MP4 directly — no BcMedia re-encoding needed.
async fn run_download_file_by_name(
    camera: &crate::common::NeoInstance,
    file_name: &str,
    output: std::path::PathBuf,
    dump_replay: Option<std::path::PathBuf>,
    dump_replay_limit: Option<usize>,
) -> Result<()> {
    log::info!("DownloadByName: requesting file '{}' via MSG 8", file_name);

    let mut stream = camera
        .run_task(|cam| {
            let name = file_name.to_string();
            let dump_path = dump_replay.clone();
            let dump_limit = dump_replay_limit;
            Box::pin(async move {
                cam.start_download_file_by_name(&name, false, 100, dump_path, dump_limit)
                    .await
                    .context("Could not start download-file-by-name on camera")
            })
        })
        .await?;

    let mut raw_bytes: u64 = 0;
    let mut raw_replay_buffer: Vec<u8> = Vec::new();

    loop {
        let res = tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                log::info!("DownloadByName: Ctrl+C received, writing partial file.");
                break;
            }
            r = stream.get_data() => r,
        };

        match res {
            Ok(Ok(BcMedia::RawReplayChunk(data))) => {
                raw_bytes += data.len() as u64;
                raw_replay_buffer.extend_from_slice(&data);
                if (raw_bytes > 32 && raw_bytes <= 1024) || raw_bytes % (1024 * 1024) == 0 {
                    log::info!("DownloadByName: received {} bytes total", raw_bytes);
                }
            }
            Ok(Ok(BcMedia::Iframe(BcMediaIframe { data, .. })))
            | Ok(Ok(BcMedia::Pframe(BcMediaPframe { data, .. }))) => {
                // BcMedia path (unexpected for direct download but handle gracefully)
                raw_replay_buffer.extend_from_slice(&data);
                raw_bytes += data.len() as u64;
            }
            Ok(Ok(BcMedia::StreamEnd)) => {
                log::info!("DownloadByName: camera signalled end of file ({} bytes).", raw_bytes);
                break;
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) | Err(e) => {
                if raw_bytes == 0 {
                    return Err(anyhow::anyhow!(
                        "Camera rejected download-by-name (MSG 8): {}. \
                         This camera may not support direct file download. \
                         Try 'replay download --name {}' instead.",
                        e, file_name
                    ));
                }
                log::warn!("DownloadByName: stream error after {} bytes: {:?}", raw_bytes, e);
                break;
            }
        }
    }

    drop(stream); // sends MSG 9 stop

    if raw_replay_buffer.is_empty() {
        return Err(anyhow::anyhow!("DownloadByName: no data received from camera"));
    }

    let stream = stream_for_mp4_assembly(&raw_replay_buffer, None);
    let to_try: &[u8] = slice_from_ftyp(stream).unwrap_or(stream);
    match assemble_mp4_with_mdat(to_try) {
        Ok(assembled) => {
            tokio::fs::write(&output, &assembled).await.context("Write assembled MP4")?;
            patch_e1_avcc_if_needed(&output).context("Patch E1 avcC")?;
            patch_avc1_colour_if_needed(&output).context("Patch avc1 colour")?;
            println!(
                "Wrote {} bytes (assembled MP4) to {}",
                assembled.len(),
                output.display()
            );
        }
        Err(_) => {
            // Not an MP4 container; try BcMedia decode
            let stream_to_decode = find_bcmedia_magic_offset(stream)
                .map(|off| &stream[off..])
                .unwrap_or(stream);
            if let Some(BcMediaDecoded { nals, fps, timestamps_us, aac_data, video_codec }) =
                try_decode_bcmedia_nals(stream_to_decode)
            {
                let recording_meta = RecordingMeta {
                    record_type: None,
                    start_time: None,
                    end_time: None,
                    camera_name: None,
                };
                let is_mp4 = output.extension().map(|e| e == "mp4").unwrap_or(false);
                if is_mp4 {
                    if mux_to_mp4(&nals, &aac_data, fps, &timestamps_us, &output, &recording_meta, video_codec)
                        .await?
                    {
                        println!(
                            "Parsed BcMedia and muxed to {}",
                            output.display()
                        );
                    } else {
                        let annex_b = annex_b_from_nals(&nals);
                        tokio::fs::write(&output, &annex_b).await.context("Write H.264")?;
                        println!("Wrote {} bytes (H.264) to {}", annex_b.len(), output.display());
                    }
                } else {
                    let annex_b = annex_b_from_nals(&nals);
                    tokio::fs::write(&output, &annex_b).await.context("Write H.264")?;
                    println!("Wrote {} bytes (H.264 Annex B) to {}", annex_b.len(), output.display());
                }
            } else {
                // Unknown format — save raw for inspection
                let raw_path = if output.extension().map(|e| e == "mp4").unwrap_or(false) {
                    output.with_extension("raw.bin")
                } else {
                    output.clone()
                };
                tokio::fs::write(&raw_path, &raw_replay_buffer)
                    .await
                    .context("Write raw download")?;
                println!(
                    "Wrote {} bytes (raw, unknown format) to {} for inspection",
                    raw_replay_buffer.len(),
                    raw_path.display()
                );
            }
        }
    }
    Ok(())
}

/// Entry point for the replay subcommand
pub(crate) async fn main(opt: Opt, reactor: NeoReactor) -> Result<()> {
    let camera = reactor.get(&opt.camera).await?;

    match opt.cmd {
        cmdline::ReplayCommand::Days { start, end } => {
            let (sy, sm, sd) = parse_date(&start)?;
            let end_str = end.as_deref().unwrap_or(&start);
            let (ey, em, ed) = parse_date(end_str)?;
            let start_time = date_to_replay_start(sy, sm, sd);
            let end_time = date_to_replay_end(ey, em, ed);

            let records = camera
                .run_task(|cam| {
                    let st = start_time.clone();
                    let et = end_time.clone();
                    Box::pin(
                        async move {
                            cam.get_day_records(st, et)
                                .await
                                .context("Could not get day records from camera")
                        },
                    )
                })
                .await?;

            let single_day = start_time.year == end_time.year
                && start_time.month == end_time.month
                && start_time.day == end_time.day;
            if records.day_type_list.is_none() && single_day {
                // E1 and some cameras don't send dayTypeList; fall back to file list for this day
                let handle_info = camera
                    .run_task({
                        let st = start_time.clone();
                        let et = end_time.clone();
                        move |cam| {
                            let st = st.clone();
                            let et = et.clone();
                            Box::pin(
                                async move {
                                    cam.get_file_list_handle("subStream", cmdline::FILE_SEARCH_RECORD_TYPES, st, et)
                                        .await
                                        .context("Could not get file list handle")
                                },
                            )
                        }
                    })
                    .await?;
                if let Some(handle) = handle_info.handle {
                    let files = camera
                        .run_task(|cam| {
                            Box::pin(
                                async move {
                                    cam.get_file_list_by_handle(handle)
                                        .await
                                        .context("Could not get file list")
                                },
                            )
                        })
                        .await?;
                    let n = files.len();
                    if n == 0 {
                        println!("No recordings for this day.");
                    } else {
                        println!("This day has {} recording(s).", n);
                    }
                } else {
                    print_day_records(&records);
                }
            } else {
                print_day_records(&records);
            }
        }
        cmdline::ReplayCommand::Files {
            date,
            stream,
            record_type,
            ai_filter,
        } => {
            let (y, m, d) = parse_date(&date)?;
            let start_time = date_to_replay_start(y, m, d);
            let end_time = date_to_replay_end(y, m, d);

            // When --stream is not specified, query both mainStream and subStream.
            // Cameras (e.g. Argus 3) store mainStream and subStream as separate files.
            let streams_to_query: Vec<String> = match stream {
                Some(s) => vec![s],
                None => vec!["mainStream".to_string(), "subStream".to_string()],
            };

            let mut files: Vec<_> = Vec::new();
            for stream_name in &streams_to_query {
                let handle_result = camera
                    .run_task(|cam| {
                        let st = start_time.clone();
                        let et = end_time.clone();
                        let sn = stream_name.clone();
                        let record_type = record_type.clone();
                        Box::pin(async move {
                            cam.get_file_list_handle(&sn, &record_type, st, et)
                                .await
                                .map_err(|e| anyhow::anyhow!("{}", e))
                        })
                    })
                    .await;

                let handle_info = match handle_result {
                    Ok(info) => info,
                    Err(e) => {
                        let msg = format!("{}", e);
                        if msg.contains("returned code 400") {
                            // No recordings for this stream/day — try next stream
                            continue;
                        }
                        return Err(e).context("Could not get file list handle from camera");
                    }
                };

                let handle = match handle_info.handle {
                    Some(h) => h,
                    None => continue, // no handle = no files for this stream
                };

                let stream_files = camera
                    .run_task(|cam| {
                        Box::pin(async move {
                            cam.get_file_list_by_handle(handle)
                                .await
                                .map_err(|e| anyhow::anyhow!("{}", e))
                        })
                    })
                    .await
                    .context("Could not get file list from camera")?;
                files.extend(stream_files);
            }

            if files.is_empty() {
                println!("No files for this day.");
                return Ok(());
            }

            // Apply --ai-filter: only show files whose recordType contains at least one requested tag
            let files = if let Some(ref filter) = ai_filter {
                let wanted: Vec<&str> = filter.split(',').map(|s| s.trim()).collect();
                files
                    .into_iter()
                    .filter(|f| {
                        if let Some(ref rt) = f.record_type {
                            let tags: Vec<&str> = rt.split(',').map(|s| s.trim()).collect();
                            wanted.iter().any(|w| tags.contains(w))
                        } else {
                            false
                        }
                    })
                    .collect()
            } else {
                files
            };

            print_file_list(&files);
        }
        cmdline::ReplayCommand::Play {
            name,
            stream: stream_type,
            speed,
            output,
            duration,
            dump_replay,
            dump_replay_limit,
        } => {
            run_replay_or_download(
                &camera,
                &name,
                &stream_type,
                speed,
                output,
                duration,
                dump_replay,
                dump_replay_limit,
                false,
            )
            .await?;
        }
        cmdline::ReplayCommand::Download {
            name,
            stream: stream_type,
            output,
            duration,
            dump_replay,
            dump_replay_limit,
        } => {
            log::info!(
                "Replay download: name={} stream={} (stops on response 300 or --duration)",
                name,
                stream_type
            );
            let output = output.ok_or_else(|| anyhow::anyhow!("Download requires --output <file>"))?;
            run_replay_or_download(
                &camera,
                &name,
                &stream_type,
                1,
                Some(output),
                duration,
                dump_replay,
                dump_replay_limit,
                true,
            )
            .await?;
        }
        cmdline::ReplayCommand::DownloadByTime {
            start,
            end,
            stream: stream_type,
            output,
            duration,
            dump_replay,
            dump_replay_limit,
        } => {
            let output =
                output.ok_or_else(|| anyhow::anyhow!("download-by-time requires --output <file>"))?;
            let (sy, sm, sd) = parse_date(&start)?;
            let end_str = end.as_deref().unwrap_or(&start);
            let (ey, em, ed) = parse_date(end_str)?;
            let start_time = date_to_replay_start(sy, sm, sd);
            let end_time = date_to_replay_end(ey, em, ed);
            run_download_by_time(
                &camera,
                start_time,
                end_time,
                &stream_type,
                output,
                duration,
                dump_replay,
                dump_replay_limit,
            )
            .await?;
        }
        cmdline::ReplayCommand::AlarmSearch {
            start,
            end,
            stream_type,
            alarm_types,
        } => {
            let (sy, sm, sd) = parse_date(&start)?;
            let end_str = end.as_deref().unwrap_or(&start);
            let (ey, em, ed) = parse_date(end_str)?;
            let start_time = date_to_replay_start(sy, sm, sd);
            let end_time = date_to_replay_end(ey, em, ed);

            let alarm_list: Vec<String> = alarm_types.split(',').map(|s| s.trim().to_string()).collect();
            let alarm_refs: Vec<&str> = alarm_list.iter().map(|s| s.as_str()).collect();

            // START: send search params, get handle
            let result = camera
                .run_task(|cam| {
                    let st = start_time.clone();
                    let et = end_time.clone();
                    let ar: Vec<String> = alarm_refs.iter().map(|s| s.to_string()).collect();
                    Box::pin(async move {
                        let refs: Vec<&str> = ar.iter().map(|s| s.as_str()).collect();
                        cam.alarm_video_search_start(stream_type, &refs, st, et)
                            .await
                            .map_err(|e| anyhow::anyhow!("{}", e))
                    })
                })
                .await;

            // Handle cameras that don't support alarm search (405 = not supported)
            let result = match result {
                Ok(r) => r,
                Err(e) => {
                    let msg = format!("{}", e);
                    if msg.contains("returned code 405") {
                        println!("This camera does not support alarm video search (MSG 175).");
                        return Ok(());
                    }
                    if msg.contains("returned code 400") {
                        println!("No alarm events found for this date range.");
                        return Ok(());
                    }
                    return Err(e).context("Alarm video search failed");
                }
            };

            println!("Alarm search response:");
            println!("  channelId:  {:?}", result.channel_id);
            println!("  fileHandle: {:?}", result.file_handle);
            println!("  streamType: {:?}", result.stream_type);
            println!("  alarmType:  {:?}", result.alarm_type);
            if let Some(ref st) = result.start_time {
                println!("  startTime:  {}", format_replay_datetime(st));
            }
            if let Some(ref et) = result.end_time {
                println!("  endTime:    {}", format_replay_datetime(et));
            }

            // If we got a handle, paginate to collect all events
            if let Some(handle) = result.file_handle {
                if handle >= 0 {
                    println!("\nPaginating with handle {}...", handle);
                    let mut page = 0;
                    loop {
                        page += 1;
                        let next_result = camera
                            .run_task(|cam| {
                                Box::pin(async move {
                                    cam.alarm_video_search_next(handle)
                                        .await
                                        .context("Alarm video search DO/paginate failed")
                                })
                            })
                            .await;
                        match next_result {
                            Ok(fav) => {
                                println!("\n--- Page {} ---", page);
                                println!("  channelId:  {:?}", fav.channel_id);
                                println!("  fileHandle: {:?}", fav.file_handle);
                                println!("  alarmType:  {:?}", fav.alarm_type);
                                if let Some(ref st) = fav.start_time {
                                    println!("  startTime:  {}", format_replay_datetime(st));
                                }
                                if let Some(ref et) = fav.end_time {
                                    println!("  endTime:    {}", format_replay_datetime(et));
                                }
                                // If no more data or handle changes, stop
                                if fav.file_handle.is_none() || fav.file_handle == Some(-1) {
                                    println!("\nEnd of alarm search results.");
                                    break;
                                }
                            }
                            Err(e) => {
                                // Camera returns non-200 when no more results
                                log::debug!("Alarm search pagination ended: {}", e);
                                println!("\nEnd of alarm search results (page {}).", page);
                                break;
                            }
                        }
                        if page >= 100 {
                            println!("Reached page limit (100). Stopping.");
                            break;
                        }
                    }
                }
            }
        }
        cmdline::ReplayCommand::DownloadByName {
            name,
            output,
            dump_replay,
            dump_replay_limit,
        } => {
            run_download_file_by_name(
                &camera,
                &name,
                output,
                dump_replay,
                dump_replay_limit,
            )
            .await?;
        }
        cmdline::ReplayCommand::Stop { name } => {
            camera
                .run_task(|cam| {
                    let name = name.clone();
                    Box::pin(
                        async move {
                            cam.replay_stop(&name)
                                .await
                                .context("Could not send replay stop to camera")
                        },
                    )
                })
                .await?;
            println!("Replay stop sent.");
        }
    }

    // Force exit to ensure the process terminates even if background tasks keep the runtime alive.
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    std::process::exit(0);
}

fn print_day_records(records: &DayRecords) {
    if let Some(ref list) = records.day_type_list {
        if list.day_type.is_empty() {
            println!("No recording days in range.");
            return;
        }
        println!("Days with recordings:");
        for dt in &list.day_type {
            // index is day-in-range (0 = first day). type is e.g. "normal"
            println!("  Day index {}: {}", dt.index, dt.type_);
        }
    } else {
        println!(
            "This camera does not report which days have recordings.\n\
             Use: neolink replay <camera> files --date YYYY-MM-DD to list files for a specific day."
        );
    }
}

/// AI detection tag names from recordType.
const AI_TAGS: &[&str] = &[
    "people", "vehicle", "face", "dog_cat", "package", "visitor", "cry",
    "crossline", "intrusion", "loitering", "nonmotorveh",
];

fn print_file_list(files: &[FileInfo]) {
    if files.is_empty() {
        println!("No files for this day.");
        return;
    }
    println!("Files ({}):", files.len());
    for f in files {
        let name = f.name.as_deref().unwrap_or("—");
        let size_l = f.size_l.unwrap_or(0);
        let size_h = f.size_h.unwrap_or(0);
        let size_bytes = size_l as u64 + ((size_h as u64) << 32);
        let stream = f.stream_type.as_deref().unwrap_or("—");
        let rec_type = f.record_type.as_deref().unwrap_or("");
        // Split recordType into trigger (md/sched/manual/pir/io) and AI detections
        let ai: Vec<&str> = rec_type
            .split(',')
            .map(|s| s.trim())
            .filter(|s| AI_TAGS.contains(s))
            .collect();
        let ai_str = if ai.is_empty() {
            String::new()
        } else {
            format!("  [AI: {}]", ai.join(", "))
        };
        let size_str = if size_bytes == 0 {
            "— ".to_string()
        } else if size_bytes < 1024 * 1024 {
            format!("{} KB", size_bytes / 1024)
        } else {
            format!("{} MB", size_bytes / (1024 * 1024))
        };
        println!("  {}  {}  {}  {}{}", name, size_str, stream, rec_type, ai_str);
    }
}
