//! Single-instance guard via a pid file. No `sysinfo` dependency (audit C8).

use std::path::{Path, PathBuf};
use uwa_core::{Result, UwaError};

#[derive(Debug)]
pub struct PidFile {
    path: PathBuf,
}

impl PidFile {
    /// Claim `path`. Fails if a *live* process already owns it.
    pub fn acquire(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| UwaError::Config(format!("read pid: {e}")))?;
            if let Ok(pid) = raw.trim().parse::<u32>() {
                if process_alive(pid) {
                    return Err(UwaError::Config(format!(
                        "another instance is running (pid {pid}); remove {} to override",
                        path.display()
                    )));
                }
            }
        }
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| UwaError::Config(format!("create {}: {e}", parent.display())))?;
            }
        }
        std::fs::write(&path, std::process::id().to_string())
            .map_err(|e| UwaError::Config(format!("write pid: {e}")))?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("uwa-pid-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn writes_and_removes() {
        let path = tmp("a.pid");
        let _ = std::fs::remove_file(&path);
        {
            let pid = PidFile::acquire(&path).unwrap();
            assert!(path.exists());
            assert_eq!(
                std::fs::read_to_string(&path).unwrap().trim(),
                std::process::id().to_string()
            );
            drop(pid);
        }
        assert!(!path.exists());
    }

    #[test]
    fn rejects_live_pid() {
        let path = tmp("b.pid");
        std::fs::write(&path, std::process::id().to_string()).unwrap();
        // The test binary itself is alive, so this must be rejected.
        let err = PidFile::acquire(&path).unwrap_err();
        assert!(matches!(err, UwaError::Config(_)));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn stale_pid_ok() {
        let path = tmp("c.pid");
        // PID 1 may be alive on unix; use a very unlikely-high number instead.
        std::fs::write(&path, "4194303").unwrap();
        let pid = PidFile::acquire(&path).unwrap();
        drop(pid);
        let _ = std::fs::remove_file(&path);
    }

    /// The suite creates `uwa-pid-test-{pid}` once per run and only removes
    /// the files inside — without this sweep the *directories* accumulate
    /// in /tmp forever. Alphabetically last, runs after the others.
    #[test]
    fn zzz_removes_the_temp_dir() {
        let dir = std::env::temp_dir().join(format!("uwa-pid-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!dir.exists());
    }
}
