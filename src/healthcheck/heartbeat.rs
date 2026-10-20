///
/// # Neolink health heartbeats
///
/// Shared between the running bridge (the writer) and the `healthcheck`
/// subcommand (the reader), which are separate processes, so the liveness
/// signal travels through small files in a shared directory rather than through
/// in-process state.
///
/// While a client is actively pulling a camera stream, the bridge keeps a
/// heartbeat file whose contents are refreshed to the current time on every
/// frame it delivers. If the bridge freezes - the process stays up and the RTSP
/// port keeps answering, but frames stop - the file stops being refreshed and
/// goes stale, which is what a plain port check cannot see. A camera that nobody
/// is pulling has no file at all, so an idle bridge is never reported unhealthy:
/// there are simply no frames that ought to be flowing.
///
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Frames arrive many times a second but the staleness window is seconds, so
/// coalesce writes to at most one per second to spare the filesystem.
const MIN_WRITE_INTERVAL_MS: u64 = 1000;

/// The BC stream names a camera can expose, matching `StreamKind`'s `Display`.
/// The reader checks these exact files, so it never has to scan or guess.
const STREAMS: [&str; 3] = ["mainStream", "subStream", "externStream"];

/// Directory holding the per-stream heartbeat files.
///
/// Overridable with `NEOLINK_HEALTH_DIR` so the writer and reader can be pointed
/// at the same place if the default temp dir is not shared. For the Docker
/// `HEALTHCHECK` both run in the same container, so the default is enough.
pub(crate) fn heartbeat_dir() -> PathBuf {
    std::env::var_os("NEOLINK_HEALTH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("neolink-health"))
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn file_for(camera: &str, stream: &str) -> PathBuf {
    heartbeat_dir().join(format!("{}__{}.beat", sanitize(camera), sanitize(stream)))
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Refcount of live heartbeats per file, so concurrent clients pulling the same
/// stream do not remove each other's file; it is deleted only when the last one
/// is dropped.
fn registry() -> &'static Mutex<HashMap<PathBuf, usize>> {
    static REG: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A live heartbeat for one client pulling one camera stream.
///
/// It is stamped on creation and refreshed by [`Heartbeat::beat`]; when the last
/// heartbeat for a stream is dropped (the client went away) its file is removed,
/// so a normal disconnect does not leave a stale file that would look like a
/// freeze.
pub(crate) struct Heartbeat {
    path: PathBuf,
    last_write_ms: AtomicU64,
}

impl Heartbeat {
    /// Register a client as actively pulling `camera`/`stream` and stamp it now,
    /// so a stream that connects but never delivers a frame still goes stale and
    /// is caught, instead of looking idle.
    pub(crate) fn new(camera: &str, stream: &str) -> Self {
        let path = file_for(camera, stream);
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        {
            let mut reg = registry().lock().unwrap();
            *reg.entry(path.clone()).or_insert(0) += 1;
        }
        let hb = Self {
            path,
            last_write_ms: AtomicU64::new(0),
        };
        hb.write_now();
        hb
    }

    fn write_now(&self) {
        let now = now_millis();
        let _ = fs::write(&self.path, now.to_string());
        self.last_write_ms.store(now as u64, Ordering::Relaxed);
    }

    /// Record that a frame just flowed for this stream, coalescing to at most
    /// one write per [`MIN_WRITE_INTERVAL_MS`].
    pub(crate) fn beat(&self) {
        let now = now_millis() as u64;
        let last = self.last_write_ms.load(Ordering::Relaxed);
        if now.saturating_sub(last) >= MIN_WRITE_INTERVAL_MS {
            self.write_now();
        }
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        let mut reg = registry().lock().unwrap();
        if let Some(count) = reg.get_mut(&self.path) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                reg.remove(&self.path);
                let _ = fs::remove_file(&self.path);
            }
        }
    }
}

/// Return the subset of `cameras` that have at least one actively-pulled stream
/// whose heartbeat has not been refreshed within `timeout` - a frozen bridge.
///
/// A camera with no heartbeat file is idle (nobody is pulling it) and is never
/// returned, because there are no frames that ought to be flowing.
pub(crate) fn stale_cameras(cameras: &[String], timeout: Duration) -> Vec<String> {
    let now = now_millis();
    let timeout_ms = timeout.as_millis();
    let mut stale = Vec::new();

    for camera in cameras {
        let frozen = STREAMS.iter().any(|stream| {
            match fs::read_to_string(file_for(camera, stream)) {
                Ok(s) => match s.trim().parse::<u128>() {
                    // Present but not refreshed within the window => frozen.
                    Ok(ts) => now.saturating_sub(ts) > timeout_ms,
                    Err(_) => false,
                },
                // Absent => this stream is not being pulled; nothing to fail.
                Err(_) => false,
            }
        });
        if frozen {
            stale.push(camera.clone());
        }
    }
    stale
}
