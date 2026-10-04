//! MCP client: talks to an external MCP server over stdio (and later, HTTP+SSE).

use crate::protocol::*;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, Mutex};
use uwa_core::{Result, UwaError};

/// A connected, initialized MCP client. All methods are &self and cheap.
#[async_trait]
pub trait McpClient: Send + Sync {
    fn server_name(&self) -> &str;
    async fn list_tools(&self) -> Result<Vec<McpTool>>;
    async fn call_tool(&self, name: &str, args: Value) -> Result<CallToolResult>;
    async fn shutdown(&self) -> Result<()>;
}

/// Stdio-based MCP client. Speaks line-delimited JSON-RPC on stdin/stdout.
pub struct StdioClient {
    name: String,
    child: Mutex<Child>,
    stdin: Mutex<tokio::process::ChildStdin>,
    next_id: AtomicI64,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<JsonRpcResponse>>>>,
    initialized: AtomicI64, // 0/1 flag
}

impl StdioClient {
    /// Spawn the child and return an *uninitialized* client. Call `.initialize()` next.
    pub async fn spawn(name: impl Into<String>, cmd: &str, args: &[String]) -> Result<Self> {
        let mut child = Command::new(cmd)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| UwaError::Transport(format!("spawn {cmd}: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| UwaError::Transport("no child stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| UwaError::Transport("no child stdout".into()))?;

        let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<JsonRpcResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let reader_pending = pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                // Server may send notifications (no id) — we currently drop them.
                let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(line) else {
                    continue;
                };
                let Some(id) = resp.id.as_i64() else { continue };
                if let Some(tx) = reader_pending.lock().await.remove(&id) {
                    let _ = tx.send(resp);
                }
            }
        });

        Ok(Self {
            name: name.into(),
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            next_id: AtomicI64::new(1),
            pending,
            initialized: AtomicI64::new(0),
        })
    }

    pub async fn initialize(&self) -> Result<InitializeResult> {
        let params = InitializeParams {
            protocol_version: PROTOCOL_VERSION.into(),
            capabilities: ClientCapabilities::default(),
            client_info: Implementation {
                name: "uwa".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        };
        let v = self
            .request("initialize", Some(serde_json::to_value(params).unwrap()))
            .await?;
        let res: InitializeResult = serde_json::from_value(v)
            .map_err(|e| UwaError::Transport(format!("bad InitializeResult: {e}")))?;
        self.initialized.store(1, Ordering::SeqCst);
        // Fire-and-forget notification per spec.
        let notif = JsonRpcRequest::notification("notifications/initialized", None);
        self.write(&notif).await?;
        Ok(res)
    }

    async fn request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = JsonRpcRequest::new(id, method, params);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        self.write(&req).await?;
        let resp = tokio::time::timeout(std::time::Duration::from_secs(30), rx)
            .await
            .map_err(|_| UwaError::Timeout(std::time::Duration::from_secs(30)))?
            .map_err(|_| UwaError::Transport("client closed".into()))?;
        if let Some(err) = resp.error {
            return Err(UwaError::Transport(format!(
                "mcp error {}: {}",
                err.code, err.message
            )));
        }
        resp.result
            .ok_or_else(|| UwaError::Transport("no result and no error".into()))
    }

    async fn write(&self, req: &JsonRpcRequest) -> Result<()> {
        let mut line = serde_json::to_vec(req)
            .map_err(|e| UwaError::Internal(format!("serialize rpc: {e}")))?;
        line.push(b'\n');
        let mut g = self.stdin.lock().await;
        g.write_all(&line)
            .await
            .map_err(|e| UwaError::Transport(format!("write stdin: {e}")))?;
        g.flush()
            .await
            .map_err(|e| UwaError::Transport(format!("flush stdin: {e}")))?;
        Ok(())
    }
}

#[async_trait]
impl McpClient for StdioClient {
    fn server_name(&self) -> &str {
        &self.name
    }

    async fn list_tools(&self) -> Result<Vec<McpTool>> {
        let v = self.request("tools/list", Some(json!({}))).await?;
        let r: ListToolsResult = serde_json::from_value(v)
            .map_err(|e| UwaError::Transport(format!("bad ListToolsResult: {e}")))?;
        Ok(r.tools)
    }

    async fn call_tool(&self, name: &str, args: Value) -> Result<CallToolResult> {
        let params = CallToolParams {
            name: name.into(),
            arguments: args,
        };
        let v = self
            .request("tools/call", Some(serde_json::to_value(params).unwrap()))
            .await?;
        let r: CallToolResult = serde_json::from_value(v)
            .map_err(|e| UwaError::Transport(format!("bad CallToolResult: {e}")))?;
        Ok(r)
    }

    async fn shutdown(&self) -> Result<()> {
        // Best-effort: send EOF by dropping stdin; kill after grace.
        let _ = self.child.lock().await.kill().await;
        Ok(())
    }
}
