//! Continue: same wire shape as Cursor, but sends `stream_options`
//! with `include_usage: true` and expects a usage chunk in the SSE tail.

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
async fn continue_usage_chunk_in_stream() {
    let r = server("Hello world from Continue.")
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
    assert!(frames.len() > 2);

    // At least one non-[DONE] frame carries a usage object with choices=[].
    let mut seen_usage = false;
    for f in &frames {
        let trimmed = f.trim();
        if trimmed == "[DONE]" {
            continue;
        }
        let v: Value =
            serde_json::from_str(trimmed).unwrap_or_else(|e| unreachable!("bad frame: {e}"));
        if v.get("choices")
            .and_then(|c| c.as_array())
            .map(|a| a.is_empty())
            .unwrap_or(false)
        {
            if let Some(u) = v.get("usage") {
                assert!(u.get("prompt_tokens").and_then(|t| t.as_u64()).unwrap_or(0) > 0);
                assert!(
                    u.get("completion_tokens")
                        .and_then(|t| t.as_u64())
                        .unwrap_or(0)
                        > 0
                );
                assert!(
                    u.get("total_tokens").and_then(|t| t.as_u64()).unwrap_or(0)
                        >= u.get("prompt_tokens").and_then(|t| t.as_u64()).unwrap_or(0)
                );
                seen_usage = true;
            }
        }
    }
    assert!(seen_usage, "no usage chunk in stream");
}

#[tokio::test]
async fn continue_without_include_usage_has_no_usage_chunk() {
    let r = server("Short.")
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .await;
    r.assert_status_ok();
    let body = r.text();
    let frames: Vec<&str> = body.split("data: ").skip(1).collect();
    for f in &frames {
        let trimmed = f.trim();
        if trimmed == "[DONE]" {
            continue;
        }
        let v: Value =
            serde_json::from_str(trimmed).unwrap_or_else(|e| unreachable!("bad frame: {e}"));
        assert!(
            v.get("usage").is_none(),
            "unexpected usage chunk without stream_options.include_usage"
        );
    }
}
