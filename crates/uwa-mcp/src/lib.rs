//! # uwa-mcp
//!
//! MCP integration in both directions:
//!
//! **Server** — expose our bridge as MCP tools to Claude Desktop / Cursor.
//! **Client** — consume external MCP servers and offer their tools to the
//!              web-UI LLM via `uwa-tools`' `<tool_call>` protocol.
//!
//! ## Passport (public API)
//! - [`protocol`] — wire types
//! - [`client::{McpClient, StdioClient}`]
//! - [`server::{McpServer, McpHandler}`]
//! - [`bridge::McpClientProvider`]
//! - [`router::ToolRouter`]
//! - [`handlers::{WebChatHandler, WebTabsHandler}`]

pub mod bridge;
pub mod client;
pub mod handlers;
pub mod protocol;
pub mod router;
pub mod server;

pub use bridge::McpClientProvider;
pub use client::{McpClient, StdioClient};
pub use handlers::{ProviderLookupFn, WebChatHandler, WebTabsHandler};
pub use protocol::{
    CallToolResult, InitializeParams, InitializeResult, JsonRpcError, JsonRpcRequest,
    JsonRpcResponse, McpContent, McpTool, PROTOCOL_VERSION,
};
pub use router::ToolRouter;
pub use server::{McpHandler, McpServer};
