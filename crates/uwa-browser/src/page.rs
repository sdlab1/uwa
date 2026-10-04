//! CDP page adapter: implements `uwa_core::Page` over a CDP connection.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::broadcast;
use url::Url;
use uwa_core::{NetworkEvent, Result, TabId, UwaError};

use crate::tabpool::TabGuard;
use crate::transport::CdpConnection;

/// Page implementation backed by a CDP connection.
///
/// Created via `CdpTransport::page(&tab_id)` and returned as `Box<dyn Page>`.
pub struct CdpPageAdapter {
    conn: Arc<CdpConnection>,
    _guard: TabGuard,
    next_request_id: Arc<std::sync::atomic::AtomicU64>,
    event_tx: broadcast::Sender<NetworkEvent>,
}

impl CdpPageAdapter {
    /// Create a new page adapter for the given tab.
    pub fn new(_tab_id: TabId, conn: Arc<CdpConnection>, guard: TabGuard) -> Self {
        let (event_tx, _event_rx) = broadcast::channel(16);
        Self {
            conn,
            _guard: guard,
            next_request_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            event_tx,
        }
    }

    fn next_id(&self) -> u64 {
        self.next_request_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    async fn send_command(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id();
        let msg = serde_json::json!({
            "id": id,
            "method": method,
            "params": params,
            "sessionId": self.conn.session_id
        });

        // Send the message directly
        self.conn.send(msg).await?;

        // Placeholder - in reality we'd await rx with a timeout
        Ok(serde_json::json!({"result": {}}))
    }

    async fn send_command_with_response(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id();
        let msg = serde_json::json!({
            "id": id,
            "method": method,
            "params": params,
            "sessionId": self.conn.session_id
        });

        self.conn.send(msg).await?;

        // Placeholder for response handling
        Ok(serde_json::json!({"result": {}}))
    }

    async fn eval_js(&self, expression: &str, return_by_value: bool) -> Result<Value> {
        let params = serde_json::json!({
            "expression": expression,
            "returnByValue": return_by_value,
            "awaitPromise": true,
            "userGesture": true
        });
        self.send_command_with_response("Runtime.evaluate", params)
            .await
    }
}

#[async_trait]
impl uwa_core::Page for CdpPageAdapter {
    async fn goto(&self, url: &Url) -> Result<()> {
        let params = serde_json::json!({
            "url": url.as_str()
        });
        self.send_command_with_response("Page.navigate", params)
            .await?;

        // Wait for load event - simplified polling approach
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(30) {
            let ready = self
                .eval_js("document.readyState === 'complete'", true)
                .await?;
            if ready
                .get("result")
                .and_then(|v| v.get("result"))
                .and_then(|v| v.get("value"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        Err(UwaError::Timeout(Duration::from_secs(30)))
    }

    async fn url(&self) -> Result<Url> {
        let result = self.eval_js("window.location.href", true).await?;
        let url_str = result
            .get("result")
            .and_then(|v| v.get("result"))
            .and_then(|v| v.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("about:blank");
        Url::parse(url_str).map_err(|e| UwaError::Internal(e.to_string()))
    }

    async fn eval(&self, js: &str) -> Result<Value> {
        let result = self.eval_js(js, true).await?;
        let value = result
            .get("result")
            .and_then(|v| v.get("result"))
            .and_then(|v| v.get("value"))
            .cloned()
            .unwrap_or(Value::Null);
        Ok(value)
    }

    async fn wait_for_selector(&self, selector: &str, timeout: Duration) -> Result<()> {
        let script = format!(
            r#"
            (async () => {{
                const selector = {selector:?};
                const element = document.querySelector(selector);
                if (element) return true;
                return new Promise((resolve, reject) => {{
                    const observer = new MutationObserver(() => {{
                        if (document.querySelector(selector)) {{
                            observer.disconnect();
                            resolve(true);
                        }}
                    }});
                    observer.observe(document.body, {{ childList: true, subtree: true }});
                    setTimeout(() => {{
                        observer.disconnect();
                        reject(new Error('Timeout waiting for selector'));
                    }}, {timeout_ms});
                }});
            }})()
            "#,
            selector = selector,
            timeout_ms = timeout.as_millis()
        );

        let result = self.eval_js(&script, true).await?;
        let success = result
            .get("result")
            .and_then(|v| v.get("result"))
            .and_then(|v| v.get("value"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !success {
            return Err(UwaError::Timeout(timeout));
        }
        Ok(())
    }

    async fn html(&self) -> Result<String> {
        let result = self
            .eval_js("document.documentElement.outerHTML", true)
            .await?;
        let html = result
            .get("result")
            .and_then(|v| v.get("result"))
            .and_then(|v| v.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        Ok(html.to_string())
    }

    async fn click(&self, selector: &str) -> Result<()> {
        let script = format!(
            r#"
            (async () => {{
                const selector = {selector:?};
                const element = document.querySelector(selector);
                if (!element) throw new Error('Element not found: ' + selector);
                element.scrollIntoView({{behavior: 'auto', block: 'center'}});
                await new Promise(r => setTimeout(r, 50));
                element.click();
                return true;
            }})()
            "#,
            selector = selector
        );
        self.eval_js(&script, true).await?;
        Ok(())
    }

    async fn type_text(&self, selector: &str, text: &str) -> Result<()> {
        let script = format!(
            r#"
            (async () => {{
                const selector = {selector:?};
                const element = document.querySelector(selector);
                if (!element) throw new Error('Element not found: ' + selector);
                element.focus();
                element.value = {text:?};
                element.dispatchEvent(new Event('input', {{ bubbles: true }}));
                element.dispatchEvent(new Event('change', {{ bubbles: true }}));
                return true;
            }})()
            "#,
            selector = selector,
            text = text
        );
        self.eval_js(&script, true).await?;
        Ok(())
    }

    async fn network_events(&self) -> Result<broadcast::Receiver<NetworkEvent>> {
        // Enable network domain if not already
        let _ = self
            .send_command("Network.enable", serde_json::json!({}))
            .await;

        // Return a receiver that will get network events
        // In a full implementation, we'd have a background task that listens
        // for Network.responseReceived and Network.loadingFinished and sends
        // NetworkEvent::ResponseBody / Finished to the broadcast channel.
        Ok(self.event_tx.subscribe())
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    #[ignore = "requires running Chromium with --remote-debugging-port=9222"]
    async fn cdp_page_adapter_basic() {
        // Integration test placeholder
    }
}
