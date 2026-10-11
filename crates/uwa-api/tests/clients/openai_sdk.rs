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
    assert_eq!(v["object"], "chat.completion");
    assert!(!v["id"].as_str().unwrap().is_empty());
    assert!(v["created"].as_u64().unwrap() > 0);
    assert_eq!(v["choices"][0]["index"], 0);
    assert_eq!(v["choices"][0]["message"]["role"], "assistant");
    assert_eq!(v["choices"][0]["message"]["content"], "The answer is 42.");
    assert!(v["choices"][0]["finish_reason"].is_string());
    assert!(v["usage"]["prompt_tokens"].as_u64().unwrap() > 0);
    assert!(v["usage"]["completion_tokens"].as_u64().unwrap() > 0);
    assert!(v["usage"]["total_tokens"].as_u64().unwrap() > 0);
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
    let first: Value = serde_json::from_str(frames[0].trim()).unwrap();
    assert_eq!(first["choices"][0]["delta"]["role"], "assistant");
    // Last data frame before [DONE] is [DONE]; the one before is finish.
    let last_data = frames.last().unwrap().trim();
    assert_eq!(last_data, "[DONE]");
}
