//! `x-uwa-*` response headers on both endpoints and both shapes.

use serde_json::json;
use std::sync::Arc;
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockProvider, MockTransport,
};

fn server(answer: &str) -> axum_test::TestServer {
    AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer(answer)))
        .build()
        .server
}

/// Returns the header value as a String, or None if the header is absent
/// or its value is not valid UTF-8.
fn header_value(resp: &axum_test::TestResponse, name: &str) -> Option<String> {
    match resp.headers().get(name) {
        Some(v) => v.to_str().ok().map(|s| s.to_string()),
        None => None,
    }
}

/// Returns the header value as a String, or an empty String if absent.
fn header_str(resp: &axum_test::TestResponse, name: &str) -> String {
    header_value(resp, name).unwrap_or_default()
}

#[tokio::test]
async fn non_stream_sets_conversation_retry_cache() {
    let r = server("hi")
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .await;
    r.assert_status_ok();

    let conv = header_value(&r, "x-uwa-conversation");
    assert!(
        conv.as_deref()
            .map(|s| s.starts_with("conv_"))
            .unwrap_or(false),
        "conversation id starts with conv_: {conv:?}"
    );
    assert_eq!(header_str(&r, "x-uwa-retry-count"), "0");
    assert_eq!(header_str(&r, "x-uwa-cache"), "miss");
    // The tab is set when the request reached the pipeline successfully.
    assert!(
        !header_str(&r, "x-uwa-tab").is_empty(),
        "tab header present"
    );
}

#[tokio::test]
async fn same_conversation_same_id_across_turns() {
    let s = server("ok");
    let first = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;
    first.assert_status_ok();
    let conv1 = header_str(&first, "x-uwa-conversation");

    let second = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "ok"},
                {"role": "user", "content": "again"}
            ]
        }))
        .await;
    second.assert_status_ok();
    let conv2 = header_str(&second, "x-uwa-conversation");

    assert_eq!(conv1, conv2, "same prefix → same conversation id");
}

#[tokio::test]
async fn different_conversation_different_id() {
    let s = server("ok");
    let a = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "alpha"}]
        }))
        .await;
    let b = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "beta"}]
        }))
        .await;
    let ca = header_str(&a, "x-uwa-conversation");
    let cb = header_str(&b, "x-uwa-conversation");
    assert_ne!(ca, cb);
}

#[tokio::test]
async fn unknown_model_has_no_conversation_header() {
    // Errors happen before the pipeline runs. The header is absent, and
    // the client gets a normal OpenAI-shaped error.
    let r = server("ok")
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "does-not-exist",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;
    r.assert_status_not_found();
    assert!(
        header_value(&r, "x-uwa-conversation").is_none(),
        "error responses should not carry conversation metadata"
    );
}

#[tokio::test]
async fn stream_sets_conversation_and_cache_headers() {
    let r = server("hi")
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;
    r.assert_status_ok();

    let ct = header_str(&r, "content-type");
    assert!(ct.starts_with("text/event-stream"), "ct: {ct}");

    let conv = header_value(&r, "x-uwa-conversation");
    assert!(
        conv.as_deref()
            .map(|s| s.starts_with("conv_"))
            .unwrap_or(false),
        "conversation id starts with conv_"
    );
    assert_eq!(header_str(&r, "x-uwa-cache"), "miss");
    // Retry count is not a meaningful header on streams (always "0" at
    // stream start); the real count arrives in the final SSE chunk's uwa_meta.
    assert_eq!(header_str(&r, "x-uwa-retry-count"), "0");
}

#[tokio::test]
async fn stream_final_chunk_carries_uwa_meta() {
    let r = server("hello stream")
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;
    r.assert_status_ok();
    let body = r.text();

    // Find the frame with finish_reason and parse it.
    let mut saw_meta = false;
    for frame in body.split("data: ").skip(1) {
        let trimmed = frame.trim();
        if trimmed == "[DONE]" {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let has_finish = v
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason"))
            .and_then(|f| f.as_str())
            .is_some();
        if has_finish {
            let meta = v
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("delta"))
                .and_then(|d| d.get("uwa_meta"));
            assert!(
                meta.map(|m| m.is_object()).unwrap_or(false),
                "finish chunk lacks uwa_meta: {v}"
            );
            let retry = meta
                .and_then(|m| m.get("retry_count"))
                .and_then(|m| m.as_u64())
                .unwrap_or(999);
            assert_eq!(retry, 0);
            let tab = meta.and_then(|m| m.get("tab"));
            assert!(
                tab.map(|t| t.is_string() || t.is_null()).unwrap_or(false),
                "tab must be present (string) or explicit null"
            );
            saw_meta = true;
        }
    }
    assert!(saw_meta, "no finish chunk found in stream:\n{body}");
}

#[tokio::test]
async fn messages_endpoint_sets_headers() {
    let r = server("hey")
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 128,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;
    r.assert_status_ok();
    let conv = header_value(&r, "x-uwa-conversation");
    assert!(
        conv.as_deref()
            .map(|s| s.starts_with("conv_"))
            .unwrap_or(false),
        "conversation id starts with conv_"
    );
    assert_eq!(header_str(&r, "x-uwa-cache"), "miss");
}

#[tokio::test]
async fn responses_endpoint_sets_headers() {
    let r = server("hey")
        .post("/v1/responses")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({"model": "gpt-4o", "input": "hi"}))
        .await;
    r.assert_status_ok();
    let conv = header_value(&r, "x-uwa-conversation");
    assert!(
        conv.as_deref()
            .map(|s| s.starts_with("conv_"))
            .unwrap_or(false),
        "conversation id starts with conv_"
    );
    assert_eq!(header_str(&r, "x-uwa-cache"), "miss");
}
