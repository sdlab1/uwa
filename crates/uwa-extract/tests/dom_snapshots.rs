//! Snapshot tests for selector-driven extraction.
//!
//! First run creates `<fixtures>/<name>.html` and `<name>.txt`; subsequent runs
//! diff against them. Run `UWA_FIXTURES=$PWD/tests/fixtures cargo test` to pin
//! snapshots to the repo.

use std::time::Duration;
use uwa_extract::dom::DomExtractor;
use uwa_extract::fixture::assert_snapshot;

fn ex() -> DomExtractor {
    DomExtractor {
        assistant_message: "[data-message-author-role=assistant]".into(),
        stop_button: None,
        dom_stable_for: Duration::from_millis(0),
        max_wait: Duration::from_millis(0),
        poll_interval: Duration::from_millis(1),
    }
}

#[test]
fn chatgpt_simple() {
    let html = r#"
        <html><body>
          <div data-message-author-role="user">ping</div>
          <div data-message-author-role="assistant">
            <div class="markdown">pong</div>
          </div>
        </body></html>
    "#;
    assert_snapshot("chatgpt_simple", html, |h| {
        ex().extract_from_html(h).unwrap()
    });
}

#[test]
fn chatgpt_multiple_messages() {
    let html = r#"
        <html><body>
          <div data-message-author-role="assistant">older</div>
          <div data-message-author-role="user">new</div>
          <div data-message-author-role="assistant">latest</div>
        </body></html>
    "#;
    assert_snapshot("chatgpt_multiple", html, |h| {
        ex().extract_from_html(h).unwrap()
    });
}

#[test]
fn whitespace_is_trimmed() {
    let html = r#"
        <html><body>
          <div data-message-author-role="assistant">   hello   </div>
        </body></html>
    "#;
    assert_snapshot("whitespace_trim", html, |h| {
        ex().extract_from_html(h).unwrap()
    });
}
