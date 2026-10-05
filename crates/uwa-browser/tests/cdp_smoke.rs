//! Live CDP smoke test.
//!
//! Ignored by default: it needs a Chromium binary (or a running debugger on
//! `UWA_CDP_URL`). Run with:
//!
//! ```sh
//! UWA_CHROMIUM=1 cargo test -p uwa-browser -- --ignored --nocapture
//! ```
//!
//! The Chromium it spawns is killed by a `Drop` guard, so a failing or
//! timing-out run leaves no browser behind.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use uwa_browser::CdpTransport;
use uwa_core::{NetworkEvent, Transport};

const CHROME_CANDIDATES: &[&str] = &[
    "/usr/bin/google-chrome",
    "/usr/bin/google-chrome-stable",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
];

/// Upper bound for the whole scenario, so a hang cannot leak a browser.
const SCENARIO_BUDGET: Duration = Duration::from_secs(90);

fn enabled() -> bool {
    matches!(std::env::var("UWA_CHROMIUM").as_deref(), Ok("1"))
}

/// Minimal HTTP/1.1 server: `/index.html` fetches `/data.json`.
async fn serve(port: u16) {
    let listener = TcpListener::bind(("127.0.0.1", port)).await.expect("bind site");
    eprintln!("[site] listening on 127.0.0.1:{port}");
    loop {
        let (mut sock, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[site] accept error: {e}");
                return;
            }
        };
        eprintln!("[site] accepted {peer}");
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let n = match sock.read(&mut chunk).await {
                    Ok(0) => {
                        eprintln!("[site] {peer} closed after {} bytes", buf.len());
                        return;
                    }
                    Ok(n) => n,
                    Err(e) => {
                        eprintln!("[site] {peer} read error: {e} ({} bytes)", buf.len());
                        return;
                    }
                };
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let head = String::from_utf8_lossy(&buf);
            let path = head.split_whitespace().nth(1).unwrap_or("/");
            eprintln!("[site] request {path}");
            let (ctype, body) = if path.starts_with("/data.json") {
                ("application/json", r#"{"ok":true,"payload":"smoke-body"}"#)
            } else {
                (
                    "text/html",
                    "<!doctype html><html><body><div id=\"app\">loading</div>\
                     <script>fetch('/data.json').then(r=>r.text())\
                     .then(t=>{document.getElementById('app').textContent='OK:'+t})</script>\
                     </body></html>",
                )
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.write_all(body.as_bytes()).await;
            let _ = sock.flush().await;
            let _ = sock.shutdown().await;
        });
    }
}

/// Prove the fixture server answers a plain client before blaming Chrome.
async fn self_check(port: u16) {
    let mut sock = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("self check: connect");
    sock.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await
        .expect("self check: write");
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = sock.read(&mut chunk).await.expect("self check: read");
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let s = String::from_utf8_lossy(&buf);
    assert!(s.contains("200 OK"), "self check failed, got: {s:?}");
    eprintln!("[site] self check ok");
}

/// Kills any child process on `Drop`, so a panicking test cannot leak it.
struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let Some(mut child) = self.0.take() else {
            return;
        };
        let _ = child.start_kill();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = child.wait().await;
            });
        }
    }
}

/// Owns the headless Chromium; kills it (and removes its profile) on `Drop`,
/// no matter how the test exits.
struct ChromeGuard {
    child: Option<Child>,
    profile: PathBuf,
}

impl ChromeGuard {
    fn spawn(port: u16) -> Option<Self> {
        let bin = if let Ok(b) = std::env::var("UWA_CHROME_BIN") {
            b
        } else {
            CHROME_CANDIDATES
                .iter()
                .find(|p| std::path::Path::new(p).exists())
                .map(|p| p.to_string())?
        };
        let profile = std::env::temp_dir().join(format!("uwa-cdp-smoke-{port}"));
        let child = Command::new(&bin)
            .arg(format!("--remote-debugging-port={port}"))
            .arg("--headless=new")
            .arg("--no-sandbox")
            .arg("--disable-gpu")
            .arg("--disable-dev-shm-usage")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--password-store=basic")
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg("about:blank")
            .process_group(0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        Some(Self {
            child: Some(child),
            profile,
        })
    }

    fn debug_url(&self, port: u16) -> String {
        format!("http://127.0.0.1:{port}")
    }
}

impl Drop for ChromeGuard {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        if let Some(pid) = child.id() {
            unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
        }
        let _ = child.start_kill();
        for _ in 0..150 {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(_) => break,
            }
        }
        let profile = std::mem::take(&mut self.profile);
        for _ in 0..5 {
            if std::fs::remove_dir_all(&profile).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// One bounded attempt at `GET /json/version`.
async fn cdp_http_ready(url: &str) -> bool {
    let rest = url
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let (host, port) = match rest.split_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.split('/').next().unwrap_or("9222").parse().unwrap_or(9222),
        ),
        None => (rest.to_string(), 9222),
    };
    let attempt = async move {
        let mut sock = TcpStream::connect((host.as_str(), port)).await.ok()?;
        let req = format!(
            "GET /json/version HTTP/1.1\r\nHost: {host}:{port}\r\n\
             Connection: close\r\n\r\n"
        );
        sock.write_all(req.as_bytes()).await.ok()?;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = sock.read(&mut chunk).await.ok()?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.windows(20).any(|w| w == b"webSocketDebuggerUrl") {
                return Some(());
            }
            if buf.len() > 65_536 {
                break;
            }
        }
        None
    };
    tokio::time::timeout(Duration::from_secs(2), attempt)
        .await
        .is_ok_and(|r| r.is_some())
}

async fn wait_for_ws(url: &str, deadline: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if cdp_http_ready(url).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

#[tokio::test]
#[ignore = "needs a Chromium binary; run with UWA_CHROMIUM=1 cargo test -p uwa-browser -- --ignored"]
async fn live_cdp_navigation_and_network_events() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off")),
        )
        .with_writer(std::io::stderr)
        .try_init();
    if !enabled() {
        eprintln!("skipped: set UWA_CHROMIUM=1 to run the live CDP smoke test");
        return;
    }

    let site_port = 38211u16;
    let chrome_port = 38210u16;
    let mut python_site = None;
    if std::env::var("UWA_SITE").as_deref() == Ok("python") {
        let child = Command::new("python3")
            .args(["-m", "http.server", &site_port.to_string()])
            .current_dir("/tmp/uwa-site")
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("python site");
        eprintln!("[site] python http.server on {site_port}");
        python_site = Some(ChildGuard::new(child));
        tokio::time::sleep(Duration::from_millis(700)).await;
    } else {
        tokio::spawn(serve(site_port));
        tokio::time::sleep(Duration::from_millis(100)).await;
        self_check(site_port).await;
    }

    let (debug_url, chrome) = if let Ok(u) = std::env::var("UWA_CDP_URL") {
        (u, None)
    } else {
        let guard =
            ChromeGuard::spawn(chrome_port).expect("no chromium found; set UWA_CHROME_BIN");
        let url = guard.debug_url(chrome_port);
        (url, Some(guard))
    };
    eprintln!("[smoke] chromium endpoint: {debug_url}");

    assert!(
        wait_for_ws(&debug_url, Duration::from_secs(20)).await,
        "CDP endpoint {debug_url} never became reachable"
    );
    eprintln!("[smoke] endpoint reachable");

    // Everything below is budget-limited; on expiry the guard still kills Chrome.
    let scenario = async {
        let transport = CdpTransport::connect(
            &debug_url,
            Duration::from_secs(30),
            Some(uwa_stealth::default_pack()),
        )
        .await
        .expect("connect");
        eprintln!("[smoke] connected");

        let tabs = transport.list_tabs().await.expect("list_tabs");
        eprintln!("[smoke] tabs={tabs:?}");
        let tab = tabs.first().expect("at least one tab").clone();

        let page = transport.page(&tab).await.expect("acquire page");
        let probe = url::Url::parse("data:text/html,<title>probe</title>").unwrap();
        match page.goto(&probe).await {
            Ok(_) => eprintln!("[smoke] data-url goto OK"),
            Err(e) => eprintln!("[smoke] data-url goto FAILED: {e}"),
        }
        // Subscribe before navigating: the page fetches data.json during load.
        let target_id = transport.target_id(&tab).await.expect("target id");
        let mut rx = transport.bus().subscribe(&target_id).await;
        let site =
            url::Url::parse(&format!("http://127.0.0.1:{site_port}/index.html")).unwrap();
        page.goto(&site).await.expect("goto");
        eprintln!("[smoke] navigated");

        let deadline = Instant::now() + Duration::from_secs(20);
        let mut got_json = false;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Ok(NetworkEvent::ResponseBody { url, body, mime })) => {
                    if url.contains("data.json") {
                        assert!(body.contains("smoke-body"), "body = {body:?}");
                        assert!(mime.contains("json"), "mime = {mime}");
                        got_json = true;
                        break;
                    }
                }
                Ok(Ok(_)) => {}
                Ok(Err(_)) => break,
                Err(_) => break,
            }
        }
        assert!(got_json, "never observed the data.json response body");
        eprintln!("[smoke] network body observed");

        // The DOM path must also see the rendered text.
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut html = String::new();
        while Instant::now() < deadline {
            html = page.html().await.expect("html");
            if html.contains("OK:{") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        assert!(html.contains("smoke-body"), "dom never rendered the payload");

        drop(page);
        assert_eq!(transport.list_tabs().await.unwrap(), vec![tab]);
    };

    tokio::time::timeout(SCENARIO_BUDGET, scenario)
        .await
        .expect("scenario exceeded its time budget");
    eprintln!("[smoke] ok");
    // Drop the guard: kills Chromium and removes its temp profile.
    drop(chrome);
    drop(python_site);
}
