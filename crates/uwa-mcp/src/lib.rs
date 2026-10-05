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
//! - [`handlers::{DispatcherFn, WebChatHandler, WebTabsHandler, WebPromptHandler}`]

pub mod bridge;
pub mod client;
pub mod handlers;
pub mod protocol;
pub mod router;
pub mod server;

#[cfg(feature = "mcp-http")]
pub mod http;

pub use bridge::McpClientProvider;
pub use client::{McpClient, StdioClient};
pub use handlers::{
    DispatcherFn, ProvidersFn, WebChatHandler, WebPromptHandler, WebTabsHandler, TABS_URI,
};
pub use protocol::{
    CallToolResult, GetPromptParams, GetPromptResult, InitializeParams, InitializeResult,
    JsonRpcError, JsonRpcRequest, JsonRpcResponse, ListPromptsResult, ListResourcesResult,
    ListToolsResult, McpContent, McpPrompt, McpPromptArgument, McpResource, McpTool, PromptMessage,
    ReadResourceParams, ReadResourceResult, ResourceContents, ServerCapabilities, PROTOCOL_VERSION,
};
pub use router::ToolRouter;
pub use server::{McpHandler, McpServer};
