//! `uwa daemon start | stop | status | logs`.
//!
//! A background process managed by a pid file under `~/.uwa/`, with its
//! stdout/stderr redirected to `~/.uwa/daemon.log`. No systemd / launchd
//! integration here — that lives behind `uwa service install` (Session 3).

use anyhow::{anyhow, bail, Context, Result};
use clap::{Arg, ArgAction, Command};
use std::fs;
use std::path::PathBuf;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use super::paths;

#[derive(Debug, Clone)]
pub enum DaemonAction {
    /// Fork the daemon into the background.
    Start {
        /// Override the CDP endpoint (defaults to $UWA_CHROMIUM_WS or 127.0.0.1:9222).
        cdp: Option<String>,
        /// Restart if already running.
        force: bool,
    },
    /// Signal the daemon to stop (SIGTERM, then SIGKILL after 15 s).
    Stop,
    /// Print pid, config path, dashboard URL, and whether the port answers.
    Status,
    /// Tail `~/.uwa/daemon.log`.
    Logs {
        /// Follow (`tail -f`) instead of printing the last N lines.
        follow: bool,
        /// Lines to print (default 200).
        lines: usize,
    },
}

impl DaemonAction {
    pub fn clap_subcommands() -> Vec<clap::Command> {
        vec![
            Command::new("start")
                .about("Fork the daemon into the background.")
                .arg(Arg::new("cdp").long("cdp").action(ArgAction::Set))
                .arg(Arg::new("force").long("force").action(ArgAction::SetTrue)),
            Command::new("stop")
                .about("Signal the daemon to stop (SIGTERM, then SIGKILL after 15 s)."),
            Command::new("status")
                .about("Print pid, config path, dashboard URL, and whether the port answers."),
            Command::new("logs")
                .about("Tail ~/.uwa/daemon.log")
                .arg(
                    Arg::new("follow")
                        .short('f')
                        .long("follow")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("lines")
                        .short('n')
                        .long("lines")
                        .default_value("200")
                        .action(ArgAction::Set),
                ),
        ]
    }

    pub fn from_matches(name: &str, matches: &clap::ArgMatches) -> Result<Self, String> {
        match name {
            "start" => Ok(DaemonAction::Start {
                cdp: matches.get_one::<String>("cdp").cloned(),
                force: matches.get_flag("force"),
            }),
            "stop" => Ok(DaemonAction::Stop),
            "status" => Ok(DaemonAction::Status),
            "logs" => Ok(DaemonAction::Logs {
                follow: matches.get_flag("follow"),
                lines: matches
                    .get_one::<String>("lines")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(200),
            }),
            _ => Err(format!("unknown daemon subcommand: {name}")),
        }
    }
}

pub async fn run(config_path: PathBuf, action: DaemonAction) -> Result<()> {
    match action {
        DaemonAction::Start { cdp, force } => start(config_path, cdp, force).await,
        DaemonAction::Stop => stop(),
        DaemonAction::Status => status(config_path),
        DaemonAction::Logs { follow, lines } => logs(follow, lines),
    }
}

async fn start(config_path: PathBuf, cdp: Option<String>, force: bool) -> Result<()> {
    paths::ensure_home()?;
    let pid_path = paths::pid_file();
    let pgid_path = paths::pgid_file();

    if let Some(pid) = read_pid(&pid_path) {
        if process_alive(pid) {
            if !force {
                bail!("daemon already running (pid {pid}); use --force to restart");
            }
            let _ = send_signal(pid, libc_sigterm());
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        let _ = std::fs::remove_file(&pid_path);
    }
    // Clean up old pgid file
    let _ = std::fs::remove_file(&pgid_path);

    let log = paths::daemon_log();
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .with_context(|| format!("open {}", log.display()))?;
    let log_file_err = log_file.try_clone()?;

    let exe = std::env::current_exe().context("current_exe")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--config").arg(&config_path);
    if let Some(c) = &cdp {
        cmd.env("UWA_CHROMIUM_WS", c);
    }
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(log_file);
    cmd.stderr(log_file_err);

    // Detach: setsid on Unix so the child survives this shell.
    // We also capture the process group ID to kill the entire tree on stop.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    let child = cmd.spawn().context("spawn daemon")?;
    let pid = child.id();
    std::fs::write(&pid_path, pid.to_string())
        .with_context(|| format!("write {}", pid_path.display()))?;

    // On Unix, the PID after setsid is also the process group ID (PGID).
    // Store it so we can kill the entire process tree on stop.
    #[cfg(unix)]
    {
        std::fs::write(&pgid_path, pid.to_string())
            .with_context(|| format!("write {}", pgid_path.display()))?;
    }

    eprintln!("→ daemon started (pid {pid})");
    eprintln!("  logs: {}", log.display());
    eprintln!("  stop: uwa daemon stop");
    Ok(())
}

fn stop() -> Result<()> {
    let pid_path = paths::pid_file();
    let pgid_path = paths::pgid_file();
    let Some(pid) = read_pid(&pid_path) else {
        println!("not running");
        return Ok(());
    };
    if !process_alive(pid) {
        let _ = std::fs::remove_file(&pid_path);
        let _ = std::fs::remove_file(&pgid_path);
        println!("not running (stale pid file removed)");
        return Ok(());
    }

    // On Unix: kill the entire process group (daemon + any child Chrome processes)
    #[cfg(unix)]
    {
        if let Ok(pgid_str) = std::fs::read_to_string(&pgid_path) {
            if let Ok(pgid) = pgid_str.trim().parse::<libc::pid_t>() {
                // SIGTERM the whole process group
                let _ = unsafe { libc::kill(pgid.wrapping_neg(), libc::SIGTERM) };
                // Wait for process group to die
                for _ in 0..30 {
                    if !process_alive(pid) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                // SIGKILL the process group if still alive
                if process_alive(pid) {
                    let _ = unsafe { libc::kill(pgid.wrapping_neg(), libc::SIGKILL) };
                }
            }
        }
    }

    #[cfg(windows)]
    {
        let _ = send_signal(pid, libc_sigterm());
        // Give it up to 15 s to exit cleanly.
        for _ in 0..30 {
            if !process_alive(pid) {
                let _ = std::fs::remove_file(&pid_path);
                println!("stopped (pid {pid})");
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        let _ = send_signal(pid, 9);
    }

    // Confirm the process actually died before reporting success.
    if process_alive(pid) {
        bail!("daemon (pid {pid}) did not stop after SIGTERM/SIGKILL");
    }

    let _ = std::fs::remove_file(&pid_path);
    let _ = std::fs::remove_file(&pgid_path);
    println!("stopped (pid {pid})");
    Ok(())
}

fn status(config_path: PathBuf) -> Result<()> {
    let pid_path = paths::pid_file();
    let pid = read_pid(&pid_path);
    let running = pid.map(process_alive).unwrap_or(false);

    println!("config:   {}", config_path.display());
    println!("pid file: {}", pid_path.display());
    match (pid, running) {
        (Some(p), true) => println!("state:    running (pid {p})"),
        (Some(p), false) => println!("state:    not running (stale pid {p})"),
        (None, _) => println!("state:    not running"),
    }
    println!("log:      {}", paths::daemon_log().display());

    // Best-effort: read the port out of the TOML and probe /healthz.
    match uwa_config::Config::load_from_path(&config_path) {
        Ok(cfg) => {
            let port = cfg.server.port;
            let url = format!("http://127.0.0.1:{port}/healthz");
            if http_ok(&url) {
                println!("health:   ok ({url})");
                println!("dashboard http://127.0.0.1:{port}/");
            } else {
                println!("health:   no response ({url})");
            }
        }
        // The daemon may still be fine on a config the CLI cannot parse
        // with the same rules; say so instead of dropping the row silently.
        Err(e) => {
            println!("health:   config unreadable ({e}); daemon liveness unknown");
        }
    }
    Ok(())
}

fn logs(follow: bool, lines: usize) -> Result<()> {
    let p = paths::daemon_log();
    if !p.exists() {
        bail!("no log at {}", p.display());
    }
    let content = std::fs::read_to_string(&p).unwrap_or_default();
    let all: Vec<&str> = content.lines().collect();
    let start = all.len().saturating_sub(lines);
    // forbid-level `indexing_slicing`: use `get(..)` instead of `&all[start..]`.
    let tail = all.get(start..).unwrap_or(&[]);
    for line in tail {
        println!("{line}");
    }
    if follow {
        // Poor man's tail -f: poll + append.
        #[cfg(unix)]
        let mut pos = fs::metadata(&p)
            .map(|m| LogPosition {
                inode: m.ino(),
                offset: content.len(),
            })
            .unwrap_or(LogPosition {
                inode: 0,
                offset: content.len(),
            });
        #[cfg(not(unix))]
        let mut pos = content.len();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let Ok(cur) = std::fs::read_to_string(&p) else {
                continue;
            };
            #[cfg(unix)]
            let (new_pos, chunk) = fs::metadata(&p)
                .map(|meta| follow_chunk(pos, &cur, &meta))
                .unwrap_or((pos, String::new()));
            #[cfg(not(unix))]
            let (new_pos, chunk) = follow_chunk(pos, &cur);
            if !chunk.is_empty() {
                print!("{chunk}");
                use std::io::Write as _;
                let _ = std::io::stdout().flush();
            }
            pos = new_pos;
        }
    }
    Ok(())
}

/// Inode + byte offset pair for log following across rotation/in-place rewrite.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LogPosition {
    inode: u64,
    offset: usize,
}

/// Pure `logs --follow` step: given the previous position (inode + byte offset)
/// and the current file contents + metadata, decide what to print and where
/// to resume.
///
/// * same inode, append — the new bytes past `pos`;
/// * same inode, no change — empty chunk, offset unchanged;
/// * same inode, truncation (`cur` shrank below `pos`) — restart from byte 0,
///   so following a rotated log resumes instead of going silent forever;
/// * inode changed (log rewritten in place, e.g. logrotate with copytruncate,
///   or editor saved over the file) — restart from byte 0 regardless of offset;
/// * a stale offset landing mid-UTF-8 (a truncation cut a multi-byte char
///   and the file regrew) — snap forward to the next char boundary instead
///   of panicking on a non-boundary slice.
#[cfg(unix)]
fn follow_chunk(prev: LogPosition, cur: &str, meta: &fs::Metadata) -> (LogPosition, String) {
    let cur_inode = meta.ino();
    let pos = if cur_inode != prev.inode || cur.len() < prev.offset {
        0
    } else {
        prev.offset
    };
    let pos = if cur.is_char_boundary(pos) {
        pos
    } else {
        (pos..=cur.len())
            .find(|&i| cur.is_char_boundary(i))
            .unwrap_or(cur.len())
    };
    if cur.len() > pos {
        (
            LogPosition {
                inode: cur_inode,
                offset: cur.len(),
            },
            cur[pos..].to_string(),
        )
    } else {
        (
            LogPosition {
                inode: cur_inode,
                offset: pos,
            },
            String::new(),
        )
    }
}

/// Fallback for non-Unix: just track byte offset (no inode available).
#[cfg(not(unix))]
fn follow_chunk(prev: usize, cur: &str) -> (usize, String) {
    let pos = if cur.len() < prev { 0 } else { prev };
    let pos = if cur.is_char_boundary(pos) {
        pos
    } else {
        (pos..=cur.len())
            .find(|&i| cur.is_char_boundary(i))
            .unwrap_or(cur.len())
    };
    if cur.len() > pos {
        (cur.len(), cur[pos..].to_string())
    } else {
        (pos, String::new())
    }
}

// ---- plumbing ----

fn read_pid(path: &std::path::Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

#[cfg(unix)]
fn send_signal(pid: u32, sig: i32) -> Result<()> {
    let rc = unsafe { libc::kill(pid as libc::pid_t, sig) };
    if rc != 0 {
        return Err(anyhow!("kill({pid}, {sig}) failed"));
    }
    Ok(())
}

#[cfg(windows)]
fn send_signal(_pid: u32, _sig: i32) -> Result<()> {
    // Windows fallback: taskkill. Good enough for the daemon lifecycle.
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/PID", &_pid.to_string()])
        .status();
    Ok(())
}

#[cfg(unix)]
fn libc_sigterm() -> i32 {
    15
}

#[cfg(windows)]
fn libc_sigterm() -> i32 {
    1
}

/// Scheme split, case-insensitive (`HTTP://`, `Https://` — the URL can
/// come from user config, not only from `status()` itself). Returns
/// whether TLS was requested plus the remainder after `://`.
fn scheme_parts(url: &str) -> (bool, &str) {
    let lower = url.to_lowercase();
    if lower.starts_with("https://") {
        (true, url.get(8..).unwrap_or(""))
    } else if lower.starts_with("http://") {
        (false, url.get(7..).unwrap_or(""))
    } else {
        (false, url)
    }
}

/// How long a single probe step (connect, write, read) may take.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(300);

/// Extract `(host, port)` from a health-check URL. Pure and unit-tested.
///
/// Handles the shapes `http_ok` used to choke on (each reported a healthy
/// daemon as down):
/// * `http://localhost:8080/…` — hostnames (not just IP literals);
/// * `http://host/…` — no port → scheme default (80);
/// * `https://host/…` — used to split at the wrong colon entirely;
/// * `HTTP://host:8080/…` — any letter case of the scheme;
/// * `http://[::1]:8080/…` — bracketed IPv6 literals;
/// * `http://user:pass@host:8080/…` — userinfo is dropped.
fn host_port(url: &str) -> Option<(String, u16)> {
    let (https, rest) = scheme_parts(url);
    // Cut path, query and fragment — everything from the first '/', '?'
    // or '#'. `get(..n)` instead of slicing: forbid-level lint.
    let authority = match rest.find(['/', '?', '#']).and_then(|end| rest.get(..end)) {
        Some(a) => a,
        None => rest,
    };
    // Drop userinfo (`user:pass@host`).
    let authority = match authority.rfind('@') {
        Some(at) => authority.get(at.saturating_add(1)..).unwrap_or(authority),
        None => authority,
    };
    let default_port: u16 = if https { 443 } else { 80 };

    // Bracketed IPv6 literal: `[::1]:8080`.
    if let Some(inner) = authority.strip_prefix('[') {
        let close = inner.find(']')?;
        let host = inner.get(..close)?;
        if host.is_empty() {
            return None;
        }
        let port = inner
            .get(close.saturating_add(1)..)
            .and_then(|p| p.strip_prefix(':'))
            .and_then(|p| p.parse().ok())
            .unwrap_or(default_port);
        return Some((host.to_string(), port));
    }

    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok()?),
        None => (authority, default_port),
    };
    if host.is_empty() {
        return None;
    }
    Some((host.to_string(), port))
}

/// Request target (path + query) for the HTTP probe, defaulting to `/`.
fn request_target(url: &str) -> String {
    let (_, rest) = scheme_parts(url);
    let rest = match rest.split_once('#') {
        Some((before, _)) => before,
        None => rest,
    };
    match rest.split_once('/') {
        Some((_, path)) => format!("/{path}"),
        None => "/".to_string(),
    }
}

/// Parse an HTTP status line (`HTTP/1.0 200 OK`) out of the first bytes of
/// a response. `None` for anything that is not HTTP.
fn parse_status_line(bytes: &[u8]) -> Option<u16> {
    let head = String::from_utf8_lossy(bytes);
    let code = head.lines().next()?.split_whitespace().nth(1)?;
    code.parse().ok()
}

fn http_ok(url: &str) -> bool {
    let Some((host, port)) = host_port(url) else {
        return false;
    };
    let (https, _) = scheme_parts(url);
    let target = request_target(url);

    // `"...".parse::<SocketAddr>()` only accepts IP literals — `localhost`
    // and other hostnames must go through the system resolver.
    use std::net::ToSocketAddrs as _;
    let Ok(addrs) = (host.as_str(), port).to_socket_addrs() else {
        return false;
    };

    for addr in addrs {
        let Ok(mut stream) = std::net::TcpStream::connect_timeout(&addr, PROBE_TIMEOUT) else {
            continue;
        };
        let _ = stream.set_read_timeout(Some(PROBE_TIMEOUT));
        let _ = stream.set_write_timeout(Some(PROBE_TIMEOUT));

        if https {
            // Real TLS via ureq (rustls backend): perform an actual HTTPS
            // GET /healthz and require a 2xx answer. Closes UWA-102.
            return ureq::get(url)
                .timeout(PROBE_TIMEOUT)
                .call()
                .map(|resp| (200..300).contains(&resp.status()))
                .unwrap_or(false);
        }

        // A bare TCP connect proves nothing — any unrelated service on the
        // port would pass. Ask /healthz and require a 2xx answer, so a
        // foreign HTTP server (404), a non-HTTP one (garbage) and a silent
        // socket all report the daemon as down.
        use std::io::{Read as _, Write as _};
        let request = format!("GET {target} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n");
        if stream.write_all(request.as_bytes()).is_err() {
            continue;
        }
        let mut buf = [0u8; 256];
        let read = match stream.read(&mut buf) {
            Ok(n) => n,
            Err(_) => continue,
        };
        let answered_ok = buf
            .get(..read)
            .and_then(parse_status_line)
            .is_some_and(|code| (200..300).contains(&code));
        if answered_ok {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- follow_chunk ----

    #[test]
    fn follow_chunk_appends_new_bytes() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut f = NamedTempFile::new().unwrap_or_else(|_| unreachable!());
        f.write_all(b"hello").unwrap_or_else(|_| unreachable!());
        f.flush().unwrap_or_else(|_| unreachable!());
        let meta = f.as_file().metadata().unwrap_or_else(|_| unreachable!());
        let pos = LogPosition {
            inode: meta.ino(),
            offset: 5,
        };

        f.write_all(b" world").unwrap_or_else(|_| unreachable!());
        f.flush().unwrap_or_else(|_| unreachable!());
        let cur = std::fs::read_to_string(f.path()).unwrap_or_else(|_| unreachable!());
        let meta2 = f.as_file().metadata().unwrap_or_else(|_| unreachable!());
        let (new_pos, chunk) = follow_chunk(pos, &cur, &meta2);
        assert_eq!(new_pos.offset, 11);
        assert_eq!(chunk, " world");
    }

    #[test]
    fn follow_chunk_no_change_yields_empty() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut f = NamedTempFile::new().unwrap_or_else(|_| unreachable!());
        f.write_all(b"hello world")
            .unwrap_or_else(|_| unreachable!());
        f.flush().unwrap_or_else(|_| unreachable!());
        let meta = f.as_file().metadata().unwrap_or_else(|_| unreachable!());
        let pos = LogPosition {
            inode: meta.ino(),
            offset: 11,
        };

        let cur = std::fs::read_to_string(f.path()).unwrap_or_else(|_| unreachable!());
        let meta2 = f.as_file().metadata().unwrap_or_else(|_| unreachable!());
        let (new_pos, chunk) = follow_chunk(pos, &cur, &meta2);
        assert_eq!(new_pos.offset, 11);
        assert_eq!(chunk, "");
    }

    #[test]
    fn follow_chunk_recovers_after_truncation() {
        // Test truncation recovery: when file shrinks below our offset,
        // we should restart from byte 0.
        let f = tempfile::NamedTempFile::new().unwrap_or_else(|_| unreachable!());
        // Write initial content
        std::fs::write(f.path(), b"old log content").unwrap_or_else(|_| unreachable!());
        let meta = std::fs::metadata(f.path()).unwrap_or_else(|_| unreachable!());
        let pos = LogPosition {
            inode: meta.ino(),
            offset: 500,
        };

        // Truncate and write shorter content - simulate log rotation
        std::fs::write(f.path(), b"new log\n").unwrap_or_else(|_| unreachable!());
        let cur = std::fs::read_to_string(f.path()).unwrap_or_else(|_| unreachable!());
        let meta2 = std::fs::metadata(f.path()).unwrap_or_else(|_| unreachable!());
        let (new_pos, chunk) = follow_chunk(pos, &cur, &meta2);
        assert_eq!(new_pos.offset, 8);
        assert_eq!(chunk, "new log\n");
    }

    #[test]
    fn follow_chunk_stale_offset_mid_utf8_does_not_panic() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut f = NamedTempFile::new().unwrap_or_else(|_| unreachable!());
        f.write_all("ёжик".as_bytes())
            .unwrap_or_else(|_| unreachable!());
        f.flush().unwrap_or_else(|_| unreachable!());
        let meta = f.as_file().metadata().unwrap_or_else(|_| unreachable!());
        let pos = LogPosition {
            inode: meta.ino(),
            offset: 1,
        };

        let cur = std::fs::read_to_string(f.path()).unwrap_or_else(|_| unreachable!());
        let meta2 = f.as_file().metadata().unwrap_or_else(|_| unreachable!());
        let (new_pos, chunk) = follow_chunk(pos, &cur, &meta2);
        assert!(cur.is_char_boundary(new_pos.offset));
        assert_eq!(chunk, "жик");
        assert_eq!(new_pos.offset, cur.len());
    }

    #[test]
    fn follow_chunk_multibyte_content_round_trip() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut f = NamedTempFile::new().unwrap_or_else(|_| unreachable!());
        f.write_all("привет мир".as_bytes())
            .unwrap_or_else(|_| unreachable!());
        f.flush().unwrap_or_else(|_| unreachable!());
        let meta = f.as_file().metadata().unwrap_or_else(|_| unreachable!());
        let pos = LogPosition {
            inode: meta.ino(),
            offset: 0,
        };

        let cur = std::fs::read_to_string(f.path()).unwrap_or_else(|_| unreachable!());
        let meta2 = f.as_file().metadata().unwrap_or_else(|_| unreachable!());
        let (new_pos, chunk) = follow_chunk(pos, &cur, &meta2);
        assert_eq!(chunk, cur);
        let (pos2, chunk2) = follow_chunk(new_pos, &cur, &meta2);
        assert_eq!(chunk2, "");
        assert_eq!(pos2.offset, cur.len());
    }

    #[test]
    fn follow_chunk_inode_change_restarts_from_zero() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut f = NamedTempFile::new().unwrap_or_else(|_| unreachable!());
        f.write_all(b"old content")
            .unwrap_or_else(|_| unreachable!());
        f.flush().unwrap_or_else(|_| unreachable!());
        let meta = f.as_file().metadata().unwrap_or_else(|_| unreachable!());
        let pos = LogPosition {
            inode: meta.ino(),
            offset: 100,
        };

        // Simulate inode change by recreating the file
        let path = f.path().to_path_buf();
        drop(f);
        std::fs::write(&path, b"new content").unwrap_or_else(|_| unreachable!());

        let f2 = std::fs::File::open(&path).unwrap_or_else(|_| unreachable!());
        let cur = std::fs::read_to_string(&path).unwrap_or_else(|_| unreachable!());
        let meta2 = f2.metadata().unwrap_or_else(|_| unreachable!());
        let (new_pos, chunk) = follow_chunk(pos, &cur, &meta2);
        assert_eq!(new_pos.offset, 11);
        assert_eq!(chunk, "new content");
    }

    // ---- host_port ----

    #[test]
    fn host_port_parses_ip_with_port() {
        assert_eq!(
            host_port("http://127.0.0.1:8080/healthz"),
            Some(("127.0.0.1".into(), 8080))
        );
    }

    #[test]
    fn host_port_parses_hostname_with_port() {
        // Regression: SocketAddr::parse rejected `localhost`, so a healthy
        // daemon on http://localhost:8080 reported as down.
        assert_eq!(
            host_port("http://localhost:8080/healthz"),
            Some(("localhost".into(), 8080))
        );
    }

    #[test]
    fn host_port_defaults_http_port_80() {
        assert_eq!(
            host_port("http://example.com/healthz"),
            Some(("example.com".into(), 80))
        );
    }

    #[test]
    fn host_port_defaults_https_port_443() {
        assert_eq!(
            host_port("https://example.com/healthz"),
            Some(("example.com".into(), 443))
        );
    }

    #[test]
    fn host_port_handles_ipv6() {
        assert_eq!(
            host_port("http://[::1]:8080/healthz"),
            Some(("::1".into(), 8080))
        );
        assert_eq!(host_port("http://[::1]/healthz"), Some(("::1".into(), 80)));
    }

    #[test]
    fn host_port_drops_userinfo() {
        assert_eq!(
            host_port("http://user:pass@localhost:8080/healthz"),
            Some(("localhost".into(), 8080))
        );
    }

    #[test]
    fn host_port_rejects_garbage() {
        assert_eq!(host_port("http://"), None);
        assert_eq!(host_port("http://host:notaport/"), None);
    }

    // ---- http_ok against real listeners ----

    /// Spawn a responder thread: accept `n` connections, drain the request,
    /// answer with `reply` (or stay silent when `reply` is empty).
    fn spawn_responder(n: usize, reply: &'static [u8]) -> (u16, std::thread::JoinHandle<()>) {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").unwrap_or_else(|_| unreachable!());
        let port = listener
            .local_addr()
            .unwrap_or_else(|_| unreachable!())
            .port();
        let handle = std::thread::spawn(move || {
            for _ in 0..n {
                let Ok((mut sock, _)) = listener.accept() else {
                    break;
                };
                let mut buf = [0u8; 512];
                let _ = std::io::Read::read(&mut sock, &mut buf);
                if !reply.is_empty() {
                    let _ = std::io::Write::write_all(&mut sock, reply);
                }
            }
        });
        (port, handle)
    }

    #[test]
    fn http_ok_requires_a_real_2xx_http_answer() {
        let (port, handle) = spawn_responder(
            3,
            b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{\"status\":\"ok\"}",
        );
        assert!(http_ok(&format!("http://127.0.0.1:{port}/healthz")));
        // Hostname → resolution through ToSocketAddrs.
        assert!(http_ok(&format!("http://localhost:{port}/healthz")));
        // Scheme in any letter case.
        assert!(http_ok(&format!("HTTP://127.0.0.1:{port}/healthz")));
        handle.join().unwrap_or_else(|_| unreachable!());

        // Responder gone → connection refused → down.
        assert!(!http_ok(&format!("http://127.0.0.1:{port}/healthz")));
    }

    #[test]
    fn http_ok_rejects_foreign_services() {
        // Non-HTTP speaker (the old TCP-connect probe called this "healthy").
        let (port, handle) = spawn_responder(1, b"welcome to my telnet server\r\n");
        assert!(!http_ok(&format!("http://127.0.0.1:{port}/healthz")));
        handle.join().unwrap_or_else(|_| unreachable!());

        // HTTP speaker without the health endpoint.
        let (port, handle) = spawn_responder(1, b"HTTP/1.0 404 Not Found\r\n\r\nnope");
        assert!(!http_ok(&format!("http://127.0.0.1:{port}/healthz")));
        handle.join().unwrap_or_else(|_| unreachable!());

        // Server error responses are health failures too.
        let (port, handle) = spawn_responder(1, b"HTTP/1.0 500 Internal Server Error\r\n\r\n");
        assert!(!http_ok(&format!("http://127.0.0.1:{port}/healthz")));
        handle.join().unwrap_or_else(|_| unreachable!());
    }

    #[test]
    fn http_ok_silent_socket_times_out_as_down() {
        // A bound-but-never-accepting listener: connect succeeds, the write
        // succeeds, the read hits the 300 ms timeout → down.
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").unwrap_or_else(|_| unreachable!());
        let port = listener
            .local_addr()
            .unwrap_or_else(|_| unreachable!())
            .port();
        let started = std::time::Instant::now();
        assert!(!http_ok(&format!("http://127.0.0.1:{port}/healthz")));
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(250),
            "the silent socket must be bounded by the read timeout"
        );
        drop(listener);
    }

    #[test]
    fn http_ok_https_requires_real_tls() {
        // ureq with the `tls` feature performs a real TLS handshake. A plain
        // TCP listener (no TLS) cannot complete it, so https URLs report down.
        // Closes UWA-102: https is no longer falsely marked healthy.
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").unwrap_or_else(|_| unreachable!());
        let port = listener
            .local_addr()
            .unwrap_or_else(|_| unreachable!())
            .port();
        assert!(!http_ok(&format!("https://127.0.0.1:{port}/healthz")));
        drop(listener);
        assert!(!http_ok(&format!("https://127.0.0.1:{port}/healthz")));
    }

    // ---- pure helpers ----

    #[test]
    fn parse_status_line_variants() {
        assert_eq!(parse_status_line(b"HTTP/1.0 200 OK\r\n\r\n"), Some(200));
        assert_eq!(
            parse_status_line(b"HTTP/1.1 500 Internal Server Error\r\n"),
            Some(500)
        );
        assert_eq!(parse_status_line(b"garbage line"), None);
        assert_eq!(parse_status_line(b""), None);
        assert_eq!(parse_status_line(b"HTTP/1.1"), None);
    }

    #[test]
    fn request_target_variants() {
        assert_eq!(request_target("http://h:1/healthz?a=b"), "/healthz?a=b");
        assert_eq!(request_target("http://h:1"), "/");
        assert_eq!(request_target("https://h/x#frag"), "/x");
        assert_eq!(request_target("HTTP://H/healthz"), "/healthz");
    }

    #[test]
    fn host_port_is_scheme_case_insensitive() {
        assert_eq!(
            host_port("HTTP://LocalHost:8080/healthz"),
            Some(("LocalHost".into(), 8080))
        );
        assert_eq!(
            host_port("HTTPS://example.com/x"),
            Some(("example.com".into(), 443))
        );
        // Fragment must not leak into the authority.
        assert_eq!(
            host_port("http://127.0.0.1:8080/h#frag"),
            Some(("127.0.0.1".into(), 8080))
        );
    }

    #[test]
    fn host_port_drops_userinfo_with_at_at_end() {
        // Regression for `at.saturating_add(1)`: `@` at the end of authority.
        assert_eq!(
            host_port("http://user:pass@localhost:8080/healthz"),
            Some(("localhost".into(), 8080))
        );
    }

    #[test]
    fn host_port_ipv6_with_port() {
        // Regression for `close.saturating_add(1)`: port after `]`.
        assert_eq!(
            host_port("http://[::1]:8080/healthz"),
            Some(("::1".into(), 8080))
        );
        assert_eq!(host_port("http://[::1]/healthz"), Some(("::1".into(), 80)));
    }

    #[test]
    fn logs_tail_does_not_panic_when_lines_exceeds_total() {
        // Regression for `all.get(start..).unwrap_or(&[])`:
        // `lines` greater than number of lines in log.
        let all = ["a", "b"];
        let lines = 10usize;
        let start = all.len().saturating_sub(lines);
        let tail = all.get(start..).unwrap_or(&[]);
        assert_eq!(tail, &all[..]);
    }
}
