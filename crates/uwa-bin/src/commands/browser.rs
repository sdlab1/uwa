//! Find a Chromium-family browser and (re)start it with a debug port.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::paths;

/// Where a CDP endpoint usually lives. `--remote-debugging-port=9222`.
pub const DEFAULT_CDP: &str = "http://127.0.0.1:9222";

/// Binary candidates, in preference order. Absolute paths last so a
/// user-installed browser earlier in `PATH` wins.
const CANDIDATES: &[&str] = &[
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
    "brave-browser",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
];

pub fn detect_chrome() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("UWA_CHROME_BIN") {
        let pb = PathBuf::from(&p);
        if pb.exists() {
            return Some(pb);
        }
    }
    for name in CANDIDATES {
        let p = Path::new(name);
        if p.is_absolute() && p.exists() {
            return Some(p.to_path_buf());
        }
        if let Ok(found) = which(name) {
            return Some(found);
        }
    }
    None
}

fn which(name: &str) -> Result<PathBuf> {
    // Small in-house `which` to avoid pulling a crate for one call.
    let path = std::env::var_os("PATH").ok_or_else(|| anyhow!("PATH unset"))?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if cand.is_file() {
            return Ok(cand);
        }
    }
    Err(anyhow!("not found: {name}"))
}

/// Probe `http://host:port/json/version` with a raw TCP + one-shot request.
/// Deliberately avoids reqwest so `uwa start` doesn't depend on TLS stacks.
pub fn cdp_ready(cdp_url: &str) -> bool {
    let rest = cdp_url
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let (host, port) = match rest.split_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.split('/')
                .next()
                .unwrap_or("9222")
                .parse()
                .unwrap_or(9222),
        ),
        None => (rest.to_string(), 9222u16),
    };
    let Ok(addr) = format!("{host}:{port}").parse() else {
        return false;
    };
    let Ok(mut sock) = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300))
    else {
        return false;
    };
    use std::io::{Read, Write};
    let _ = sock.set_read_timeout(Some(Duration::from_millis(300)));
    let _ = sock.write_all(
        format!("GET /json/version HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    );
    let mut buf = [0u8; 128];
    let _ = sock.read(&mut buf);
    buf.windows(11).any(|w| w == b"webSocketDe")
}

/// Ensure a CDP endpoint is up. Returns (endpoint, `Some(child)` if we
/// launched it and the caller is responsible for cleanup, `None` if an
/// existing endpoint was reused).
pub fn ensure_cdp(cdp_url: &str, launch_if_needed: bool) -> Result<(String, Option<Child>)> {
    if cdp_ready(cdp_url) {
        return Ok((cdp_url.to_string(), None));
    }
    if !launch_if_needed {
        return Err(anyhow!(
            "no CDP endpoint at {cdp_url}, and auto-launch is disabled"
        ));
    }
    let bin = detect_chrome().ok_or_else(|| {
        anyhow!("no Chrome/Chromium found. Install one, or set UWA_CHROME_BIN=/path/to/chrome")
    })?;

    let profile = paths::chromium_profile();
    std::fs::create_dir_all(&profile)?;
    let port = cdp_url
        .rsplit(':')
        .next()
        .and_then(|p| p.split('/').next())
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(9222);

    let child = Command::new(&bin)
        .arg(format!("--remote-debugging-port={port}"))
        .arg(format!("--user-data-dir={}", profile.display()))
        .args([
            "--no-first-run",
            "--no-default-browser-check",
            "--password-store=basic", // avoids gnome-keyring stalls
            "about:blank",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawn {}", bin.display()))?;

    // Wait for CDP with a deadline — 15 s is generous even on a cold SSD.
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if cdp_ready(cdp_url) {
            return Ok((cdp_url.to_string(), Some(child)));
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    // Dead on arrival: kill what we started so we don't leak it.
    let mut child = child;
    let _ = child.kill();
    let _ = child.wait();
    Err(anyhow!(
        "Chromium started but never exposed CDP at {cdp_url}"
    ))
}
