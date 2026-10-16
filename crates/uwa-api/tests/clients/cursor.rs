//! Cursor's request shape: OpenAI chat completions with `tools` in the
//! standard function format, stream enabled, and a system message that
//! includes the tool definitions inline. Cursor parses
//! `choices[0].message.tool_calls[].function.arguments` as JSON.

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
async fn cursor_tool_call_round_trips() {
    let answer = "Let me check.\n<tool_call>\n{\"name\":\"read_file\",\"arguments\":{\"path\":\"src/main.rs\"}}\n</tool_call>";
    let r = server(answer)
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [
                { "role": "system", "content": "You are a coding assistant." },
                { "role": "user", "content": "Read main.rs" }
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "read_file",
                    "description": "read a file",
                    "parameters": {
                        "type": "object",
                        "properties": { "path": { "type": "string" } },
                        "required": ["path"]
                    }
                }
            }],
            "tool_choice": "auto",
            "stream": false
        }))
        .await;
    r.assert_status_ok();

    let v: Value = r.json();
    assert_eq!(
        v.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason"))
            .and_then(|f| f.as_str()),
        Some("tool_calls")
    );
    let tc = v
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("tool_calls"))
        .and_then(|t| t.get(0))
        .unwrap_or_else(|| unreachable!("missing tool_call[0]"));
    assert_eq!(
        tc.get("type").and_then(|t| t.as_str()).unwrap_or(""),
        "function"
    );
    assert_eq!(
        tc.get("function")
            .and_then(|f| f.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or(""),
        "read_file"
    );

    // Cursor parses `arguments` as JSON.
    let args_str = tc
        .get("function")
        .and_then(|f| f.get("arguments"))
        .and_then(|a| a.as_str())
        .unwrap_or("");
    let args: Value =
        serde_json::from_str(args_str).unwrap_or_else(|e| unreachable!("bad args: {e}"));
    assert_eq!(
        args.get("path").and_then(|p| p.as_str()).unwrap_or(""),
        "src/main.rs"
    );

    // Usage must be present so Cursor's context budget works.
    assert!(
        v.get("usage")
            .and_then(|u| u.get("total_tokens"))
            .and_then(|t| t.as_u64())
            .unwrap_or(0)
            > 0
    );
    assert!(
        v.get("usage")
            .and_then(|u| u.get("prompt_tokens"))
            .and_then(|t| t.as_u64())
            .unwrap_or(0)
            > 0
    );
}

#[tokio::test]
async fn cursor_accepts_tool_result_follow_up() {
    let r = server("Done.")
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [
                { "role": "user", "content": "Read main.rs" },
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "read_file", "arguments": "{\"path\":\"src/main.rs\"}" }
                    }]
                },
                { "role": "tool", "tool_call_id": "call_1", "content": "fn main() {}" }
            ]
        }))
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(
        v.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason"))
            .and_then(|f| f.as_str()),
        Some("stop")
    );
    assert_eq!(
        v.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str()),
        Some("Done.")
    );
}
