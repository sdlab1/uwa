//! Helpers to send CDP commands scoped to a specific session.
//!
//! In flatten mode every command carries an optional `sessionId`. The
//! generated command structs have a `session_id` field we can set.
//! This module wraps the pattern so call sites stay clean.
//!
//! Note: chromiumoxide 0.7.0's `Browser::execute` does not use the command's
//! `session_id` field. It always uses the default browser session.
//! For true session-scoped commands, we need lower-level API access.
//! These functions are placeholders that compile but execute on the default session.

use chromiumoxide::cdp::browser_protocol::target::SessionId;
use chromiumoxide::types::Command;
use chromiumoxide::Browser;
use uwa_core::{Result, UwaError};

/// Send a command in a specific CDP session.
///
/// Currently executes on the default browser session because chromiumoxide 0.7.0
/// does not expose a way to set the session ID on `Browser::execute`.
/// TODO: Implement using low-level API when available.
pub async fn execute_in_session<C>(
    browser: &Browser,
    cmd: C,
    _session_id: SessionId,
) -> Result<C::Response>
where
    C: Command,
{
    // We cannot set session_id on the generic Command trait.
    // The concrete command structs have the field, but the trait doesn't expose it.
    // Browser::execute ignores the command's session_id field anyway.
    let resp = browser
        .execute(cmd)
        .await
        .map_err(|e| UwaError::Transport(format!("cdp session command: {e:?}")))?;
    Ok(resp.result)
}

/// Runtime.evaluate in a specific session, returning the JSON value.
pub async fn eval_in_session(
    browser: &Browser,
    _session_id: SessionId,
    expression: &str,
) -> Result<serde_json::Value> {
    use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
    let params = EvaluateParams::builder()
        .expression(expression)
        .return_by_value(true)
        .await_promise(true)
        .build()
        .map_err(|e| UwaError::Internal(format!("build EvaluateParams: {e}")))?;
    let resp = execute_in_session(browser, params, _session_id).await?;
    Ok(resp.result.value.unwrap_or(serde_json::Value::Null))
}

/// Page.navigate in a specific session.
pub async fn navigate_in_session(
    browser: &Browser,
    _session_id: SessionId,
    url: &str,
) -> Result<()> {
    use chromiumoxide::cdp::browser_protocol::page::NavigateParams;
    let params = NavigateParams::new(url);
    execute_in_session(browser, params, _session_id).await?;
    Ok(())
}

/// Fetch the full frame tree of a session (used to resolve parent_frame_id
/// when TargetInfo doesn't provide it).
pub async fn frame_tree(
    browser: &Browser,
    _session_id: SessionId,
) -> Result<chromiumoxide::cdp::browser_protocol::page::GetFrameTreeReturns> {
    use chromiumoxide::cdp::browser_protocol::page::GetFrameTreeParams;
    let params = GetFrameTreeParams::default();
    execute_in_session(browser, params, _session_id).await
}
