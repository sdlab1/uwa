//! Built-in MCP handlers exposing the bridge itself.

use crate::protocol::{CallToolResult, McpContent, McpTool};
use crate::server::McpHandler;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use uwa_core::{Result, SiteProvider, Transport, UwaError};

/// Type alias for the provider lookup function type.
pub type ProviderLookupFn = Arc<dyn Fn(&str) -> Option<Arc<dyn SiteProvider>> + Send + Sync>;

/// `web_chat(provider, message)` — one-shot ask to a web-UI LLM.
pub struct WebChatHandler {
    providers: ProviderLookupFn,
    transport: Arc<dyn Transport>,
}

impl WebChatHandler {
    pub fn new(providers: ProviderLookupFn, transport: Arc<dyn Transport>) -> Self {
        Self {
            providers,
            transport,
        }
    }
}

#[async_trait]
impl McpHandler for WebChatHandler {
    fn namespace(&self) -> &str {
        "web"
    }

    fn tools(&self) -> Vec<McpTool> {
        vec![McpTool {
            name: "web__chat".into(),
            description: "Send a message to a logged-in web LLM (chatgpt/claude/gemini/deepseek) and return its reply.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "provider": {"type": "string", "description": "e.g. chatgpt, claude"},
                    "message":  {"type": "string"}
                },
                "required": ["provider", "message"]
            }),
        }]
    }

    async fn call(&self, tool: &str, args: Value) -> Result<CallToolResult> {
        if tool != "chat" {
            return Err(UwaError::BadRequest(format!("unknown tool `web__{tool}`")));
        }
        let provider_name = args
            .get("provider")
            .and_then(Value::as_str)
            .ok_or_else(|| UwaError::BadRequest("missing `provider`".into()))?;
        let message = args
            .get("message")
            .and_then(Value::as_str)
            .ok_or_else(|| UwaError::BadRequest("missing `message`".into()))?;

        let site = (self.providers)(provider_name)
            .ok_or_else(|| UwaError::UnknownModel(provider_name.into()))?;
        let tab = self
            .transport
            .list_tabs()
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| UwaError::Unavailable("no tabs".into()))?;
        let page = self.transport.page(&tab).await?;
        site.send_message(page.as_ref(), message).await?;
        let answer = site.wait_response(page.as_ref()).await?;
        Ok(CallToolResult {
            content: vec![McpContent::Text { text: answer }],
            is_error: false,
        })
    }
}

/// `web_tabs_list()` — health/status.
pub struct WebTabsHandler {
    transport: Arc<dyn Transport>,
}

impl WebTabsHandler {
    pub fn new(transport: Arc<dyn Transport>) -> Self {
        Self { transport }
    }
}

#[async_trait]
impl McpHandler for WebTabsHandler {
    fn namespace(&self) -> &str {
        "web"
    }

    fn tools(&self) -> Vec<McpTool> {
        vec![McpTool {
            name: "web__list_tabs".into(),
            description: "List currently connected browser tabs.".into(),
            input_schema: json!({"type": "object", "properties": {}}),
        }]
    }

    async fn call(&self, tool: &str, _args: Value) -> Result<CallToolResult> {
        if tool != "list_tabs" {
            return Err(UwaError::BadRequest(format!("unknown tool `web__{tool}`")));
        }
        let tabs = self.transport.list_tabs().await?;
        let text = serde_json::to_string(&tabs.iter().map(|t| t.as_str()).collect::<Vec<_>>())
            .unwrap_or_else(|_| "[]".into());
        Ok(CallToolResult::text(text))
    }
}
