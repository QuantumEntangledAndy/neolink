use crate::Credentials;

pub use super::crypto::EncryptionProtocol;
pub use super::xml::{BcPayloads, BcXml, Extension};
use std::cell::RefCell;
use std::collections::HashSet;

pub(super) const MAGIC_HEADER: u32 = 0x0abcdef0;
/// Sometimes will get the BE magic header even though all other numbers are LE?
/// Seems to happens with certain messages like snap that produce jpegs, so perhaps it
/// it is meant to be a hint as to the endianess of the binary payload
pub(super) const MAGIC_HEADER_REV: u32 = 0x0fedcba0;

/// Login messages have this ID
pub const MSG_ID_LOGIN: u32 = 1;
/// Logout messages have this ID
pub const MSG_ID_LOGOUT: u32 = 2;
/// Video and Audio Streams messages have this ID
pub const MSG_ID_VIDEO: u32 = 3;
/// ID used to stop the video stream
pub const MSG_ID_VIDEO_STOP: u32 = 4;
/// TalkAbility messages have this ID
pub const MSG_ID_TALKABILITY: u32 = 10;
/// TalkReset messages have this ID
pub const MSG_ID_TALKRESET: u32 = 11;
/// PtzControl messages have this ID
pub const MSG_ID_PTZ_CONTROL: u32 = 18;
/// PTZ goto preset position
pub const MSG_ID_PTZ_CONTROL_PRESET: u32 = 19;
/// Reboot messages have this ID
pub const MSG_ID_REBOOT: u32 = 23;
/// Request motion detection messages
pub const MSG_ID_MOTION_REQUEST: u32 = 31;
/// Motion detection messages
pub const MSG_ID_MOTION: u32 = 33;
/// Set service ports
pub const MSG_ID_SET_SERVICE_PORTS: u32 = 36;
/// Get service ports
pub const MSG_ID_GET_SERVICE_PORTS: u32 = 37;
/// Get Email setting
pub const MSG_ID_GET_EMAIL: u32 = 42;
/// Set email settings
pub const MSG_ID_SET_EMAIL: u32 = 43;
/// Get users and general system info
pub const MSG_ID_GET_ABILITY_SUPPORT: u32 = 58;
/// Update, create and remove users
pub const MSG_ID_UPDATE_USER_LIST: u32 = 59;
/// Version messages have this ID
pub const MSG_ID_VERSION: u32 = 80;
/// Ping messages have this ID
pub const MSG_ID_PING: u32 = 93;
/// General system info messages have this ID
pub const MSG_ID_GET_GENERAL: u32 = 104;
/// Setting general system info (clock mostly) messages have this ID
pub const MSG_ID_SET_GENERAL: u32 = 105;
/// Snapshot to get a jpeg image
pub const MSG_ID_SNAP: u32 = 109;
/// Used to grab the UID
pub const MSG_ID_UID: u32 = 114;
/// Used to pass the token and client ID for push notifications
pub const MSG_ID_PUSH_INFO: u32 = 124;
/// Send a test email configuration
pub const MSG_ID_TEST_EMAIL: u32 = 141;
/// StreamInfoList messages have this ID
pub const MSG_ID_STREAM_INFO_LIST: u32 = 146;
/// Used to get the abilities of a user
pub const MSG_ID_ABILITY_INFO: u32 = 151;
/// Get the available PTZ position presets
pub const MSG_ID_GET_PTZ_PRESET: u32 = 190;
/// Get the support details (ptz, talk et)
pub const MSG_ID_GET_SUPPORT: u32 = 199;
/// Will send the talk config for talk back data to follow this msg
pub const MSG_ID_TALKCONFIG: u32 = 201;
/// Used to send talk back binary data
pub const MSG_ID_TALK: u32 = 202;
/// Getting the LED status is done with this ID
pub const MSG_ID_GET_LED_STATUS: u32 = 208;
/// Setting the LED status is done with this ID
pub const MSG_ID_SET_LED_STATUS: u32 = 209;
/// Getting PIR status messages have this ID
pub const MSG_ID_GET_PIR_ALARM: u32 = 212;
/// Setting PIR status messages have this ID
pub const MSG_ID_START_PIR_ALARM: u32 = 213;
/// Set Email Task
pub const MSG_ID_SET_EMAIL_TASK: u32 = 216;
/// Get Email Task
pub const MSG_ID_GET_EMAIL_TASK: u32 = 217;
/// UDP Keep alive
pub const MSG_ID_UDP_KEEP_ALIVE: u32 = 234;
/// Battery message initiaed by the camera
pub const MSG_ID_BATTERY_INFO_LIST: u32 = 252;
/// Battery message initiaed by the client
pub const MSG_ID_BATTERY_INFO: u32 = 253;
/// Used for to play sounds like the siren
pub const MSG_ID_PLAY_AUDIO: u32 = 263;
/// Manual Floodlight Control
pub const MSG_ID_FLOODLIGHT_MANUAL: u32 = 288;
/// Set Floodlight tasks xml
pub const MSG_ID_FLOODLIGHT_TASKS_WRITE: u32 = 290;
/// Floodlight status report from the camera
pub const MSG_ID_FLOODLIGHT_STATUS_LIST: u32 = 291;
/// Used for camera Zoom read
pub const MSG_ID_GET_ZOOM_FOCUS: u32 = 294;
/// Used for camera Zoom write
pub const MSG_ID_SET_ZOOM_FOCUS: u32 = 295;
/// Get the floodlight task xml
pub const MSG_ID_FLOODLIGHT_TASKS_READ: u32 = 438;
/// Get HDD/SD disk list (HDDInfoList)
pub const MSG_ID_HDD_INFO_LIST: u32 = 102;
/// Format disk(s) (HddInitList)
pub const MSG_ID_HDD_INIT_LIST: u32 = 103;
/// Replay: start playback (FileInfoList / BCMedia stream)
pub const MSG_ID_REPLAY_START: u32 = 5;
/// Replay: alternate start (some cameras accept this instead of MSG 5 for replay)
/// NOTE: This is the same wire ID as MSG_ID_DOWNLOAD_FILE_BY_NAME (NET_DOWNLOAD_V20 in Android SDK).
/// The camera dispatches on payload format: XML FileInfoList = replay; binary BC_DOWNLOAD_BY_NAME_INFO = file download.
pub const MSG_ID_REPLAY_START_ALT: u32 = 8;
/// Download a specific SD card file by name (NET_DOWNLOAD_V20, binary BC_DOWNLOAD_BY_NAME_INFO payload 0xD48 bytes).
/// Confirmed from Android SDK Ghidra analysis: table entry key=8, BaichuanDownloader::downloadFileByName sends MSG 8.
pub const MSG_ID_DOWNLOAD_FILE_BY_NAME: u32 = 8;
/// Stop download-file-by-name (NET_DOWNLOAD_STOP_V20, key=9). No payload.
pub const MSG_ID_DOWNLOAD_FILE_STOP: u32 = 9;
/// Replay: stop playback
pub const MSG_ID_REPLAY_STOP: u32 = 7;
/// Replay: file details by name (FileInfoList)
pub const MSG_ID_REPLAY_FILE_BY_NAME: u32 = 13;
/// Replay: get file list handle for day (FileInfoList)
pub const MSG_ID_REPLAY_FILE_LIST_HANDLE: u32 = 14;
/// Replay: list files by handle (FileInfoList)
pub const MSG_ID_REPLAY_FILE_LIST: u32 = 15;
/// Replay: FileInfoList variant (camera replies 200 OK only)
pub const MSG_ID_REPLAY_FILE_LIST_16: u32 = 16;
/// Replay: seek to position (ReplaySeek)
pub const MSG_ID_REPLAY_SEEK: u32 = 123;
/// Replay: desktop app binary replay (CMD 0x17d, payload 0x944 = 20-byte inner + 0x930 body)
pub const MSG_ID_REPLAY_DESKTOP: u32 = 0x17d;
/// Replay: get day records in range (DayRecords)
pub const MSG_ID_DAY_RECORDS: u32 = 142;
/// Download by time range (BC_DOWNLOAD_BY_TIME_INFO binary 0x480 bytes). End-of-stream = response 331.
pub const MSG_ID_DOWNLOAD_BY_TIME: u32 = 143;
/// Stop download-by-time (no payload)
pub const MSG_ID_DOWNLOAD_STOP: u32 = 144;
/// Alarm video search: find recordings by alarm/AI type (findAlarmVideo). 0xAF = 175.
pub const MSG_ID_ALARM_VIDEO_SEARCH: u32 = 175;

/// An empty password in legacy format
pub const EMPTY_LEGACY_PASSWORD: &str =
    "\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";

/// Top level bc message
#[derive(Debug, PartialEq)]
pub struct Bc {
    /// header part of the message
    pub meta: BcMeta,
    /// body part of the message which can either be Legacy or Modern
    pub body: BcBody,
}

///
///  The body of a bc message is either legacy or modern
///
#[derive(Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum BcBody {
    /// Legacy is unsupported except for login where it is used
    /// to negotiate the initial login and upgrade to modern
    LegacyMsg(LegacyMsg),
    /// Modern is the current reolink protocol it is mostly
    /// xml based
    ModernMsg(ModernMsg),
}

/// Modern messages have two payloads split by the `payload_offset` in the header
///
/// The first payload is extension which describes the second payload. If the
/// `payload_offset` is `0` then their is no `extension` (usually because it has
/// already been negotiated in a previous message) and it is `None`
///
/// The second payload contains the actual data of interest and is all bytes after
/// the `payload_offset` up to the `body_len`. If `payload_offset`
/// equals `body_len` then there is not payload and it is `None`
///
/// If `payload_offset` is `0` and equal to `body_len` then there is neither
/// `extension` or `payload` these are header only messages. This usually occurs to acknoledge receipt
/// of a command. In such cases the header `response_code` should be checked.
///
#[derive(Debug, Default, PartialEq)]
pub struct ModernMsg {
    /// Extension describes the following payload such as which channel id it is for
    pub extension: Option<Extension>,
    /// Primary payload which is dependant on MsgID
    pub payload: Option<BcPayloads>,
}

/// Legacy login formats. Only login is supported
#[derive(Debug, PartialEq, Eq)]
pub enum LegacyMsg {
    /// Legacy login data is constructed from username and password
    /// that will (usually but not always, depending on camera) be hashed
    LoginMsg {
        /// Username for a legacy login
        username: String,
        /// Password for a legacy login
        password: String,
    },
    /// Sent to upgrade to modern and not exposed the MD5 username/password
    LoginUpgrade,
    /// Any other type of legacy message will be collected here
    UnknownMsg,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct BcHeader {
    pub body_len: u32,
    pub msg_id: u32,
    pub channel_id: u8,
    pub stream_type: u8,
    pub msg_num: u16,
    pub response_code: u16,
    pub class: u16,
    pub payload_offset: Option<u32>,
}

/// The components of the Baichuan TLV header that are not
/// descriptions of the Body (the application dictates these)
#[derive(Debug, PartialEq, Eq)]
pub struct BcMeta {
    /// Message ID dictaes the major content of the message
    pub msg_id: u32,
    /// In most cases 0 but can be other values for NVRs
    pub channel_id: u8,
    /// In most cases this is unimportant but 0 means Clear Stream while 1 means Fluent stream
    /// This is only really used during `[MSG_ID_VIDEO]` streams when the SD `subStreams` are requested
    pub stream_type: u8,
    /// On modern messages this is the response code
    /// When sending a command it is set to `0`. The reply from the camera can be
    ///
    /// - `200` for OK
    ///
    /// - `400` for bad request
    ///
    /// A malformed packet will return a `400` code
    pub response_code: u16,
    /// A message ID is used to match replies with requests. The camera will parrot back
    /// this number in its reply
    ///
    /// If there a message is too long to fit in one packet it will be split over multiple
    /// messages all with the same `msg_num` (this can happing in video streams, talk and when
    /// sending a firmware update)
    pub msg_num: u16,
    /// The class is mostly an unknown quanitiy but does dictate the size of the header
    /// know values are
    ///
    /// - 0x6514: "legacy" 20 bytes
    /// - 0x6614: "modern" 20 bytes
    /// - 0x6414: "modern" 24 bytes
    /// - 0x0000: "modern" 24 bytes
    pub class: u16,
}

#[derive(Debug)]
pub(crate) struct BcContext {
    pub(crate) credentials: Credentials,
    pub(crate) in_bin_mode: RefCell<HashSet<u16>>,
    pub(crate) encryption_protocol: EncryptionProtocol,
    pub(crate) debug: bool,
    /// If true, replay binary payloads (MSG 5/8) are passed through without decryption.
    /// Set via env NEOLINK_REPLAY_RAW=1 to test cameras that send replay in plaintext.
    pub(crate) replay_raw_binary: bool,
}

impl Bc {
    /// Constructs a xml payload only Bc message
    pub fn new_from_xml(meta: BcMeta, xml: BcXml) -> Bc {
        Self::new(meta, None, Some(BcPayloads::BcXml(xml)))
    }

    /// Constructs an Extension only Bc message
    pub fn new_from_ext(meta: BcMeta, ext: Extension) -> Bc {
        Self::new(meta, Some(ext), None)
    }

    /// Constucts a header only Bc message
    pub fn new_from_meta(meta: BcMeta) -> Bc {
        Self::new(meta, None, None)
    }

    /// Constructs a message with both extension and xml payload
    pub fn new_from_ext_xml(meta: BcMeta, ext: Extension, xml: BcXml) -> Bc {
        Self::new(meta, Some(ext), Some(BcPayloads::BcXml(xml)))
    }

    /// General method to constructs a Bc message
    ///
    /// Use this if your constructing a binary payload but otherwise the other constructors
    /// are better suited to the task
    pub fn new(meta: BcMeta, extension: Option<Extension>, payload: Option<BcPayloads>) -> Bc {
        Bc {
            meta,
            body: BcBody::ModernMsg(ModernMsg { extension, payload }),
        }
    }
}

impl BcContext {
    pub(crate) fn new(credentials: Credentials) -> BcContext {
        let replay_raw_binary = std::env::var("NEOLINK_REPLAY_RAW")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        BcContext {
            credentials,
            in_bin_mode: RefCell::new(HashSet::new()),
            encryption_protocol: EncryptionProtocol::Unencrypted,
            debug: false,
            replay_raw_binary,
        }
    }

    #[allow(unused)] // Used in tests
    pub(crate) fn new_with_encryption(encryption_protocol: EncryptionProtocol) -> BcContext {
        BcContext {
            credentials: Default::default(),
            in_bin_mode: RefCell::new(HashSet::new()),
            encryption_protocol,
            debug: false,
            replay_raw_binary: false,
        }
    }

    pub(crate) fn set_encrypted(&mut self, encryption_protocol: EncryptionProtocol) {
        self.encryption_protocol = encryption_protocol;
    }

    pub(crate) fn get_encrypted(&self) -> &EncryptionProtocol {
        &self.encryption_protocol
    }

    pub(crate) fn binary_on(&mut self, msg_id: u16) {
        self.in_bin_mode.borrow_mut().insert(msg_id);
    }

    #[allow(unused)] // Used in tests
    pub(crate) fn binary_off(&mut self, msg_id: u16) {
        self.in_bin_mode.borrow_mut().remove(&msg_id);
    }

    /// Enable binary mode for msg_id (used from parser with &context).
    pub(crate) fn binary_on_shared(&self, msg_id: u16) {
        self.in_bin_mode.borrow_mut().insert(msg_id);
    }

    pub(crate) fn debug_on(&mut self) {
        self.debug = true;
    }
}

impl BcHeader {
    /// Check if this header corresponds to a known modern message class
    pub fn is_modern(&self) -> bool {
        // Most modern messages have an extra word at the end of the header; this
        // serves as the start offset of the appended payload data, if any.
        // A notable exception is the encrypted reply to the login message;
        // in this case the message is modern (with XML encryption etc), but there is
        // no extra word.
        // Here are the message classes:
        // 0x6514: legacy, no  bin offset (initial login message, encrypted or not)
        // 0x6614: modern, no  bin offset (reply to encrypted 0x6514 login)
        // 0x6414: modern, has bin offset, encrypted if supported (re-sent login message)
        // 0x0000, modern, has bin offset (most modern messages)
        self.class != 0x6514
    }

    /// Converts a header into a `BcMeta` this mostly works by striping aspects that are
    /// not desciptions of the data such as `msg_len`
    pub fn to_meta(&self) -> BcMeta {
        BcMeta {
            msg_id: self.msg_id,
            msg_num: self.msg_num,
            channel_id: self.channel_id,
            response_code: self.response_code,
            stream_type: self.stream_type,
            class: self.class,
        }
    }

    /// Constuct a [`BcHeader`] from a [`BcMeta`]
    ///
    /// This requires additional data such as the `body_len`
    ///
    /// # Parameters
    ///
    /// * `meta` - the [`BcMeta`] to convert
    ///
    /// * `body_len` - The length of the body (extension and payload) in bytes
    ///
    /// * `payload_offset` - The location in bytes where the payload starts and extension ends
    ///
    /// # Returns
    ///
    /// returns the new `BcHeader`
    ///
    pub fn from_meta(meta: &BcMeta, body_len: u32, payload_offset: Option<u32>) -> BcHeader {
        BcHeader {
            payload_offset,
            body_len,
            msg_id: meta.msg_id,
            channel_id: meta.channel_id,
            stream_type: meta.stream_type,
            response_code: meta.response_code,
            msg_num: meta.msg_num,
            class: meta.class,
        }
    }
}

pub(super) fn has_payload_offset(class: u16) -> bool {
    // See BcHeader::is_modern() for a description of which packets have the bin offset
    class == 0x6414 || class == 0x0000
}
