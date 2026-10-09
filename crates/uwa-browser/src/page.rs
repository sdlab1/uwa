//! `uwa_core::Page` implementation over a `chromiumoxide::Page`.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use chromiumoxide::cdp::browser_protocol::page::{
    AddScriptToEvaluateOnNewDocumentParams, NavigateParams,
};
use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
use chromiumoxide::js::Evaluation;
use chromiumoxide::Page as CdpPage;
use serde_json::Value;
use tokio::sync::broadcast;
use url::Url;
use uwa_core::{NetworkEvent, Result, UwaError};

use crate::bus::NetBus;
use crate::tabpool::TabGuard;

/// Page implementation backed by CDP. Created via `CdpTransport::page`.
pub struct CdpPageAdapter {
    page: CdpPage,
    target_id: String,
    bus: NetBus,
    /// OOPIF registry — maps frame IDs to attached CDP sessions. Used to
    /// decide whether `eval_in_frame` targets an OOPIF (separate renderer)
    /// or a same-process frame.
    oopif: std::sync::Arc<crate::oopif::OopifRegistry>,
    /// Debug URL (http://host:port) — used to resolve OOPIF WebSocket URLs
    /// via `/json/list` for direct CDP connections.
    debug_url: std::sync::Arc<String>,
    _guard: TabGuard,
}

impl CdpPageAdapter {
    pub fn new(
        page: CdpPage,
        target_id: String,
        bus: NetBus,
        guard: TabGuard,
        oopif: std::sync::Arc<crate::oopif::OopifRegistry>,
        debug_url: std::sync::Arc<String>,
    ) -> Self {
        Self {
            page,
            target_id,
            bus,
            oopif,
            debug_url,
            _guard: guard,
        }
    }

    pub fn target_id(&self) -> &str {
        &self.target_id
    }

    pub fn cdp_page(&self) -> &CdpPage {
        &self.page
    }

    async fn eval_raw(&self, js: &str) -> Result<Value> {
        let params = EvaluateParams::builder()
            .expression(js.to_string())
            .return_by_value(true)
            .await_promise(true)
            .user_gesture(true)
            .build()
            .map_err(UwaError::Internal)?;
        let res = self
            .page
            .evaluate(Evaluation::Expression(params))
            .await
            .map_err(|e| UwaError::Transport(format!("evaluate: {e}")))?;
        Ok(res.value().cloned().unwrap_or(Value::Null))
    }

    async fn wait_ready(&self, timeout: Duration) -> Result<()> {
        let start = Instant::now();
        loop {
            if let Ok(v) = self.eval_raw("document.readyState === 'complete'").await {
                if v.as_bool() == Some(true) {
                    return Ok(());
                }
            }
            if start.elapsed() >= timeout {
                return Err(UwaError::Timeout(timeout));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[async_trait]
impl uwa_core::Page for CdpPageAdapter {
    async fn goto(&self, url: &Url) -> Result<()> {
        self.page
            .goto(NavigateParams::new(url.as_str()))
            .await
            .map_err(|e| UwaError::Transport(format!("navigate {url}: {e}")))?;
        self.wait_ready(Duration::from_secs(30)).await
    }

    async fn url(&self) -> Result<Url> {
        let raw = self
            .page
            .url()
            .await
            .map_err(|e| UwaError::Transport(format!("page.url: {e}")))?
            .unwrap_or_else(|| "about:blank".to_string());
        Url::parse(&raw).map_err(|e| UwaError::Internal(format!("bad page url `{raw}`: {e}")))
    }

    async fn eval(&self, js: &str) -> Result<Value> {
        self.eval_raw(js).await
    }

    async fn wait_for_selector(&self, selector: &str, timeout: Duration) -> Result<()> {
        let start = Instant::now();
        loop {
            if self.page.find_element(selector).await.is_ok() {
                return Ok(());
            }
            if start.elapsed() >= timeout {
                return Err(UwaError::Timeout(timeout));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn html(&self) -> Result<String> {
        self.page
            .content()
            .await
            .map_err(|e| UwaError::Transport(format!("page.content: {e}")))
    }

    async fn click(&self, selector: &str) -> Result<()> {
        let el = self
            .page
            .find_element(selector)
            .await
            .map_err(|_| UwaError::Transport(format!("click `{selector}`: not found")))?;
        el.click()
            .await
            .map_err(|e| UwaError::Transport(format!("click `{selector}`: {e}")))?;
        Ok(())
    }

    async fn type_text(&self, selector: &str, text: &str) -> Result<()> {
        let js = format!(
            r#"(() => {{
                const el = document.querySelector({sel});
                if (!el) return false;
                el.focus();
                const proto = el.tagName === "TEXTAREA"
                    ? window.HTMLTextAreaElement.prototype
                    : window.HTMLInputElement.prototype;
                const setter = Object.getOwnPropertyDescriptor(proto, "value").set;
                setter.call(el, {txt});
                el.dispatchEvent(new Event("input", {{ bubbles: true }}));
                el.dispatchEvent(new Event("change", {{ bubbles: true }}));
                return true;
            }})()"#,
            sel = serde_json::to_string(selector).map_err(|e| UwaError::Internal(e.to_string()))?,
            txt = serde_json::to_string(text).map_err(|e| UwaError::Internal(e.to_string()))?,
        );
        let v = self.eval_raw(&js).await?;
        if v.as_bool() == Some(true) {
            Ok(())
        } else {
            Err(UwaError::Transport(format!(
                "type `{selector}`: element not found"
            )))
        }
    }

    async fn network_events(&self) -> Result<broadcast::Receiver<NetworkEvent>> {
        Ok(self.bus.subscribe(&self.target_id).await)
    }

    async fn eval_early(&self, js: &str) -> Result<()> {
        self.page
            .execute(AddScriptToEvaluateOnNewDocumentParams::new(js.to_string()))
            .await
            .map_err(|e| UwaError::Transport(format!("add_init_script: {e}")))?;
        Ok(())
    }

    async fn frame_tree(&self) -> Result<Vec<(String, String)>> {
        use chromiumoxide::cdp::browser_protocol::page::GetFrameTreeParams;

        let tree = self
            .page
            .execute(GetFrameTreeParams::default())
            .await
            .map_err(|e| UwaError::Transport(format!("getFrameTree: {e}")))?;

        fn walk(
            tree: &chromiumoxide::cdp::browser_protocol::page::FrameTree,
            out: &mut Vec<(String, String)>,
        ) {
            out.push((tree.frame.id.inner().clone(), tree.frame.url.clone()));
            if let Some(children) = &tree.child_frames {
                for child in children {
                    walk(child, out);
                }
            }
        }

        let mut frames = Vec::new();
        walk(&tree.result.frame_tree, &mut frames);
        Ok(frames)
    }

    async fn eval_in_frame(&self, frame_id: &str, js: &str) -> Result<Value> {
        // If this frame is a tracked OOPIF (cross-origin, separate renderer
        // process), evaluate via its dedicated WebSocket connection. This
        // bypasses chromiumoxide's session-scoped command limitation.
        if let Some(session) = self.oopif.session_for_frame(frame_id).await {
            let tid = session.target_id.inner();
            let ws_url = crate::oopif_ws::resolve_ws_url(&self.debug_url, tid).await?;
            return crate::oopif_ws::eval_in_oopif_ws(&ws_url, js).await;
        }

        // Same-process frame: create an isolated world and evaluate in it.
        use chromiumoxide::cdp::browser_protocol::page::CreateIsolatedWorldParams;
        let world = self
            .page
            .execute(
                CreateIsolatedWorldParams::builder()
                    .frame_id(frame_id.to_string())
                    .grant_univeral_access(true)
                    .world_name("uwa-eval")
                    .build()
                    .map_err(|e| UwaError::Internal(format!("build CreateIsolatedWorld: {e}")))?,
            )
            .await
            .map_err(|e| {
                UwaError::Transport(format!("createIsolatedWorld for frame `{frame_id}`: {e}"))
            })?;

        let params = EvaluateParams::builder()
            .expression(js.to_string())
            .context_id(world.result.execution_context_id)
            .return_by_value(true)
            .await_promise(true)
            .build()
            .map_err(UwaError::Internal)?;
        let res = self
            .page
            .evaluate(Evaluation::Expression(params))
            .await
            .map_err(|e| UwaError::Transport(format!("evaluate in frame `{frame_id}`: {e}")))?;
        Ok(res.value().cloned().unwrap_or(Value::Null))
    }
}
