//! In-crate test support. Not part of the public API — gated by `#[cfg(test)]`
//! in `lib.rs`:
//!
//! ```ignore
//! #[cfg(test)]
//! pub(crate) mod test_support;
//! ```
//!
//! In the future, when `uwa-testkit` is stable, this can be replaced by
//! `uwa_testkit::MockPage`.

use async_trait::async_trait;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;
use url::Url;
use uwa_core::{NetworkEvent, Page, Result};

/// Scripted `Page` for provider tests.
//
// `script`: ordered `(substring, response)` pairs. On `eval(js)`, the first
// entry whose substring appears in `js` is popped and returned. If nothing
// matches, returns `default` (which starts as `false`).
//
// `html`: fixed string returned by `html()`.
#[allow(dead_code)]
pub(crate) struct ScriptedPage {
    script: Mutex<Vec<(String, Value)>>,
    default: Value,
    html: Mutex<String>,
    log: Mutex<Vec<String>>,
}

#[allow(dead_code)]
impl ScriptedPage {
    #[allow(dead_code)]
    pub fn new(script: Vec<(&str, Value)>) -> Self {
        Self {
            script: Mutex::new(
                script
                    .into_iter()
                    .map(|(s, v)| (s.to_string(), v))
                    .collect(),
            ),
            default: Value::Bool(false),
            html: Mutex::new(String::new()),
            log: Mutex::new(Vec::new()),
        }
    }

    #[allow(dead_code)]
    pub fn with_html(self, h: &str) -> Self {
        *self.html.lock().unwrap() = h.to_string();
        self
    }

    #[allow(dead_code)]
    pub fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

#[async_trait]
impl Page for ScriptedPage {
    async fn goto(&self, _: &Url) -> Result<()> {
        Ok(())
    }
    async fn url(&self) -> Result<Url> {
        Ok("https://chatgpt.com/".parse().unwrap())
    }
    async fn eval(&self, js: &str) -> Result<Value> {
        self.log.lock().unwrap().push(js.to_string());
        let mut s = self.script.lock().unwrap();
        if let Some(idx) = s.iter().position(|(pat, _)| js.contains(pat.as_str())) {
            let (_, v) = s.remove(idx);
            Ok(v)
        } else {
            Ok(self.default.clone())
        }
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
    async fn network_events(&self) -> Result<broadcast::Receiver<NetworkEvent>> {
        let (tx, rx) = broadcast::channel(1);
        drop(tx);
        Ok(rx)
    }
}

#[allow(dead_code)]
fn _keep(_: Arc<()>) {}
