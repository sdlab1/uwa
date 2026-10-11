//! `uwa daemon start | stop | status | logs`.
//!
//! A background process managed by a pid file under `~/.uwa/`, with its
//! stdout/stderr redirected to `~/.uwa/daemon.log`. No systemd / launchd
//! integration here — that lives behind `uwa service install` (Session 3).

use anyhow::{anyhow, bail, Context, Result};
use clap::Subcommand;
use std::path::PathBuf;

use super::paths;

#[derive(Subcommand)]
pub enum DaemonAction {
    /// Fork the daemon into the background.
    Start {
        /// Override the CDP endpoint (defaults to $UWA_CHROMIUM_WS or 127.0.0.1:9222).
        #[arg(long)]
        cdp: Option<String>,
        /// Restart if already running.
        #[arg(long)]
        force: bool,
    },
    /// Signal the daemon to stop (SIGTERM, then SIGKILL after 15 s).
    Stop,
    /// Print pid, config path, dashboard URL, and whether the port answers.
    Status,
    /// Tail `~/.uwa/daemon.log`.
    Logs {
        /// Follow (`tail -f`) instead of printing the last N lines.
        #[arg(short, long)]
        follow: bool,
        /// Lines to print (default 200).
        #[arg(short = 'n', long, default_value_t = 200)]
        lines: usize,
    },
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
                let _ = unsafe { libc::kill(-pgid, libc::SIGTERM) };
                // Wait for process group to die
                for _ in 0..30 {
                    if !process_alive(pid) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                // SIGKILL the process group if still alive
                if process_alive(pid) {
                    let _ = unsafe { libc::kill(-pgid, libc::SIGKILL) };
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
    if let Ok(cfg) = uwa_config::Config::load_from_path(&config_path) {
        let port = cfg.server.port;
        let url = format!("http://127.0.0.1:{port}/healthz");
        if http_ok(&url) {
            println!("health:   ok ({url})");
            println!("dashboard http://127.0.0.1:{port}/");
        } else {
            println!("health:   no response ({url})");
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
    for line in &all[start..] {
        println!("{line}");
    }
    if follow {
        // Poor man's tail -f: poll + append.
        let mut pos = content.len();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let Ok(cur) = std::fs::read_to_string(&p) else {
                continue;
            };
            if cur.len() > pos {
                print!("{}", &cur[pos..]);
                pos = cur.len();
            }
        }
    }
    Ok(())
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

fn http_ok(url: &str) -> bool {
    let rest = url.trim_start_matches("http://");
    let (host, port_path) = match rest.split_once(':') {
        Some((h, p)) => (h.to_string(), p.to_string()),
        None => return false,
    };
    let port = port_path
        .split('/')
        .next()
        .unwrap_or("80")
        .parse()
        .unwrap_or(80);
    let Ok(addr) = format!("{host}:{port}").parse() else {
        return false;
    };
    std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(300)).is_ok()
}
