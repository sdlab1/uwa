//! Platform home-dir helpers. Kept separate so the tests can fake `$HOME`.

use std::path::PathBuf;

pub fn uwa_home() -> PathBuf {
    if let Ok(v) = std::env::var("UWA_HOME") {
        return PathBuf::from(v);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".uwa")
}

pub fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

/// `~/.config` on Linux, `~/Library/Application Support` on macOS,
/// `%APPDATA%` on Windows. Falls back to `$HOME/.config` when the
/// platform-specific var is unset.
pub fn xdg_config() -> Option<PathBuf> {
    if let Some(p) = dirs::config_dir() {
        return Some(p);
    }
    home().map(|h| h.join(".config"))
}
