//! Events endpoints against a directly-fed `EventLog`.

use serde_json::{json, Value};
use std::sync::Arc;
use uwa_api::events::{EventLog, EventLogCfg};
use uwa_core::{NetworkEvent, TappedEvent};
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockProvider, MockTransport,
};

fn app_with_events() -> (axum_test::TestServer, Arc<EventLog>) {
    let log = Arc::new(EventLog::new(EventLogCfg::default()));
    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("ok")))
        .with_events(log.clone())
        .build();
    (app.server, log)
}

async fn seed(log: &EventLog) {
    log.ingest(TappedEvent {
        target: "tab_a".into(),
        event: NetworkEvent::ResponseBody {
            request_id: "req_a".into(),
            url: "https://chatgpt.com/backend-api/conversation/abc".into(),
            body: "data: {\"v\":{\"message\":\"hi\"}}\n\n".into(),
            mime: "text/event-stream".into(),
            cf_mitigated_challenge: false,
        },
    })
    .await;
    log.ingest(TappedEvent {
        target: "tab_a".into(),
        event: NetworkEvent::Finished {
            request_id: "r1".into(),
        },
    })
    .await;
    log.ingest(TappedEvent {
        target: "tab_b".into(),
        event: NetworkEvent::ResponseBody {
            request_id: "req_b".into(),
            url: "https://claude.ai/api/xyz".into(),
            body: "data: {}\n\n".into(),
            mime: "text/event-stream".into(),
            cf_mitigated_challenge: false,
        },
    })
    .await;
}

#[tokio::test]
async fn list_returns_recent_events_newest_first() {
    let (s, log) = app_with_events();
    seed(&log).await;

    let r = s
        .get("/admin/events?limit=10")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(
        v.get("available").and_then(|a| a.as_bool()),
        Some(true),
        "available should be true: {v}"
    );
    let events = v
        .get("events")
        .and_then(|e| e.as_array())
        .unwrap_or_else(|| unreachable!("events array missing: {v}"));
    assert_eq!(events.len(), 3);
    // Newest first: claude.
    assert_eq!(
        events.first().and_then(|e| e.get("target")).and_then(|t| t.as_str()),
        Some("tab_b"),
        "first target wrong: {v}"
    );
    assert_eq!(
        events.get(2).and_then(|e| e.get("target")).and_then(|t| t.as_str()),
        Some("tab_a"),
        "third target wrong: {v}"
    );
    assert_eq!(
        events.get(2).and_then(|e| e.get("kind")).and_then(|k| k.as_str()),
        Some("response_body"),
        "third kind wrong: {v}"
    );
}

#[tokio::test]
async fn list_filters_by_target_kind_url() {
    let (s, log) = app_with_events();
    seed(&log).await;

    let target = s
        .get("/admin/events?target=tab_a")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    target.assert_status_ok();
    let v: Value = target.json();
    assert_eq!(
        v.get("events")
            .and_then(|e| e.as_array())
            .map(|a| a.len())
            .unwrap_or(0),
        2,
        "tab_a events count wrong: {v}"
    );

    let kind = s
        .get("/admin/events?kind=finished")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    let v: Value = kind.json();
    assert_eq!(
        v.get("events")
            .and_then(|e| e.as_array())
            .map(|a| a.len())
            .unwrap_or(0),
        1,
        "finished events count wrong: {v}"
    );

    let url = s
        .get("/admin/events?url=claude.ai")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    let v: Value = url.json();
    assert_eq!(
        v.get("events")
            .and_then(|e| e.as_array())
            .map(|a| a.len())
            .unwrap_or(0),
        1,
        "claude.ai events count wrong: {v}"
    );
}

#[tokio::test]
async fn summary_reports_targets() {
    let (s, log) = app_with_events();
    seed(&log).await;
    let r = s
        .get("/admin/events/summary")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(
        v.get("summary")
            .and_then(|s| s.get("total"))
            .and_then(|t| t.as_u64()),
        Some(3),
        "total wrong: {v}"
    );
    assert_eq!(
        v.get("summary")
            .and_then(|s| s.get("retained"))
            .and_then(|t| t.as_u64()),
        Some(3),
        "retained wrong: {v}"
    );
    let targets = v
        .get("summary")
        .and_then(|s| s.get("targets"))
        .and_then(|t| t.as_array())
        .unwrap_or_else(|| unreachable!("targets array missing: {v}"));
    let names: Vec<&str> = targets
        .iter()
        .map(|t| t.get("target").and_then(|t| t.as_str()).unwrap_or_else(|| unreachable!("target missing")))
        .collect();
    assert!(names.contains(&"tab_a"));
    assert!(names.contains(&"tab_b"));
}

#[tokio::test]
async fn clear_empties_the_ring_but_keeps_total() {
    let (s, log) = app_with_events();
    seed(&log).await;

    s.post("/admin/events/clear")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await
        .assert_status_ok();

    let r = s
        .get("/admin/events/summary")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    let v: Value = r.json();
    assert_eq!(
        v.get("summary")
            .and_then(|s| s.get("retained"))
            .and_then(|t| t.as_u64()),
        Some(0),
        "retained should be 0: {v}"
    );
    assert_eq!(
        v.get("summary")
            .and_then(|s| s.get("total"))
            .and_then(|t| t.as_u64()),
        Some(3),
        "total should be 3: {v}"
    );

    let r = s
        .get("/admin/events")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    let v: Value = r.json();
    assert_eq!(
        v.get("events")
            .and_then(|e| e.as_array())
            .map(|a| a.len())
            .unwrap_or(0),
        0,
        "events should be empty: {v}"
    );
}

#[tokio::test]
async fn unavailable_when_no_log_wired() {
    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("ok")))
        .build();
    let r = app
        .server
        .get("/admin/events")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(
        v.get("available").and_then(|a| a.as_bool()),
        Some(false),
        "available should be false: {v}"
    );
}

#[tokio::test]
async fn body_truncation_is_visible_in_listing() {
    let (s, log) = app_with_events();
    log.ingest(TappedEvent {
        target: "t".into(),
        event: NetworkEvent::ResponseBody {
            request_id: "r".into(),
            url: "https://x".into(),
            body: "y".repeat(70_000),
            mime: "text/event-stream".into(),
            cf_mitigated_challenge: false,
        },
    })
    .await;
    let r = s
        .get("/admin/events?limit=1")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    let v: Value = r.json();
    let ev = v
        .get("events")
        .and_then(|e| e.get(0))
        .unwrap_or_else(|| unreachable!("events[0] missing: {v}"));
    assert_eq!(
        ev.get("truncated").and_then(|t| t.as_bool()),
        Some(true),
        "truncated should be true: {ev}"
    );
    assert_eq!(
        ev.get("size_bytes").and_then(|s| s.as_u64()),
        Some(70_000),
        "size_bytes wrong: {ev}"
    );
    assert!(
        ev.get("body")
            .and_then(|b| b.as_str())
            .map(|s| s.len() <= 64 * 1024 + 8)
            .unwrap_or(false),
        "body too long: {ev}"
    );
    let _ = json!({}); // silence unused-import lint on non-debug builds
}
