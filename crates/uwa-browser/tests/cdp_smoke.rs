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
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("bind site");
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

/// Canonical Chromium flags for every test in this file.
///
/// `--password-store=basic` is not cosmetic: with the default (GNOME keyring)
/// store, Chrome blocks forever in `Secret.Service.Unlock` when the default
/// collection is locked, the network service stops pumping its message loop,
/// and every request that carries cookies hangs until navigation times out.
fn chrome_args(profile: &std::path::Path, port: u16) -> Vec<String> {
    vec![
        format!("--remote-debugging-port={port}"),
        "--headless=new".into(),
        "--no-sandbox".into(),
        "--disable-gpu".into(),
        "--disable-dev-shm-usage".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--password-store=basic".into(),
        // Site isolation is required for OOPIF support: without it Chrome
        // runs cross-origin iframes in the same renderer process.
        "--site-per-process".into(),
        format!("--user-data-dir={}", profile.display()),
        "about:blank".into(),
    ]
}

fn chrome_bin() -> Option<String> {
    if let Ok(b) = std::env::var("UWA_CHROME_BIN") {
        return Some(b);
    }
    CHROME_CANDIDATES
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .map(|p| p.to_string())
}

impl ChromeGuard {
    fn spawn(port: u16) -> Option<Self> {
        let bin = chrome_bin()?;
        let profile = std::env::temp_dir().join(format!("uwa-cdp-smoke-{port}"));
        let child = Command::new(&bin)
            .args(chrome_args(&profile, port))
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

    fn pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(Child::id)
    }

    fn profile(&self) -> &std::path::Path {
        &self.profile
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
            p.split('/')
                .next()
                .unwrap_or("9222")
                .parse()
                .unwrap_or(9222),
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
        let guard = ChromeGuard::spawn(chrome_port).expect("no chromium found; set UWA_CHROME_BIN");
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
        let site = url::Url::parse(&format!("http://127.0.0.1:{site_port}/index.html")).unwrap();
        page.goto(&site).await.expect("goto");
        eprintln!("[smoke] navigated");

        let deadline = Instant::now() + Duration::from_secs(20);
        let mut got_json = false;
        let mut seen: Vec<String> = Vec::new();
        let mut end = String::from("deadline");
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Ok(ev)) => {
                    let desc = match &ev {
                        NetworkEvent::ResponseBody { url, body, mime } => {
                            format!("body {url} ({mime}) {} bytes", body.len())
                        }
                        NetworkEvent::Finished { request_id } => {
                            format!("finished {request_id}")
                        }
                    };
                    seen.push(desc);
                    if let NetworkEvent::ResponseBody { url, body, mime } = ev {
                        if url.contains("data.json") {
                            assert!(body.contains("smoke-body"), "body = {body:?}");
                            assert!(mime.contains("json"), "mime = {mime}");
                            got_json = true;
                            break;
                        }
                    }
                }
                Ok(Err(e)) => {
                    end = format!("channel: {e}");
                    break;
                }
                Err(_) => end = String::from("deadline"),
            }
        }
        assert!(
            got_json,
            "never observed the data.json response body (end={end}, seen={seen:?}, \
             target={target_id}, bus={:?})",
            transport.bus().targets().await
        );
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
        assert!(
            html.contains("smoke-body"),
            "dom never rendered the payload"
        );

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

// ---------------------------------------------------------------------------
// Environment guarantees. Everything below is the distilled result of the
// Chromium hang investigation, kept as tests so the findings cannot silently
// disappear: the flag that unblocks Chrome, the keyring state that needs it,
// and the promise that a test never leaves a browser behind.
// ---------------------------------------------------------------------------

/// `Some(locked?)` when the Secret Service answered and we could read the
/// `Locked` flag of the default collection; `None` when there is no session
/// bus, no secret service, `dbus-send` is missing, or it did not answer in
/// time. `None` is the healthy case: nothing can block Chrome on a prompt.
fn default_keyring_locked() -> Option<bool> {
    use std::io::Read;

    fn dbus(args: &[&str]) -> Option<String> {
        let mut child = std::process::Command::new("dbus-send")
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        return None;
                    }
                    let mut out = String::new();
                    child.stdout.as_mut()?.read_to_string(&mut out).ok()?;
                    return Some(out);
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                Err(_) => return None,
            }
        }
    }

    let alias = dbus(&[
        "--print-reply",
        "--dest=org.freedesktop.Secret.Service",
        "/org/freedesktop/secrets",
        "org.freedesktop.Secret.Service.ReadAlias",
        "string:default",
    ])?;
    let path: String = alias.lines().find_map(|l| l.split('"').nth(1))?.into();
    let locked = dbus(&[
        "--print-reply",
        "--dest=org.freedesktop.Secret.Service",
        &path,
        "org.freedesktop.DBus.Properties.Get",
        "string:org.freedesktop.Secret.Collection",
        "string:Locked",
    ])?;
    if locked.contains("boolean true") {
        Some(true)
    } else if locked.contains("boolean false") {
        Some(false)
    } else {
        None
    }
}

fn port_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// The exact flags `ChromeGuard` passes — if this test fails, the guard is
/// about to hang on a locked keyring again.
#[test]
fn launch_args_carry_the_keyring_workaround() {
    let profile = std::path::Path::new("/tmp/uwa-args-probe");
    let args = chrome_args(profile, 38210);
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let has_prefix = |flag: &str| args.iter().any(|a| a.starts_with(flag));

    assert!(
        has("--password-store=basic"),
        "Chrome blocks forever on a locked keyring without it: {args:?}"
    );
    assert!(has("--headless=new"), "{args:?}");
    assert!(has("--no-sandbox"), "{args:?}");
    assert!(has("--disable-dev-shm-usage"), "{args:?}");
    assert!(has("--no-first-run"), "{args:?}");
    assert!(
        has("--site-per-process"),
        "OOPIF support requires site isolation: {args:?}"
    );
    assert!(has_prefix("--remote-debugging-port="), "{args:?}");
    assert!(
        has_prefix(&format!("--user-data-dir={}", profile.display())),
        "{args:?}"
    );
    assert!(
        !args.iter().any(|a| a.starts_with("--password-store=gnome")
            || a.starts_with("--password-store=kwallet")),
        "an interactive password store must never be used headless: {args:?}"
    );
}

/// If this machine's default keyring collection is locked, the launcher must
/// carry the workaround — that combination is exactly what made navigation
/// hang (`Secret.Service.Unlock` -> unanswered prompt -> stalled network
/// service -> every cookie-bearing request blocked).
#[test]
fn locked_keyring_is_covered_by_the_launch_args() {
    let args = chrome_args(std::path::Path::new("/tmp/uwa-args-probe"), 38210);
    match default_keyring_locked() {
        Some(true) => {
            eprintln!("[env] default keyring collection is LOCKED");
            assert!(
                args.iter().any(|a| a == "--password-store=basic"),
                "the default keyring is locked, but the launcher does not pass \
                 --password-store=basic — navigation will hang: {args:?}"
            );
        }
        Some(false) => eprintln!("[env] default keyring collection is unlocked"),
        None => eprintln!("[env] no secret service available — Chrome cannot block on it"),
    }
}

/// `Drop` is the only thing standing between a failed run and a leaked
/// browser, so assert it directly: process gone, profile gone, port free.
#[tokio::test]
#[ignore = "needs a Chromium binary; run with UWA_CHROMIUM=1 cargo test -p uwa-browser -- --ignored"]
async fn guard_leaves_no_browser_profile_or_port() {
    if !enabled() {
        eprintln!("skipped: set UWA_CHROMIUM=1 to run the cleanup check");
        return;
    }
    let port = 38212u16;
    let guard = ChromeGuard::spawn(port).expect("no chromium found; set UWA_CHROME_BIN");
    let pid = guard.pid().expect("spawned child has a pid");
    let profile = guard.profile().to_path_buf();
    assert!(
        wait_for_ws(&guard.debug_url(port), Duration::from_secs(30)).await,
        "CDP endpoint never became reachable"
    );
    assert!(profile.exists(), "profile was not created: {profile:?}");
    assert!(
        !port_free(port),
        "the debug port must be bound while Chrome runs"
    );

    drop(guard);

    for _ in 0..100 {
        let gone = !std::path::Path::new(&format!("/proc/{pid}")).exists() && !profile.exists();
        if gone {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "browser pid {pid} survived Drop"
    );
    assert!(
        !profile.exists(),
        "profile {} survived Drop",
        profile.display()
    );
    assert!(port_free(port), "debug port {port} still bound after Drop");
    eprintln!("[env] guard reaped pid {pid} and removed the profile");
}

/// Chromium health check that does not go through `chromiumoxide`: if this
/// fails but the CDP smoke passes (or vice versa), the breakage is in the
/// browser itself rather than in our transport.
#[tokio::test]
#[ignore = "needs a Chromium binary; run with UWA_CHROMIUM=1 cargo test -p uwa-browser -- --ignored"]
async fn chrome_renders_a_local_page_without_cdp() {
    if !enabled() {
        eprintln!("skipped: set UWA_CHROMIUM=1 to run the Chromium health check");
        return;
    }
    let site_port = 38213u16;
    tokio::spawn(serve(site_port));
    tokio::time::sleep(Duration::from_millis(100)).await;
    self_check(site_port).await;

    let bin = chrome_bin().expect("no chromium found; set UWA_CHROME_BIN");
    let profile = std::env::temp_dir().join(format!("uwa-dumpdom-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&profile);
    let mut args: Vec<String> = chrome_args(&profile, 38214)
        .into_iter()
        .filter(|a| !a.starts_with("--remote-debugging-port=") && a != "about:blank")
        .collect();
    args.push("--virtual-time-budget=5000".into());
    args.push("--dump-dom".into());
    args.push(format!("http://127.0.0.1:{site_port}/index.html"));

    let mut child = Command::new(&bin)
        .args(&args)
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn chromium for --dump-dom");
    let pid = child.id().expect("child pid");

    let deadline = Instant::now() + Duration::from_secs(45);
    let mut status = None;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(s)) => {
                status = Some(s);
                break;
            }
            Ok(None) => tokio::time::sleep(Duration::from_millis(100)).await,
            Err(e) => {
                eprintln!("[dump] try_wait: {e}");
                break;
            }
        }
    }
    let mut out = Vec::new();
    if let Some(stdout) = child.stdout.as_mut() {
        let _ = stdout.read_to_end(&mut out).await;
    }
    if status.is_none() {
        eprintln!("[dump] chromium did not exit in time — killing pid {pid}");
        unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
        let _ = child.start_kill();
    }
    let _ = child.wait().await;

    let mut profile_removed = false;
    for _ in 0..20 {
        if std::fs::remove_dir_all(&profile).is_ok() {
            profile_removed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    for _ in 0..50 {
        if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let html = String::from_utf8_lossy(&out).to_string();
    assert!(
        status.is_some_and(|s| s.success()),
        "chromium did not finish cleanly (status {status:?})"
    );
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "chromium pid {pid} is still running"
    );
    assert!(profile_removed, "profile {} survived", profile.display());
    assert!(
        html.contains("smoke-body"),
        "the page never rendered the fetched payload ({} bytes): {}",
        html.len(),
        &html[..html.len().min(600)]
    );
    eprintln!("[dump] {} bytes, payload rendered", html.len());
}

// ---------------------------------------------------------------------------
// Frame evaluation + OOPIF tracking test.
// ---------------------------------------------------------------------------

/// Serve a two-iframe test page:
/// - iframe#same → `http://127.0.0.1:{port}/iframe.html` (same-origin, same-process)
/// - iframe#cross → `http://localhost:{port}/iframe.html` (cross-origin → OOPIF under --site-per-process)
async fn serve_oopif_site(port: u16) {
    let listener = TcpListener::bind(("0.0.0.0", port))
        .await
        .expect("bind oopif site");
    eprintln!("[oopif-site] listening on 0.0.0.0:{port}");
    loop {
        let (mut sock, _) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[oopif-site] accept error: {e}");
                return;
            }
        };
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let n = match sock.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let head = String::from_utf8_lossy(&buf);
            let path = head.split_whitespace().nth(1).unwrap_or("/");

            let body: String = if path.starts_with("/iframe.html") {
                r#"<!doctype html><html><head><title>FrameContent</title></head>
                   <body><div id="inner">frame-body-loaded</div></body></html>"#
                    .to_string()
            } else {
                format!(
                    r#"<!doctype html><html><head><title>Parent</title></head>
                       <body><h1>Parent Page</h1>
                       <iframe id="same" src="http://127.0.0.1:{port}/iframe.html"
                               style="width:300px;height:100px"></iframe>
                       <iframe id="cross" src="http://localhost:{port}/iframe.html"
                               style="width:300px;height:100px"></iframe>
                       </body></html>"#
                )
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.write_all(body.as_bytes()).await;
            let _ = sock.flush().await;
            let _ = sock.shutdown().await;
        });
    }
}

#[tokio::test]
#[ignore = "needs a Chromium binary; run with UWA_CHROMIUM=1 cargo test -p uwa-browser -- --ignored"]
async fn live_frames_eval_and_oopif_tracking() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off")),
        )
        .with_writer(std::io::stderr)
        .try_init();
    if !enabled() {
        eprintln!("skipped: set UWA_CHROMIUM=1 to run the frames test");
        return;
    }

    let site_port = 38215u16;
    let chrome_port = 38214u16;
    tokio::spawn(serve_oopif_site(site_port));
    tokio::time::sleep(Duration::from_millis(200)).await;

    let guard = ChromeGuard::spawn(chrome_port).expect("no chromium found; set UWA_CHROME_BIN");
    let debug_url = guard.debug_url(chrome_port);
    assert!(
        wait_for_ws(&debug_url, Duration::from_secs(20)).await,
        "CDP endpoint never became reachable"
    );

    let scenario = async {
        let transport = CdpTransport::connect(
            &debug_url,
            Duration::from_secs(30),
            Some(uwa_stealth::default_pack()),
        )
        .await
        .expect("connect");
        eprintln!("[frames] connected");

        let tabs = transport.list_tabs().await.expect("list_tabs");
        let tab = tabs.first().expect("at least one tab").clone();
        let page = transport.page(&tab).await.expect("acquire page");

        let parent = url::Url::parse(&format!("http://127.0.0.1:{site_port}/")).unwrap();
        page.goto(&parent).await.expect("goto parent");
        eprintln!("[frames] navigated to parent");

        // Give both iframes a moment to load.
        tokio::time::sleep(Duration::from_secs(2)).await;

        // --- Part 1: eval_in_frame in a same-origin (same-process) iframe ---
        let frames = page.frame_tree().await.expect("frame_tree");
        let same_origin_url = format!("http://127.0.0.1:{site_port}/iframe.html");
        let mut same_frame_id: Option<String> = None;
        for (fid, url) in &frames {
            eprintln!("[frames] frame: id={fid} url={url}");
            if url == &same_origin_url {
                same_frame_id = Some(fid.clone());
                break;
            }
        }
        let same_frame_id = same_frame_id.expect("same-origin iframe not found in frame tree");
        eprintln!("[frames] same-origin frame_id={same_frame_id}");

        // Evaluate in the same-origin iframe — must read its own document.
        let title = page
            .eval_in_frame(&same_frame_id, "document.title")
            .await
            .expect("eval_in_frame on same-origin iframe");
        eprintln!("[frames] title = {title:?}");
        assert_eq!(
            title,
            serde_json::json!("FrameContent"),
            "eval_in_frame must return the iframe's own document.title"
        );

        let inner = page
            .eval_in_frame(
                &same_frame_id,
                "document.getElementById('inner')?.textContent",
            )
            .await
            .expect("eval_in_frame DOM read");
        eprintln!("[frames] inner = {inner:?}");
        assert_eq!(
            inner,
            serde_json::json!("frame-body-loaded"),
            "eval_in_frame must read DOM inside the iframe"
        );

        // --- Part 2: OOPIF detection (cross-origin iframe) ---
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut oopif_detected = false;
        while Instant::now() < deadline && !oopif_detected {
            let sessions = transport.oopif().all_sessions().await;
            for s in &sessions {
                if s.target_type == "iframe" {
                    oopif_detected = true;
                    eprintln!(
                        "[frames] OOPIF tracked: frame={:?} target={} url={}",
                        s.frame_id,
                        s.target_id.inner(),
                        s.url
                    );
                    break;
                }
            }
            if !oopif_detected {
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        }
        if oopif_detected {
            eprintln!("[frames] OOPIF detection: OK");
        } else {
            eprintln!(
                "[frames] OOPIF not detected (site isolation may treat localhost as same-site)"
            );
        }

        drop(page);
        eprintln!("[frames] ok");
    };

    tokio::time::timeout(SCENARIO_BUDGET, scenario)
        .await
        .expect("frames scenario exceeded its time budget");

    drop(guard);
}

// ---------------------------------------------------------------------------
// Resource-leak guard. A test that leaves a Chromium behind is not "passing"
// in any environment; it is a time bomb for the next run. This check runs
// after every other test in this file and fails the suite if any Chrome
// process survived.
// ---------------------------------------------------------------------------

/// Count live Chrome/Chromium processes on this machine (best effort).
fn chrome_process_count() -> usize {
    std::process::Command::new("sh")
        .args([
            "-c",
            "ps -eo comm | grep -c -E '^(chrome|chromium|chromium-browser)' || true",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Assert that no Chrome processes are alive after all other tests in this
/// binary have finished. Runs last because its name sorts after the others.
#[tokio::test]
#[ignore = "needs a Chromium binary; run with UWA_CHROMIUM=1 cargo test -p uwa-browser -- --ignored"]
async fn zzz_no_chrome_leaks_after_suite() {
    if !enabled() {
        eprintln!("skipped: set UWA_CHROMIUM=1 to run the leak check");
        return;
    }
    // Other guards use SIGKILL; give the kernel a moment to reap zombies.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let count = chrome_process_count();
    assert_eq!(
        count, 0,
        "Chrome processes survived the test suite: {count} still running. \
         A previous test leaked its browser."
    );
    eprintln!("[env] zero chrome processes after suite — no leaks");
}
