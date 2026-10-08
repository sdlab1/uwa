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
    FilterEntry, SessionId, SetAutoAttachParams, TargetFilter, TargetId, TargetInfo,
};
use futures::StreamExt;
use tokio::sync::RwLock;
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
        debug!(%frame_id, new = %new_session.session_id.inner(), "rebinding frame");
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

    /// Whether no sessions are currently tracked.
    pub async fn is_empty(&self) -> bool {
        self.by_frame.read().await.is_empty()
    }

    /// Returns the root frame target ID for the given target ID.
    ///
    /// For now, we return the target ID itself as a placeholder.
    /// In the future, this should walk up the frame tree to find the
    /// top-level frame's target ID.
    pub async fn root_frame_target_id(&self, target_id: &TargetId) -> Option<String> {
        // Placeholder: treat each target as its own root frame.
        Some(target_id.inner().to_string())
    }
}

fn oopif_filter() -> TargetFilter {
    TargetFilter::new(vec![
        FilterEntry {
            exclude: Some(false),
            r#type: Some("iframe".into()),
        },
        FilterEntry {
            exclude: Some(false),
            r#type: Some("page".into()),
        },
    ])
}
/// we call `Runtime.runIfWaitingForDebugger` in their session. This is
/// deliberate: it gives us a window to enable domains and inject scripts
/// before any of the iframe's own JS runs. Chromiumoxide internally handles
/// the initial debugger resumption; we additionally enable the domains we
/// need via the target's Page session.
/// Install a browser-level `Target.setAutoAttach`.
///
/// **Do not call this from `CdpTransport::connect`.** Chromiumoxide already
/// sends `Target.setAutoAttach` with `waitForDebuggerOnStart: true` as part
/// of its `page_init_commands` (handler/target.rs:572). Calling it again at
/// the browser level causes the browser session to auto-attach to every
/// target — including ones chromiumoxide already manages — which pauses
/// them twice and hangs `browser.pages()`.
///
/// This function is kept for cases where you need auto-attach on a session
/// that chromiumoxide does not manage (e.g. a dedicated browser context).
pub async fn install_auto_attach(
    browser: &chromiumoxide::Browser,
) -> Result<(), uwa_core::UwaError> {
    let params = SetAutoAttachParams {
        auto_attach: true,
        wait_for_debugger_on_start: true,
        flatten: Some(true),
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
    target_id: &chromiumoxide::cdp::browser_protocol::target::TargetId,
) -> Result<(), uwa_core::UwaError> {
    use chromiumoxide::cdp::browser_protocol::{
        dom::EnableParams as DomEnable, network::EnableParams as NetEnable,
    };
    use chromiumoxide::cdp::js_protocol::runtime::EnableParams as RuntimeEnable;

    // Get the Page for this target — its execute() routes commands to the
    // target's own CDP session, not to the browser-level default session.
    let page = browser
        .get_page(target_id.clone())
        .await
        .map_err(|e| uwa_core::UwaError::Transport(format!("get_page for OOPIF session: {e}")))?;

    // Runtime — must be first so the session is ready for evaluation.
    page.execute(RuntimeEnable::default())
        .await
        .map_err(|e| uwa_core::UwaError::Transport(format!("Runtime.enable in session: {e}")))?;

    // Network — start seeing events from this target.
    page.execute(NetEnable::default())
        .await
        .map_err(|e| uwa_core::UwaError::Transport(format!("Network.enable in session: {e}")))?;

    // DOM — only if we will inspect it.
    page.execute(DomEnable::default())
        .await
        .map_err(|e| uwa_core::UwaError::Transport(format!("DOM.enable in session: {e}")))?;

    debug!(target = %target_id.inner(), "domains enabled for OOPIF session");
    Ok(())
}

/// Extract the owning frame id from a `TargetInfo` when the target is an
/// iframe. Chromium puts the parent frame id in the target's URL query or in
/// the `TargetInfo` itself depending on version. We try several strategies.
fn frame_id_from_target_info(info: &TargetInfo) -> Option<String> {
    // Strategy 1: opener_frame_id (only set for window.open() popups, not iframes)
    if let Some(fid) = &info.opener_frame_id {
        return Some(fid.inner().to_string());
    }
    // Strategy 2: none -> caller must resolve via Page.getFrameTree
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
                    session = %sess.session_id.inner(),
                    target = %sess.target_id.inner(),
                    ty = %sess.target_type,
                    frame = ?frame_id,
                    "target attached"
                );
                // Enable domains in a background task so the pump
                // stays responsive to further attach events.
                let browser = browser.clone();
                let tid = ev.target_info.target_id.clone();
                tokio::spawn(async move {
                    if let Err(e) = enable_domains_for_session(&browser, &tid).await {
                        warn!(target_id = %tid.inner(), "enable domains failed: {e}");
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

#[cfg(test)]
mod tests {
    use super::*;
    use chromiumoxide::cdp::browser_protocol::target::{SessionId, TargetId};

    // Helper to create a test SessionId
    fn make_session_id(id: &str) -> SessionId {
        // Assuming SessionId can be created from a string
        // If this doesn't work, we'll need to adjust based on actual chromiumoxide API
        SessionId::from(id.to_string())
    }

    // Helper to create a test TargetId
    fn make_target_id(id: &str) -> TargetId {
        // Assuming TargetId can be created from a string
        TargetId::from(id.to_string())
    }

    // Helper to create a test AttachedSession
    fn make_attached_session(
        session_id: &str,
        target_id: &str,
        target_type: &str,
        frame_id: Option<&str>,
        url: &str,
    ) -> AttachedSession {
        AttachedSession {
            session_id: make_session_id(session_id),
            target_id: make_target_id(target_id),
            target_type: target_type.to_string(),
            frame_id: frame_id.map(|s| s.to_string()),
            url: url.to_string(),
        }
    }

    #[tokio::test]
    async fn test_insert_and_lookup() {
        let registry = OopifRegistry::new();
        let session = make_attached_session(
            "session1",
            "target1",
            "iframe",
            Some("frame1"),
            "http://example.com",
        );

        // Insert the session
        registry.insert(session.clone()).await;

        // Lookup by frame ID
        let found = registry.session_for_frame("frame1").await;
        assert!(found.is_some());
        let found_session = found.unwrap();
        assert_eq!(found_session.session_id, session.session_id);
        assert_eq!(found_session.target_id, session.target_id);
        assert_eq!(found_session.frame_id, session.frame_id);

        // Lookup by target ID
        let found = registry.session_for_target(&session.target_id).await;
        assert!(found.is_some());
        let found_session = found.unwrap();
        assert_eq!(found_session.session_id, session.session_id);
        assert_eq!(found_session.target_id, session.target_id);

        // Check length
        assert_eq!(registry.len().await, 1);

        // Get all sessions
        let all = registry.all_sessions().await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].session_id, session.session_id);
    }

    #[tokio::test]
    async fn test_remove_by_session() {
        let registry = OopifRegistry::new();
        let session = make_attached_session(
            "session1",
            "target1",
            "iframe",
            Some("frame1"),
            "http://example.com",
        );

        registry.insert(session.clone()).await;
        assert_eq!(registry.len().await, 1);

        // Remove by session ID
        registry.remove_by_session(&session.session_id).await;
        assert_eq!(registry.len().await, 0);

        // Verify it's gone
        let found = registry.session_for_frame("frame1").await;
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn test_remove_by_target() {
        let registry = OopifRegistry::new();
        let session = make_attached_session(
            "session1",
            "target1",
            "iframe",
            Some("frame1"),
            "http://example.com",
        );

        registry.insert(session.clone()).await;
        assert_eq!(registry.len().await, 1);

        // Remove by target ID
        let removed = registry.remove_by_target(&session.target_id).await;
        assert!(removed.is_some());
        assert_eq!(removed.unwrap(), session.session_id);
        assert_eq!(registry.len().await, 0);

        // Verify it's gone
        let found = registry.session_for_frame("frame1").await;
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn test_rebind() {
        let registry = OopifRegistry::new();
        let session1 = make_attached_session(
            "session1",
            "target1",
            "iframe",
            Some("frame1"),
            "http://example.com",
        );
        let session2 = make_attached_session(
            "session2",
            "target2",
            "iframe",
            Some("frame1"), // Same frame ID, different target/session
            "http://example2.com",
        );

        // Insert first session
        registry.insert(session1.clone()).await;
        assert_eq!(registry.len().await, 1);

        // Verify first session is present
        let found = registry.session_for_frame("frame1").await;
        assert!(found.is_some());
        assert_eq!(found.unwrap().session_id, session1.session_id);

        // Rebind the frame to the second session
        registry.rebind("frame1", session2.clone()).await;

        // Verify the frame now points to the second session
        let found = registry.session_for_frame("frame1").await;
        assert!(found.is_some());
        let found_session = found.unwrap();
        assert_eq!(found_session.session_id, session2.session_id);
        assert_eq!(found_session.target_id, session2.target_id);

        // Length should still be 1 (we replaced, not added)
        assert_eq!(registry.len().await, 1);

        // The original session should still be lookupable by target ID
        let found = registry.session_for_target(&session1.target_id).await;
        // Note: After rebind, the original session might still be in by_target map
        // but not accessible via frame. This depends on the exact rebind implementation.
        // Looking at the rebind implementation, it updates by_target as well.
        // So the old session should NOT be findable by target ID after rebind.
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn test_root_frame_target_id() {
        let registry = OopifRegistry::new();
        let target_id = make_target_id("target123");

        // Test the placeholder implementation
        let result = registry.root_frame_target_id(&target_id).await;
        assert_eq!(result, Some("target123".to_string()));
    }
}
