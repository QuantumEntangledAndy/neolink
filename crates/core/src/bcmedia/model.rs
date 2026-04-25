/// Video streams encapsulate a stream of BcMedia
#[derive(Debug, Clone)]
pub enum BcMedia {
    /// Holds info on the stream
    InfoV1(BcMediaInfoV1),
    /// Holds info on the stream
    InfoV2(BcMediaInfoV2),
    /// Holds an IFrame either H264 or H265
    Iframe(BcMediaIframe),
    /// Holds a PFrame either H264 or H265
    Pframe(BcMediaPframe),
    /// Holds AAC audio
    Aac(BcMediaAac),
    /// Holds ADPCM audio
    Adpcm(BcMediaAdpcm),
    /// Raw container data (e.g. MP4) from replay; receive-only, not BcMedia-framed.
    RawReplayChunk(Vec<u8>),
    /// Replay stream started with this message ID (5, 8, or 0x17d). Receive-only; consumer uses it to
    /// skip the first 32 bytes only when msg_id == 5 (app parity per BCMEDIA_REPLAY_FORMAT §5).
    ReplayStarted(u16),
    /// Replay/download stream ended normally (camera sent response 300 or 331). Receive-only sentinel.
    StreamEnd,
}
//
pub(super) const MAGIC_HEADER_BCMEDIA_INFO_V1: u32 = 0x31303031;

/// The start of a BcMedia stream contains this message
/// which describes the data to follow
#[derive(Debug, Clone)]
pub struct BcMediaInfoV1 {
    // This is the size of the header so it's actually a fixed value
    // The other messages have body size here so maybe that's why
    // it's included
    // pub header_size: u32,
    /// Width of the video
    pub video_width: u32,
    /// Height of the video
    pub video_height: u32,
    // pub unknown: u8,
    /// Frames per second. On older cameras this seems to be an index of the FPS on a lookup table
    pub fps: u8,
    /// Start year of the stream
    pub start_year: u8,
    /// Start month of the stream
    pub start_month: u8,
    /// Start day of the stream
    pub start_day: u8,
    /// Start hour of the stream
    pub start_hour: u8,
    /// Start minute of the stream
    pub start_min: u8,
    /// Start seconds of the stream
    pub start_seconds: u8,
    /// End year of the video probably only useful for the recorded files on the SD card
    pub end_year: u8,
    /// End month of the video probably only useful for the recorded files on the SD card
    pub end_month: u8,
    /// End day of the video probably only useful for the recorded files on the SD card
    pub end_day: u8,
    /// End hour of the video probably only useful for the recorded files on the SD card
    pub end_hour: u8,
    /// End min of the video probably only useful for the recorded files on the SD card
    pub end_min: u8,
    /// End seconds of the video probably only useful for the recorded files on the SD card
    pub end_seconds: u8,
    // unknown: u16
}
//
pub(super) const MAGIC_HEADER_BCMEDIA_INFO_V2: u32 = 0x32303031;

/// The start of a BcMedia stream contains this message
/// which describes the data to follow
#[derive(Debug, Clone)]
pub struct BcMediaInfoV2 {
    // This is the size of the header so it's actually a fixed value
    // The other messages have body size here so maybe that's why
    // it's included
    // pub header_size: u32,
    /// Width of the video
    pub video_width: u32,
    /// Height of the video
    pub video_height: u32,
    // pub unknown: u8,
    /// Frames per second. On older cameras this seems to be an index of the FPS on a lookup table
    pub fps: u8,
    /// Start year of the stream
    pub start_year: u8,
    /// Start month of the stream
    pub start_month: u8,
    /// Start day of the stream
    pub start_day: u8,
    /// Start hour of the stream
    pub start_hour: u8,
    /// Start minute of the stream
    pub start_min: u8,
    /// Start seconds of the stream
    pub start_seconds: u8,
    /// End year of the video probably only useful for the recorded files on the SD card
    pub end_year: u8,
    /// End month of the video probably only useful for the recorded files on the SD card
    pub end_month: u8,
    /// End day of the video probably only useful for the recorded files on the SD card
    pub end_day: u8,
    /// End hour of the video probably only useful for the recorded files on the SD card
    pub end_hour: u8,
    /// End min of the video probably only useful for the recorded files on the SD card
    pub end_min: u8,
    /// End seconds of the video probably only useful for the recorded files on the SD card
    pub end_seconds: u8,
    // unknown: u16
}

// IFrame magics include the channel number in them
pub(super) const MAGIC_HEADER_BCMEDIA_IFRAME: u32 = 0x63643030;
pub(super) const MAGIC_HEADER_BCMEDIA_IFRAME_LAST: u32 = 0x63643039;

/// Video Types for I/PFrame
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoType {
    /// H264 video data
    H264,
    /// H265 video data
    H265,
}

/// This is a BcMedia video IFrame.
#[derive(Clone)]
pub struct BcMediaIframe {
    /// "H264", or "H265"
    pub video_type: VideoType,
    // Size of payload after header in bytes
    // pub payload_size: u32,
    // unknown: u32, // NVR channel count? Known values 1-00/08 2-00 3-00 4-00
    /// Timestamp in microseconds
    pub microseconds: u32,
    // unknown: u32, // Known values 1-00/23/5A 2-00 3-00 4-00
    /// POSIX time (seconds since 00:00:00 Jan 1 1970)
    pub time: Option<u32>,
    //unknown: u32, // Known values 1-00/06/29 2-00/01 3-00/C3 4-00
    /// Raw IFrame data
    pub data: Vec<u8>,
}

impl std::fmt::Debug for BcMediaIframe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entry(&"video_type", &self.video_type)
            // .entry(&"payload_size", &self.payload_size)
            .entry(&"microseconds", &self.microseconds)
            .entry(&"time", &self.time)
            .entry(
                &"data[0..10]",
                &self.data[0..std::cmp::min(20, self.data.len())].to_vec(),
            )
            .entry(
                &"data[-10..-1]",
                &self.data[std::cmp::max(0, self.data.len() - 20)..self.data.len()].to_vec(),
            )
            .entry(&"data.len()", &self.data.len())
            .finish()
    }
}

// PFrame magics include the channel number in them
pub(super) const MAGIC_HEADER_BCMEDIA_PFRAME: u32 = 0x63643130;
pub(super) const MAGIC_HEADER_BCMEDIA_PFRAME_LAST: u32 = 0x63643139;

/// This is a BcMedia video PFrame.
#[derive(Clone)]
pub struct BcMediaPframe {
    /// "H264", or "H265"
    pub video_type: VideoType,
    // Size of payload after header in bytes
    // pub payload_size: u32,
    // unknown: u32, // NVR channel count? Known values 1-00/08 2-00 3-00 4-00
    /// Timestamp in microseconds
    pub microseconds: u32,
    // unknown: u32, // Known values 1-00/23/5A 2-00 3-00 4-00
    /// Raw PFrame data
    pub data: Vec<u8>,
}

impl std::fmt::Debug for BcMediaPframe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entry(&"video_type", &self.video_type)
            // .entry(&"payload_size", &self.payload_size)
            .entry(&"microseconds", &self.microseconds)
            .entry(
                &"data[0..20]",
                &self.data[0..std::cmp::min(20, self.data.len())].to_vec(),
            )
            .entry(
                &"data[-20..-1]",
                &self.data[std::cmp::max(0, self.data.len() - 20)..self.data.len()].to_vec(),
            )
            .entry(&"data.len()", &self.data.len())
            .finish()
    }
}

pub(super) const MAGIC_HEADER_BCMEDIA_AAC: u32 = 0x62773530;

/// This contains BcMedia audio data in AAC format
#[derive(Debug, Clone)]
pub struct BcMediaAac {
    // Size of payload after header in bytes
    // pub payload_size: u16,
    // Size of payload after header in bytes exactly the same as before
    // pub payload_size_b: u16,
    /// Raw AAC data
    pub data: Vec<u8>,
}

impl BcMediaAac {
    /// Read the ADTS header to learn the duration in micro secs
    pub fn duration(&self) -> Option<u32> {
        if self.data.len() < 8 {
            // Too small for the header
            return None;
        }
        if self.data[0] != 0b11111111 {
            // Syncword incorrect
            return None;
        }
        if (self.data[1] & 0b11110000) != 0b11110000 {
            // Syncword incorrect
            return None;
        }
        let frequency_index = (self.data[2] & 0b00111100) >> 2;
        let sample_frequency = match frequency_index {
            0 => Some(96000u32),
            1 => Some(88200u32),
            2 => Some(64000u32),
            3 => Some(48000u32),
            4 => Some(44100u32),
            5 => Some(32000u32),
            6 => Some(24000u32),
            7 => Some(22050u32),
            8 => Some(16000u32),
            9 => Some(12000u32),
            10 => Some(11025u32),
            11 => Some(8000u32),
            12 => Some(7350u32),
            _ => None,
        }?;
        log::trace!("sample_frequency: {sample_frequency}");

        let frames = (self.data[6] & 0b00000011) + 1;
        log::trace!("frames: {frames}");
        let samples = frames as u32 * 1024;
        log::trace!("samples: {samples}");
        const MICROSECONDS: u32 = 1000000;
        let duration = samples * MICROSECONDS / sample_frequency;
        Some(duration)
    }
}

pub(super) const MAGIC_HEADER_BCMEDIA_ADPCM: u32 = 0x62773130;

pub(super) const MAGIC_HEADER_BCMEDIA_ADPCM_DATA: u16 = 0x0100;

/// This contains BcMedia audio data in ADPCM format
#[derive(Debug, Clone)]
pub struct BcMediaAdpcm {
    // Size of payload after header in bytes
    // pub payload_size: u16,
    // Size of payload after header in bytes exactly the same as before
    // pub payload_size_b: u16,
    // more_magic: MAGIC_HEADER_BCMEDIA_ADPCM_DATA
    // Adpcm sample_block_size in bytes
    //
    // These bytes (and the MAGIC_HEADER_BCMEDIA_ADPCM_DATA) are included as
    // part of the payload_size. It may be more prudent to sealise them to
    // another structure.
    // pub sample_block_size: u16,
    /// The raw adpcm data in DVI-4 layout.
    ///
    /// One `data` should contain 4 bytes of the adpcm predictor state then one block
    /// of adpcm samples
    ///
    /// To calculate the block-align size simply remove 4 from the `len()`
    pub data: Vec<u8>,
}

impl BcMediaAdpcm {
    /// The block size, this is bytes without the block header
    pub fn block_size(&self) -> u32 {
        self.data.len() as u32 - 4
    }

    /// Returns duration in micro seconds;
    pub fn duration(&self) -> Option<u32> {
        let samples = self.block_size() * 2;
        // Always 8000Hz for ADPCM
        const SAMPLE_FREQUENCY: u32 = 8000;
        const MICROSECONDS: u32 = 1000000;
        let duration = samples * MICROSECONDS / SAMPLE_FREQUENCY;
        Some(duration)
    }
}
