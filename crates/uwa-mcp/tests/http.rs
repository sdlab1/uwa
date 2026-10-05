//! HTTP + SSE transport for the MCP server (feature `mcp-http`).

#![cfg(feature = "mcp-http")]

use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use uwa_core::{Result, UwaError};
use uwa_mcp::{CallToolResult, McpHandler, McpServer, McpTool};

struct Echo;

#[async_trait]
impl McpHandler for Echo {
    fn namespace(&self) -> &str {
        ""
    }
    fn tools(&self) -> Vec<McpTool> {
        vec![McpTool {
            name: "echo".into(),
            description: "echo".into(),
            input_schema: json!({"type": "object"}),
        }]
    }
    async fn call(&self, tool: &str, args: Value) -> Result<CallToolResult> {
        if tool != "echo" {
            return Err(UwaError::BadRequest("unknown".into()));
        }
        Ok(CallToolResult::text(
            args.get("text").and_then(Value::as_str).unwrap_or(""),
        ))
    }
}

fn server() -> axum_test::TestServer {
    let mcp = Arc::new(McpServer::new("http-srv").register(Arc::new(Echo)));
    axum_test::TestServer::new(uwa_mcp::http::router(mcp)).unwrap()
}

#[tokio::test]
async fn initialize_answers_over_http() {
    let r = server()
        .post("/mcp")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}))
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(v["result"]["serverInfo"]["name"], "http-srv");
    assert_eq!(v["result"]["capabilities"]["tools"]["listChanged"], false);
    assert!(v["result"]["capabilities"].get("resources").is_none());
}

#[tokio::test]
async fn tool_calls_run_over_http() {
    let r = server()
        .post("/mcp")
        .json(&json!({
            "jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"echo","arguments":{"text":"hi"}}
        }))
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(v["result"]["content"][0]["text"], "hi");

    let bad = server()
        .post("/mcp")
        .json(&json!({
            "jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"other","arguments":{}}
        }))
        .await;
    bad.assert_status_ok();
    let v: Value = bad.json();
    assert_eq!(v["error"]["code"], -32602);

    let unknown_ns = server()
        .post("/mcp")
        .json(&json!({
            "jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"ghost__echo","arguments":{}}
        }))
        .await;
    unknown_ns.assert_status_ok();
    let v: Value = unknown_ns.json();
    assert_eq!(v["error"]["code"], -32601);
}

#[tokio::test]
async fn notifications_are_accepted_without_a_body() {
    let r = server()
        .post("/mcp")
        .json(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .await;
    r.assert_status(axum::http::StatusCode::ACCEPTED);
}

#[tokio::test]
async fn sse_announces_the_message_endpoint() {
    let r = server().get("/mcp/sse").await;
    r.assert_status_ok();
    let ct = r
        .headers()
        .get("content-type")
        .expect("content-type")
        .to_str()
        .unwrap()
        .to_string();
    assert!(ct.contains("text/event-stream"), "content-type: {ct}");
    let body = r.text();
    assert!(body.contains("event: endpoint"), "body: {body}");
    assert!(body.contains("/mcp?sessionId=http-srv"), "body: {body}");
}
