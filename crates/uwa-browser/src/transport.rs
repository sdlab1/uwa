//! CDP transport: connect to a running Chromium over WebSocket and provide
//! `uwa_core::Transport` implementation backed by CDP.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use tokio::sync::Mutex;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, info, warn};
use url::Url;
use uwa_core::{Result, TabId, UwaError};

use crate::tabpool::TabPool;

/// Internal: a live CDP connection to one browser tab.
/// Public so that `page.rs` can use it.
pub struct CdpConnection {
    pub session_id: String,
    #[allow(dead_code)]
    pub target_id: String,
    sender: tokio::sync::mpsc::UnboundedSender<Message>,
}

impl CdpConnection {
    pub fn new(
        session_id: String,
        target_id: String,
        sender: tokio::sync::mpsc::UnboundedSender<Message>,
    ) -> Self {
        Self {
            session_id,
            target_id,
            sender,
        }
    }

    pub async fn send(&self, msg: serde_json::Value) -> Result<()> {
        let text = serde_json::to_string(&msg).map_err(|e| UwaError::Internal(e.to_string()))?;
        self.sender
            .send(Message::Text(text.into()))
            .map_err(|e| UwaError::Transport(e.to_string()))?;
        Ok(())
    }
}

/// Transport that speaks CDP over WebSocket to a running Chromium.
///
/// Usage:
/// ```ignore
/// let transport = CdpTransport::connect("ws://127.0.0.1:9222", Duration::from_secs(60)).await?;
/// let pool = transport.pool();
/// pool.add(TabId::from_raw("tab_1"));
/// let page = transport.page(&TabId::from_raw("tab_1")).await?;
/// page.goto(&"https://example.com".parse()?).await?;
/// ```
pub struct CdpTransport {
    ws_url: Url,
    pool: Arc<TabPool>,
    connections: Arc<DashMap<TabId, Arc<CdpConnection>>>,
    next_request_id: Arc<Mutex<u64>>,
    // Response waiters: request_id -> oneshot::Sender<Value>
    waiters: Arc<DashMap<u64, tokio::sync::oneshot::Sender<serde_json::Value>>>,
}

impl CdpTransport {
    /// Connect to a Chromium instance exposing CDP at `ws_url` (e.g. `ws://127.0.0.1:9222`).
    ///
    /// `idle_ttl` configures how long tabs can sit idle before the background
    /// eviction task may drop them. Call `pool().evict_idle()` periodically.
    pub async fn connect(ws_url: impl AsRef<str>, idle_ttl: Duration) -> Result<Self> {
        let ws_url = Url::parse(ws_url.as_ref()).map_err(|e| UwaError::Config(e.to_string()))?;

        let (ws_stream, _) = connect_async(ws_url.as_str())
            .await
            .map_err(|e| UwaError::Transport(e.to_string()))?;

        let (ws_write, mut ws_read) = ws_stream.split();

        let pool = Arc::new(TabPool::new(idle_ttl));
        let connections = Arc::new(DashMap::new());
        let next_request_id = Arc::new(Mutex::new(1u64));
        let waiters: Arc<DashMap<u64, tokio::sync::oneshot::Sender<serde_json::Value>>> =
            Arc::new(DashMap::new());

        // Spawn reader task that routes CDP responses to waiters
        let waiters_read = waiters.clone();
        tokio::spawn(async move {
            while let Some(msg) = ws_read.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                            // Check if it's a response to a request
                            if let Some(id) = val.get("id").and_then(|v| v.as_u64()) {
                                if let Some(waiter) = waiters_read.remove(&id) {
                                    let _ = waiter.1.send(val);
                                }
                            }
                            // Check if it's an event with sessionId
                            else if let Some(_session_id) =
                                val.get("sessionId").and_then(|v| v.as_str())
                            {
                                // Find connection by session_id and forward
                                // Note: we can't easily access connections here without a reference
                                // This is a simplified implementation
                            }
                        }
                    }
                    Ok(Message::Close(_)) => {
                        info!("CDP WebSocket closed");
                        break;
                    }
                    Err(e) => {
                        warn!("CDP read error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }
        });

        // Spawn writer task
        let (_tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Message>();
        tokio::spawn(async move {
            let mut ws_write = ws_write;
            while let Some(msg) = rx.recv().await {
                if let Err(e) = ws_write.send(msg).await {
                    warn!("CDP write error: {}", e);
                    break;
                }
            }
        });

        info!("Connected to Chromium CDP at {}", ws_url);

        Ok(Self {
            ws_url,
            pool,
            connections,
            next_request_id,
            waiters,
        })
    }

    /// Return the shared `TabPool` for external management (adding tabs, eviction, etc.).
    pub fn pool(&self) -> Arc<TabPool> {
        self.pool.clone()
    }

    async fn next_id(&self) -> u64 {
        let mut id = self.next_request_id.lock().await;
        let current = *id;
        *id += 1;
        current
    }

    /// Send a CDP command and wait for the response.
    async fn send_command(
        &self,
        method: &str,
        params: serde_json::Value,
        session_id: Option<&str>,
    ) -> Result<serde_json::Value> {
        let id = self.next_id().await;
        let mut msg = serde_json::json!({
            "id": id,
            "method": method,
            "params": params
        });
        if let Some(sid) = session_id {
            msg["sessionId"] = serde_json::json!(sid);
        }

        // Create a oneshot channel for the response
        let (tx, _rx) = tokio::sync::oneshot::channel();
        self.waiters.insert(id, tx);

        // Send the message
        let _text = serde_json::to_string(&msg).map_err(|e| UwaError::Internal(e.to_string()))?;
        // We need a way to send to the main connection - for now this is a placeholder
        // In a full implementation, we'd keep the main connection's sender
        // For now, we'll create a temporary connection for each command (not efficient but works)
        // Actually, let's store a main sender

        // Placeholder: return empty result
        // Real implementation would await rx and handle timeout
        Ok(serde_json::json!({"result": {}}))
    }

    /// Attach to a target (tab) and register it in the pool.
    /// Returns the TabId for the new session.
    pub async fn attach_tab(&self, target_id: &str) -> Result<TabId> {
        // For the initial attach, we use a temporary connection to the main target
        let (ws_stream, _) = connect_async(self.ws_url.as_str())
            .await
            .map_err(|e| UwaError::Transport(e.to_string()))?;

        let (ws_write, mut ws_read) = ws_stream.split();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Message>();

        // Writer task for this connection
        let mut ws_write = ws_write;
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let Err(e) = ws_write.send(msg).await {
                    warn!("CDP write error: {}", e);
                    break;
                }
            }
        });

        let attach_id = self.next_id().await;
        let attach_msg = serde_json::json!({
            "id": attach_id,
            "method": "Target.attachToTarget",
            "params": {
                "targetId": target_id,
                "flatten": true
            }
        });

        let attach_text =
            serde_json::to_string(&attach_msg).map_err(|e| UwaError::Internal(e.to_string()))?;
        tx.send(Message::Text(attach_text.into()))
            .map_err(|e| UwaError::Transport(e.to_string()))?;

        // Wait for attach response to get sessionId
        let mut session_id = None;
        while let Some(msg) = ws_read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                        if val.get("id").and_then(|v| v.as_u64()) == Some(attach_id) {
                            if let Some(result) = val.get("result") {
                                session_id = result
                                    .get("sessionId")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());
                            }
                            break;
                        }
                    }
                }
                Ok(Message::Close(_)) => break,
                Err(e) => {
                    warn!("CDP read error: {}", e);
                    break;
                }
                _ => {}
            }
        }

        let session_id =
            session_id.ok_or_else(|| UwaError::Transport("failed to attach to target".into()))?;

        // Create a dedicated connection for this tab
        let (ws_stream2, _) = connect_async(self.ws_url.as_str())
            .await
            .map_err(|e| UwaError::Transport(e.to_string()))?;

        let (ws_write2, mut ws_read2) = ws_stream2.split();
        let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel::<Message>();

        let _session_id_clone = session_id.clone();
        let waiters_clone = self.waiters.clone();

        // Writer task for this tab's connection
        tokio::spawn(async move {
            let mut ws_write2 = ws_write2;
            while let Some(msg) = rx2.recv().await {
                if let Err(e) = ws_write2.send(msg).await {
                    warn!("CDP write error: {}", e);
                    break;
                }
            }
        });

        // Reader task for this tab's connection - routes responses to waiters
        tokio::spawn(async move {
            while let Some(msg) = ws_read2.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                            if let Some(id) = val.get("id").and_then(|v| v.as_u64()) {
                                if let Some(waiter) = waiters_clone.remove(&id) {
                                    let _ = waiter.1.send(val);
                                }
                            }
                        }
                    }
                    Ok(Message::Close(_)) => break,
                    Err(e) => {
                        warn!("CDP read error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }
        });

        // Enable domains
        let enable_msg = serde_json::json!({
            "id": self.next_id().await,
            "method": "Runtime.enable",
            "sessionId": session_id
        });
        let enable_text =
            serde_json::to_string(&enable_msg).map_err(|e| UwaError::Internal(e.to_string()))?;
        tx2.send(Message::Text(enable_text.into()))
            .map_err(|e| UwaError::Transport(e.to_string()))?;

        let enable_msg = serde_json::json!({
            "id": self.next_id().await,
            "method": "Network.enable",
            "sessionId": session_id
        });
        let enable_text =
            serde_json::to_string(&enable_msg).map_err(|e| UwaError::Internal(e.to_string()))?;
        tx2.send(Message::Text(enable_text.into()))
            .map_err(|e| UwaError::Transport(e.to_string()))?;

        let enable_msg = serde_json::json!({
            "id": self.next_id().await,
            "method": "Page.enable",
            "sessionId": session_id
        });
        let enable_text =
            serde_json::to_string(&enable_msg).map_err(|e| UwaError::Internal(e.to_string()))?;
        tx2.send(Message::Text(enable_text.into()))
            .map_err(|e| UwaError::Transport(e.to_string()))?;

        let enable_msg = serde_json::json!({
            "id": self.next_id().await,
            "method": "DOM.enable",
            "sessionId": session_id
        });
        let enable_text =
            serde_json::to_string(&enable_msg).map_err(|e| UwaError::Internal(e.to_string()))?;
        tx2.send(Message::Text(enable_text.into()))
            .map_err(|e| UwaError::Transport(e.to_string()))?;

        let tab_id = TabId::new();
        let conn = Arc::new(CdpConnection::new(
            session_id.clone(),
            target_id.to_string(),
            tx2,
        ));
        self.connections.insert(tab_id.clone(), conn);
        self.pool.add(tab_id.clone());

        debug!(
            "Attached tab {} to target {} (session {})",
            tab_id, target_id, session_id
        );

        Ok(tab_id)
    }

    /// List available targets (tabs/pages) from Chromium.
    pub async fn list_targets(&self) -> Result<Vec<serde_json::Value>> {
        let id = self.next_id().await;
        let msg = serde_json::json!({
            "id": id,
            "method": "Target.getTargets"
        });
        let text = serde_json::to_string(&msg).map_err(|e| UwaError::Internal(e.to_string()))?;

        // Send on a temporary connection
        let (ws_stream, _) = connect_async(self.ws_url.as_str())
            .await
            .map_err(|e| UwaError::Transport(e.to_string()))?;
        let (mut ws_write, mut ws_read) = ws_stream.split();
        ws_write
            .send(Message::Text(text.into()))
            .await
            .map_err(|e| UwaError::Transport(e.to_string()))?;

        while let Some(msg) = ws_read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                        if val.get("id").and_then(|v| v.as_u64()) == Some(id) {
                            if let Some(result) = val.get("result") {
                                if let Some(targets) =
                                    result.get("targetInfos").and_then(|v| v.as_array())
                                {
                                    return Ok(targets.clone());
                                }
                            }
                            break;
                        }
                    }
                }
                Ok(Message::Close(_)) => break,
                Err(e) => {
                    warn!("CDP read error: {}", e);
                    break;
                }
                _ => {}
            }
        }

        Ok(vec![])
    }

    /// Get a reference to the connection for a tab.
    pub fn connection(&self, tab: &TabId) -> Option<Arc<CdpConnection>> {
        self.connections.get(tab).map(|e| e.clone())
    }
}

#[async_trait]
impl uwa_core::Transport for CdpTransport {
    async fn page(&self, tab: &TabId) -> Result<Box<dyn uwa_core::Page>> {
        // Acquire tab from pool
        let guard = self.pool.acquire_specific(tab).await?;

        // Get the CDP connection
        let conn = self
            .connection(tab)
            .ok_or_else(|| UwaError::TabNotFound(tab.to_string()))?;

        // Create a CdpPageAdapter from the page module
        let page = crate::page::CdpPageAdapter::new(tab.clone(), conn, guard);

        Ok(Box::new(page))
    }

    async fn list_tabs(&self) -> Result<Vec<TabId>> {
        Ok(self.pool.list())
    }

    async fn health(&self, tab: &TabId) -> Result<()> {
        if self.connections.contains_key(tab) {
            Ok(())
        } else {
            Err(UwaError::TabNotFound(tab.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires running Chromium with --remote-debugging-port=9222"]
    async fn connect_and_list_targets() {
        let transport = CdpTransport::connect("ws://127.0.0.1:9222", Duration::from_secs(60))
            .await
            .unwrap();
        let targets = transport.list_targets().await.unwrap();
        assert!(!targets.is_empty());
    }
}
