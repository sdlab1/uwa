//! MCP server: exposes our bridge to MCP clients over stdio.

use crate::protocol::*;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use uwa_core::{Result, UwaError};

/// One handler = one namespace of tools we expose over MCP.
#[async_trait]
pub trait McpHandler: Send + Sync {
    fn namespace(&self) -> &str;
    fn tools(&self) -> Vec<McpTool>;
    async fn call(&self, tool: &str, args: Value) -> Result<CallToolResult>;
}

pub struct McpServer {
    name: String,
    handlers: Vec<Arc<dyn McpHandler>>,
}

impl McpServer {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            handlers: Vec::new(),
        }
    }
    pub fn register(mut self, h: Arc<dyn McpHandler>) -> Self {
        self.handlers.push(h);
        self
    }

    /// Run the stdio loop until stdin closes.
    pub async fn serve_stdio(self) -> Result<()> {
        let stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        let mut lines = BufReader::new(stdin).lines();

        while let Some(line) = lines
            .next_line()
            .await
            .map_err(|e| UwaError::Transport(e.to_string()))?
        {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let req: JsonRpcRequest = match serde_json::from_str(line) {
                Ok(r) => r,
                Err(e) => {
                    let err =
                        JsonRpcResponse::err(Value::Null, JsonRpcError::parse_error(e.to_string()));
                    Self::write(&mut stdout, &err).await?;
                    continue;
                }
            };
            let id = req.id.clone();
            let resp = match self.dispatch(req).await {
                Ok(Some(result)) => id.map(|i| JsonRpcResponse::ok(i, result)),
                Ok(None) => None, // notification
                Err(e) => Some(JsonRpcResponse::err(id.unwrap_or(Value::Null), e)),
            };
            if let Some(r) = resp {
                Self::write(&mut stdout, &r).await?;
            }
        }
        Ok(())
    }

    async fn dispatch(
        &self,
        req: JsonRpcRequest,
    ) -> std::result::Result<Option<Value>, JsonRpcError> {
        match req.method.as_str() {
            "initialize" => {
                let res = InitializeResult {
                    protocol_version: PROTOCOL_VERSION.into(),
                    capabilities: ServerCapabilities {
                        tools: Some(json!({"listChanged": false})),
                    },
                    server_info: Implementation {
                        name: self.name.clone(),
                        version: env!("CARGO_PKG_VERSION").into(),
                    },
                };
                Ok(Some(
                    serde_json::to_value(res).map_err(|e| JsonRpcError::internal(e.to_string()))?,
                ))
            }
            "notifications/initialized" => Ok(None),
            "tools/list" => {
                let tools: Vec<McpTool> = self.handlers.iter().flat_map(|h| h.tools()).collect();
                let res = ListToolsResult { tools };
                Ok(Some(
                    serde_json::to_value(res).map_err(|e| JsonRpcError::internal(e.to_string()))?,
                ))
            }
            "tools/call" => {
                let params: CallToolParams =
                    serde_json::from_value(req.params.unwrap_or(Value::Null))
                        .map_err(|e| JsonRpcError::invalid_params(e.to_string()))?;
                let (ns, tool) = split_namespaced(&params.name);
                let handler = self
                    .handlers
                    .iter()
                    .find(|h| h.namespace() == ns)
                    .ok_or_else(|| JsonRpcError::method_not_found(&params.name))?;
                match handler.call(tool, params.arguments).await {
                    Ok(r) => Ok(Some(
                        serde_json::to_value(r)
                            .map_err(|e| JsonRpcError::internal(e.to_string()))?,
                    )),
                    Err(UwaError::BadRequest(m)) => Err(JsonRpcError::invalid_params(m)),
                    Err(e) => Err(JsonRpcError::internal(e.to_string())),
                }
            }
            other => Err(JsonRpcError::method_not_found(other)),
        }
    }

    async fn write(out: &mut tokio::io::Stdout, resp: &JsonRpcResponse) -> Result<()> {
        let mut buf = serde_json::to_vec(resp).map_err(|e| UwaError::Internal(e.to_string()))?;
        buf.push(b'\n');
        out.write_all(&buf)
            .await
            .map_err(|e| UwaError::Transport(e.to_string()))?;
        out.flush()
            .await
            .map_err(|e| UwaError::Transport(e.to_string()))?;
        Ok(())
    }
}

/// Tool names over MCP are namespaced as `namespace__tool` when a server has
/// multiple handlers. A single-handler server may use bare names.
fn split_namespaced(name: &str) -> (&str, &str) {
    match name.split_once("__") {
        Some((ns, t)) => (ns, t),
        None => ("", name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaced_split() {
        assert_eq!(split_namespaced("web__chat"), ("web", "chat"));
        assert_eq!(split_namespaced("bare"), ("", "bare"));
    }
}
