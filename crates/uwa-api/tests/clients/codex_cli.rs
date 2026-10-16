//! Codex CLI: `/v1/responses` with `input` as an array of message objects
//! and `instructions` for the system prompt. Streaming requested but
//! currently buffered — Codex accepts a complete body.

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
async fn codex_cli_plain_response() {
    let r = server("Hello from codex.")
        .post("/v1/responses")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "instructions": "Be terse.",
            "input": [
                { "role": "user", "content": [
                    { "type": "input_text", "text": "hi" }
                ]}
            ],
            "stream": false
        }))
        .await;
    r.assert_status_ok();
    let v: Value = r.json();

    assert_eq!(
        v.get("object").and_then(|v| v.as_str()).unwrap_or(""),
        "response"
    );
    assert_eq!(
        v.get("status").and_then(|v| v.as_str()).unwrap_or(""),
        "completed"
    );
    assert_eq!(
        v.get("output_text").and_then(|v| v.as_str()).unwrap_or(""),
        "Hello from codex."
    );
    assert_eq!(
        v.get("output")
            .and_then(|o| o.get(0))
            .and_then(|o| o.get("type"))
            .and_then(|t| t.as_str()),
        Some("message")
    );
    assert_eq!(
        v.get("output")
            .and_then(|o| o.get(0))
            .and_then(|o| o.get("content"))
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("type"))
            .and_then(|t| t.as_str()),
        Some("output_text")
    );
    assert_eq!(
        v.get("output")
            .and_then(|o| o.get(0))
            .and_then(|o| o.get("content"))
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str()),
        Some("Hello from codex.")
    );
}

#[tokio::test]
async fn codex_cli_string_input() {
    let r = server("ok")
        .post("/v1/responses")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({ "model": "gpt-4o", "input": "say ok" }))
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(
        v.get("output_text").and_then(|v| v.as_str()).unwrap_or(""),
        "ok"
    );
}
