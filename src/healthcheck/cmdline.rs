use clap::Parser;

/// The healthcheck command reports a camera unhealthy when the bridge is
/// serving its stream to a client but has stopped delivering frames, and exits
/// non-zero. Intended for a container HEALTHCHECK, so a frozen bridge (process
/// still "running" but serving no frames) is detected and can be restarted. It
/// reads the bridge's own heartbeat rather than opening an RTSP pull, so it
/// never competes with a real consumer for the camera's limited stream slots.
#[derive(Parser, Debug)]
pub struct Opt {
    /// Only check this camera (a name from the config). Default: every configured camera.
    pub camera: Option<String>,
    /// Seconds a served stream may go without a frame before it is unhealthy.
    #[arg(short, long, default_value_t = 10)]
    pub timeout: u64,
}
