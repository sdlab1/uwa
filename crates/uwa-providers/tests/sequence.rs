//! Sequence tests: a scripted page answers every DOM probe in order, so the
//! exact call sequence of `send_message` / `cancel` is asserted without a
//! browser.

use std::sync::Arc;

use serde_json::{json, Value};
use uwa_config::{ExtractionStrategy, ProviderCfg, Selectors};
use uwa_core::{Capabilities, SiteProvider, UwaError};
use uwa_extract::ExtractionPipeline;
use uwa_providers::GenericProvider;
use uwa_testkit::MockPage;

/// The `eval` calls a page saw, in order — `click:`/`type:` entries are not
/// script traffic.
fn evals(page: &MockPage) -> Vec<String> {
    page.log()
        .into_iter()
        .filter(|e| !e.starts_with("click:") && !e.starts_with("type:"))
        .collect()
}

fn evals_containing(page: &MockPage, needle: &str) -> usize {
    evals(page).iter().filter(|e| e.contains(needle)).count()
}

fn provider_cfg() -> ProviderCfg {
    ProviderCfg {
        name: "demo".into(),
        url_patterns: vec!["https://demo.test/*".into()],
        capabilities: Capabilities::default(),
        selectors: Selectors {
            input: Some("#prompt".into()),
            send_button: Some("#send".into()),
            stop_button: Some("#stop".into()),
            assistant_message: Some(".assistant".into()),
            conversation_root: None,
        },
        extraction: ExtractionStrategy::DomOnly,
        net: None,
        backend: None,
        finisher: uwa_core::FinisherTuning {
            dom_stable_ms: 30,
            poll_ms: 10,
            min_wait_ms: 30,
            max_wait_ms: 500,
        },
        selectors_version: None,
    }
}

fn provider(cfg: ProviderCfg) -> GenericProvider {
    GenericProvider::new(cfg, Arc::new(ExtractionPipeline::new()))
}

/// Everything the happy path needs: composer found, filled, send enabled.
fn happy_page() -> MockPage {
    MockPage::new()
        .expect_default(Value::Null)
        .expect_seq("!!document.querySelector", vec![json!(true)])
        .expect_seq("el.focus()", vec![json!({ "ok": true })])
        .expect_seq("el.disabled", vec![json!(true)])
        .expect_seq("el.click()", vec![json!(true)])
        .expect_seq("el.value !== undefined", vec![json!(true)])
}

#[tokio::test]
async fn send_message_runs_the_full_sequence() {
    let page = happy_page();
    provider(provider_cfg())
        .send_message(&page, "hello world")
        .await
        .expect("send succeeds");

    let seen = evals(&page);
    let inject = seen
        .iter()
        .position(|e| e.contains("el.focus()"))
        .expect("the composer was filled");
    let click = seen
        .iter()
        .position(|e| e.contains("el.click()"))
        .expect("the send button was clicked");
    assert!(inject < click, "fill must precede click: {seen:?}");
    assert!(
        seen.iter().any(|e| e.contains("hello world")),
        "the text reached the page: {seen:?}"
    );
    assert!(
        seen.iter().any(|e| e.contains("#stop")),
        "generation start was probed via the stop button: {seen:?}"
    );
}

#[tokio::test]
async fn send_message_fails_when_the_composer_is_gone() {
    let page = MockPage::new()
        .expect_default(Value::Null)
        .expect_seq("!!document.querySelector", vec![json!(true)])
        .expect_seq(
            "el.focus()",
            vec![json!({ "ok": false, "reason": "no-element" })],
        );
    let err = provider(provider_cfg())
        .send_message(&page, "hi")
        .await
        .expect_err("a rejected injection must surface");
    assert!(matches!(err, UwaError::Transport(_)), "{err:?}");
}

#[tokio::test]
async fn send_message_waits_for_the_send_button_to_enable() {
    let page = MockPage::new()
        .expect_default(Value::Null)
        .expect_seq("!!document.querySelector", vec![json!(true)])
        .expect_seq("el.focus()", vec![json!({ "ok": true })])
        .expect_seq("el.disabled", vec![json!(false), json!(true)])
        .expect_seq("el.click()", vec![json!(true)])
        .expect_seq("el.value !== undefined", vec![json!(true)]);
    provider(provider_cfg())
        .send_message(&page, "hi")
        .await
        .expect("button enables on the second probe");
    assert!(
        evals_containing(&page, "el.disabled") >= 2,
        "the disabled button was polled more than once"
    );
}

#[tokio::test]
async fn send_message_requires_the_configured_selectors() {
    let mut cfg = provider_cfg();
    cfg.selectors.input = None;
    let page = MockPage::new();
    let err = provider(cfg)
        .send_message(&page, "hi")
        .await
        .expect_err("a missing selector is a config error");
    assert!(matches!(err, UwaError::Config(_)), "{err:?}");
    assert!(evals(&page).is_empty(), "no page traffic before validation");
}

#[tokio::test]
async fn cancel_clicks_the_stop_button() {
    let page = MockPage::new()
        .expect_default(Value::Null)
        .expect_seq("el.click()", vec![json!(true)]);
    provider(provider_cfg())
        .cancel(&page)
        .await
        .expect("cancel is best-effort");
    assert_eq!(evals_containing(&page, "el.click()"), 1);
    assert!(
        evals(&page).iter().any(|e| e.contains("#stop")),
        "the stop selector was clicked: {:?}",
        evals(&page)
    );
}
