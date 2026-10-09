//! Bridge semantics: UWA returns tool_calls to the client, never executes them.
//!
//! UWA is a translation bridge: tools come from the client request, the
//! browser LLM's tool-call markers are parsed into standard OpenAI
//! `tool_calls`, and the client executes them however it wants (via MCP,
//! shell, anything) before re-requesting with `role:"tool"` messages.

use axum_test::TestServer;
use serde_json::json;
use std::sync::Arc;
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockProvider, MockTransport,
};

/// The tool-call tags the browser LLM emits. Written as escapes so the
/// literal tags never appear verbatim in this source file.
const OPEN_TAG: &str = "\u{3C}tool_call\u{3E}";
const CLOSE_TAG: &str = "\u{3C}/tool_call\u{3E}";

fn server(answer: &str) -> TestServer {
    AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer(answer)))
        .build()
        .server
}

#[tokio::test]
async fn tool_call_returned_to_client() {
    let answer = format!(
        "Let me check.\n{OPEN_TAG}{{\"name\":\"get_weather\",\"arguments\":{{\"city\":\"NYC\"}}}}\n{CLOSE_TAG}"
    );
    let r = server(&answer)
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "weather?"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "weather",
                    "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}
                }
            }]
        }))
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();

    // The client receives the tool_call — UWA does not execute it.
    assert_eq!(v["choices"][0]["finish_reason"], "tool_calls");
    let tc = &v["choices"][0]["message"]["tool_calls"][0];
    assert_eq!(tc["function"]["name"], "get_weather");
    assert_eq!(v["choices"][0]["message"]["content"], "Let me check.");
}

#[tokio::test]
async fn tool_result_roundtrip() {
    // Client sends back the tool result as role:"tool"; UWA forwards it
    // to the browser. The browser answers with final text.
    let s = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(
            MockProvider::new("chatgpt").with_answer("It is 22C in NYC."),
        ))
        .build()
        .server;

    let r = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "user", "content": "weather in NYC?"},
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_1", "type": "function",
                    "function": {"name": "get_weather", "arguments": "{\"city\":\"NYC\"}"}
                }]},
                {"role": "tool", "tool_call_id": "call_1", "content": "22C"}
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "parameters": {"type": "object"}
                }
            }]
        }))
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(v["choices"][0]["finish_reason"], "stop");
    assert_eq!(v["choices"][0]["message"]["content"], "It is 22C in NYC.");
}

#[tokio::test]
async fn no_tools_no_tool_calls() {
    // Without tools, plain text comes back.
    let r = server("plain answer")
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(v["choices"][0]["message"]["content"], "plain answer");
    assert!(v["choices"][0]["message"].get("tool_calls").is_none());
}
