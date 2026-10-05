//! CDP transport: connect to a running Chromium and expose it as
//! `uwa_core::Transport` backed by `chromiumoxide`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use chromiumoxide::cdp::browser_protocol::network::{
    EventLoadingFinished, EventResponseReceived, GetResponseBodyParams, GetResponseBodyReturns,
};
use chromiumoxide::cdp::browser_protocol::page::EventFrameAttached;
use chromiumoxide::cdp::browser_protocol::target::{
    EventTargetCreated, EventTargetDestroyed, TargetId as CdpTargetId,
};
use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
use chromiumoxide::js::Evaluation;
use chromiumoxide::{Browser, Page as CdpPage};
use futures::StreamExt;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use uwa_core::{NetworkEvent, Result, TabId, UwaError};
use uwa_stealth::StealthPack;

use crate::attach::attach_stealth;
use crate::bus::NetBus;
use crate::frame::FrameMap;
use crate::page::CdpPageAdapter;
use crate::tab_id::{tab_id_from_target, target_id_from_tab};
use crate::tabpool::TabPool;

/// Transport backed by a Chromium instance reachable over CDP.
pub struct CdpTransport {
    browser: Arc<Browser>,
    pool: Arc<TabPool>,
    bus: NetBus,
    target_ids: Arc<Mutex<HashMap<TabId, String>>>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for CdpTransport {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl CdpTransport {
    /// Connect to a Chromium exposing CDP. `ws_url` may be an `http(s)` URL
    /// (resolved via `/json/version`) or a direct `ws://` debugger URL.
    pub async fn connect(
        ws_url: &str,
        idle_ttl: Duration,
        stealth: Option<StealthPack>,
    ) -> Result<Self> {
        let (browser, mut handler) = Browser::connect(ws_url.to_string())
            .await
            .map_err(|e| UwaError::Transport(format!("connect {ws_url}: {e}")))?;
        let bus = NetBus::new();
        let pool = Arc::new(TabPool::new(idle_ttl));
        let target_ids = Arc::new(Mutex::new(HashMap::new()));
        let pumped = Arc::new(Mutex::new(HashSet::new()));

        // The handler must be polled: it drives the websocket, the commands
        // and every event listener installed below.
        let handler_task = tokio::spawn(async move {
            while let Some(res) = handler.next().await {
                if let Err(e) = res {
                    debug!("cdp handler: {e}");
                }
            }
        });
        let mut tasks: Vec<JoinHandle<()>> = vec![handler_task];

        // Listeners go in before the page enumeration so a target created in
        // between is registered exactly once (see `pumped`).
        let created = browser
            .event_listener::<EventTargetCreated>()
            .await
            .map_err(|e| UwaError::Transport(format!("listen targets: {e}")))?;
        let destroyed = browser
            .event_listener::<EventTargetDestroyed>()
            .await
            .map_err(|e| UwaError::Transport(format!("listen target destroy: {e}")))?;

        // `Target.setDiscoverTargets` announces existing and new tabs, so
        // `fetch_targets()` is deliberately NOT used here: it issues a second
        // `Target.attachToTarget` for every target that the target state
        // machine already attaches to, which splits one page over two CDP
        // sessions and stalls navigation.
        let browser = Arc::new(browser);

        let created_task = {
            let browser = browser.clone();
            let pool = pool.clone();
            let bus = bus.clone();
            let target_ids = target_ids.clone();
            let pumped = pumped.clone();
            let stealth = stealth.clone();
            tokio::spawn(async move {
                let mut created = created;
                while let Some(ev) = created.next().await {
                    if ev.target_info.r#type != "page" {
                        continue;
                    }
                    let tid = ev.target_info.target_id.inner().clone();
                    match get_page_retry(&browser, &tid).await {
                        Ok(page) => {
                            register_target(page, &pool, &bus, &target_ids, &pumped, &stealth).await
                        }
                        Err(e) => warn!(target = %tid, "get_page: {e}"),
                    }
                }
            })
        };
        let destroyed_task = {
            let pool = pool.clone();
            let bus = bus.clone();
            let target_ids = target_ids.clone();
            tokio::spawn(async move {
                let mut destroyed = destroyed;
                while let Some(ev) = destroyed.next().await {
                    let tid = ev.target_id.inner().clone();
                    let tab = tab_id_from_target(&tid);
                    target_ids.lock().await.remove(&tab);
                    pool.remove(&tab);
                    bus.remove(&tid).await;
                    debug!(target = %tid, tab = %tab, "target destroyed");
                }
            })
        };
        tasks.push(created_task);
        tasks.push(destroyed_task);

        // `Target.getTargets` is answered asynchronously on the first request,
        // so `pages()` reports the pre-existing tabs only after a moment.
        let mut pages = Vec::new();
        for _ in 0..50 {
            pages = browser
                .pages()
                .await
                .map_err(|e| UwaError::Transport(format!("pages: {e}")))?;
            if !pages.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        debug!(count = pages.len(), "enumerated pages");
        if pages.is_empty() {
            warn!("connected to {ws_url} but it exposes no pages");
        }
        for page in pages {
            register_target(page, &pool, &bus, &target_ids, &pumped, &stealth).await;
        }

        info!("cdp connected to {ws_url}");
        Ok(Self {
            browser,
            pool,
            bus,
            target_ids,
            tasks,
        })
    }

    pub fn pool(&self) -> Arc<TabPool> {
        self.pool.clone()
    }

    pub fn bus(&self) -> NetBus {
        self.bus.clone()
    }

    /// Resolve a logical tab to its CDP target id (for bus subscriptions).
    pub async fn target_id(&self, tab: &TabId) -> Result<String> {
        self.resolve_target(tab).await
    }

    async fn resolve_target(&self, tab: &TabId) -> Result<String> {
        if let Some(t) = self.target_ids.lock().await.get(tab) {
            return Ok(t.clone());
        }
        target_id_from_tab(tab)
            .map(String::from)
            .ok_or_else(|| UwaError::TabNotFound(tab.to_string()))
    }
}

#[async_trait]
impl uwa_core::Transport for CdpTransport {
    async fn page(&self, tab: &TabId) -> Result<Box<dyn uwa_core::Page>> {
        let target_id = self.resolve_target(tab).await?;
        let page = get_page_retry(&self.browser, &target_id).await?;
        if !self.pool.exists(tab) {
            self.pool.add(tab.clone());
        }
        let guard = self.pool.acquire_specific(tab).await?;
        Ok(Box::new(CdpPageAdapter::new(
            page,
            target_id,
            self.bus.clone(),
            guard,
        )))
    }

    async fn list_tabs(&self) -> Result<Vec<TabId>> {
        Ok(self.pool.list())
    }

    /// Health probe that never takes the per-tab lease; otherwise a busy tab
    /// would look dead to the session sweeper.
    async fn health(&self, tab: &TabId) -> Result<()> {
        let target_id = self.resolve_target(tab).await?;
        let page = get_page_retry(&self.browser, &target_id).await?;
        eval_on(&page, "1").await?;
        Ok(())
    }
}

/// Ask the handler for a page, retrying while the target is still attaching.
async fn get_page_retry(browser: &Browser, target_id: &str) -> Result<CdpPage> {
    let tid = CdpTargetId::new(target_id);
    let mut last = String::from("unavailable");
    for _ in 0..10 {
        match browser.get_page(tid.clone()).await {
            Ok(page) => return Ok(page),
            Err(e) => {
                last = e.to_string();
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
    Err(UwaError::TabNotFound(format!("{target_id}: {last}")))
}

async fn eval_on(page: &CdpPage, js: &str) -> Result<serde_json::Value> {
    let params = EvaluateParams::builder()
        .expression(js.to_string())
        .return_by_value(true)
        .await_promise(true)
        .build()
        .map_err(UwaError::Internal)?;
    let res = page
        .evaluate(Evaluation::Expression(params))
        .await
        .map_err(|e| UwaError::Transport(format!("evaluate: {e}")))?;
    Ok(res.value().cloned().unwrap_or(serde_json::Value::Null))
}

/// Register a target in the pool/bus and start its network pump.
///
/// Returns immediately if the target already has a pump, so the initial page
/// enumeration and `Target.targetCreated` never double-subscribe.
async fn register_target(
    page: CdpPage,
    pool: &Arc<TabPool>,
    bus: &NetBus,
    target_ids: &Arc<Mutex<HashMap<TabId, String>>>,
    pumped: &Arc<Mutex<HashSet<String>>>,
    stealth: &Option<StealthPack>,
) {
    let tid = page.target_id().inner().clone();
    if !pumped.lock().await.insert(tid.clone()) {
        debug!(target = %tid, "already registered");
        return;
    }

    // Publish the tab *before* anything that awaits CDP (stealth scripts queue
    // behind target init and would otherwise leave the pool empty).
    let tab = tab_id_from_target(&tid);
    bus.sender_for(tid.clone()).await;
    target_ids.lock().await.insert(tab.clone(), tid.clone());
    if !pool.exists(&tab) {
        pool.add(tab.clone());
    }
    debug!(target = %tid, tab = %tab, pool = pool.list().len(), "registered target");

    if let Some(pack) = stealth {
        if let Err(e) = attach_stealth(&page, pack).await {
            warn!(target = %tid, "stealth: {e}");
        }
    }
    tokio::spawn(pump_page(page, bus.clone(), tid));
}

/// Per-page network pump: attributes responses to frames, pulls the body once
/// the request finishes and republishes it on the target's [`NetBus`] channel.
async fn pump_page(page: CdpPage, bus: NetBus, target_id: String) {
    let mut frames = match page.event_listener::<EventFrameAttached>().await {
        Ok(s) => s,
        Err(e) => {
            warn!(target = %target_id, "frame listener: {e}");
            return;
        }
    };
    let mut responses = match page.event_listener::<EventResponseReceived>().await {
        Ok(s) => s,
        Err(e) => {
            warn!(target = %target_id, "response listener: {e}");
            return;
        }
    };
    let mut finished = match page.event_listener::<EventLoadingFinished>().await {
        Ok(s) => s,
        Err(e) => {
            warn!(target = %target_id, "loading listener: {e}");
            return;
        }
    };

    let mut frame_map = FrameMap::for_target(target_id.clone());
    let mut inflight: HashMap<String, (String, String, String)> = HashMap::new();
    // `tokio::select!` picks a ready branch at random, so `loadingFinished` is
    // regularly handled before the `responseReceived` that precedes it on the
    // wire. Remember those ids and publish as soon as the response lands —
    // dropping the pair silently loses the body.
    let mut finished_first: HashSet<String> = HashSet::new();

    loop {
        tokio::select! {
            ev = frames.next() => {
                let Some(ev) = ev else { break };
                let fid = ev.frame_id.inner().clone();
                let parent = Some(ev.parent_frame_id.inner().clone());
                frame_map.attach(&fid, parent.as_deref());
            }
            ev = responses.next() => {
                let Some(ev) = ev else { break };
                let req = ev.request_id.inner().clone();
                let owner = ev
                    .frame_id
                    .as_ref()
                    .and_then(|f| frame_map.target_for(f.inner()).map(str::to_string))
                    .unwrap_or_else(|| target_id.clone());
                debug!(target = %target_id, req = %req, url = %ev.response.url, "response");
                inflight.insert(
                    req.clone(),
                    (ev.response.url.clone(), ev.response.mime_type.clone(), owner),
                );
                if finished_first.remove(&req) {
                    debug!(target = %target_id, req = %req, "response caught up with its finish");
                    resolve_body(&page, &bus, &req, &mut inflight).await;
                }
            }
            ev = finished.next() => {
                let Some(ev) = ev else { break };
                let req = ev.request_id.inner().clone();
                if inflight.contains_key(&req) {
                    resolve_body(&page, &bus, &req, &mut inflight).await;
                } else if finished_first.len() < 4096 {
                    debug!(target = %target_id, req = %req, "finish precedes its response");
                    finished_first.insert(req);
                }
            }
        }
    }
    debug!(target = %target_id, "network pump stopped");
}

/// Fetch the body of a finished request and publish it on the target's bus.
async fn resolve_body(
    page: &CdpPage,
    bus: &NetBus,
    req: &str,
    inflight: &mut HashMap<String, (String, String, String)>,
) {
    let Some((url, mime, owner)) = inflight.remove(req) else {
        return;
    };
    let body = match page
        .execute(GetResponseBodyParams::new(req.to_string()))
        .await
    {
        Ok(resp) => decode_body(&resp.result),
        Err(e) => {
            debug!("get_response_body {req}: {e}");
            String::new()
        }
    };
    let tx = bus.sender_for(owner).await;
    let _ = tx.send(NetworkEvent::ResponseBody { url, body, mime });
    let _ = tx.send(NetworkEvent::Finished {
        request_id: req.to_string(),
    });
}

fn decode_body(returns: &GetResponseBodyReturns) -> String {
    if returns.base64_encoded {
        base64::engine::general_purpose::STANDARD
            .decode(returns.body.as_bytes())
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_else(|_| returns.body.clone())
    } else {
        returns.body.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_body_handles_plain_and_base64() {
        let plain = GetResponseBodyReturns::new("hello", false);
        assert_eq!(decode_body(&plain), "hello");
        let b64 = GetResponseBodyReturns::new(
            base64::engine::general_purpose::STANDARD.encode("hi"),
            true,
        );
        assert_eq!(decode_body(&b64), "hi");
    }
}
