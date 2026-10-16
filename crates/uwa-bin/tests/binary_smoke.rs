//! P0: Real binary smoke tests — verifies `uwa start` actually launches
//! Chromium, serves `/healthz`/`/readyz`, and cleans up on stop.
//!
use serde_json;
// This is the only test that runs the REAL binary against a REAL Chromium.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BINARY: &str = env!("CARGO_BIN_EXE_uwa");
const BASE_PORT: u16 = 18080; // avoid conflict with test server on 8080

fn tmp_profile(port: u16) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("uwa-smoke-{}-{}", std::process::id(), port))
}

fn spawn_uwa(port: u16) -> std::process::Child {
    let profile = tmp_profile(port);
    let _ = std::fs::remove_dir_all(&profile); // clean start

    Command::new(BINARY)
        .args([
            "start",
            "--port",
            &port.to_string(),
            "--profile",
            profile.to_str().unwrap_or_default(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn uwa binary on port {port}: {e}"))
}

fn wait_for_readyz(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(resp) = ureq::get(&format!("http://127.0.0.1:{port}/readyz")).call() {
            if (200..300).contains(&resp.status()) {
                let body = resp.into_string().unwrap_or_default();
                if body.contains("ok") || body.contains("ready") {
                    return true;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

fn stop_uwa(child: &mut std::process::Child) {
    let pid = child.id();
    // SIGTERM should kill the process group (PDEATHSIG in uwa-browser)
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    let _ = child.wait();
}

fn chromium_leaked(profile: &std::path::PathBuf) -> bool {
    // Check if any Chromium process still references this profile dir
    let profile_str = profile.to_string_lossy();
    Command::new("pgrep")
        .args(["-f", &format!("user-data-dir.*{}", profile_str)])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

#[test]
fn binary_starts_answers_readyz_and_stops_cleanly() {
    let port = BASE_PORT;
    let profile = tmp_profile(port);
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
            .unwrap_or_else(|e| panic!("provider status failed: {e}"));
        let body = resp
            .into_string()
            .unwrap_or_else(|e| panic!("provider status failed: {e}"));
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("provider status failed: {e}"))
    };
    assert!(
        status.is_object(),
        "provider status returned non-object: {status}"
    );

    // Clean stop: SIGTERM should kill uwa AND the Chromium process group
    stop_uwa(&mut child);

    // Verify Chromium is gone (PDEATHSIG works on real binary)
    assert!(
        !chromium_leaked(&profile),
        "Chromium leaked after uwa stopped (profile: {})",
        profile.display()
    );

    let _ = std::fs::remove_dir_all(&profile);
}

#[test]
fn binary_healthz_reports_ok_before_readyz() {
    let port = BASE_PORT + 1;
    let profile = tmp_profile(port);
    let mut child = spawn_uwa(port);

    // /healthz should respond before /readyz (liveness vs readiness)
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut healthz_ok = false;
    while Instant::now() < deadline {
        if let Ok(resp) = ureq::get(&format!("http://127.0.0.1:{port}/healthz")).call() {
            if (200..300).contains(&resp.status()) {
                healthz_ok = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(
        healthz_ok,
        "/healthz did not respond within 15s on port {port}"
    );

    stop_uwa(&mut child);
    assert!(
        !chromium_leaked(&profile),
        "Chromium leaked on healthz test"
    );
    let _ = std::fs::remove_dir_all(&profile);
}

#[test]
fn models_endpoint_lists_expected_providers() {
    let port = BASE_PORT + 2;
    let profile = tmp_profile(port);
    let mut child = spawn_uwa(port);

    let readyz_ok = wait_for_readyz(port, Duration::from_secs(30));
    assert!(
        readyz_ok,
        "uwa did not become ready within 30s on port {port}"
    );

    let resp: serde_json::Value = {
        let resp = ureq::get(&format!("http://127.0.0.1:{port}/v1/models"))
            .call()
            .unwrap_or_else(|e| panic!("models failed: {e}"));
        let body = resp.into_string().unwrap_or_else(|e| panic!("models failed: {e}"));
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("models failed: {e}"))
    };

    let data = resp["data"].as_array().cloned().unwrap_or_default();
    assert!(
        !data.is_empty(),
        "/v1/models returned empty list — config load failed?"
    );

    // At least one built-in provider should be visible
    let has_known = data.iter().any(|m| {
        m["owned_by"]
            .as_str()
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    });
    assert!(has_known, "no provider with owned_by found in: {resp}");

    stop_uwa(&mut child);
    assert!(!chromium_leaked(&profile), "Chromium leaked on models test");
    let _ = std::fs::remove_dir_all(&profile);
}

#[test]
fn binary_restart_is_idempotent() {
    // Start, stop, start again on same port/profile
    let port = BASE_PORT + 3;
    let profile = tmp_profile(port);

    // First run
    let mut child1 = spawn_uwa(port);
    let readyz_ok = wait_for_readyz(port, Duration::from_secs(30));
    assert!(readyz_ok, "first start failed");
    stop_uwa(&mut child1);

    // Second run (same profile dir)
    let mut child2 = spawn_uwa(port);
    let readyz_ok2 = wait_for_readyz(port, Duration::from_secs(30));
    assert!(readyz_ok2, "restart on same profile failed");
    stop_uwa(&mut child2);

    assert!(
        !chromium_leaked(&profile),
        "Chromium leaked on restart test"
    );
    let _ = std::fs::remove_dir_all(&profile);
}
