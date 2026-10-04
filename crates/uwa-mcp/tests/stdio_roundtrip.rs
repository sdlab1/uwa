//! Round-trip test: spawn `uwa-mcp` echo server as a child and talk to it.
//!
//! The child is our own `examples/echo_server.rs` binary.

use serde_json::json;
use uwa_mcp::{McpClient, StdioClient};

#[tokio::test]
async fn client_talks_to_echo_server() {
    // Build path to example binary produced by cargo.
    let bin = env!("CARGO_BIN_EXE_uwa-mcp-echo");
    let client = StdioClient::spawn("echo", bin, &[]).await.unwrap();
    client.initialize().await.unwrap();
    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");
    let r = client
        .call_tool("echo", json!({"text": "hi"}))
        .await
        .unwrap();
    assert!(!r.is_error);
    assert!(r.as_text().contains("hi"));
    client.shutdown().await.unwrap();
}
