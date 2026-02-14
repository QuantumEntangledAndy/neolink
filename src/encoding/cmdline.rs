use clap::{Parser, Subcommand, ValueEnum};

/// Control video encoding settings per stream
#[derive(Parser, Debug)]
pub struct Opt {
    /// The name of the camera. Must be a name in the config
    pub camera: String,
    #[command(subcommand)]
    pub cmd: EncodingAction,
}

#[derive(Subcommand, Debug)]
pub enum EncodingAction {
    /// Get the current encoding settings for all streams
    Get,
    /// Set encoding parameters for a stream
    Set {
        /// Which stream to modify
        #[arg(long)]
        stream: StreamName,
        /// Bitrate in kbps (e.g. 4096)
        #[arg(long)]
        bitrate: Option<u32>,
        /// Frames per second (e.g. 15)
        #[arg(long)]
        fps: Option<u32>,
        /// Rate control mode
        #[arg(long)]
        rate_control: Option<RateControl>,
        /// Encoder profile
        #[arg(long)]
        profile: Option<Profile>,
    },
}

#[derive(Debug, Clone, ValueEnum)]
pub enum StreamName {
    Main,
    Sub,
    Third,
}

#[derive(Debug, Clone, ValueEnum)]
pub enum RateControl {
    Cbr,
    Vbr,
}

#[derive(Debug, Clone, ValueEnum)]
pub enum Profile {
    Default,
    BaseLine,
    High,
    Main,
}

impl std::fmt::Display for RateControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RateControl::Cbr => write!(f, "cbr"),
            RateControl::Vbr => write!(f, "vbr"),
        }
    }
}

impl std::fmt::Display for Profile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Profile::Default => write!(f, "default"),
            Profile::BaseLine => write!(f, "baseLine"),
            Profile::High => write!(f, "high"),
            Profile::Main => write!(f, "main"),
        }
    }
}
