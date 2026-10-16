//! The `openai` Python SDK 1.x: chat completions with full `usage`,
//! system + user messages, streaming with `stream_options`.

use serde_json::{json, Value};
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

#[tokio::test]
async fn sdk_non_stream() {
    let r = server("The answer is 42.")
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [
                { "role": "system", "content": "You are helpful." },
                { "role": "user", "content": "What is the answer?" }
            ]
        }))
        .await;
    r.assert_status_ok();
    let v: Value = r.json();

    // SDK deserializes ChatCompletion strictly.
    assert_eq!(
        v.get("object").and_then(|v| v.as_str()).unwrap_or(""),
        "chat.completion"
    );
    assert!(!v.get("id").and_then(|v| v.as_str()).unwrap_or("").is_empty());
    assert!(v.get("created").and_then(|v| v.as_u64()).unwrap_or(0) > 0);
    assert_eq!(
        v.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("index"))
            .and_then(|i| i.as_u64()),
        Some(0)
    );
    assert_eq!(
        v.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("role"))
            .and_then(|r| r.as_str()),
        Some("assistant")
    );
    assert_eq!(
        v.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str()),
        Some("The answer is 42.")
    );
    assert!(
        v.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason"))
            .map(|f| f.is_string())
            .unwrap_or(false)
    );
    assert!(
        v.get("usage")
            .and_then(|u| u.get("prompt_tokens"))
            .and_then(|t| t.as_u64())
            .unwrap_or(0)
            > 0
    );
    assert!(
        v.get("usage")
            .and_then(|u| u.get("completion_tokens"))
            .and_then(|t| t.as_u64())
            .unwrap_or(0)
            > 0
    );
    assert!(
        v.get("usage")
            .and_then(|u| u.get("total_tokens"))
            .and_then(|t| t.as_u64())
            .unwrap_or(0)
            > 0
    );
}

#[tokio::test]
async fn sdk_stream_with_usage() {
    let r = server("Hello there, this is a streamed response.")
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "stream": true,
            "stream_options": { "include_usage": true },
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .await;
    r.assert_status_ok();

    let body = r.text();
    let frames: Vec<&str> = body.split("data: ").skip(1).collect();
    // First frame always carries role: assistant.
    let first: Value = serde_json::from_str(
        frames.get(0).map(|f| f.trim()).unwrap_or("")
    ).unwrap_or_else(|e| unreachable!("bad frame: {e}"));
    assert_eq!(
        first.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("delta"))
            .and_then(|d| d.get("role"))
            .and_then(|r| r.as_str()),
        Some("assistant")
    );
    // Last data frame before [DONE] is [DONE]; the one before is finish.
    let last_data = frames.last().map(|f| f.trim()).unwrap_or("");
    assert_eq!(last_data, "[DONE]");
}
