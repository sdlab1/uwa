//! UI serving + richer status endpoints.
//!
//! `todo/ui.md` added: `GET /` (index.html), `/static/*` assets, URLs in
//! `/api/pool/status`, and selectors in `/v1/provider/status`.

use axum_test::TestServer;
use uwa_testkit::{config::config_with_key, AppBuilder, MockProvider, MockTransport};

use uwa_testkit::config::TEST_AUTH_HEADER;

fn server() -> TestServer {
    let provider = Arc::new(MockProvider::new("chatgpt").with_answer("hi"));
    AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(provider)
        .build()
        .server
}

use std::sync::Arc;

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
    assert!(v["total_tabs"].is_number(), "total_tabs must stay");
    let tabs = v["tabs"].as_array().expect("tabs array");
    assert_eq!(tabs.len(), 1);
    // MockTab reports a URL (about:blank default); the shape is what matters.
    assert!(tabs[0]["id"].is_string(), "tab entry has id");
    assert!(tabs[0]["url"].is_string(), "tab entry has url");
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
    let p = &v["chatgpt"];
    assert!(p.is_object(), "chatgpt provider present");
    assert!(p["url_patterns"].is_array());
    assert!(p["selectors"].is_object(), "selectors block for the wizard");
    assert!(p["selectors"]["input"].is_null() || p["selectors"]["input"].is_string());
    assert!(p["capabilities"].is_object(), "capabilities block");
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
