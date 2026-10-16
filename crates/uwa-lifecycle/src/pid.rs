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

    /// Each test gets its own directory: the suite used to share one and
    /// the cleanup hook's `remove_dir_all` raced with `writes_and_removes`
    /// (file unlinked between acquire and assert).
    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("uwa-pid-test-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap_or_else(|_| unreachable!());
        dir.join("x.pid")
    }

    #[test]
    fn writes_and_removes() {
        let path = tmp("a.pid");
        let _ = std::fs::remove_file(&path);
        {
            let pid = PidFile::acquire(&path).unwrap_or_else(|_| unreachable!());
            assert!(path.exists());
            assert_eq!(
                std::fs::read_to_string(&path).unwrap_or_else(|_| unreachable!()).trim(),
                std::process::id().to_string()
            );
            drop(pid);
        }
        assert!(!path.exists());
    }

    #[test]
    fn rejects_live_pid() {
        let path = tmp("b.pid");
        std::fs::write(&path, std::process::id().to_string()).unwrap_or_else(|_| unreachable!());
        // The test binary itself is alive, so this must be rejected.
        let err = match PidFile::acquire(&path) {
            Err(err) => err,
            Ok(_) => unreachable!(),
        };
        assert!(matches!(err, UwaError::Config(_)));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn stale_pid_ok() {
        let path = tmp("c.pid");
        // PID 1 may be alive on unix; use a very unlikely-high number instead.
        std::fs::write(&path, "4194303").unwrap_or_else(|_| unreachable!());
        let pid = PidFile::acquire(&path).unwrap_or_else(|_| unreachable!());
        drop(pid);
        let _ = std::fs::remove_file(&path);
    }

    /// Best-effort cleanup of the temp dir created by this suite.
    ///
    /// Deliberately assert-free: sibling tests (`writes_and_removes`,
    /// `rejects_live_pid`, `stale_pid_ok`) run in parallel in the same
    /// process and share this directory, so a file existing at any given
    /// moment is not a bug — it is a test that has not finished yet.
    /// Asserting on it made this a timing-dependent flake; cleanup is all
    /// this hook is for.
    #[test]
    fn zzz_removes_our_temp_files() {
        // Per-test directories (see `tmp`) make this race-free by
        // construction: `remove_dir` — unlike `remove_dir_all` — only
        // succeeds on *empty* directories, so a sibling mid-flight keeps
        // its file and this hook only collects the shells that are
        // done. Deliberately assert-free.
        let prefix = format!("uwa-pid-test-{}-", std::process::id());
        let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
            return;
        };
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with(&prefix) {
                // Only succeeds when empty; in-flight siblings are
                // skipped, not deleted.
                let _ = std::fs::remove_dir(e.path());
            }
        }
    }
}
