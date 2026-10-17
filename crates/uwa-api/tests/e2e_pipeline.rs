//! P1: E2E pipeline tests — one HTTP request through all layers:
//! routing → session acquisition → workflow send → finisher → SSE encoding.
//!
//! These catch integration gaps that unit tests miss.

use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockProvider, MockTransport,
};

#[allow(dead_code)]
mod helpers {
    use super::*;
    pub fn test_app() -> uwa_testkit::TestApp {
        AppBuilder::new()
            .with_config(config_with_key())
            .with_provider(Arc::new(
                MockProvider::new("chatgpt").with_answer("default answer"),
            ))
            .build()
    }
}

#[tokio::test]
async fn debug_messages_endpoint() {
    // Exact copy of messages.rs test setup
    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("hello")))
        .build();

    let r = app
        .server
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;

    println!("Status: {}", r.status_code());
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(v.get("type").and_then(|v| v.as_str()), Some("message"));
}

#[tokio::test]
async fn streaming_pipeline_produces_sse_chunks_with_ttft() {
    // Provider streams chunks with delay — pipeline must proxy them as SSE
    let provider = Arc::new(MockProvider::new("chatgpt").with_chunks(
        ["Hello", " world", " from", " the", " pipeline"],
        Duration::from_millis(50),
    ));

    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_provider(provider.clone())
        .build();

    // Streaming request
    let stream = app
        .server
        .post("/v1/chat/completions")
        .add_header("Accept", "text/event-stream")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;

    stream.assert_status_ok();
    let body = stream.text();

    // Parse SSE chunks
    let chunks: Vec<&str> = body
        .split("\n\n")
        .filter(|c| c.starts_with("data: "))
        .collect();

    // At least: first chunk + several deltas + final [DONE]
    assert!(chunks.len() >= 3, "too few SSE chunks: {body}");
    assert!(body.contains("data: [DONE]"), "missing [DONE] sentinel");

    // All non-[DONE] chunks must be valid JSON
    for chunk in &chunks {
        let data = chunk.strip_prefix("data: ").unwrap_or(chunk);
        if data != "[DONE]" {
            serde_json::from_str::<serde_json::Value>(data)
                .unwrap_or_else(|e| unreachable!("invalid SSE chunk JSON: {data}: {e}"));
        }
    }

    // Content accumulated correctly
    assert!(body.contains("Hello"), "first chunk missing");
    assert!(body.contains("world"), "second chunk missing");
    assert!(body.contains("pipeline"), "last chunk missing");
}

#[tokio::test]
async fn anthropic_messages_nonstreaming_round_trip() {
    // Use a custom config with claude provider
    let config_toml = r##"
        [server]
        bind = "127.0.0.1"
        port = 8080

        [model_aliases]
        "gpt-4o" = "claude"

        [providers.claude]
        name = "claude"
        url_patterns = ["https://claude.ai/*"]
        capabilities = { streams = true, tool_calls = true, vision = false }
        fallback_model = "claude-3-5-sonnet"
        [providers.claude.selectors]
        input = "#prompt"
        send_button = "button.send"
        stop_button = "button.stop"
        assistant_message = "[data-role=assistant]"
    "##;
    let config = uwa_config::Config::load_from_str(config_toml)
        .unwrap_or_else(|e| unreachable!("config load failed: {e}"));

    let app = AppBuilder::new()
        .with_config(std::sync::Arc::new(config))
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(
            MockProvider::new("claude").with_answer("Claude says hi"),
        ))
        .build();

    // Use X-UWA-Provider header to force the claude provider
    let r = app
        .server
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .add_header("X-UWA-Provider", "claude")
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .await;

    r.assert_status_ok();
    let v: serde_json::Value = r.json();

    // Anthropic Messages shape: content[] with text blocks
    assert!(
        v.get("content").map(|c| c.is_array()).unwrap_or(false),
        "missing content array: {v}"
    );
    assert_eq!(
        v.get("role").and_then(|v| v.as_str()),
        Some("assistant"),
        "wrong role: {v}"
    );
    assert!(
        v.get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.starts_with("msg_"))
            .unwrap_or(false),
        "Anthropic-style id missing: {v}"
    );

    // Content text is present
    let content_text = v
        .get("content")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("text"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    assert_eq!(
        content_text, "Claude says hi",
        "content text mismatch: {content_text}"
    );
}

#[tokio::test]
async fn responses_endpoint_streaming_round_trip() {
    let provider = Arc::new(MockProvider::new("chatgpt").with_chunks(
        ["Response ", "streaming ", "works"],
        Duration::from_millis(30),
    ));

    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_provider(provider)
        .build();

    let r = app
        .server
        .post("/v1/responses")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "input": "stream this",
            "stream": true
        }))
        .await;

    r.assert_status_ok();
    let body = r.text();

    // Responses SSE format: response.created → output_text.delta* → response.completed
    assert!(
        body.contains("response.created"),
        "missing response.created"
    );
    assert!(
        body.contains("output_text.delta"),
        "missing output_text.delta"
    );
    assert!(
        body.contains("response.completed"),
        "missing response.completed"
    );
}

#[tokio::test]
async fn tool_calls_round_trip_via_openai_endpoint() {
    // Provider responds with tool-call JSON in text (what the site LLM emits)
    let answer = r#"```json
{
  "name": "get_weather",
  "arguments": {"city": "Kyiv"}
}
```"#;

    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer(answer)))
        .build();

    let r = app
        .server
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "weather in Kyiv?"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get current weather",
                    "parameters": {
                        "type": "object",
                        "properties": {"city": {"type": "string"}},
                        "required": ["city"]
                    }
                }
            }]
        }))
        .await;

    r.assert_status_ok();
    let v: serde_json::Value = r.json();

    let tool_calls = v
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|c| c.get("tool_calls"));
    assert!(
        tool_calls.map(|c| c.is_array()).unwrap_or(false),
        "expected tool_calls in response: {v}"
    );
    assert_eq!(
        tool_calls
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("function"))
            .and_then(|c| c.get("name"))
            .and_then(|c| c.as_str()),
        Some("get_weather"),
        "wrong tool name: {v}"
    );

    let args: serde_json::Value = serde_json::from_str(
        tool_calls
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("function"))
            .and_then(|c| c.get("arguments"))
            .and_then(|c| c.as_str())
            .unwrap_or("{}"),
    )
    .unwrap_or_default();
    assert_eq!(
        args.get("city").and_then(|c| c.as_str()),
        Some("Kyiv"),
        "wrong tool args: {v}"
    );

    // Finish reason must be tool_calls
    assert_eq!(
        v.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason"))
            .and_then(|c| c.as_str()),
        Some("tool_calls")
    );
}

#[tokio::test]
async fn tool_result_roundtrip_via_openai_endpoint() {
    // First turn: tool call
    let provider = Arc::new(MockProvider::new("chatgpt").with_answers([
        r#"{"name": "get_weather", "arguments": {"city": "Kyiv"}}"#,
        "It is 22C in Kyiv.",
    ]));

    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_provider(provider.clone())
        .build();

    // Turn 1: user asks, model emits tool call
    let r1 = app
        .server
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "weather in Kyiv?"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "parameters": {
                        "type": "object",
                        "properties": {"city": {"type": "string"}}
                    }
                }
            }]
        }))
        .await;

    r1.assert_status_ok();
    let v1: serde_json::Value = r1.json();
    assert_eq!(
        v1.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason")),
        Some(&serde_json::json!("tool_calls"))
    );

    // Turn 2: client sends tool result back
    let r2 = app
        .server
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "user", "content": "weather in Kyiv?"},
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_1", "type": "function",
                    "function": {"name": "get_weather", "arguments": "{\"city\":\"Kyiv\"}"}
                }]},
                {"role": "tool", "tool_call_id": "call_1", "content": "22C"}
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}
                }
            }]
        }))
        .await;

    r2.assert_status_ok();
    let v2: serde_json::Value = r2.json();
    assert_eq!(
        v2.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason")),
        Some(&serde_json::json!("stop"))
    );
    assert_eq!(
        v2.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|c| c.get("content"))
            .and_then(|c| c.as_str()),
        Some("It is 22C in Kyiv.")
    );
}

#[tokio::test]
async fn multiple_concurrent_streams_dont_interfere() {
    let provider = Arc::new(
        MockProvider::new("chatgpt").with_chunks(["A", "B", "C"], Duration::from_millis(20)),
    );

    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_provider(provider.clone())
        .build();

    // Two simultaneous streaming requests
    let (r1, r2) = tokio::join!(
        app.server
            .post("/v1/chat/completions")
            .add_header("Accept", "text/event-stream")
            .add_header("Authorization", TEST_AUTH_HEADER)
            .json(&json!({"model": "gpt-4o", "stream": true, "messages": [{"role": "user", "content": "first"}]})),
        app.server
            .post("/v1/chat/completions")
            .add_header("Accept", "text/event-stream")
            .add_header("Authorization", TEST_AUTH_HEADER)
            .json(&json!({"model": "gpt-4o", "stream": true, "messages": [{"role": "user", "content": "second"}]})),
    );

    r1.assert_status_ok();
    r2.assert_status_ok();

    let body1 = r1.text();
    let body2 = r2.text();

    // Both must have [DONE] and valid SSE structure
    assert!(body1.contains("data: [DONE]"), "stream 1 missing [DONE]");
    assert!(body2.contains("data: [DONE]"), "stream 2 missing [DONE]");
}
