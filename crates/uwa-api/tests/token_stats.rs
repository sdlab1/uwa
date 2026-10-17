//! Tokens recorded per request, rolled up per provider and per profile.

use serde_json::{json, Value};
use std::sync::Arc;
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockProvider, MockTransport,
};

async fn app_with_history() -> axum_test::TestServer {
    let store = Arc::new(
        uwa_history::HistoryStore::new(uwa_history::HistoryCfg::default(), false)
            .await
            .unwrap_or_else(|e| unreachable!("history store: {e}")),
    );
    AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(
            MockProvider::new("chatgpt").with_answer("hello there, this is a longer reply"),
        ))
        .with_history(store)
        .build()
        .server
}

#[tokio::test]
async fn non_stream_response_records_usage() {
    let s = app_with_history().await;
    let r = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role":"user","content":"hello world, tell me something"}]
        }))
        .await;
    r.assert_status_ok();
    let body: Value = r.json();
    // Client sees usage.
    let p_in = body
        .get("usage")
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let p_out = body
        .get("usage")
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert!(p_in > 0, "prompt tokens must be positive: {body}");
    assert!(p_out > 0, "completion tokens must be positive: {body}");

    // History has the same numbers.
    let h = s
        .get("/admin/history?limit=1")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    h.assert_status_ok();
    let v: Value = h.json();
    let rec = v
        .get("records")
        .and_then(|r| r.as_array())
        .and_then(|a| a.first())
        .unwrap_or(&Value::Null);
    assert_eq!(
        rec.pointer("/response/usage/prompt_tokens"),
        body.get("usage").and_then(|u| u.get("prompt_tokens")),
        "history usage must mirror the response"
    );
    assert_eq!(
        rec.pointer("/response/usage/completion_tokens"),
        body.get("usage").and_then(|u| u.get("completion_tokens")),
    );
}

#[tokio::test]
async fn stats_accumulate_tokens() {
    let s = app_with_history().await;
    for _ in 0..3 {
        s.post("/v1/chat/completions")
            .add_header("Authorization", TEST_AUTH_HEADER)
            .json(&json!({
                "model": "gpt-4o",
                "messages": [{"role":"user","content":"hi"}]
            }))
            .await
            .assert_status_ok();
    }

    let r = s
        .get("/admin/stats?window=all")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert!(
        v.get("tokens_in").and_then(|x| x.as_u64()).unwrap_or(0) > 0,
        "tokens_in must accumulate: {v}"
    );
    assert!(
        v.get("tokens_out").and_then(|x| x.as_u64()).unwrap_or(0) > 0,
        "tokens_out must accumulate: {v}"
    );
    assert_eq!(
        v.get("tokens_in_per_minute")
            .and_then(|x| x.as_array())
            .map(Vec::len),
        Some(60),
        "60 per-minute buckets"
    );
    let providers = v
        .get("providers")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    let chatgpt = providers
        .iter()
        .find(|p| p.get("provider").and_then(|x| x.as_str()) == Some("chatgpt"))
        .unwrap_or(&Value::Null);
    assert!(
        chatgpt
            .get("tokens_in")
            .and_then(|x| x.as_u64())
            .unwrap_or(0)
            > 0,
        "provider rollup must carry tokens: {chatgpt}"
    );
    assert!(
        chatgpt
            .get("tokens_out")
            .and_then(|x| x.as_u64())
            .unwrap_or(0)
            > 0
    );
}

#[tokio::test]
async fn profile_is_recorded_and_aggregated() {
    let s = app_with_history().await;
    s.post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({"model":"gpt-4o","messages":[{"role":"user","content":"a"}]}))
        .await
        .assert_status_ok();

    // Without the X-UWA-Session header the request runs in the default
    // profile; the stats rollup must show it as such.
    let r = s
        .get("/admin/stats?window=all")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    let profiles = v
        .get("profiles")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        profiles
            .iter()
            .any(|p| p.get("profile").and_then(|x| x.as_str()) == Some("default")),
        "default profile must appear: {v}"
    );
}
