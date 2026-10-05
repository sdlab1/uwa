//! Programmable [`Page`] mock.
//!
//! * **Script matching**: queue of `(substring, value)`. On `eval(js)`, the
//!   first entry whose substring appears in `js` is popped and its value
//!   returned. If nothing matches, the default is returned.
//! * **HTML**: either fixed or a sequence (popped per call, last repeated).
//! * **Network events**: broadcast channel; pre-loaded or triggered by the
//!   test through [`MockPage::network_sender`].
//! * **Log**: every `eval`/`click`/`type_text` is recorded for assertions.

use async_trait::async_trait;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;
use url::Url;
use uwa_core::{NetworkEvent, Page, Result, UwaError};

/// A [`Page`] whose every answer the test chooses.
#[derive(Clone)]
pub struct MockPage {
    inner: Arc<Inner>,
}

struct Inner {
    url: Mutex<Url>,
    html: Mutex<Html>,
    script: Mutex<Vec<ScriptRule>>,
    default: Mutex<Value>,
    wait_selector: Mutex<Result<(), String>>,
    net_tx: broadcast::Sender<NetworkEvent>,
    /// Events queued before anyone subscribed. A `broadcast` only delivers
    /// what was sent *after* `subscribe()`, so these are replayed there.
    net_pending: Mutex<Vec<NetworkEvent>>,
    log: Mutex<Vec<String>>,
}

enum Html {
    Fixed(String),
    Sequence(VecDeque<String>),
}

/// One `eval` rule: the JS substring it matches and what to answer.
struct ScriptRule {
    pattern: String,
    values: VecDeque<Value>,
    /// Keep answering the last value forever instead of falling through to
    /// the default. Single-shot rules (`expect`) stop after their answers
    /// run out.
    repeat: bool,
}

impl MockPage {
    pub fn new() -> Self {
        let (net_tx, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(Inner {
                url: Mutex::new("about:blank".parse().expect("static url")),
                html: Mutex::new(Html::Fixed(String::new())),
                script: Mutex::new(Vec::new()),
                default: Mutex::new(Value::Bool(false)),
                wait_selector: Mutex::new(Ok(())),
                net_tx,
                net_pending: Mutex::new(Vec::new()),
                log: Mutex::new(Vec::new()),
            }),
        }
    }

    pub fn with_url(self, u: &str) -> Self {
        *self.inner.url.lock().expect("url lock") = u.parse().expect("bad url");
        self
    }

    pub fn with_html(self, h: &str) -> Self {
        *self.inner.html.lock().expect("html lock") = Html::Fixed(h.into());
        self
    }

    /// Queue a sequence; each `html()` call pops one. When only one is left
    /// it is returned forever (the "DOM stabilised" case).
    pub fn with_html_seq<I, S>(self, seq: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let q: VecDeque<String> = seq.into_iter().map(Into::into).collect();
        *self.inner.html.lock().expect("html lock") = Html::Sequence(q);
        self
    }

    /// On `eval(js)`, if `js` contains `pattern`, return `value` (once).
    pub fn expect(self, pattern: &str, value: Value) -> Self {
        self.push_rule(pattern, vec![value], false);
        self
    }

    /// Like [`expect`](Self::expect), but answers in order and then keeps
    /// repeating the last value — what a page that only ever reports one
    /// state looks like.
    pub fn expect_seq<S: Into<Value>>(self, pattern: &str, values: Vec<S>) -> Self {
        self.push_rule(pattern, values.into_iter().map(Into::into).collect(), true);
        self
    }

    fn push_rule(&self, pattern: &str, values: Vec<Value>, repeat: bool) {
        self.inner
            .script
            .lock()
            .expect("script lock")
            .push(ScriptRule {
                pattern: pattern.into(),
                values: values.into(),
                repeat,
            });
    }

    /// Answer for `eval` when no [`expect`](Self::expect) matches.
    pub fn expect_default(self, value: Value) -> Self {
        *self.inner.default.lock().expect("default lock") = value;
        self
    }

    /// Make `wait_for_selector` fail with [`UwaError::Timeout`].
    pub fn expect_wait_timeout(self) -> Self {
        *self.inner.wait_selector.lock().expect("wait lock") = Err("timeout".into());
        self
    }

    /// Queue network events for the next [`network_events`](Page::network_events)
    /// call, which replays them to the fresh subscriber (a `broadcast` does
    /// not deliver messages sent before `subscribe`).
    pub fn with_network_events(self, events: Vec<NetworkEvent>) -> Self {
        self.inner
            .net_pending
            .lock()
            .expect("pending lock")
            .extend(events);
        self
    }

    /// External handle for tests that emit network events *after* they
    /// subscribed through [`network_events`](Page::network_events).
    pub fn network_sender(&self) -> broadcast::Sender<NetworkEvent> {
        self.inner.net_tx.clone()
    }

    /// Every `eval`/`click`/`type_text` recorded so far.
    pub fn log(&self) -> Vec<String> {
        self.inner.log.lock().expect("log lock").clone()
    }
}

impl Default for MockPage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Page for MockPage {
    async fn goto(&self, url: &Url) -> Result<()> {
        *self.inner.url.lock().expect("url lock") = url.clone();
        Ok(())
    }

    async fn url(&self) -> Result<Url> {
        Ok(self.inner.url.lock().expect("url lock").clone())
    }

    async fn eval(&self, js: &str) -> Result<Value> {
        self.inner
            .log
            .lock()
            .expect("log lock")
            .push(js.to_string());
        let mut script = self.inner.script.lock().expect("script lock");
        if let Some(idx) = script.iter().position(|r| js.contains(r.pattern.as_str())) {
            let rule = &mut script[idx];
            let v = rule.values.front().cloned().unwrap_or(Value::Null);
            if rule.values.len() > 1 {
                rule.values.pop_front();
            } else if !rule.repeat {
                script.remove(idx);
            }
            return Ok(v);
        }
        Ok(self.inner.default.lock().expect("default lock").clone())
    }

    /// The stealth pack registers pre-load scripts here; recorded as
    /// `early:<js>` so tests can tell them apart from [`eval`](Page::eval).
    async fn eval_early(&self, js: &str) -> Result<()> {
        self.inner
            .log
            .lock()
            .expect("log lock")
            .push(format!("early:{js}"));
        Ok(())
    }

    async fn wait_for_selector(&self, _sel: &str, timeout: Duration) -> Result<()> {
        match &*self.inner.wait_selector.lock().expect("wait lock") {
            Ok(()) => Ok(()),
            Err(_) => Err(UwaError::Timeout(timeout)),
        }
    }

    async fn html(&self) -> Result<String> {
        match &mut *self.inner.html.lock().expect("html lock") {
            Html::Fixed(s) => Ok(s.clone()),
            Html::Sequence(q) => {
                if q.len() > 1 {
                    Ok(q.pop_front().expect("non-empty"))
                } else {
                    Ok(q.front().cloned().unwrap_or_default())
                }
            }
        }
    }

    async fn click(&self, selector: &str) -> Result<()> {
        self.inner
            .log
            .lock()
            .expect("log lock")
            .push(format!("click:{selector}"));
        Ok(())
    }

    async fn type_text(&self, selector: &str, text: &str) -> Result<()> {
        self.inner
            .log
            .lock()
            .expect("log lock")
            .push(format!("type:{selector}={text}"));
        Ok(())
    }

    async fn network_events(&self) -> Result<broadcast::Receiver<NetworkEvent>> {
        let rx = self.inner.net_tx.subscribe();
        let pending = std::mem::take(&mut *self.inner.net_pending.lock().expect("pending lock"));
        for e in pending {
            let _ = self.inner.net_tx.send(e);
        }
        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn eval_matches_substring_once() {
        let p = MockPage::new().expect("querySelector(\"#x\")", json!(true));
        let js = "!!document.querySelector(\"#x\")";
        assert_eq!(p.eval(js).await.expect("first hit"), json!(true));
        assert_eq!(p.eval(js).await.expect("fallback"), json!(false));
    }

    #[tokio::test]
    async fn expect_seq_repeats_the_last_value() {
        let p = MockPage::new().expect_seq("query", vec![json!(true), json!(false)]);
        assert_eq!(p.eval("query here").await.expect("first"), json!(true));
        assert_eq!(p.eval("query here").await.expect("second"), json!(false));
        assert_eq!(p.eval("query here").await.expect("repeats"), json!(false));
    }

    #[tokio::test]
    async fn eval_early_is_recorded_separately() {
        let p = MockPage::new();
        p.eval_early("stealth-a").await.expect("early");
        p.eval("live-a").await.expect("live");
        assert_eq!(
            p.log(),
            vec!["early:stealth-a".to_string(), "live-a".to_string()]
        );
    }

    #[tokio::test]
    async fn eval_falls_back_to_default() {
        let p = MockPage::new().expect_default(json!({"ok": true}));
        assert_eq!(
            p.eval("anything").await.expect("default"),
            json!({"ok": true})
        );
    }

    #[tokio::test]
    async fn html_sequence_repeats_last() {
        let p = MockPage::new().with_html_seq(vec!["a", "b"]);
        assert_eq!(p.html().await.expect("a"), "a");
        assert_eq!(p.html().await.expect("b"), "b");
        assert_eq!(p.html().await.expect("again"), "b");
    }

    #[tokio::test]
    async fn wait_for_selector_can_time_out() {
        let p = MockPage::new().expect_wait_timeout();
        let err = p
            .wait_for_selector("#never", Duration::from_millis(5))
            .await
            .expect_err("must time out");
        assert!(matches!(err, UwaError::Timeout(_)), "{err:?}");
    }

    #[tokio::test]
    async fn log_records_evals_clicks_and_typing() {
        let p = MockPage::new();
        let _ = p.eval("1").await;
        let _ = p.click("#btn").await;
        let _ = p.type_text("#in", "hi").await;
        let l = p.log();
        assert_eq!(l.len(), 3);
        assert_eq!(l[0], "1");
        assert!(l[1].starts_with("click:"), "{l:?}");
        assert!(l[2].starts_with("type:#in=hi"), "{l:?}");
    }

    #[tokio::test]
    async fn network_events_reach_subscribers() {
        let p = MockPage::new().with_network_events(vec![NetworkEvent::ResponseBody {
            url: "https://x".into(),
            body: "data".into(),
            mime: "text/event-stream".into(),
        }]);
        let mut rx = p.network_events().await.expect("subscribed");
        let ev = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("event must arrive within 1s")
            .expect("event");
        match ev {
            NetworkEvent::ResponseBody { url, body, .. } => {
                assert_eq!(url, "https://x");
                assert_eq!(body, "data");
            }
            other => panic!("unexpected event {other:?}"),
        }
    }

    #[tokio::test]
    async fn goto_moves_the_url() {
        let p = MockPage::new();
        let target = Url::parse("https://chatgpt.com/c/1").expect("url");
        p.goto(&target).await.expect("goto");
        assert_eq!(p.url().await.expect("url"), target);
    }
}
