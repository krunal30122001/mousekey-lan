use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tracing_subscriber::EnvFilter;

const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Structured logging to `<data>/logs/mousekey.log` (rotated once at 5 MB).
/// Input events are never logged. Override verbosity with `MKL_LOG=debug`.
pub fn init(data_dir: &Path) -> PathBuf {
    let dir = data_dir.join("logs");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("mousekey.log");
    if fs::metadata(&path).map(|m| m.len() > MAX_LOG_BYTES).unwrap_or(false) {
        let _ = fs::rename(&path, dir.join("mousekey.old.log"));
    }
    let filter = EnvFilter::try_from_env("MKL_LOG").unwrap_or_else(|_| EnvFilter::new("info,mdns_sd=warn"));
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .with_writer(Mutex::new(file))
                .try_init();
        }
        Err(_) => {
            let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
        }
    }
    path
}
