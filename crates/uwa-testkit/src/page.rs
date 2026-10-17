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
use uwa_core::{CookieRecord, MediaResource, NetworkEvent, Page, Result, UwaError};

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
    /// Backing store for the cookie trait methods; import replaces any
    /// record sharing (name, domain, path), mirroring real browser
    /// semantics, and everything else is appended.
    cookies: Mutex<Vec<CookieRecord>>,
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
                url: Mutex::new(crate::static_url("about:blank")),
                html: Mutex::new(Html::Fixed(String::new())),
                script: Mutex::new(Vec::new()),
                default: Mutex::new(Value::Bool(false)),
                wait_selector: Mutex::new(Ok(())),
                net_tx,
                net_pending: Mutex::new(Vec::new()),
                log: Mutex::new(Vec::new()),
                cookies: Mutex::new(Vec::new()),
            }),
        }
    }

    pub fn with_url(self, u: &str) -> Self {
        // A malformed literal degrades to about:blank — the test's own URL
        // assertions catch the mistake loudly enough.
        *crate::lock(&self.inner.url) = u
            .parse()
            .unwrap_or_else(|_| crate::static_url("about:blank"));
        self
    }

    pub fn with_html(self, h: &str) -> Self {
        *crate::lock(&self.inner.html) = Html::Fixed(h.into());
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
        *crate::lock(&self.inner.html) = Html::Sequence(q);
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
        crate::lock(&self.inner.script).push(ScriptRule {
            pattern: pattern.into(),
            values: values.into(),
            repeat,
        });
    }

    /// Answer for `eval` when no [`expect`](Self::expect) matches.
    pub fn expect_default(self, value: Value) -> Self {
        *crate::lock(&self.inner.default) = value;
        self
    }

    /// Make `wait_for_selector` fail with [`UwaError::Timeout`].
    pub fn expect_wait_timeout(self) -> Self {
        *crate::lock(&self.inner.wait_selector) = Err("timeout".into());
        self
    }

    /// Queue network events for the next [`network_events`](Page::network_events)
    /// call, which replays them to the fresh subscriber (a `broadcast` does
    /// not deliver messages sent before `subscribe`).
    pub fn with_network_events(self, events: Vec<NetworkEvent>) -> Self {
        crate::lock(&self.inner.net_pending).extend(events);
        self
    }

    /// External handle for tests that emit network events *after* they
    /// subscribed through [`network_events`](Page::network_events).
    pub fn network_sender(&self) -> broadcast::Sender<NetworkEvent> {
        self.inner.net_tx.clone()
    }

    /// Pre-load the backing store `export_cookies` will return. Mostly
    /// useful for the export round-trip; import tests usually start empty.
    pub fn with_cookies(self, cookies: Vec<CookieRecord>) -> Self {
        *crate::lock(&self.inner.cookies) = cookies;
        self
    }

    /// Snapshot of the current backing store — assert what an import
    /// actually stored without going through `export_cookies` again.
    pub fn cookies(&self) -> Vec<CookieRecord> {
        crate::lock(&self.inner.cookies).clone()
    }

    /// Every `eval`/`click`/`type_text` recorded so far.
    pub fn log(&self) -> Vec<String> {
        crate::lock(&self.inner.log).clone()
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
        *crate::lock(&self.inner.url) = url.clone();
        Ok(())
    }

    async fn url(&self) -> Result<Url> {
        Ok(crate::lock(&self.inner.url).clone())
    }

    async fn eval(&self, js: &str) -> Result<Value> {
        crate::lock(&self.inner.log).push(js.to_string());
        let mut script = crate::lock(&self.inner.script);
        if let Some(idx) = script.iter().position(|r| js.contains(r.pattern.as_str())) {
            let Some(rule) = script.get_mut(idx) else {
                // Unreachable: the index came from this very vec.
                unreachable!("script rule vanished mid-iteration");
            };
            let v = rule.values.front().cloned().unwrap_or(Value::Null);
            if rule.values.len() > 1 {
                rule.values.pop_front();
            } else if !rule.repeat {
                script.remove(idx);
            }
            return Ok(v);
        }
        Ok(crate::lock(&self.inner.default).clone())
    }

    /// The stealth pack registers pre-load scripts here; recorded as
    /// `early:<js>` so tests can tell them apart from [`eval`](Page::eval).
    async fn eval_early(&self, js: &str) -> Result<()> {
        crate::lock(&self.inner.log).push(format!("early:{js}"));
        Ok(())
    }

    async fn wait_for_selector(&self, _sel: &str, timeout: Duration) -> Result<()> {
        match &*crate::lock(&self.inner.wait_selector) {
            Ok(()) => Ok(()),
            Err(_) => Err(UwaError::Timeout(timeout)),
        }
    }

    async fn html(&self) -> Result<String> {
        match &mut *crate::lock(&self.inner.html) {
            Html::Fixed(s) => Ok(s.clone()),
            Html::Sequence(q) => {
                if q.len() > 1 {
                    Ok(q.pop_front().unwrap_or_default())
                } else {
                    Ok(q.front().cloned().unwrap_or_default())
                }
            }
        }
    }

    async fn click(&self, selector: &str) -> Result<()> {
        crate::lock(&self.inner.log).push(format!("click:{selector}"));
        Ok(())
    }

    async fn type_text(&self, selector: &str, text: &str) -> Result<()> {
        crate::lock(&self.inner.log).push(format!("type:{selector}={text}"));
        Ok(())
    }

    async fn network_events(&self) -> Result<broadcast::Receiver<NetworkEvent>> {
        let rx = self.inner.net_tx.subscribe();
        let pending = std::mem::take(&mut *crate::lock(&self.inner.net_pending));
        for e in pending {
            let _ = self.inner.net_tx.send(e);
        }
        Ok(rx)
    }

    /// Record the capture intent so tests can assert the provider drove
    /// it. The real JS lives in `uwa-browser::media_capture`.
    async fn start_audio_capture(&self) -> Result<()> {
        crate::lock(&self.inner.log).push("audio_capture:start".into());
        Ok(())
    }

    /// Mirrors [`MockPage::start_audio_capture`]: records the call and
    /// returns whatever `expect_default` scripts (None by default).
    async fn stop_audio_capture(&self) -> Result<Option<Vec<u8>>> {
        crate::lock(&self.inner.log).push("audio_capture:stop".into());
        let scripted = self.eval("stop_audio_capture").await?;
        match scripted {
            Value::Array(chunks) => {
                let mut buf = Vec::new();
                for c in chunks {
                    if let Some(s) = c.as_str() {
                        buf.extend_from_slice(s.as_bytes());
                    }
                }
                if buf.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(buf))
                }
            }
            _ => Ok(None),
        }
    }

    /// Mirrors [`MockPage::start_audio_capture`]: records the call and
    /// returns whatever `expect_default` scripts.
    async fn scan_media(&self) -> Result<Vec<MediaResource>> {
        crate::lock(&self.inner.log).push("media_scan".into());
        let scripted = self.eval("scan_media").await?;
        Ok(serde_json::from_value(scripted).unwrap_or_default())
    }

    async fn export_cookies(&self) -> Result<Vec<CookieRecord>> {
        crate::lock(&self.inner.log).push("export_cookies".into());
        Ok(crate::lock(&self.inner.cookies).clone())
    }

    async fn import_cookies(&self, cookies: &[CookieRecord]) -> Result<usize> {
        let mut written: usize = 0;
        let mut store = crate::lock(&self.inner.cookies);
        for c in cookies {
            let slot = store.iter().position(|existing| {
                existing.name == c.name && existing.domain == c.domain && existing.path == c.path
            });
            if let Some(i) = slot {
                let Some(existing) = store.get_mut(i) else {
                    // Unreachable: `i` was just found by `position`.
                    unreachable!("cookie slot vanished mid-import");
                };
                *existing = c.clone();
            } else {
                store.push(c.clone());
            }
            written = written.saturating_add(1);
        }
        crate::lock(&self.inner.log).push("import_cookies".into());
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn import_replaces_matching_cookie() {
        let p = MockPage::new().with_cookies(vec![CookieRecord {
            name: "sid".into(),
            value: "old".into(),
            domain: "chat.example.com".into(),
            path: "/".into(),
            expires: Some(1_800_000_000.0),
            http_only: true,
            secure: true,
            session: false,
            same_site: Some("Lax".into()),
        }]);
        p.import_cookies(&[CookieRecord {
            name: "sid".into(),
            value: "new".into(),
            domain: "chat.example.com".into(),
            path: "/".into(),
            expires: None,
            http_only: false,
            secure: true,
            session: true,
            same_site: None,
        }])
        .await
        .unwrap_or_else(|_| unreachable!("import is mocked"));
        p.import_cookies(&[CookieRecord {
            name: "extra".into(),
            value: "v".into(),
            domain: "other.example.com".into(),
            path: "/".into(),
            expires: None,
            http_only: false,
            secure: false,
            session: true,
            same_site: None,
        }])
        .await
        .unwrap_or_else(|_| unreachable!("import is mocked"));

        let exported = p
            .export_cookies()
            .await
            .unwrap_or_else(|_| unreachable!("export is mocked"));
        assert_eq!(exported.len(), 2, "same-name import replaces, new appends");
        let sid = exported
            .iter()
            .find(|c| c.name == "sid")
            .cloned()
            .unwrap_or_else(|| unreachable!("sid survived"));
        assert_eq!(sid.value, "new");
        assert!(sid.session);
        assert!(p.log().iter().any(|l| l == "import_cookies"));
        assert!(p.log().iter().any(|l| l == "export_cookies"));
    }

    #[tokio::test]
    async fn eval_matches_substring_once() {
        let p = MockPage::new().expect("querySelector(\"#x\")", json!(true));
        let js = "!!document.querySelector(\"#x\")";
        assert_eq!(
            p.eval(js)
                .await
                .unwrap_or_else(|_| unreachable!("first hit")),
            json!(true)
        );
        assert_eq!(
            p.eval(js)
                .await
                .unwrap_or_else(|_| unreachable!("fallback")),
            json!(false)
        );
    }

    #[tokio::test]
    async fn expect_seq_repeats_the_last_value() {
        let p = MockPage::new().expect_seq("query", vec![json!(true), json!(false)]);
        assert_eq!(
            p.eval("query here")
                .await
                .unwrap_or_else(|_| unreachable!("first")),
            json!(true)
        );
        assert_eq!(
            p.eval("query here")
                .await
                .unwrap_or_else(|_| unreachable!("second")),
            json!(false)
        );
        assert_eq!(
            p.eval("query here")
                .await
                .unwrap_or_else(|_| unreachable!("repeats")),
            json!(false)
        );
    }

    #[tokio::test]
    async fn eval_early_is_recorded_separately() {
        let p = MockPage::new();
        p.eval_early("stealth-a")
            .await
            .unwrap_or_else(|_| unreachable!("early"));
        p.eval("live-a")
            .await
            .unwrap_or_else(|_| unreachable!("live"));
        assert_eq!(
            p.log(),
            vec!["early:stealth-a".to_string(), "live-a".to_string()]
        );
    }

    #[tokio::test]
    async fn eval_falls_back_to_default() {
        let p = MockPage::new().expect_default(json!({"ok": true}));
        assert_eq!(
            p.eval("anything")
                .await
                .unwrap_or_else(|_| unreachable!("default")),
            json!({"ok": true})
        );
    }

    #[tokio::test]
    async fn html_sequence_repeats_last() {
        let p = MockPage::new().with_html_seq(vec!["a", "b"]);
        assert_eq!(p.html().await.unwrap_or_else(|_| unreachable!("a")), "a");
        assert_eq!(p.html().await.unwrap_or_else(|_| unreachable!("b")), "b");
        assert_eq!(
            p.html().await.unwrap_or_else(|_| unreachable!("again")),
            "b"
        );
    }

    #[tokio::test]
    async fn wait_for_selector_can_time_out() {
        let p = MockPage::new().expect_wait_timeout();
        let err = match p
            .wait_for_selector("#never", Duration::from_millis(5))
            .await
        {
            Err(err) => err,
            Ok(_) => unreachable!("must time out"),
        };
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
        assert_eq!(l.first(), Some(&"1".to_string()));
        assert!(l.get(1).is_some_and(|s| s.starts_with("click:")), "{l:?}");
        assert!(
            l.get(2).is_some_and(|s| s.starts_with("type:#in=hi")),
            "{l:?}"
        );
    }

    #[tokio::test]
    async fn network_events_reach_subscribers() {
        let p = MockPage::new().with_network_events(vec![NetworkEvent::ResponseBody {
            request_id: "r1".into(),
            url: "https://x".into(),
            body: "data".into(),
            mime: "text/event-stream".into(),
            cf_mitigated_challenge: false,
        }]);
        let mut rx = p
            .network_events()
            .await
            .unwrap_or_else(|_| unreachable!("subscribed"));
        let ev = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap_or_else(|_| unreachable!("event must arrive within 1s"))
            .unwrap_or_else(|_| unreachable!("event"));
        match ev {
            NetworkEvent::ResponseBody { url, body, .. } => {
                assert_eq!(url, "https://x");
                assert_eq!(body, "data");
            }
            other => unreachable!("unexpected event {other:?}"),
        }
    }

    #[tokio::test]
    async fn goto_moves_the_url() {
        let p = MockPage::new();
        let target = Url::parse("https://chatgpt.com/c/1").unwrap_or_else(|_| unreachable!("url"));
        p.goto(&target)
            .await
            .unwrap_or_else(|_| unreachable!("goto"));
        assert_eq!(
            p.url().await.unwrap_or_else(|_| unreachable!("url")),
            target
        );
    }
}
