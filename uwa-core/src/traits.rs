//! Cross-crate contracts. Everything that touches the browser or a
//! provider MUST go through these traits so we can unit-test with mocks.

use crate::error::Result;
use crate::ids::TabId;
use async_trait::async_trait;
use serde_json::Value;
use url::Url;

/// A logical browser page we can drive. Abstracts over `chaser-oxide`.
#[async_trait]
pub trait Page: Send + Sync {
    async fn goto(&self, url: &Url) -> Result<()>;
    async fn url(&self) -> Result<Url>;
    /// Evaluate JS in the page and return the JSON-decoded result.
    async fn eval(&self, js: &str) -> Result<Value>;
    /// Wait for a CSS selector to appear; returns Err(Timeout) if not.
    async fn wait_for_selector(&self, selector: &str, timeout: std::time::Duration) -> Result<()>;
    /// Return the current HTML of the page.
    async fn html(&self) -> Result<String>;
    /// Click an element by CSS selector.
    async fn click(&self, selector: &str) -> Result<()>;
    /// Type text into an input matched by CSS selector.
    async fn type_text(&self, selector: &str, text: &str) -> Result<()>;
    /// Subscribe to CDP network events (SSE/JSON bodies).
    async fn network_events(&self) -> Result<tokio::sync::broadcast::Receiver<NetworkEvent>>;
    /// Register a script that runs on every navigation *before* page JS.
    /// Default: no-op (test mocks don't need stealth).
    async fn eval_early(&self, _js: &str) -> Result<()> {
        Ok(())
    }
}

/// A network event relevant to response extraction.
#[derive(Debug, Clone)]
pub enum NetworkEvent {
    ResponseBody {
        url: String,
        body: String,
        mime: String,
    },
    Finished {
        request_id: String,
    },
}

/// Site adapter: everything provider-specific lives behind this trait.
#[async_trait]
pub trait SiteProvider: Send + Sync {
    /// Stable identifier, e.g. `"chatgpt"`.
    fn name(&self) -> &str;

    /// Whether this provider can handle the given page URL.
    fn matches(&self, url: &Url) -> bool;

    /// Send the user's message into the currently-open conversation.
    async fn send_message(&self, page: &dyn Page, text: &str) -> Result<()>;

    /// Block until the assistant's answer for the last message is complete.
    /// Returns the extracted, normalized text.
    async fn wait_response(&self, page: &dyn Page) -> Result<String>;

    /// Best-effort cancellation (click "Stop").
    async fn cancel(&self, page: &dyn Page) -> Result<()>;

    /// Capabilities advertised via `/v1/models`.
    fn capabilities(&self) -> Capabilities;
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Capabilities {
    pub streams: bool,
    pub tool_calls: bool,
    pub vision: bool,
    pub max_context_tokens: Option<u32>,
}

/// Factory that produces `Page`s from a `TabId` (implemented by uwa-browser).
#[async_trait]
pub trait Transport: Send + Sync {
    async fn page(&self, tab: &TabId) -> Result<Box<dyn Page>>;
    async fn list_tabs(&self) -> Result<Vec<TabId>>;
    async fn health(&self, tab: &TabId) -> Result<()>;
}

/// One tool definition, decoupled from MCP/OpenAI formats.
/// `uwa-tools` has the OpenAI <-> this converters; `uwa-mcp` has MCP <-> this.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for arguments.
    pub parameters: Value,
}

/// Something that can list and execute tools. Implemented by:
/// - `uwa-tools::LocalToolProvider` (inline tools from the chat request)
/// - `uwa-mcp::McpClientProvider` (external MCP servers)
#[async_trait]
pub trait ToolProvider: Send + Sync {
    /// Namespace used to prefix tools when multiple providers are registered.
    /// `""` means "no namespace".
    fn namespace(&self) -> &str;
    async fn list_tools(&self) -> Result<Vec<ToolSpec>>;
    /// Execute a tool by name (already de-namespaced by the router).
    /// Returns the text content of the result (concatenated).
    async fn call_tool(&self, name: &str, args: Value) -> Result<String>;
}
