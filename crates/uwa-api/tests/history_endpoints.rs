use serde_json::json;
use std::sync::Arc;
use uwa_history::{HistoryCfg, HistoryStore};
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockProvider, MockTransport,
};

async fn server_with_history() -> axum_test::TestServer {
    let store = Arc::new(
        HistoryStore::new(HistoryCfg::default(), false)
            .await
            .unwrap_or_else(|e| unreachable!("history store: {e}")),
    );
    AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("hello")))
        .with_history(store)
        .build()
        .server
}

#[tokio::test]
async fn history_starts_empty() {
    let s = server_with_history().await;
    let r = s
        .get("/admin/history")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(
        v.get("count").and_then(|c| c.as_u64()),
        Some(0),
        "count should be 0: {v}"
    );
}

#[tokio::test]
async fn history_records_after_chat() {
    let s = server_with_history().await;
    // Trigger a chat.
    let _ = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;

    let r = s
        .get("/admin/history")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(
        v.get("count").and_then(|c| c.as_u64()),
        Some(1),
        "count should be 1: {v}"
    );
    let rec = v
        .get("records")
        .and_then(|r| r.get(0))
        .unwrap_or_else(|| unreachable!("records[0] missing: {v}"));
    assert_eq!(
        rec.get("provider").and_then(|p| p.as_str()),
        Some("chatgpt"),
        "wrong provider: {rec}"
    );
    assert_eq!(
        rec.get("status").and_then(|s| s.as_str()),
        Some("success"),
        "wrong status: {rec}"
    );
    assert_eq!(
        rec.get("response")
            .and_then(|r| r.get("text_preview"))
            .and_then(|t| t.as_str()),
        Some("hello"),
        "wrong text_preview: {rec}"
    );
}

#[tokio::test]
async fn stats_reflect_requests() {
    let s = server_with_history().await;
    for _ in 0..3 {
        let _ = s
            .post("/v1/chat/completions")
            .add_header("Authorization", TEST_AUTH_HEADER)
            .json(&json!({
                "model": "gpt-4o",
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .await;
    }

    let r = s
        .get("/admin/stats")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(
        v.get("total").and_then(|t| t.as_u64()),
        Some(3),
        "total should be 3: {v}"
    );
    assert_eq!(
        v.get("success").and_then(|s| s.as_u64()),
        Some(3),
        "success should be 3: {v}"
    );
}

#[tokio::test]
async fn history_detail_by_id() {
    let s = server_with_history().await;
    let _ = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;

    // Get the list to find the id.
    let r = s
        .get("/admin/history")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    let v: serde_json::Value = r.json();
    let id = v
        .get("records")
        .and_then(|r| r.get(0))
        .and_then(|r| r.get("id"))
        .and_then(|i| i.as_str())
        .unwrap_or_else(|| unreachable!("records[0].id missing: {v}"))
        .to_string();

    let r2 = s
        .get(&format!("/admin/history/{id}"))
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r2.assert_status_ok();
    let v2: serde_json::Value = r2.json();
    assert_eq!(
        v2.get("id").and_then(|i| i.as_str()),
        Some(id.as_str()),
        "id mismatch: {v2}"
    );
}

#[tokio::test]
async fn selector_test_on_live_tab() {
    let s = server_with_history().await;
    let r = s
        .post("/admin/selector-test")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "provider": "chatgpt",
            "selector": "#prompt-textarea"
        }))
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert!(
        v.get("duration_ms").and_then(|d| d.as_u64()).is_some(),
        "duration_ms missing: {v}"
    );
}

async fn history_count(s: &axum_test::TestServer, query: &str) -> u64 {
    let r = s
        .get(&format!("/admin/history{query}"))
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
    r.json::<serde_json::Value>()
        .get("count")
        .and_then(|c| c.as_u64())
        .unwrap_or_else(|| unreachable!("count missing"))
}

#[tokio::test]
async fn history_provider_and_status_filters_combine() {
    // Regression: `?provider=..&status=..` honoured only the provider and
    // silently dropped the status filter.
    let s = server_with_history().await;
    let _ = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}))
        .await;

    assert_eq!(history_count(&s, "?provider=chatgpt").await, 1);
    assert_eq!(history_count(&s, "?status=success").await, 1);
    assert_eq!(
        history_count(&s, "?provider=chatgpt&status=success").await,
        1
    );
    assert_eq!(
        history_count(&s, "?provider=chatgpt&status=error").await,
        0,
        "the status filter must apply together with the provider filter"
    );
    assert_eq!(
        history_count(&s, "?provider=claude&status=success").await,
        0
    );
}

#[tokio::test]
async fn history_rejects_unknown_status_even_with_a_provider_filter() {
    let s = server_with_history().await;
    for q in ["?status=bogus", "?provider=chatgpt&status=bogus"] {
        s.get(&format!("/admin/history{q}"))
            .add_header("Authorization", TEST_AUTH_HEADER)
            .await
            .assert_status_bad_request();
    }
}

#[tokio::test]
async fn history_limit_is_capped_and_zero_is_empty() {
    let s = server_with_history().await;
    let _ = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}))
        .await;
    assert_eq!(history_count(&s, "?limit=0").await, 0);
    assert_eq!(history_count(&s, "?limit=1000000").await, 1);
}

#[tokio::test]
async fn selector_test_refuses_tabs_the_transport_never_listed() {
    // An arbitrary `tab_id` used to be handed straight to the transport,
    // making the endpoint a page-text reader for any tab in the browser.
    let s = server_with_history().await;
    let r = s
        .post("/admin/selector-test")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "provider": "chatgpt",
            "selector": "body",
            "tab_id": "tab_not_in_the_list"
        }))
        .await;
    r.assert_status(axum::http::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        r.json::<serde_json::Value>()
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(|c| c.as_str()),
        Some("no_tab"),
        "error code should be no_tab"
    );
}

#[tokio::test]
async fn selector_test_rejects_unknown_provider() {
    let s = server_with_history().await;
    s.post("/admin/selector-test")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({"provider": "nope", "selector": "body"}))
        .await
        .assert_status_not_found();
}
