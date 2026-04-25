use clap::Parser;

/// Standard record types for file search (MSG 14). AI-specific tags (people, vehicle, etc.)
/// are in the response recordType field, not valid search parameters. Use --ai-filter to
/// filter by AI detection tags client-side.
pub const FILE_SEARCH_RECORD_TYPES: &str = "manual,sched,md,pir,io";

/// All known record/alarm types including AI detections. Used for alarm search (MSG 175).
pub const ALL_RECORD_TYPES: &str =
    "manual,sched,md,pir,io,people,vehicle,face,dog_cat,package,visitor,cry,crossline,intrusion,loitering,nonmotorveh,other,legacy,loss";

/// Replay: list recording days, list files, start/stop playback (SD card).
#[derive(Parser, Debug)]
pub struct Opt {
    /// Camera name from config
    pub camera: String,
    #[command(subcommand)]
    pub cmd: ReplayCommand,
}

#[derive(Parser, Debug)]
pub enum ReplayCommand {
    /// List days that have recordings in a date range
    Days {
        /// Start date (YYYY-MM-DD)
        #[arg(long)]
        start: String,
        /// End date (YYYY-MM-DD). Defaults to start if omitted
        #[arg(long)]
        end: Option<String>,
    },
    /// List recording files for a single day
    Files {
        /// Date (YYYY-MM-DD)
        #[arg(long)]
        date: String,
        /// Stream type: mainStream or subStream. If omitted, lists files for both streams.
        #[arg(long)]
        stream: Option<String>,
        /// Record types to search for (comma-separated). Use --ai-filter to filter by AI tags.
        #[arg(long, default_value = FILE_SEARCH_RECORD_TYPES)]
        record_type: String,
        /// Filter results by AI detection type (comma-separated, e.g. "people,vehicle,dog_cat").
        /// Only files whose recordType contains at least one of these tags are shown.
        #[arg(long)]
        ai_filter: Option<String>,
    },
    /// Start replay playback (stream BCMedia for a recording file)
    Play {
        /// File name from "replay files" output
        #[arg(long)]
        name: String,
        /// Stream type: mainStream or subStream
        #[arg(long, default_value = "subStream")]
        stream: String,
        /// Play speed (1 = normal)
        #[arg(long, default_value = "1")]
        speed: u32,
        /// Output path for replay (e.g. .mp4). If omitted, stream is discarded (logs only; use for testing).
        #[arg(long)]
        output: Option<std::path::PathBuf>,
        /// Stop after N seconds and close the file (camera keeps streaming otherwise). Use e.g. 10 for a 10s clip.
        #[arg(long)]
        duration: Option<u64>,
        /// Debug: write raw replay stream (after 32-byte header) to this file. Use xxd or hex editor to inspect for ftyp/moov/mdat.
        #[arg(long)]
        dump_replay: Option<std::path::PathBuf>,
        /// Max bytes to write to --dump-replay (default 131072). Use 0 for full stream.
        #[arg(long)]
        dump_replay_limit: Option<usize>,
    },
    /// Download a recording file (same as play; stops when camera sends response 300 or after --duration)
    Download {
        /// File name from "replay files" output
        #[arg(long)]
        name: String,
        /// Stream type: mainStream or subStream
        #[arg(long, default_value = "subStream")]
        stream: String,
        /// Output path for the downloaded file (e.g. .mp4)
        #[arg(long)]
        output: Option<std::path::PathBuf>,
        /// Stop after N seconds if camera does not send response 300
        #[arg(long)]
        duration: Option<u64>,
        /// Debug: write raw replay stream to this file (default limit 128KB; use --dump-replay-limit 0 for full).
        #[arg(long)]
        dump_replay: Option<std::path::PathBuf>,
        #[arg(long)]
        dump_replay_limit: Option<usize>,
    },
    /// Download a specific SD card file directly by name (MSG 8, NET_DOWNLOAD_V20).
    /// Requests the raw MP4 file from the camera without BcMedia re-encoding.
    /// Uses a binary BC_DOWNLOAD_BY_NAME_INFO payload; camera streams the file back.
    /// Faster than 'download' for cameras that support MSG 8.
    DownloadByName {
        /// File name from "replay files" output (e.g. "01_20240204120000")
        #[arg(long)]
        name: String,
        /// Output path for the downloaded file (e.g. out.mp4)
        #[arg(long)]
        output: std::path::PathBuf,
        /// Debug: write raw stream to this file for inspection
        #[arg(long)]
        dump_replay: Option<std::path::PathBuf>,
        /// Max bytes to write to --dump-replay (default 131072). Use 0 for full stream.
        #[arg(long)]
        dump_replay_limit: Option<usize>,
    },
    /// Download by time range (MSG 143; camera sends response 331 at end). Use when you want a date range instead of a file name.
    DownloadByTime {
        /// Start date (YYYY-MM-DD); time 00:00:00
        #[arg(long)]
        start: String,
        /// End date (YYYY-MM-DD); time 23:59:59. Defaults to start if omitted
        #[arg(long)]
        end: Option<String>,
        /// Stream type: mainStream or subStream
        #[arg(long, default_value = "subStream")]
        stream: String,
        /// Output path for the downloaded file (e.g. .mp4)
        #[arg(long)]
        output: Option<std::path::PathBuf>,
        /// Stop after N seconds if camera does not send response 331
        #[arg(long)]
        duration: Option<u64>,
        /// Debug: write raw replay stream to this file (default limit 128KB; use --dump-replay-limit 0 for full).
        #[arg(long)]
        dump_replay: Option<std::path::PathBuf>,
        #[arg(long)]
        dump_replay_limit: Option<usize>,
    },
    /// Stop replay playback (pass file name from "replay files")
    Stop {
        /// File name to stop (e.g. from "replay files" output)
        #[arg(long)]
        name: String,
    },
    /// Search recordings by alarm/AI type (MSG 175). Server-side filtering by detection type.
    AlarmSearch {
        /// Start date (YYYY-MM-DD)
        #[arg(long)]
        start: String,
        /// End date (YYYY-MM-DD). Defaults to start if omitted
        #[arg(long)]
        end: Option<String>,
        /// Stream type: 0 = mainStream, 1 = subStream
        #[arg(long, default_value = "1")]
        stream_type: u8,
        /// Alarm/AI types to search for (comma-separated, e.g. "md,people,vehicle,dog_cat").
        /// Defaults to all known types.
        #[arg(long, default_value = ALL_RECORD_TYPES)]
        alarm_types: String,
    },
}
