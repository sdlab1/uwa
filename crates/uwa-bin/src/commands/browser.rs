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
    let _ = sock.set_write_timeout(Some(Duration::from_millis(300)));
    if sock
        .write_all(
            format!("GET /json/version HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .is_err()
    {
        return false;
    }
    // Read until EOF/timeout (up to 4 KB): real Chromium's /json/version
    // puts `webSocketDebuggerUrl` well past the first TCP segment — a
    // single 128-byte read misses it.
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 512];
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(400))
        .unwrap_or_else(|| unreachable!());
    loop {
        if Instant::now() > deadline {
            break;
        }
        match sock.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(chunk.get(..n).unwrap_or(&[]));
                if buf.len() > 4096 {
                    break;
                }
            }
            Err(_) => break, // timeout / closed
        }
    }
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
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(15))
        .unwrap_or_else(|| unreachable!());
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Serve one HTTP response on an ephemeral port, then return the port.
    /// The response mirrors Chromium's /json/version closely enough for
    /// [`cdp_ready`]: headers first, `webSocketDebuggerUrl` in the body —
    /// *after* the first 128 bytes, which is exactly where the old
    /// single-read implementation missed it.
    fn serve_cdp_once(body: &str) -> u16 {
        use std::io::Write as _;
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").unwrap_or_else(|_| unreachable!());
        let port = listener
            .local_addr()
            .unwrap_or_else(|_| unreachable!())
            .port();
        let body = body.to_string();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut req = [0u8; 256];
                let _ = std::io::Read::read(&mut sock, &mut req);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     X-Pad: {}{}\r\nConnection: close\r\n\r\n{body}",
                    "a".repeat(200),
                    ""
                );
                let _ = sock.write_all(resp.as_bytes());
            }
        });
        port
    }

    #[test]
    fn cdp_ready_finds_marker_past_the_first_segment() {
        let port = serve_cdp_once(
            "{\"Browser\":\"Chrome/128\",\"webSocketDebuggerUrl\":\"ws://127.0.0.1/devtools/browser/x\"}",
        );
        assert!(
            cdp_ready(&format!("http://127.0.0.1:{port}")),
            "marker must be found"
        );
    }

    #[test]
    fn cdp_ready_rejects_non_cdp_http() {
        let port = serve_cdp_once("{\"status\":\"ok\"}");
        assert!(!cdp_ready(&format!("http://127.0.0.1:{port}")));
    }

    #[test]
    fn cdp_ready_rejects_dead_ports() {
        // Port 1 on loopback: nothing listens there in a test container.
        assert!(!cdp_ready("http://127.0.0.1:1"));
    }
}
