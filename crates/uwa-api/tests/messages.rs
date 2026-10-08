use axum_test::TestServer;
use serde_json::json;
use std::sync::Arc;
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockProvider, MockTransport,
};

fn server(answer: &str) -> TestServer {
    AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer(answer)))
        .build()
        .server
}

#[tokio::test]
async fn plain_text_end_turn() {
    let r = server("hello")
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(v["type"], "message");
    assert_eq!(v["role"], "assistant");
    assert_eq!(v["content"][0]["type"], "text");
    assert_eq!(v["content"][0]["text"], "hello");
    assert_eq!(v["stop_reason"], "end_turn");
}

#[tokio::test]
async fn tool_use_block_emitted() {
    let answer = "checking\n<tool_call>{\"name\":\"get_weather\",\"arguments\":{\"city\":\"NYC\"}}</tool_call>";
    let r = server(answer)
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "weather?"}],
            "tools": [{
                "name": "get_weather",
                "description": "w",
                "input_schema": {"type": "object"}
            }]
        }))
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(v["stop_reason"], "tool_use");
    let tool_use = v["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "tool_use")
        .unwrap();
    assert_eq!(tool_use["name"], "get_weather");
    assert_eq!(tool_use["input"]["city"], "NYC");
}

#[tokio::test]
async fn streaming_shape() {
    let r = server("hi")
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 64,
            "stream": true,
            "messages": [{"role": "user", "content": "x"}]
        }))
        .await;
    r.assert_status_ok();
    let body = r.text();
    assert!(body.contains("message_start"));
    assert!(body.contains("content_block_start"));
    assert!(body.contains("content_block_delta"));
    assert!(body.contains("content_block_stop"));
    assert!(body.contains("message_delta"));
    assert!(body.contains("message_stop"));
}

#[tokio::test]
async fn tool_result_roundtrip() {
    // Multi-turn: assistant asked for a tool, user supplies the result.
    let r = server("thanks")
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 64,
            "messages": [
                {"role": "user", "content": "weather?"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "t1", "name": "w", "input": {"city": "NYC"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "22C"}
                ]}
            ]
        }))
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(v["content"][0]["text"], "thanks");
}
