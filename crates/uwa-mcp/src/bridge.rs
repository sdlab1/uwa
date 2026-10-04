//! Wraps an `McpClient` as a `uwa_core::ToolProvider` so the chat pipeline can
//! consume external MCP tools without knowing MCP exists.

use crate::client::McpClient;
use crate::protocol::McpTool;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use uwa_core::traits::{ToolProvider, ToolSpec};
use uwa_core::{Result, UwaError};

pub struct McpClientProvider {
    client: Arc<dyn McpClient>,
    namespace: String,
}

impl McpClientProvider {
    pub fn new(client: Arc<dyn McpClient>) -> Self {
        let namespace = client.server_name().to_string();
        Self { client, namespace }
    }
}

#[async_trait]
impl ToolProvider for McpClientProvider {
    fn namespace(&self) -> &str {
        &self.namespace
    }

    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        let tools: Vec<McpTool> = self.client.list_tools().await?;
        Ok(tools
            .into_iter()
            .map(|t| ToolSpec {
                name: t.name,
                description: t.description,
                parameters: t.input_schema,
            })
            .collect())
    }

    async fn call_tool(&self, name: &str, args: Value) -> Result<String> {
        let r = self.client.call_tool(name, args).await?;
        if r.is_error {
            return Err(UwaError::Extraction(format!(
                "MCP tool `{name}` returned error: {}",
                r.as_text()
            )));
        }
        Ok(r.as_text())
    }
}
