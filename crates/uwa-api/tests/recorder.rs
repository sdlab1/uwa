//! Recorder HTTP surface, driven against a scripted page.

use serde_json::json;
use std::sync::Arc;
use uwa_core::TabId;
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockPage, MockProvider, MockTransport,
};

fn app() -> axum_test::TestServer {
    let page = MockPage::new().with_url("https://chatgpt.com/");
    // The install script answers `{ok:true, installed:true}`; polls
    // return the raw buffer each time — the poller filters by `seq` on
    // its side, so the event count grows only once.
    let scripted = page.clone().expect_default(json!({
        "ok": true,
        "installed": true,
        "events": [
            { "kind": "click", "selector": "#new-chat", "seq": 1, "at_ms": 100, "src": "https://chatgpt.com/" },
            { "kind": "input", "selector": "#prompt-textarea", "text": "hello", "seq": 2, "at_ms": 200, "src": "https://chatgpt.com/" },
            { "kind": "click", "selector": "[data-testid=send-button]", "seq": 3, "at_ms": 400, "src": "https://chatgpt.com/" }
        ]
    }));

    let t = MockTransport::new().with_page(TabId::from_raw("t1"), Arc::new(scripted));
    uwa_testkit::test_server(
        AppBuilder::new()
            .with_config(config_with_key())
            .with_transport(Arc::new(t))
            .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("ok")))
            .build_state(),
    )
}

#[tokio::test]
async fn start_status_stop_round_trip() {
    let s = app();

    // Start. The recorder picks the first tab and installs the observer.
    let start = s
        .post("/admin/recorder/start")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({ "provider": "chatgpt" }))
        .await;
    start.assert_status_ok();
    let v: serde_json::Value = start.json();
    assert_eq!(
        v.get("provider").and_then(|v| v.as_str()),
        Some("chatgpt")
    );
    assert!(
        v.get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .starts_with("rec_")
    );

    // The poller emits its first batch at t≈0 (it polls once before
    // settling into the interval; see `recorder.rs`). Wait for that batch
    // instead of sleeping a blind 900ms — bounded, and never flaky.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let status = s
            .get("/admin/recorder/status")
            .add_header("Authorization", TEST_AUTH_HEADER)
            .await;
        status.assert_status_ok();
        let sv: serde_json::Value = status.json();
        if sv.get("events").and_then(|v| v.as_u64()).unwrap_or(0) >= 1 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "recorder produced no events within 2s"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    // Status reflects progress.
    let status = s
        .get("/admin/recorder/status")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    status.assert_status_ok();
    let sv: serde_json::Value = status.json();
    assert_eq!(sv.get("active").and_then(|v| v.as_bool()), Some(true));
    assert!(
        sv.get("events").and_then(|v| v.as_u64()).unwrap_or(0) >= 1
    );

    // Stop, get the proposal.
    let stop = s
        .post("/admin/recorder/stop")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    stop.assert_status_ok();
    let stv: serde_json::Value = stop.json();
    assert_eq!(
        stv.get("provider").and_then(|v| v.as_str()),
        Some("chatgpt")
    );
    assert!(
        stv.get("event_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) >= 1
    );
    assert!(
        stv.get("toml_snippet")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .contains("[[providers.chatgpt.workflow]]")
    );

    // Status is now inactive.
    let status = s
        .get("/admin/recorder/status")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    let sv: serde_json::Value = status.json();
    assert_eq!(sv.get("active").and_then(|v| v.as_bool()), Some(false));
}

#[tokio::test]
async fn second_start_is_rejected_while_active() {
    let s = app();
    s.post("/admin/recorder/start")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({ "provider": "chatgpt" }))
        .await
        .assert_status_ok();

    let second = s
        .post("/admin/recorder/start")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({ "provider": "chatgpt" }))
        .await;
    second.assert_status_bad_request();
    let v: serde_json::Value = second.json();
    assert!(
        v.get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or_default()
            .contains("already in progress")
    );

    // Clean up so other tests don't see a leaked recorder.
    s.post("/admin/recorder/stop")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await
        .assert_status_ok();
}

#[tokio::test]
async fn stop_without_start_is_rejected() {
    let s = app();
    s.post("/admin/recorder/stop")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await
        .assert_status_bad_request();
}

#[tokio::test]
async fn unknown_provider_is_rejected() {
    let s = app();
    let r = s
        .post("/admin/recorder/start")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({ "provider": "nope" }))
        .await;
    r.assert_status_not_found();
}
