//! Sequence tests: a scripted page answers every DOM probe in order, so the
//! exact call sequence of `send_message` / `cancel` is asserted without a
//! browser.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use url::Url;
use uwa_config::{ExtractionStrategy, ProviderCfg, Selectors};
use uwa_core::{Capabilities, Page, Result, SiteProvider, UwaError};
use uwa_extract::ExtractionPipeline;
use uwa_providers::GenericProvider;

/// `(needle, replies)`: the first rule whose needle occurs in the JS wins.
/// A rule with several replies pops one per call and then repeats the last.
struct Rule {
    needle: String,
    replies: VecDeque<Value>,
    last: Value,
}

/// A [`Page`] that records every `eval` and answers from [`Rule`]s.
struct ScriptedPage {
    rules: Mutex<Vec<Rule>>,
    evals: Mutex<Vec<String>>,
    html: Mutex<String>,
}

impl ScriptedPage {
    fn new() -> Self {
        Self {
            rules: Mutex::new(Vec::new()),
            evals: Mutex::new(Vec::new()),
            html: Mutex::new(String::new()),
        }
    }

    fn on(mut self, needle: &str, replies: &[Value]) -> Self {
        let replies: VecDeque<Value> = replies.iter().cloned().collect();
        let last = replies.back().cloned().unwrap_or(Value::Null);
        self.rules
            .get_mut()
            .expect("script is built before any concurrency")
            .push(Rule {
            needle: needle.to_string(),
            replies,
            last,
        });
        self
    }

    fn evals(&self) -> Vec<String> {
        self.evals.lock().unwrap().clone()
    }

    fn evals_containing(&self, needle: &str) -> usize {
        self.evals().iter().filter(|e| e.contains(needle)).count()
    }
}

#[async_trait]
impl Page for ScriptedPage {
    async fn goto(&self, url: &Url) -> Result<()> {
        *self.html.lock().unwrap() = format!("<html><body>{url}</body></html>");
        Ok(())
    }

    async fn url(&self) -> Result<Url> {
        Ok("https://demo.test/chat".parse().expect("static url"))
    }

    async fn eval(&self, js: &str) -> Result<Value> {
        self.evals.lock().unwrap().push(js.to_string());
        let mut rules = self.rules.lock().unwrap();
        for rule in rules.iter_mut() {
            if js.contains(&rule.needle) {
                let reply = if rule.replies.len() > 1 {
                    rule.replies.pop_front().unwrap_or(Value::Null)
                } else {
                    rule.last.clone()
                };
                return Ok(reply);
            }
        }
        Ok(Value::Null)
    }

    async fn wait_for_selector(&self, _: &str, _: Duration) -> Result<()> {
        Ok(())
    }

    async fn html(&self) -> Result<String> {
        Ok(self.html.lock().unwrap().clone())
    }

    async fn click(&self, _: &str) -> Result<()> {
        Ok(())
    }

    async fn type_text(&self, _: &str, _: &str) -> Result<()> {
        Ok(())
    }

    async fn network_events(&self) -> Result<tokio::sync::broadcast::Receiver<uwa_core::NetworkEvent>>
    {
        Err(UwaError::Unavailable("no network in this mock".into()))
    }
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
fn happy_page() -> ScriptedPage {
    ScriptedPage::new()
        .on("!!document.querySelector", &[json!(true)])
        .on("el.focus()", &[json!({ "ok": true })])
        .on("el.disabled", &[json!(true)])
        .on("el.click()", &[json!(true)])
        .on("el.value !== undefined", &[json!(true)])
}

#[tokio::test]
async fn send_message_runs_the_full_sequence() {
    let page = happy_page();
    provider(provider_cfg())
        .send_message(&page, "hello world")
        .await
        .expect("send succeeds");

    let evals = page.evals();
    let inject = evals
        .iter()
        .position(|e| e.contains("el.focus()"))
        .expect("the composer was filled");
    let click = evals
        .iter()
        .position(|e| e.contains("el.click()"))
        .expect("the send button was clicked");
    assert!(inject < click, "fill must precede click: {evals:?}");
    assert!(
        evals.iter().any(|e| e.contains("hello world")),
        "the text reached the page: {evals:?}"
    );
    assert!(
        evals.iter().any(|e| e.contains("#stop")),
        "generation start was probed via the stop button: {evals:?}"
    );
}

#[tokio::test]
async fn send_message_fails_when_the_composer_is_gone() {
    let page = ScriptedPage::new()
        .on("!!document.querySelector", &[json!(true)])
        .on("el.focus()", &[json!({ "ok": false, "reason": "no-element" })]);
    let err = provider(provider_cfg())
        .send_message(&page, "hi")
        .await
        .expect_err("a rejected injection must surface");
    assert!(matches!(err, UwaError::Transport(_)), "{err:?}");
}

#[tokio::test]
async fn send_message_waits_for_the_send_button_to_enable() {
    let page = ScriptedPage::new()
        .on("!!document.querySelector", &[json!(true)])
        .on("el.focus()", &[json!({ "ok": true })])
        .on("el.disabled", &[json!(false), json!(true)])
        .on("el.click()", &[json!(true)])
        .on("el.value !== undefined", &[json!(true)]);
    provider(provider_cfg())
        .send_message(&page, "hi")
        .await
        .expect("button enables on the second probe");
    assert!(
        page.evals_containing("el.disabled") >= 2,
        "the disabled button was polled more than once"
    );
}

#[tokio::test]
async fn send_message_requires_the_configured_selectors() {
    let mut cfg = provider_cfg();
    cfg.selectors.input = None;
    let page = ScriptedPage::new();
    let err = provider(cfg)
        .send_message(&page, "hi")
        .await
        .expect_err("a missing selector is a config error");
    assert!(matches!(err, UwaError::Config(_)), "{err:?}");
    assert!(page.evals().is_empty(), "no page traffic before validation");
}

#[tokio::test]
async fn cancel_clicks_the_stop_button() {
    let page = ScriptedPage::new().on("el.click()", &[json!(true)]);
    provider(provider_cfg())
        .cancel(&page)
        .await
        .expect("cancel is best-effort");
    assert_eq!(page.evals_containing("el.click()"), 1);
    assert!(
        page.evals().iter().any(|e| e.contains("#stop")),
        "the stop selector was clicked: {:?}",
        page.evals()
    );
}
