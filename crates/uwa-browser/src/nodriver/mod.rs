//! Nodriver sidecar transport.
//!
//! Spawns a Python subprocess speaking line-delimited JSON-RPC, wraps it
//! as a `uwa_core::Transport`. Chrome is launched *by the sidecar* with
//! stealth flags — no `--remote-debugging-port`, no `enable-automation`.
//!
//! ## Protocol
//!
//! ```text
//! → {"id": 1, "method": "eval", "params": {"tab": "...", "js": "1+1"}}
//! ← {"id": 1, "result": {"value": 2}}
//! ← {"event": "network.response", "data": {...}}
//! ```

mod client;
mod page;

pub use client::SidecarClient;
pub use page::NodriverPage;

use crate::tabpool::TabPool;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use uwa_config::NodriverBackendCfg;
use uwa_core::{NetworkEvent, Result, TabId, Transport, UwaError};

pub struct NodriverTransport {
    client: Arc<SidecarClient>,
    pool: Arc<TabPool>,
    /// TabId → nodriver tab id (e.g. `tab_1`).
    tab_ids: Arc<Mutex<HashMap<TabId, String>>>,
    /// Broadcast channel per nodriver tab for network events.
    net_buses: Arc<Mutex<HashMap<String, tokio::sync::broadcast::Sender<NetworkEvent>>>>,
    _event_task: tokio::task::JoinHandle<()>,
}

impl NodriverTransport {
    pub async fn spawn(cfg: &NodriverBackendCfg) -> Result<Self> {
        let client = Arc::new(SidecarClient::spawn(&cfg.python, &cfg.script).await?);

        // Initialize the sidecar with the browser configuration.
        client
            .request(
                "initialize",
                serde_json::json!({
                    "headless": cfg.headless,
                    "user_data_dir": cfg.user_data_dir.as_ref().map(|p| p.to_string_lossy().to_string()),
                    "browser_path": cfg.browser_path.as_ref().map(|p| p.to_string_lossy().to_string()),
                    "extra_args": cfg.extra_args,
                }),
            )
            .await?;

        let pool = Arc::new(TabPool::new(Duration::from_secs(1800)));
        let tab_ids = Arc::new(Mutex::new(HashMap::new()));
        let net_buses: Arc<Mutex<HashMap<String, tokio::sync::broadcast::Sender<NetworkEvent>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Event pump: forward sidecar events to per-tab broadcast channels.
        let event_rx = client.take_event_receiver();
        let net_buses_for_pump = net_buses.clone();
        let event_task = tokio::spawn(async move {
            let mut rx = event_rx;
            while let Some(ev) = rx.recv().await {
                let name = ev.get("event").and_then(|v| v.as_str()).unwrap_or("");
                let data = ev.get("data").cloned().unwrap_or_default();
                let tab = data
                    .get("tab")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if tab.is_empty() {
                    continue;
                }
                let tx = {
                    let mut g = net_buses_for_pump.lock().await;
                    g.entry(tab.clone())
                        .or_insert_with(|| tokio::sync::broadcast::channel(256).0)
                        .clone()
                };
                match name {
                    "network.response" => {
                        let _ = tx.send(NetworkEvent::ResponseBody {
                            url: data
                                .get("url")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            body: String::new(),
                            mime: data
                                .get("mime")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                        });
                    }
                    "network.finished" => {
                        // The sidecar already fetched the body; emit it
                        // as a ResponseBody followed by a Finished marker.
                        let body = data.get("body").and_then(|v| v.as_str()).unwrap_or("");
                        if !body.is_empty() {
                            let _ = tx.send(NetworkEvent::ResponseBody {
                                url: String::new(),
                                body: body.to_string(),
                                mime: String::new(),
                            });
                        }
                        let _ = tx.send(NetworkEvent::Finished {
                            request_id: data
                                .get("request_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                        });
                    }
                    _ => {}
                }
            }
        });

        Ok(Self {
            client,
            pool,
            tab_ids,
            net_buses,
            _event_task: event_task,
        })
    }

    /// Open a new tab with the given URL. Used by `wiring.rs` to
    /// pre-provision one tab per provider.
    pub async fn open_tab(&self, url: &str) -> Result<TabId> {
        let r = self
            .client
            .request("tab_open", serde_json::json!({ "url": url }))
            .await?;
        let nodriver_tab = r
            .get("tab")
            .and_then(|v| v.as_str())
            .ok_or_else(|| UwaError::Transport("tab_open: no tab id in response".into()))?
            .to_string();
        let tab_id = TabId::new();
        self.tab_ids
            .lock()
            .await
            .insert(tab_id.clone(), nodriver_tab);
        self.pool.add(tab_id.clone());
        Ok(tab_id)
    }

    async fn nodriver_tab_id(&self, tab: &TabId) -> Result<String> {
        self.tab_ids
            .lock()
            .await
            .get(tab)
            .cloned()
            .ok_or_else(|| UwaError::TabNotFound(tab.to_string()))
    }

    pub async fn shutdown(&self) -> Result<()> {
        let _ = self.client.request("shutdown", serde_json::json!({})).await;
        Ok(())
    }
}

#[async_trait]
impl Transport for NodriverTransport {
    async fn page(&self, tab: &TabId) -> Result<Box<dyn uwa_core::Page>> {
        let nodriver_tab = self.nodriver_tab_id(tab).await?;
        let bus = {
            let mut g = self.net_buses.lock().await;
            g.entry(nodriver_tab.clone())
                .or_insert_with(|| tokio::sync::broadcast::channel(256).0)
                .clone()
        };
        // Ensure network events are flowing for this tab.
        let _ = self
            .client
            .request("network_enable", serde_json::json!({ "tab": nodriver_tab }))
            .await;
        Ok(Box::new(NodriverPage::new(
            self.client.clone(),
            nodriver_tab,
            bus,
        )))
    }

    async fn list_tabs(&self) -> Result<Vec<TabId>> {
        Ok(self.pool.list())
    }

    async fn health(&self, tab: &TabId) -> Result<()> {
        let _ = self.nodriver_tab_id(tab).await?;
        Ok(())
    }
}
