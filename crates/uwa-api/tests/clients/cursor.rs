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
    assert_eq!(v["choices"][0]["finish_reason"], "tool_calls");
    let tc = &v["choices"][0]["message"]["tool_calls"][0];
    assert_eq!(tc["type"], "function");
    assert_eq!(tc["function"]["name"], "read_file");

    // Cursor parses `arguments` as JSON.
    let args: Value = serde_json::from_str(tc["function"]["arguments"].as_str().unwrap()).unwrap();
    assert_eq!(args["path"], "src/main.rs");

    // Usage must be present so Cursor's context budget works.
    assert!(v["usage"]["total_tokens"].as_u64().unwrap() > 0);
    assert!(v["usage"]["prompt_tokens"].as_u64().unwrap() > 0);
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
    assert_eq!(v["choices"][0]["finish_reason"], "stop");
    assert_eq!(v["choices"][0]["message"]["content"], "Done.");
}
