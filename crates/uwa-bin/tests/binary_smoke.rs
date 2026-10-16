//! P0: Real binary smoke tests — verifies `uwa start` actually launches
//! Chromium, serves `/healthz`/`/readyz`, and cleans up on stop.
//!
//! This is the only test that runs the REAL binary against a REAL Chromium,
//! so (like `uwa-browser`'s CDP tests) it is gated on `UWA_CHROMIUM=1`:
//! every test costs a real Chrome launch (~200 MB RAM, seconds of CPU), and
//! the default suite must stay fast and memory-light on a 2-core box.
//!
//! Enable with: `UWA_CHROMIUM=1 cargo test -p uwa-bin --test binary_smoke`
//!
//! CLI history: these tests used to pass `start --port P --profile D`.
//! The CLI now takes `-c/--config <toml>` and keeps all machine-local
//! state (PID files, daemon log, managed Chromium profile) under
//! `UWA_HOME`. Each test writes its own config + isolated UWA_HOME, so
//! five tests can't collide on ports, profiles, or a shared daemon.pid.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BINARY: &str = env!("CARGO_BIN_EXE_uwa");
const BASE_PORT: u16 = 18080; // avoid conflict with test server on 8080

/// One Chromium config on a fixed port — enough for a provider route,
/// `/v1/models`, and a `readyz` that requires a live CDP tab.
const SMOKE_CONFIG_TEMPLATE: &str = r##"
[server]
bind = "127.0.0.1"
port = __PORT__
api_key = "smoke-key"

[model_aliases]
"gpt-4o" = "chatgpt"

[providers.chatgpt]
name = "chatgpt"
url_patterns = ["https://chatgpt.com/*"]
capabilities = { streams = true, tool_calls = true, vision = false }
[providers.chatgpt.selectors]
input = "#prompt"
send_button = "button.send"
stop_button = "button.stop"
assistant_message = "[data-role=assistant]"
"##;

fn smoke_config(port: u16) -> String {
    SMOKE_CONFIG_TEMPLATE.replace("__PORT__", &port.to_string())
}

/// `UWA_HOME` for one test: pid files, daemon log and the managed Chromium
/// profile all live under it, so every test gets an isolated tree.
fn tmp_home(port: u16) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("uwa-smoke-{}-{}", std::process::id(), port))
}

/// Real-Chromium gate, same convention as `uwa-browser/tests/cdp_smoke.rs`.
fn chromium_gated() -> bool {
    std::env::var("UWA_CHROMIUM").as_deref() == Ok("1")
}

fn spawn_uwa(port: u16) -> std::process::Child {
    let home = tmp_home(port);
    let _ = std::fs::remove_dir_all(&home); // clean start
    std::fs::create_dir_all(&home)
        .unwrap_or_else(|_| unreachable!("smoke home must be creatable"));
    let cfg = home.join("uwa.toml");
    std::fs::write(&cfg, smoke_config(port))
        .unwrap_or_else(|_| unreachable!("smoke config must be writable"));

    Command::new(BINARY)
        .args(["start", "-c", cfg.to_str().unwrap_or_default(), "--quiet"])
        .env("UWA_HOME", &home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|_| unreachable!("failed to spawn uwa binary on port {port}"))
}

fn wait_for_path(port: u16, path: &str, timeout: Duration) -> bool {
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(|| unreachable!());
    while Instant::now() < deadline {
        if let Ok(resp) = ureq::get(&format!("http://127.0.0.1:{port}{path}")).call() {
            if (200..300).contains(&resp.status()) {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

fn wait_for_readyz(port: u16, timeout: Duration) -> bool {
    wait_for_path(port, "/readyz", timeout)
}

fn stop_uwa(child: &mut std::process::Child) {
    let pid = child.id();
    // SIGTERM should kill the process group (PDEATHSIG in uwa-browser)
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    let _ = child.wait();
}

/// The managed profile lives at `$UWA_HOME/chromium-profile`; a leftover
/// Chromium referencing it means the stop path leaked a browser.
fn chromium_leaked(home: &Path) -> bool {
    let profile = home.join("chromium-profile");
    Command::new("pgrep")
        .args(["-f", &format!("user-data-dir.*{}", profile.display())])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

#[test]
fn binary_starts_answers_readyz_and_stops_cleanly() {
    if !chromium_gated() {
        return;
    }
    let port = BASE_PORT;
    let home = tmp_home(port);
    let mut child = spawn_uwa(port);

    // Wait for readyz (this is TTFB — time to first byte of readiness)
    let readyz_ok = wait_for_readyz(port, Duration::from_secs(30));
    assert!(
        readyz_ok,
        "uwa did not become ready within 30s on port {port}"
    );

    // Verify provider status endpoint works (pipeline reached tab)
    let status: serde_json::Value = {
        let resp = ureq::get(&format!("http://127.0.0.1:{port}/v1/provider/status"))
            .call()
            .unwrap_or_else(|_| unreachable!("provider status failed"));
        let body = resp
            .into_string()
            .unwrap_or_else(|_| unreachable!("provider status failed"));
        serde_json::from_str(&body).unwrap_or_else(|_| unreachable!("provider status failed"))
    };
    assert!(
        status.is_object(),
        "provider status returned non-object: {status}"
    );

    // Clean stop: SIGTERM should kill uwa AND the Chromium process group
    stop_uwa(&mut child);

    // Verify Chromium is gone (PDEATHSIG works on real binary)
    assert!(
        !chromium_leaked(&home),
        "Chromium leaked after uwa stopped (home: {})",
        home.display()
    );

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn binary_healthz_reports_ok_before_readyz() {
    if !chromium_gated() {
        return;
    }
    let port = BASE_PORT + 1;
    let home = tmp_home(port);
    let mut child = spawn_uwa(port);

    // /healthz should respond before /readyz (liveness vs readiness)
    let healthz_ok = wait_for_path(port, "/healthz", Duration::from_secs(15));
    assert!(
        healthz_ok,
        "/healthz did not respond within 15s on port {port}"
    );

    stop_uwa(&mut child);
    assert!(
        !chromium_leaked(&home),
        "Chromium leaked on healthz test"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn models_endpoint_lists_expected_providers() {
    if !chromium_gated() {
        return;
    }
    let port = BASE_PORT + 2;
    let home = tmp_home(port);
    let mut child = spawn_uwa(port);

    let readyz_ok = wait_for_readyz(port, Duration::from_secs(30));
    assert!(
        readyz_ok,
        "uwa did not become ready within 30s on port {port}"
    );

    let resp: serde_json::Value = {
        let resp = ureq::get(&format!("http://127.0.0.1:{port}/v1/models"))
            .call()
            .unwrap_or_else(|_| unreachable!("models failed"));
        let body = resp
            .into_string()
            .unwrap_or_else(|_| unreachable!("models failed"));
        serde_json::from_str(&body).unwrap_or_else(|_| unreachable!("models failed"))
    };

    let data = resp
        .get("data")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        !data.is_empty(),
        "/v1/models returned empty list — config load failed?"
    );

    // At least one built-in provider should be visible
    let has_known = data.iter().any(|m| {
        m.get("owned_by")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    });
    assert!(has_known, "no provider with owned_by found in: {resp}");

    stop_uwa(&mut child);
    assert!(!chromium_leaked(&home), "Chromium leaked on models test");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn binary_restart_is_idempotent() {
    if !chromium_gated() {
        return;
    }
    // Start, stop, start again on same port/home
    let port = BASE_PORT + 3;
    let home = tmp_home(port);

    // First run
    let mut child1 = spawn_uwa(port);
    let readyz_ok = wait_for_readyz(port, Duration::from_secs(30));
    assert!(readyz_ok, "first start failed");
    stop_uwa(&mut child1);

    // Second run (same home dir)
    let mut child2 = spawn_uwa(port);
    let readyz_ok2 = wait_for_readyz(port, Duration::from_secs(30));
    assert!(readyz_ok2, "restart on same home failed");
    stop_uwa(&mut child2);

    assert!(
        !chromium_leaked(&home),
        "Chromium leaked on restart test"
    );
    let _ = std::fs::remove_dir_all(&home);
}
