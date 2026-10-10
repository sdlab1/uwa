//! Snapshot tests for selector-driven extraction (`insta`).
//!
//! Run `cargo insta review` after an intentional extraction change.

use std::time::Duration;
use uwa_extract::dom::DomExtractor;

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
    let html = include_str!("fixtures/chatgpt_simple.html");
    let out = ex().extract_from_html(html).unwrap();
    insta::assert_snapshot!(out);
}

#[test]
fn chatgpt_multiple_messages() {
    let html = include_str!("fixtures/chatgpt_multiple.html");
    let out = ex().extract_from_html(html).unwrap();
    insta::assert_snapshot!(out);
}

#[test]
fn whitespace_is_trimmed() {
    let html = include_str!("fixtures/whitespace_trim.html");
    let out = ex().extract_from_html(html).unwrap();
    insta::assert_snapshot!(out);
}

#[test]
fn markdown_blocks_concatenated() {
    let html = include_str!("fixtures/markdown_blocks.html");
    let out = ex().extract_from_html(html).unwrap();
    insta::assert_snapshot!(out);
}
