//! Claude Code: Anthropic Messages with content blocks, tool_use /
//! tool_result round trips, and `system` as a string.

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
async fn claude_code_tool_use_flow() {
    let answer = "Checking.\n<tool_call>\n{\"name\":\"read_file\",\"arguments\":{\"path\":\"README.md\"}}\n</tool_call>";
    let r = server(answer)
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 512,
            "system": "You help with code.",
            "messages": [
                { "role": "user", "content": "Read README.md" }
            ],
            "tools": [{
                "name": "read_file",
                "description": "read a file",
                "input_schema": {
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"]
                }
            }]
        }))
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(v["type"], "message");
    assert_eq!(v["stop_reason"], "tool_use");

    let tool = v["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "tool_use")
        .expect("tool_use block");
    assert_eq!(tool["name"], "read_file");
    assert_eq!(tool["input"]["path"], "README.md");

    // Claude Code uses usage to update its status line.
    assert!(v["usage"]["input_tokens"].as_u64().unwrap() > 0);
    assert!(v["usage"]["output_tokens"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn claude_code_tool_result_continuation() {
    let r = server("The file says hello.")
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 512,
            "messages": [
                { "role": "user", "content": "Read README.md" },
                { "role": "assistant", "content": [
                    { "type": "tool_use", "id": "toolu_1", "name": "read_file",
                      "input": { "path": "README.md" } }
                ]},
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "toolu_1", "content": "hello world" }
                ]}
            ]
        }))
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(v["stop_reason"], "end_turn");
    assert_eq!(v["content"][0]["text"], "The file says hello.");
}
