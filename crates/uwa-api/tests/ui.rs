//! UI serving + richer status endpoints.
//!
//! `todo/ui.md` added: `GET /` (index.html), `/static/*` assets, URLs in
//! `/api/pool/status`, and selectors in `/v1/provider/status`.

use axum_test::TestServer;
use std::sync::Arc;
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockProvider, MockTransport,
};

fn server() -> TestServer {
    let provider = Arc::new(MockProvider::new("chatgpt").with_answer("hi"));
    AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(provider)
        .build()
        .server
}

#[tokio::test]
async fn index_served_when_built() {
    // The built UI lives in `static/` next to the crate. Skip when it
    // hasn't been built yet (verify.sh runs build.sh first).
    let index = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("static/index.html");
    if !index.exists() {
        return;
    }
    let s = server();
    let r = s.get("/").await;
    assert_eq!(r.status_code(), 200);
    let body = r.text();
    assert!(body.contains("uwa"), "index.html should mention uwa");
}

#[tokio::test]
async fn pool_status_has_urls() {
    let s = server();
    let r = s
        .get("/api/pool/status")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    assert_eq!(r.status_code(), 200);
    let v: serde_json::Value = r.json();
    assert!(
        v.get("total_tabs").map(|v| v.is_number()).unwrap_or(false),
        "total_tabs must stay"
    );
    let tabs = v
        .get("tabs")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| unreachable!("tabs array missing"));
    assert_eq!(tabs.len(), 1);
    // MockTab reports a URL (about:blank default); the shape is what matters.
    assert!(
        tabs.get(0).and_then(|t| t.get("id")).and_then(|v| v.is_string()).unwrap_or(false),
        "tab entry has id"
    );
    assert!(
        tabs.get(0).and_then(|t| t.get("url")).and_then(|v| v.is_string()).unwrap_or(false),
        "tab entry has url"
    );
}

#[tokio::test]
async fn provider_status_has_selectors_and_caps() {
    let s = server();
    let r = s
        .get("/v1/provider/status")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    assert_eq!(r.status_code(), 200);
    let v: serde_json::Value = r.json();
    let p = v.get("chatgpt").unwrap_or_else(|| unreachable!("chatgpt provider present"));
    assert!(p.is_object(), "chatgpt provider present");
    assert!(
        p.get("url_patterns").map(|v| v.is_array()).unwrap_or(false)
    );
    assert!(
        p.get("selectors").map(|v| v.is_object()).unwrap_or(false),
        "selectors block for the wizard"
    );
    assert!(
        p.get("selectors")
            .and_then(|s| s.get("input"))
            .map(|v| v.is_null() || v.is_string())
            .unwrap_or(false)
    );
    assert!(
        p.get("capabilities").map(|v| v.is_object()).unwrap_or(false),
        "capabilities block"
    );
}

#[tokio::test]
async fn admin_sessions_shape() {
    let s = server();
    let r = s
        .get("/admin/sessions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    // No session manager in the default testkit builder → 500 with a clear
    // message, or 200 with an empty list if one is wired. Either way it
    // must be JSON, not a panic.
    let body = r.text();
    assert!(
        body.contains("session") || r.status_code().is_success(),
        "unexpected body: {body}"
    );
}

#[tokio::test]
async fn log_stream_is_sse() {
    // SSE never terminates by design (live tail), so we must not await the
    // body — the axum-test harness would hang. Call the handler directly
    // and inspect the response headers, then drop the stream.
    let provider = Arc::new(MockProvider::new("chatgpt").with_answer("hi"));
    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(provider)
        .build();
    let resp = uwa_api::routes::admin::log_stream(axum::extract::State(app.state)).await;
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.starts_with("text/event-stream"),
        "log stream must be SSE, got {ct}"
    );
}
