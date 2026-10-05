//! Minimal MCP server exposing one tool: `echo`.
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
            description: "Echoes text back.".into(),
            input_schema: json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}),
        }]
    }
    async fn call(&self, tool: &str, args: Value) -> Result<CallToolResult> {
        if tool != "echo" {
            return Err(UwaError::BadRequest("unknown".into()));
        }
        let t = args.get("text").and_then(Value::as_str).unwrap_or("");
        Ok(CallToolResult::text(t))
    }
}

#[tokio::main]
async fn main() {
    Arc::new(McpServer::new("uwa-mcp-echo").register(Arc::new(Echo)))
        .serve_stdio()
        .await
        .unwrap();
}
