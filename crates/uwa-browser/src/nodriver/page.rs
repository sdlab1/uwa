//! `Page` impl for nodriver sidecar.

use crate::nodriver::SidecarClient;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use url::Url;
use uwa_core::{NetworkEvent, Page, Result, UwaError};

pub struct NodriverPage {
    client: Arc<SidecarClient>,
    tab: String,
    net_tx: broadcast::Sender<NetworkEvent>,
}

impl NodriverPage {
    pub fn new(
        client: Arc<SidecarClient>,
        tab: String,
        net_tx: broadcast::Sender<NetworkEvent>,
    ) -> Self {
        Self {
            client,
            tab,
            net_tx,
        }
    }

    fn p(&self, extra: Value) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("tab".into(), json!(self.tab));
        if let Value::Object(m) = extra {
            for (k, v) in m {
                obj.insert(k, v);
            }
        }
        Value::Object(obj)
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        self.client.request(method, params).await
    }
}

#[async_trait]
impl Page for NodriverPage {
    async fn goto(&self, url: &Url) -> Result<()> {
        self.rpc("navigate", self.p(json!({ "url": url.as_str() })))
            .await?;
        Ok(())
    }

    async fn url(&self) -> Result<Url> {
        let r = self.rpc("current_url", self.p(json!({}))).await?;
        let s = r
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("about:blank");
        s.parse()
            .map_err(|e| UwaError::Transport(format!("bad url `{s}`: {e}")))
    }

    async fn eval(&self, js: &str) -> Result<Value> {
        let r = self.rpc("eval", self.p(json!({ "js": js }))).await?;
        Ok(r.get("value").cloned().unwrap_or(Value::Null))
    }

    async fn eval_in_frame(&self, frame_id: &str, js: &str) -> Result<Value> {
        let r = self
            .rpc(
                "eval_in_frame",
                self.p(json!({ "frame_id": frame_id, "js": js })),
            )
            .await?;
        Ok(r.get("value").cloned().unwrap_or(Value::Null))
    }

    async fn frame_tree(&self) -> Result<Vec<(String, String)>> {
        let r = self.rpc("frames_list", self.p(json!({}))).await?;
        let frames = r
            .get("frames")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(frames
            .into_iter()
            .filter_map(|f| {
                let id = f.get("frame_id")?.as_str()?.to_string();
                let url = f.get("url")?.as_str()?.to_string();
                Some((id, url))
            })
            .collect())
    }

    async fn wait_for_selector(&self, selector: &str, timeout: Duration) -> Result<()> {
        self.rpc(
            "wait_for_selector",
            self.p(json!({
                "selector": selector,
                "timeout_ms": timeout.as_millis() as u64,
            })),
        )
        .await?;
        Ok(())
    }

    async fn html(&self) -> Result<String> {
        let r = self.rpc("get_content", self.p(json!({}))).await?;
        Ok(r.get("html")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    }

    async fn click(&self, selector: &str) -> Result<()> {
        self.rpc("click", self.p(json!({ "selector": selector })))
            .await?;
        Ok(())
    }

    async fn type_text(&self, selector: &str, text: &str) -> Result<()> {
        self.rpc(
            "type_text",
            self.p(json!({ "selector": selector, "text": text })),
        )
        .await?;
        Ok(())
    }

    async fn network_events(&self) -> Result<broadcast::Receiver<NetworkEvent>> {
        Ok(self.net_tx.subscribe())
    }
}
