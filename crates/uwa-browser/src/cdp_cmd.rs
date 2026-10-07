//! Helpers to send CDP commands scoped to a specific session.
//!
//! In flatten mode every command carries an optional `sessionId`. The
//! `chromiumoxide::Command` trait exposes a `session_id` field we can set.
//! This module wraps the pattern so call sites stay clean.

use chromiumoxide::cdp::browser_protocol::target::SessionId;
use chromiumoxide::types::Command;
use chromiumoxide::Browser;
use uwa_core::{Result, UwaError};

/// Send a command in a specific CDP session.
pub async fn execute_in_session<C>(
    browser: &Browser,
    mut cmd: C,
    session_id: SessionId,
) -> Result<C::Response>
where
    C: Command,
{
    cmd.set_session_id(session_id);
    browser
        .execute(cmd)
        .await
        .map_err(|e| UwaError::Transport(format!("cdp session command: {e}")))
}

/// Runtime.evaluate in a specific session, returning the JSON value.
pub async fn eval_in_session(
    browser: &Browser,
    session_id: SessionId,
    expression: &str,
) -> Result<serde_json::Value> {
    use chromiumoxide::cdp::browser_protocol::runtime::EvaluateParams;
    let params = EvaluateParams::new(expression)
        .return_by_value(true)
        .await_promise(true);
    let resp = execute_in_session(browser, params, session_id).await?;
    Ok(resp.result.value.unwrap_or(serde_json::Value::Null))
}

/// Page.navigate in a specific session.
pub async fn navigate_in_session(
    browser: &Browser,
    session_id: SessionId,
    url: &str,
) -> Result<()> {
    use chromiumoxide::cdp::browser_protocol::page::NavigateParams;
    let params = NavigateParams::new(url);
    execute_in_session(browser, params, session_id).await?;
    Ok(())
}

/// Fetch the full frame tree of a session (used to resolve parent_frame_id
/// when TargetInfo doesn't provide it).
pub async fn frame_tree(
    browser: &Browser,
    session_id: SessionId,
) -> Result<chromiumoxide::cdp::browser_protocol::page::GetFrameTreeReturns> {
    use chromiumoxide::cdp::browser_protocol::page::GetFrameTreeParams;
    let params = GetFrameTreeParams::default();
    execute_in_session(browser, params, session_id).await
}
