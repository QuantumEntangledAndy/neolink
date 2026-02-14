use clap::{crate_authors, crate_version, Parser};
use std::path::PathBuf;
use std::str::FromStr;

/// A standards-compliant bridge to Reolink IP cameras
///
/// Neolink is free software released under the GNU AGPL v3.
/// You can find its source code at https://github.com/thirtythreeforty/neolink
#[derive(Parser, Debug)]
#[command(name = "neolink", arg_required_else_help = true, version = crate_version!(), author = crate_authors!("\n"))]
pub struct Opt {
    #[arg(short, long, global = true, value_parser = PathBuf::from_str)]
    pub config: Option<PathBuf>,
    #[structopt(subcommand)]
    pub cmd: Option<Command>,
}

#[derive(Parser, Debug)]
pub enum Command {
    #[cfg(feature = "gstreamer")]
    Rtsp(super::rtsp::Opt),
    StatusLight(super::statusled::Opt),
    Reboot(super::reboot::Opt),
    Pir(super::pir::Opt),
    Ptz(super::ptz::Opt),
    #[cfg(feature = "gstreamer")]
    Talk(super::talk::Opt),
    Mqtt(super::mqtt::Opt),
    #[cfg(feature = "gstreamer")]
    MqttRtsp(super::mqtt::Opt),
    #[cfg(feature = "gstreamer")]
    Image(super::image::Opt),
    Battery(super::battery::Opt),
    Services(super::services::Opt),
    Users(super::users::Opt),
    Encoding(super::encoding::Opt),
}
