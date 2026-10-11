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

    assert_eq!(v["object"], "response");
    assert_eq!(v["status"], "completed");
    assert_eq!(v["output_text"], "Hello from codex.");
    assert_eq!(v["output"][0]["type"], "message");
    assert_eq!(v["output"][0]["content"][0]["type"], "output_text");
    assert_eq!(v["output"][0]["content"][0]["text"], "Hello from codex.");
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
    assert_eq!(v["output_text"], "ok");
}
