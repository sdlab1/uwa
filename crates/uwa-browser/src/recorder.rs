//! Recorder: JS-side observation of user interactions, exposed to the
//! HTTP layer as a poll + accumulate API.
//!
//! The observer (see `js/recorder_install.js`) stores events in
//! `sessionStorage`, which survives navigation within an origin. The
//! Rust side polls `events_since(seq)` at a fixed interval, accumulates
//! into a growing `Vec<TraceEvent>`, and never has to reason about page
//! lifecycles.
//!
//! Installation is idempotent — the poller re-injects the observer every
//! tick, so a navigation mid-recording just means "the next tick
//! reinstalls it and the sequence counter keeps climbing from where it
//! stopped".

use serde_json::Value;
use uwa_core::workflow::TraceEvent;
use uwa_core::{Page, Result, UwaError};

const INSTALL_JS: &str = include_str!("js/recorder_install.js");

/// Polling script. `LAST_SEQ` is substituted with a decimal integer.
/// Returns `{ installed: bool, events: [TraceEvent, ...] }`.
const POLL_JS: &str = r#"(function (lastSeq) {
    if (!window.__uwaRecorder) return { installed: false, events: [] };
    return { installed: true, events: window.__uwaRecorder.events_since(lastSeq) };
})(LAST_SEQ)"#;

/// Install (or refresh) the observer on the current document.
pub async fn install(page: &dyn Page) -> Result<()> {
    let v = page.eval(INSTALL_JS).await?;
    if v.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(UwaError::Transport(format!(
            "recorder install returned {v}"
        )));
    }
    Ok(())
}

/// Fetch every event with `seq > last_seq`. Returns the installed flag so
/// the caller can distinguish "page hasn't been instrumented yet" from
/// "no new events".
pub async fn poll(page: &dyn Page, last_seq: u64) -> Result<(bool, Vec<TraceEvent>)> {
    let js = POLL_JS.replace("LAST_SEQ", &last_seq.to_string());
    let v = page.eval(&js).await?;
    let installed = v.get("installed").and_then(Value::as_bool).unwrap_or(false);
    let events_json = v
        .get("events")
        .cloned()
        .unwrap_or_else(|| Value::Array(vec![]));
    let parsed: Vec<TraceEvent> = serde_json::from_value(events_json).unwrap_or_default();
    Ok((installed, parsed))
}

/// Clear the observer's event buffer. Called at the start of a new
/// recording so a previous run's events don't bleed into the new trace.
pub async fn reset(page: &dyn Page) -> Result<()> {
    let _ = page
        .eval(
            "window.__uwaRecorder && window.__uwaRecorder.reset && window.__uwaRecorder.reset(); true",
        )
        .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uwa_testkit::MockPage;

    #[tokio::test]
    async fn install_reports_ok() {
        let p = MockPage::new().expect_default(json!({ "ok": true, "installed": true }));
        install(&p).await.unwrap_or_else(|e| unreachable!("{e}"));
    }

    #[tokio::test]
    async fn install_failure_is_error() {
        let p = MockPage::new().expect_default(json!({ "ok": false }));
        assert!(install(&p).await.is_err());
    }

    #[tokio::test]
    async fn poll_parses_events() {
        let p = MockPage::new().expect_default(json!({
            "installed": true,
            "events": [
                { "kind": "click", "selector": "#a", "seq": 1, "at_ms": 10, "src": "https://x/" }
            ]
        }));
        let (installed, events) = poll(&p, 0).await.unwrap_or_else(|e| unreachable!("{e}"));
        assert!(installed);
        assert_eq!(events.len(), 1);
        assert!(matches!(events.first().unwrap_or_else(|| unreachable!()), TraceEvent::Click { selector, .. } if selector == "#a"));
    }

    #[tokio::test]
    async fn poll_not_installed_returns_empty() {
        let p = MockPage::new().expect_default(json!({ "installed": false, "events": [] }));
        let (installed, events) = poll(&p, 0).await.unwrap_or_else(|e| unreachable!("{e}"));
        assert!(!installed);
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn poll_substitutes_the_sequence_number() {
        let p = MockPage::new().expect_default(json!({ "installed": true, "events": [] }));
        let _ = poll(&p, 42).await.unwrap_or_else(|e| unreachable!("{e}"));
        let calls = p.log();
        let last = calls
            .iter()
            .rev()
            .find(|s| s.contains("events_since"))
            .unwrap_or_else(|| unreachable!("install + poll must both eval"));
        assert!(last.contains(")(42)"), "seq not substituted: {last}");
    }
}
