///
/// # Neolink Healthcheck
///
/// This module handles the healthcheck subcommand.
///
/// It reports a camera unhealthy when the bridge is actively serving that
/// camera's stream to a client but has stopped delivering frames - a frozen
/// bridge, where the process keeps "running" and the RTSP port still answers but
/// no frames flow, which a plain TCP/port check would miss. Crucially it does
/// **not** open its own RTSP pull: Reolink cameras allow only a few concurrent
/// streams, so probing by connecting would either be refused (a false
/// "unhealthy") or steal a slot from a real consumer. Instead the running bridge
/// writes a heartbeat as it serves each client (see [`heartbeat`]), and this
/// command only reads it.
///
/// A camera that no client is pulling has no heartbeat and is treated as healthy
/// - there are no frames that ought to be flowing.
///
/// # Usage
///
/// ```bash
/// neolink healthcheck --config=config.toml [CameraName]
/// ```
///
/// Exit code 0 = no actively-served stream is frozen; non-zero = at least one is.
///
use anyhow::{anyhow, Result};
use std::time::Duration;

mod cmdline;
pub(crate) mod heartbeat;

pub(crate) use cmdline::Opt;

use crate::config::Config;

/// Entry point for the healthcheck subcommand
pub(crate) async fn main(opt: Opt, config: Config) -> Result<()> {
    let cameras: Vec<String> = match &opt.camera {
        Some(name) => {
            if !config.cameras.iter().any(|c| &c.name == name) {
                return Err(anyhow!("No camera named {name:?} in the config"));
            }
            vec![name.clone()]
        }
        None => config.cameras.iter().map(|c| c.name.clone()).collect(),
    };
    if cameras.is_empty() {
        return Err(anyhow!("No cameras configured to check"));
    }

    let timeout = Duration::from_secs(opt.timeout.max(1));
    let stale = heartbeat::stale_cameras(&cameras, timeout);

    if stale.is_empty() {
        for name in &cameras {
            log::info!("healthcheck {name}: OK");
        }
        Ok(())
    } else {
        for name in &stale {
            log::warn!(
                "healthcheck {name}: FAILED (serving a client but no frame in {}s)",
                timeout.as_secs()
            );
        }
        Err(anyhow!(
            "{} of {} camera(s) frozen (serving a client but no frame in {}s): {}",
            stale.len(),
            cameras.len(),
            timeout.as_secs(),
            stale.join(", ")
        ))
    }
}
