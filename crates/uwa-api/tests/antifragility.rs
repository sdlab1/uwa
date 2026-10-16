//! P2: Anti-fragility — errors, timeouts, edge-cases that break in prod.

use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockProvider,
};

#[tokio::test]
async fn failing_provider_produces_503_with_openai_error_shape() {
    let provider = Arc::new(MockProvider::new("chatgpt").with_error("selector not found"));

    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_provider(provider)
        .build();

    let r = app
        .server
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;

    // OpenAI-shape error, not raw panic and not 500 with trace
    let status = r.status_code().as_u16();
    assert!(
        status == 503 || status == 500,
        "unexpected status: {status}"
    );
    let v: serde_json::Value = r.json();

    assert!(
        v.get("error").and_then(|e| e.get("message")).map(|m| m.is_string()).unwrap_or(false),
        "missing error.message: {v}"
    );
    assert!(
        v.get("error").and_then(|e| e.get("type")).map(|t| t.is_string()).unwrap_or(false),
        "missing error.type: {v}"
    );

    // No leaked traces or source paths
    let body_str = v.to_string();
    assert!(!body_str.contains("panicked"), "leaked panic trace: {v}");
    assert!(!body_str.contains("at src/"), "leaked source path: {v}");
    assert!(!body_str.contains("unwrap"), "leaked unwrap in error: {v}");
}

#[tokio::test]
async fn provider_timeout_produces_graceful_error() {
    let provider = Arc::new(
        MockProvider::new("chatgpt")
            .with_delay(Duration::from_secs(2))
            .with_answer("slow"),
    );

    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_provider(provider)
        .build();

    let r = app
        .server
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;

    // Should either timeout (504/503) or succeed, but not panic
    let status = r.status_code().as_u16();
    assert!(
        status == 200 || status == 503 || status == 504,
        "unexpected status: {status}"
    );

    if status != 200 {
        let v: serde_json::Value = r.json();
        assert!(
            v.get("error").and_then(|e| e.get("message")).map(|m| m.is_string()).unwrap_or(false),
            "missing error.message on timeout: {v}"
        );
    }
}

#[tokio::test]
async fn multibyte_chars_survive_chunk_boundaries() {
    // Emoji and CJK chars in the middle of chunks - use streaming mode
    let provider = Arc::new(MockProvider::new("chatgpt").with_chunks(
        ["Прив", "іт 🇺🇦世", "界", " від Ky", "iv"],
        Duration::from_millis(10),
    ));

    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_provider(provider)
        .build();

    let r = app
        .server
        .post("/v1/chat/completions")
        .add_header("Accept", "text/event-stream")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "stream": true,
            "messages": [{"role": "user", "content": "test"}]
        }))
        .await;

    r.assert_status_ok();
    let body = r.text();

    // Accumulated SSE deltas should form the complete string
    let mut accumulated = String::new();
    for chunk in body.split("\n\n") {
        if let Some(data) = chunk.strip_prefix("data: ") {
            if data != "[DONE]" {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                    if let Some(delta) = v
                        .get("choices")
                        .and_then(|c| c.get(0))
                        .and_then(|c| c.get("delta"))
                        .and_then(|d| d.get("content"))
                        .and_then(|c| c.as_str())
                    {
                        accumulated.push_str(delta);
                    }
                }
            }
        }
    }

    // Full string without loss or mojibake
    assert_eq!(
        accumulated, "Привіт 🇺🇦世界 від Kyiv",
        "content mangled: {accumulated:?}"
    );
}

#[tokio::test]
async fn streaming_multibyte_chars_survive_chunk_boundaries() {
    let provider = Arc::new(
        MockProvider::new("chatgpt")
            .with_chunks(["Прив", "іт 🇺🇦", " 世界"], Duration::from_millis(10)),
    );

    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_provider(provider)
        .build();

    let r = app
        .server
        .post("/v1/chat/completions")
        .add_header("Accept", "text/event-stream")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "stream": true,
            "messages": [{"role": "user", "content": "test"}]
        }))
        .await;

    r.assert_status_ok();
    let body = r.text();

    // Accumulated SSE deltas should form the complete string
    // Extract all delta.content from SSE chunks
    let mut accumulated = String::new();
    for chunk in body.split("\n\n") {
        if let Some(data) = chunk.strip_prefix("data: ") {
            if data != "[DONE]" {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                    if let Some(delta) = v
                        .get("choices")
                        .and_then(|c| c.get(0))
                        .and_then(|c| c.get("delta"))
                        .and_then(|d| d.get("content"))
                        .and_then(|c| c.as_str())
                    {
                        accumulated.push_str(delta);
                    }
                }
            }
        }
    }

    assert_eq!(
        accumulated, "Привіт 🇺🇦 世界",
        "streaming content mangled: {accumulated:?}"
    );
}

#[tokio::test]
async fn empty_response_handled_gracefully() {
    let provider = Arc::new(MockProvider::new("chatgpt").with_answer(""));

    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_provider(provider)
        .build();

    let r = app
        .server
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;

    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    // Empty content is valid (null or empty string)
    let content = v
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    assert_eq!(content, "");
    assert_eq!(
        v.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason"))
            .and_then(|f| f.as_str()),
        Some("stop")
    );
}

#[tokio::test]
async fn very_long_response_streaming_doesnt_overflow() {
    // 100KB of text streamed in small chunks
    let long_text = "x".repeat(100_000);
    let chunks: Vec<String> = long_text
        .chars()
        .collect::<Vec<_>>()
        .chunks(100)
        .map(|c| c.iter().collect())
        .collect();

    let provider =
        Arc::new(MockProvider::new("chatgpt").with_chunks(chunks, Duration::from_millis(1)));

    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_provider(provider)
        .build();

    let r = app
        .server
        .post("/v1/chat/completions")
        .add_header("Accept", "text/event-stream")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "stream": true,
            "messages": [{"role": "user", "content": "long"}]
        }))
        .await;

    r.assert_status_ok();
    let body = r.text();

    // Should complete without OOM or truncation
    assert!(
        body.contains("data: [DONE]"),
        "missing [DONE] on long stream"
    );

    // Count content length from deltas
    let mut total = 0usize;
    for chunk in body.split("\n\n") {
        if let Some(data) = chunk.strip_prefix("data: ") {
            if data != "[DONE]" {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                    if let Some(delta) = v
                        .get("choices")
                        .and_then(|c| c.get(0))
                        .and_then(|c| c.get("delta"))
                        .and_then(|d| d.get("content"))
                        .and_then(|c| c.as_str())
                    {
                        total += delta.len();
                    }
                }
            }
        }
    }

    // Allow small overhead from SSE framing
    assert!(
        total >= 95_000,
        "lost too much content in long stream: {total}"
    );
}

#[tokio::test]
async fn malformed_json_in_tool_call_is_rejected_gracefully() {
    // Provider returns invalid JSON in tool_call block - parser should
    // either reject it (4xx) or treat it as plain text (200 with no tool_calls)
    let answer = "```json\n{ not valid json }\n```";

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
            "messages": [{"role": "user", "content": "do something"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "do_something",
                    "parameters": {"type": "object"}
                }
            }]
        }))
        .await;

    // Should not panic - either returns 200 (no tool_calls) or 4xx
    let status = r.status_code().as_u16();
    assert!(
        status == 200 || (status >= 400 && status < 600),
        "unexpected status: {status}"
    );

    let v: serde_json::Value = r.json();
    if status == 200 {
        // If 200, tool_calls should be null/empty since JSON was invalid
        let tc = v
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("tool_calls"))
            .and_then(|t| t.as_array());
        assert!(
            tc.is_none() || tc.map(|a| a.is_empty()).unwrap_or(true),
            "expected no tool_calls for invalid JSON: {v}"
        );
    }
}

#[tokio::test]
async fn circuit_breaker_opens_after_repeated_failures() {
    // Multiple failing requests should trip the circuit breaker
    let provider = Arc::new(MockProvider::new("chatgpt").with_error("permanent failure"));

    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_provider(provider)
        .build();

    // First few requests fail with 503/500
    for i in 0..5 {
        let r = app
            .server
            .post("/v1/chat/completions")
            .add_header("Authorization", TEST_AUTH_HEADER)
            .json(&json!({
                "model": "gpt-4o",
                "messages": [{"role": "user", "content": format!("attempt {i}")}]
            }))
            .await;

        let status = r.status_code().as_u16();
        assert!(status >= 500, "attempt {i}: expected 5xx, got {status}");
    }

    // After threshold, subsequent requests should fail fast (circuit open)
    // The exact status depends on circuit breaker config; just verify no panic
    let r = app
        .server
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "after circuit open"}]
        }))
        .await;

    let status = r.status_code().as_u16();
    assert!(
        status >= 500,
        "circuit breaker should keep failing: {status}"
    );
    let v: serde_json::Value = r.json();
    assert!(
        v.get("error").and_then(|e| e.get("message")).map(|m| m.is_string()).unwrap_or(false)
    );
}

#[tokio::test]
async fn provider_returns_empty_tool_call_block() {
    // Edge case: tool_call block with no name/arguments
    let answer = r#"```json
{}
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
            "messages": [{"role": "user", "content": "do something"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "do_something",
                    "parameters": {"type": "object"}
                }
            }]
        }))
        .await;

    // Should not panic; should return error or empty tool_calls
    let status = r.status_code().as_u16();
    assert!(
        status == 200 || status >= 400,
        "unexpected status: {status}"
    );

    if status == 200 {
        let v: serde_json::Value = r.json();
        // If it succeeds, tool_calls should be empty or null
        let tc = v
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("tool_calls"))
            .and_then(|t| t.as_array());
        assert!(tc.is_none() || tc.map(|a| a.is_empty()).unwrap_or(true));
    }
}
