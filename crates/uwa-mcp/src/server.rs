//! MCP server: exposes our bridge to MCP clients over stdio.

use crate::protocol::*;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use uwa_core::{Result, UwaError};

/// One handler = one namespace of tools we expose over MCP.
///
/// Everything except `namespace` has a default so a handler only implements
/// what it actually serves.
#[async_trait]
pub trait McpHandler: Send + Sync {
    fn namespace(&self) -> &str;
    fn tools(&self) -> Vec<McpTool> {
        vec![]
    }
    async fn call(&self, tool: &str, args: Value) -> Result<CallToolResult> {
        let _ = (tool, args);
        Err(UwaError::BadRequest("tools not implemented".into()))
    }
    fn resources(&self) -> Vec<McpResource> {
        vec![]
    }
    async fn read_resource(&self, uri: &str) -> Result<ReadResourceResult> {
        Err(UwaError::BadRequest(format!("resource `{uri}` not found")))
    }
    fn prompts(&self) -> Vec<McpPrompt> {
        vec![]
    }
    async fn get_prompt(&self, name: &str, args: Value) -> Result<GetPromptResult> {
        let _ = (name, args);
        Err(UwaError::BadRequest("prompt not found".into()))
    }
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

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The capabilities we actually serve, derived from the registered
    /// handlers: a server with no resources must not advertise them.
    pub fn server_capabilities(&self) -> ServerCapabilities {
        let has_tools = self.handlers.iter().any(|h| !h.tools().is_empty());
        let has_resources = self.handlers.iter().any(|h| !h.resources().is_empty());
        let has_prompts = self.handlers.iter().any(|h| !h.prompts().is_empty());
        ServerCapabilities {
            tools: has_tools.then(|| json!({"listChanged": false})),
            resources: has_resources.then(|| json!({"subscribe": false, "listChanged": false})),
            prompts: has_prompts.then(|| json!({"listChanged": false})),
        }
    }

    /// Answer a single JSON-RPC message. Notifications get an `ok(null)` for
    /// transports that must always answer; the stdio loop filters them out.
    pub async fn dispatch_public(&self, req: JsonRpcRequest) -> JsonRpcResponse {
        let id = req.id.clone().unwrap_or(Value::Null);
        match self.dispatch(req).await {
            Ok(Some(v)) => JsonRpcResponse::ok(id, v),
            Ok(None) => JsonRpcResponse::ok(id, Value::Null),
            Err(e) => JsonRpcResponse::err(id, e),
        }
    }

    /// Run the stdio loop until stdin closes. `Arc<Self>` because transports
    /// (stdio here, HTTP in `http.rs`) share one server instance.
    pub async fn serve_stdio(self: Arc<Self>) -> Result<()> {
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
                    capabilities: self.server_capabilities(),
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
            "resources/list" => {
                let resources: Vec<McpResource> =
                    self.handlers.iter().flat_map(|h| h.resources()).collect();
                Ok(Some(
                    serde_json::to_value(ListResourcesResult { resources })
                        .map_err(|e| JsonRpcError::internal(e.to_string()))?,
                ))
            }
            "resources/read" => {
                let params: ReadResourceParams =
                    serde_json::from_value(req.params.unwrap_or(Value::Null))
                        .map_err(|e| JsonRpcError::invalid_params(e.to_string()))?;
                let handler = self
                    .handlers
                    .iter()
                    .find(|h| h.resources().iter().any(|r| r.uri == params.uri))
                    .ok_or_else(|| {
                        JsonRpcError::invalid_params(format!("resource `{}` not found", params.uri))
                    })?;
                match handler.read_resource(&params.uri).await {
                    Ok(r) => Ok(Some(
                        serde_json::to_value(r)
                            .map_err(|e| JsonRpcError::internal(e.to_string()))?,
                    )),
                    Err(UwaError::BadRequest(m)) => Err(JsonRpcError::invalid_params(m)),
                    Err(e) => Err(JsonRpcError::internal(e.to_string())),
                }
            }
            "prompts/list" => {
                let prompts: Vec<McpPrompt> =
                    self.handlers.iter().flat_map(|h| h.prompts()).collect();
                Ok(Some(
                    serde_json::to_value(ListPromptsResult { prompts })
                        .map_err(|e| JsonRpcError::internal(e.to_string()))?,
                ))
            }
            "prompts/get" => {
                let params: GetPromptParams =
                    serde_json::from_value(req.params.unwrap_or(Value::Null))
                        .map_err(|e| JsonRpcError::invalid_params(e.to_string()))?;
                let handler = self
                    .handlers
                    .iter()
                    .find(|h| h.prompts().iter().any(|p| p.name == params.name))
                    .ok_or_else(|| {
                        JsonRpcError::invalid_params(format!("prompt `{}` not found", params.name))
                    })?;
                match handler.get_prompt(&params.name, params.arguments).await {
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

#[cfg(test)]
mod dispatch_tests {
    use super::*;

    /// Serves one tool, one resource and one prompt.
    struct Full;

    #[async_trait]
    impl McpHandler for Full {
        fn namespace(&self) -> &str {
            "full"
        }
        fn tools(&self) -> Vec<McpTool> {
            vec![McpTool {
                name: "full__echo".into(),
                description: "echo".into(),
                input_schema: json!({"type": "object"}),
            }]
        }
        async fn call(&self, _tool: &str, args: Value) -> Result<CallToolResult> {
            Ok(CallToolResult::text(format!("got {args}")))
        }
        fn resources(&self) -> Vec<McpResource> {
            vec![McpResource {
                uri: "uwa://x".into(),
                name: "x".into(),
                description: None,
                mime_type: Some("text/plain".into()),
            }]
        }
        async fn read_resource(&self, uri: &str) -> Result<ReadResourceResult> {
            Ok(ReadResourceResult {
                contents: vec![ResourceContents {
                    uri: uri.into(),
                    mime_type: None,
                    text: "body".into(),
                }],
            })
        }
        fn prompts(&self) -> Vec<McpPrompt> {
            vec![McpPrompt {
                name: "ask".into(),
                description: None,
                arguments: vec![McpPromptArgument {
                    name: "question".into(),
                    description: None,
                    required: Some(true),
                }],
            }]
        }
        async fn get_prompt(&self, _name: &str, args: Value) -> Result<GetPromptResult> {
            Ok(GetPromptResult {
                description: None,
                messages: vec![PromptMessage {
                    role: "user".into(),
                    content: McpContent::Text {
                        text: args
                            .get("question")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .into(),
                    },
                }],
            })
        }
    }

    /// Serves no tools at all — used to prove capabilities are derived.
    struct Silent;

    #[async_trait]
    impl McpHandler for Silent {
        fn namespace(&self) -> &str {
            "silent"
        }
    }

    async fn call(server: &McpServer, id: u32, method: &str, params: Value) -> JsonRpcResponse {
        server
            .dispatch_public(JsonRpcRequest::new(id, method, Some(params)))
            .await
    }

    #[tokio::test]
    async fn initialize_advertises_only_what_handlers_serve() {
        let full = McpServer::new("full").register(Arc::new(Full));
        let caps = full.server_capabilities();
        assert!(caps.tools.is_some());
        assert!(caps.resources.is_some());
        assert!(caps.prompts.is_some());

        let silent = McpServer::new("silent").register(Arc::new(Silent));
        let caps = silent.server_capabilities();
        assert_eq!(caps.tools, None);
        assert_eq!(caps.resources, None);
        assert_eq!(caps.prompts, None);

        let resp = call(&full, 1, "initialize", json!({})).await;
        let caps = resp.result.unwrap()["capabilities"].clone();
        assert_eq!(caps["tools"]["listChanged"], false);
        assert_eq!(caps["resources"]["subscribe"], false);
        assert_eq!(caps["prompts"]["listChanged"], false);
    }

    #[tokio::test]
    async fn resources_and_prompts_round_trip() {
        let server = Arc::new(McpServer::new("srv").register(Arc::new(Full)));

        let list = server
            .dispatch_public(JsonRpcRequest::new(1, "resources/list", Some(json!({}))))
            .await;
        assert_eq!(list.result.unwrap()["resources"][0]["uri"], "uwa://x");

        let read = server
            .dispatch_public(JsonRpcRequest::new(
                2,
                "resources/read",
                Some(json!({"uri": "uwa://x"})),
            ))
            .await;
        assert_eq!(read.result.unwrap()["contents"][0]["text"], "body");

        let missing = server
            .dispatch_public(JsonRpcRequest::new(
                3,
                "resources/read",
                Some(json!({"uri": "uwa://nope"})),
            ))
            .await;
        assert_eq!(missing.error.unwrap().code, -32602);

        let prompts = server
            .dispatch_public(JsonRpcRequest::new(4, "prompts/list", Some(json!({}))))
            .await;
        assert_eq!(prompts.result.unwrap()["prompts"][0]["name"], "ask");

        let get = server
            .dispatch_public(JsonRpcRequest::new(
                5,
                "prompts/get",
                Some(json!({"name": "ask", "arguments": {"question": "why"}})),
            ))
            .await;
        assert_eq!(get.result.unwrap()["messages"][0]["content"]["text"], "why");
    }

    #[tokio::test]
    async fn dispatch_public_normalizes_errors_and_notifications() {
        let server = Arc::new(McpServer::new("srv").register(Arc::new(Full)));

        let unknown = server
            .dispatch_public(JsonRpcRequest::new(7, "no/such", None))
            .await;
        assert_eq!(unknown.id, json!(7));
        assert_eq!(unknown.error.unwrap().code, -32601);

        let bad_params = server
            .dispatch_public(JsonRpcRequest::new(8, "resources/read", Some(json!({}))))
            .await;
        assert_eq!(bad_params.error.unwrap().code, -32602);

        let note = server
            .dispatch_public(JsonRpcRequest::notification(
                "notifications/initialized",
                None,
            ))
            .await;
        assert_eq!(note.id, Value::Null);
        assert_eq!(note.result, Some(Value::Null));
    }
}
