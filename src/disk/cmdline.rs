use clap::Parser;

/// Disk subcommand: list HDD/SD or format disk(s). Default is list.
#[derive(Parser, Debug)]
pub struct Opt {
    /// Camera name from config
    pub camera: String,
    /// Subcommand (default: list)
    #[command(subcommand)]
    pub cmd: Option<DiskCommand>,
}

#[derive(Parser, Debug)]
pub enum DiskCommand {
    /// List HDD/SD disks and their status
    List,
    /// Format one or more disks (e.g. SD card slots)
    Format {
        /// Disk/slot number(s) to format (from `disk list`). Example: 0 or 0 1
        #[arg(short, long, value_delimiter = ' ', num_args = 1..)]
        disk: Vec<u8>,
        /// Perform full format instead of quick format
        #[arg(long)]
        full: bool,
    },
}
