//! Session-scoped CDP command helpers.
//!
//! In flatten mode every CDP command carries an optional `sessionId`. For
//! OOPIF targets, commands must be routed to the target's own session, not
//! the browser-level default session.
//!
//! ## How it works
//!
//! `Browser::get_page(target_id)` returns a `Page` whose internal sender is
//! bound to the target's CDP session. Every `page.execute(cmd)` therefore
//! reaches the correct session without manual `sessionId` bookkeeping.
//! This is the production path — no placeholder fallback, no default-session
//! leakage.

use std::sync::Arc;

use chromiumoxide::cdp::browser_protocol::target::TargetId;
use chromiumoxide::Browser;
use uwa_core::{Result, UwaError};

use crate::oopif::OopifRegistry;

/// Resolve a frame to the CDP target that owns it.
///
/// OOPIF frames are tracked in the [`OopifRegistry`]; if the frame is not
/// there, it is a same-process frame owned by `default_target_id`.
pub async fn target_for_frame(
    registry: &OopifRegistry,
    frame_id: &str,
    default_target_id: &TargetId,
) -> TargetId {
    match registry.session_for_frame(frame_id).await {
        Some(session) => session.target_id,
        None => default_target_id.clone(),
    }
}

/// Get the `Page` for a target. This is the production way to get a
/// session-scoped command executor: every command sent through the returned
/// `Page` goes to that target's CDP session.
pub async fn page_for_target(
    browser: &Arc<Browser>,
    target_id: &TargetId,
) -> Result<chromiumoxide::Page> {
    browser.get_page(target_id.clone()).await.map_err(|e| {
        UwaError::Transport(format!("get_page for target `{}`: {e}", target_id.inner()))
    })
}

/// Evaluate JS in a specific target's session, returning the JSON value.
///
/// Use this when you have a target ID (from the OOPIF registry or the frame
/// map) and need to run JS in that target's context.
pub async fn eval_in_target(
    browser: &Arc<Browser>,
    target_id: &TargetId,
    expression: &str,
) -> Result<serde_json::Value> {
    use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
    use chromiumoxide::js::Evaluation;

    let page = page_for_target(browser, target_id).await?;
    let params = EvaluateParams::builder()
        .expression(expression)
        .return_by_value(true)
        .await_promise(true)
        .build()
        .map_err(|e| UwaError::Internal(format!("build EvaluateParams: {e}")))?;
    let res = page
        .evaluate(Evaluation::Expression(params))
        .await
        .map_err(|e| {
            UwaError::Transport(format!("evaluate in target `{}`: {e}", target_id.inner()))
        })?;
    Ok(res.value().cloned().unwrap_or(serde_json::Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn target_for_frame_returns_default_when_not_oopif() {
        let registry = OopifRegistry::new();
        let default = TargetId::new("default-target");
        let result = target_for_frame(&registry, "unknown-frame", &default).await;
        assert_eq!(result, default);
    }

    #[tokio::test]
    async fn target_for_frame_returns_oopif_target_when_tracked() {
        let registry = OopifRegistry::new();
        let oopif_target = TargetId::new("oopif-target");
        let session = crate::oopif::AttachedSession {
            session_id: chromiumoxide::cdp::browser_protocol::target::SessionId::from(
                "sess-1".to_string(),
            ),
            target_id: oopif_target.clone(),
            target_type: "iframe".into(),
            frame_id: Some("frame-oopif".into()),
            url: "https://cross-origin.example.com/".into(),
        };
        registry.insert(session).await;

        let default = TargetId::new("default-target");
        let result = target_for_frame(&registry, "frame-oopif", &default).await;
        assert_eq!(result, oopif_target);
    }
}
