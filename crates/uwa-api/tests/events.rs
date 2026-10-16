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
    assert_eq!(v["available"], true);
    let events = v["events"].as_array().unwrap();
    assert_eq!(events.len(), 3);
    // Newest first: claude.
    assert_eq!(events[0]["target"], "tab_b");
    assert_eq!(events[2]["target"], "tab_a");
    assert_eq!(events[2]["kind"], "response_body");
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
    assert_eq!(v["events"].as_array().unwrap().len(), 2);

    let kind = s
        .get("/admin/events?kind=finished")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    let v: Value = kind.json();
    assert_eq!(v["events"].as_array().unwrap().len(), 1);

    let url = s
        .get("/admin/events?url=claude.ai")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    let v: Value = url.json();
    assert_eq!(v["events"].as_array().unwrap().len(), 1);
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
    assert_eq!(v["summary"]["total"], 3);
    assert_eq!(v["summary"]["retained"], 3);
    let targets = v["summary"]["targets"].as_array().unwrap();
    let names: Vec<&str> = targets
        .iter()
        .map(|t| t["target"].as_str().unwrap())
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
    assert_eq!(v["summary"]["retained"], 0);
    assert_eq!(v["summary"]["total"], 3);

    let r = s
        .get("/admin/events")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    let v: Value = r.json();
    assert_eq!(v["events"].as_array().unwrap().len(), 0);
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
    assert_eq!(v["available"], false);
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
    let ev = &v["events"][0];
    assert_eq!(ev["truncated"], true);
    assert_eq!(ev["size_bytes"], 70_000);
    assert!(ev["body"].as_str().unwrap().len() <= 64 * 1024 + 8);
    let _ = json!({}); // silence unused-import lint on non-debug builds
}
