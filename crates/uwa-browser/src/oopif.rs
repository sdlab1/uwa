//! OOPIF target mapping via `Target.setAutoAttach({ flatten: true })`.
//!
//! # Why
//!
//! Cross-origin iframes under Chromium's Site Isolation run as separate
//! processes (OOPIFs). They are invisible to the parent page's CDP session.
//! Without auto-attach, we lose:
//!
//! * SSE streams coming from inside the iframe;
//! * DOM elements inside the iframe;
//! * the ability to `Runtime.evaluate` in the iframe's context.
//!
//! # How
//!
//! Following [OhMyPerf ADR-002], we install a single browser-level
//! `Target.setAutoAttach` with `flatten: true`. Chromium then emits a
//! `Target.attachedToTarget` event for every iframe, popup and worker, with
//! a `sessionId` we can route commands to.
//!
//! ```text
//! Browser WS (one socket)
//!    ├── sessionId=null   ← Target.* commands
//!    ├── sessionId=A      ← main frame
//!    └── sessionId=B      ← OOPIF
//! ```
//!
//! # Keying
//!
//! State is keyed by **frameId**, not targetId. On cross-origin navigation
//! within the same frame slot the `frameId` stays stable while `targetId` and
//! `sessionId` change. This is a load-bearing detail: if we keyed by
//! targetId, every navigation inside an iframe would invalidate our mapping.
//!
//! [OhMyPerf ADR-002]: https://github.com/hoainho/ohmyperf

use std::collections::HashMap;
use std::sync::Arc;

use chromiumoxide::cdp::browser_protocol::target::{
    EventAttachedToTarget, EventDetachedFromTarget, EventTargetDestroyed, EventTargetInfoChanged,
    SessionId, SetAutoAttachParams, TargetFilter, TargetId, TargetInfo,
};
use futures::StreamExt;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};

/// A live CDP session attached to one target (main page, OOPIF, worker).
#[derive(Debug, Clone)]
pub struct AttachedSession {
    pub session_id: SessionId,
    pub target_id: TargetId,
    pub target_type: String,
    /// Set when the target is an iframe and we can resolve its owning frame.
    pub frame_id: Option<String>,
    /// URL at attach time; kept for diagnostics.
    pub url: String,
}

/// Mapping between frames and CDP sessions.
///
/// All accessors are async and take `&self` — the map is behind a `RwLock`
/// so concurrent reads are cheap and writes are serialized.
#[derive(Default)]
pub struct OopifRegistry {
    /// frameId → attached session.
    by_frame: RwLock<HashMap<String, AttachedSession>>,
    /// sessionId → frameId (reverse index for detach handling).
    by_session: RwLock<HashMap<SessionId, String>>,
    /// targetId → sessionId (some events only carry target_id).
    by_target: RwLock<HashMap<TargetId, SessionId>>,
}

impl OopifRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn insert(&self, sess: AttachedSession) {
        if let Some(fid) = sess.frame_id.clone() {
            self.by_frame
                .write()
                .await
                .insert(fid.clone(), sess.clone());
            self.by_session
                .write()
                .await
                .insert(sess.session_id.clone(), fid);
        }
        self.by_target
            .write()
            .await
            .insert(sess.target_id.clone(), sess.session_id.clone());
    }

    pub async fn remove_by_session(&self, session_id: &SessionId) {
        let frame = self.by_session.write().await.remove(session_id);
        let target = {
            let mut g = self.by_target.write().await;
            let t = g
                .iter()
                .find(|(_, s)| *s == session_id)
                .map(|(t, _)| t.clone());
            if let Some(t) = &t {
                g.remove(t);
            }
            t
        };
        if let Some(fid) = frame {
            self.by_frame.write().await.remove(&fid);
        }
        let _ = target;
    }

    pub async fn remove_by_target(&self, target_id: &TargetId) -> Option<SessionId> {
        let sid = self.by_target.write().await.remove(target_id)?;
        self.remove_by_session(&sid).await;
        Some(sid)
    }

    pub async fn session_for_frame(&self, frame_id: &str) -> Option<AttachedSession> {
        self.by_frame.read().await.get(frame_id).cloned()
    }

    pub async fn session_for_target(&self, target_id: &TargetId) -> Option<AttachedSession> {
        let sid = self.by_target.read().await.get(target_id).cloned()?;
        let fid = self.by_session.read().await.get(&sid).cloned()?;
        self.by_frame.read().await.get(&fid).cloned()
    }

    /// Re-bind an existing frame to a new session (cross-origin navigation).
    pub async fn rebind(&self, frame_id: &str, new_session: AttachedSession) {
        debug!(%frame_id, new = %new_session.session_id, "rebinding frame");
        if let Some(old) = self
            .by_frame
            .write()
            .await
            .insert(frame_id.into(), new_session.clone())
        {
            self.by_session.write().await.remove(&old.session_id);
        }
        self.by_session
            .write()
            .await
            .insert(new_session.session_id.clone(), frame_id.into());
        self.by_target.write().await.insert(
            new_session.target_id.clone(),
            new_session.session_id.clone(),
        );
    }

    pub async fn all_sessions(&self) -> Vec<AttachedSession> {
        self.by_frame.read().await.values().cloned().collect()
    }

    pub async fn len(&self) -> usize {
        self.by_frame.read().await.len()
    }
}

/// Filter we pass to `Target.setAutoAttach`. Only iframes and pages; workers
/// and service workers are noise for our use case and can be noisy on
/// ad-heavy sites.
fn oopif_filter() -> Vec<TargetFilter> {
    vec![
        TargetFilter {
            type_: Some("iframe".into()),
            exclude: Some(false),
            exclude_default: None,
        },
        TargetFilter {
            type_: Some("page".into()),
            exclude: Some(false),
            exclude_default: None,
        },
    ]
}

/// Install auto-attach at the browser level.
///
/// `waitForDebuggerOnStart: true` means newly created OOPIFs are paused until
/// we call `Runtime.runIfWaitingForDebugger` in their session. This is
/// deliberate: it gives us a window to enable domains and inject scripts
/// before any of the iframe's own JS runs.
pub async fn install_auto_attach(
    browser: &chromiumoxide::Browser,
) -> Result<(), uwa_core::UwaError> {
    let params = SetAutoAttachParams {
        auto_attach: true,
        wait_for_debugger_on_start: true,
        flatten: true,
        filter: Some(oopif_filter()),
    };
    browser
        .execute(params)
        .await
        .map_err(|e| uwa_core::UwaError::Transport(format!("Target.setAutoAttach: {e}")))?;
    info!("OOPIF auto-attach installed (flatten: true)");
    Ok(())
}

/// Enable the domains we need on a freshly attached session.
///
/// Order matters: enable `Runtime` and `Page` first, then `Network` (so we
/// start seeing events), then `DOM` (only if we'll inspect it). Finally,
/// resume the target.
pub async fn enable_domains_for_session(
    browser: &chromiumoxide::Browser,
    session_id: &SessionId,
) -> Result<(), uwa_core::UwaError> {
    use chromiumoxide::cdp::browser_protocol::{
        dom::EnableParams as DomEnable,
        network::EnableParams as NetEnable,
        page::EnableParams as PageEnable,
        runtime::{EnableParams as RuntimeEnable, RunIfWaitingForDebuggerParams},
    };

    // Runtime — must be first so we can `runIfWaitingForDebugger` later.
    let mut cmd = RuntimeEnable::default();
    cmd.session_id = Some(session_id.clone());
    browser.execute(cmd).await.map_err(|e| {
        uwa_core::UwaError::Transport(format!("Runtime.enable [{session_id}]: {e}"))
    })?;

    let mut cmd = PageEnable::default();
    cmd.session_id = Some(session_id.clone());
    browser
        .execute(cmd)
        .await
        .map_err(|e| uwa_core::UwaError::Transport(format!("Page.enable [{session_id}]: {e}")))?;

    let mut cmd = NetEnable::default();
    cmd.session_id = Some(session_id.clone());
    browser.execute(cmd).await.map_err(|e| {
        uwa_core::UwaError::Transport(format!("Network.enable [{session_id}]: {e}"))
    })?;

    let mut cmd = DomEnable::default();
    cmd.session_id = Some(session_id.clone());
    browser
        .execute(cmd)
        .await
        .map_err(|e| uwa_core::UwaError::Transport(format!("DOM.enable [{session_id}]: {e}")))?;

    // Resume the paused OOPIF.
    let mut cmd = RunIfWaitingForDebuggerParams::default();
    cmd.session_id = Some(session_id.clone());
    browser.execute(cmd).await.map_err(|e| {
        uwa_core::UwaError::Transport(format!(
            "Runtime.runIfWaitingForDebugger [{session_id}]: {e}"
        ))
    })?;

    debug!(%session_id, "domains enabled + target resumed");
    Ok(())
}

/// Extract the owning frame id from a `TargetInfo` when the target is an
/// iframe. Chromium puts the parent frame id in the target's URL query or in
/// the `TargetInfo` itself depending on version. We try several strategies.
fn frame_id_from_target_info(info: &TargetInfo) -> Option<String> {
    // Strategy 1: TargetInfo exposes `parent_frame_id` in newer Chromium.
    // chromiumoxide maps this into `TargetInfo.parent_frame_id` when present.
    // (Not all versions expose the field; we fall back below.)
    if let Some(pid) = info.parent_frame_id.as_ref().map(|f| f.inner().clone()) {
        return Some(pid);
    }
    // Strategy 2: the target's own frame id, when the target *is* the frame.
    // For iframe targets, `target_id` generally equals the frame id of the
    // iframe's root frame.
    None
}

/// Pump target lifecycle events into the registry.
///
/// Consumes three event streams concurrently:
/// * `Target.attachedToTarget` — new OOPIF appeared;
/// * `Target.targetInfoChanged` — existing frame navigated cross-origin;
/// * `Target.detachedFromTarget` + `Target.targetDestroyed` — cleanup.
pub async fn pump_target_lifecycle(
    browser: Arc<chromiumoxide::Browser>,
    registry: Arc<OopifRegistry>,
) {
    let mut attached = match browser.event_listener::<EventAttachedToTarget>().await {
        Ok(s) => s,
        Err(e) => {
            warn!("Target.attachedToTarget listener unavailable: {e}");
            return;
        }
    };
    let mut info_changed = browser
        .event_listener::<EventTargetInfoChanged>()
        .await
        .ok();
    let mut detached = browser
        .event_listener::<EventDetachedFromTarget>()
        .await
        .ok();
    let mut destroyed = browser.event_listener::<EventTargetDestroyed>().await.ok();

    loop {
        tokio::select! {
            Some(ev) = attached.next() => {
                let info = &ev.target_info;
                let frame_id = frame_id_from_target_info(info);
                let sess = AttachedSession {
                    session_id: ev.session_id.clone(),
                    target_id: info.target_id.clone(),
                    target_type: info.r#type.clone(),
                    frame_id: frame_id.clone(),
                    url: info.url.clone(),
                };
                registry.insert(sess.clone()).await;
                debug!(
                    session = %sess.session_id,
                    target = %sess.target_id,
                    ty = %sess.target_type,
                    frame = ?frame_id,
                    "target attached"
                );
                // Enable domains + resume in a background task so the pump
                // stays responsive to further attach events.
                let browser = browser.clone();
                let sid = ev.session_id.clone();
                tokio::spawn(async move {
                    if let Err(e) = enable_domains_for_session(&browser, &sid).await {
                        warn!(%sid, "enable domains failed: {e}");
                    }
                });
            }
            Some(ev) = async { info_changed.as_mut()?.next().await }, if info_changed.is_some() => {
                let info = &ev.target_info;
                if let Some(fid) = frame_id_from_target_info(info) {
                    if let Some(existing) = registry.session_for_frame(&fid).await {
                        if existing.target_id != info.target_id {
                            // Same frame slot, new target — cross-origin nav.
                            registry.rebind(&fid, AttachedSession {
                                session_id: existing.session_id,
                                target_id: info.target_id.clone(),
                                target_type: info.r#type.clone(),
                                frame_id: Some(fid.clone()),
                                url: info.url.clone(),
                            }).await;
                        }
                    }
                }
            }
            Some(ev) = async { detached.as_mut()?.next().await }, if detached.is_some() => {
                registry.remove_by_session(&ev.session_id).await;
            }
            Some(ev) = async { destroyed.as_mut()?.next().await }, if destroyed.is_some() => {
                registry.remove_by_target(&ev.target_id).await;
            }
            else => break,
        }
    }
}

/// Fallback resolver: given a target_info without parent_frame_id, walk the
/// main session's frame tree and match by targetId.
pub async fn resolve_parent_frame_via_tree(
    browser: &chromiumoxide::Browser,
    main_session: &SessionId,
    target_id: &TargetId,
) -> Option<String> {
    use crate::cdp_cmd::frame_tree;
    let tree = frame_tree(browser, main_session.clone()).await.ok()?;
    fn walk(
        frame: &chromiumoxide::cdp::browser_protocol::page::FrameTree,
        target_id: &TargetId,
        parent: Option<&str>,
    ) -> Option<String> {
        if frame.frame.id.inner() == target_id.inner() {
            return parent.map(str::to_string);
        }
        for child in &frame.child_frames {
            if let Some(found) = walk(child, target_id, Some(frame.frame.id.inner())) {
                return Some(found);
            }
        }
        None
    }
    walk(&tree.frame_tree, target_id, None)
}
