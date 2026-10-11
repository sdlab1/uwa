//! Where uwa keeps its state.
//!
//! Everything machine-local (PID files, daemon logs, the default Chromium
//! profile) lives under a single directory so `rm -rf` is a clean uninstall.

use std::path::PathBuf;

pub fn uwa_home() -> PathBuf {
    if let Ok(v) = std::env::var("UWA_HOME") {
        return PathBuf::from(v);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".uwa")
}

pub fn pid_file() -> PathBuf {
    uwa_home().join("uwa.pid")
}

pub fn daemon_log() -> PathBuf {
    uwa_home().join("daemon.log")
}

/// The managed Chromium profile. Separate from the user's daily browser
/// on purpose — otherwise `uwa start` would either collide with a running
/// Chrome or reuse the user's logins without asking.
pub fn chromium_profile() -> PathBuf {
    uwa_home().join("chromium-profile")
}

pub fn ensure_home() -> std::io::Result<PathBuf> {
    let h = uwa_home();
    std::fs::create_dir_all(&h)?;
    Ok(h)
}
