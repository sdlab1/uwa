//! Where the pack meets the `Page`.
//!
//! ## Wrapping convention
//!
//! Every script is wrapped in an IIFE with `try/catch`. Rationale:
//! * If a site overrides `Object.defineProperty`, our patch fails silently
//!   instead of throwing on load. A thrown error at `addScriptToEvaluateOnNewDocument`
//!   can poison every later navigation.
//! * It also keeps each script isolated: a variable declared in one can't
//!   leak into another.
//!
//! ## Audit C7
//!
//! `Page::eval_early` has a default no-op implementation, so test mocks
//! that don't care about stealth don't need to implement it.

use crate::{StealthPack, StealthScript};
use uwa_core::{Page, Result, UwaError};

/// Apply every script in the pack.
///
/// * `apply_before_load = true` → `Page::eval_early` (CDP
///   `Page.addScriptToEvaluateOnNewDocument`).
/// * `apply_before_load = false` → `Page::eval` (immediate).
pub async fn apply_pack(page: &dyn Page, pack: &StealthPack) -> Result<()> {
    for script in pack.scripts() {
        apply_one(page, script).await?;
    }
    Ok(())
}

async fn apply_one(page: &dyn Page, s: &StealthScript) -> Result<()> {
    let wrapped = wrap(&s.js);
    if s.apply_before_load {
        page.eval_early(&wrapped)
            .await
            .map_err(|e| UwaError::Transport(format!("stealth `{}`: {e}", s.name)))
    } else {
        page.eval(&wrapped)
            .await
            .map_err(|e| UwaError::Transport(format!("stealth `{}`: {e}", s.name)))?;
        Ok(())
    }
}

/// Wrap a script body in an IIFE with a try/catch guard.
fn wrap(body: &str) -> String {
    format!("(function(){{try{{{body}}}catch(_e){{}}}})();")
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::Value;
    use std::sync::Mutex;
    use std::time::Duration;
    use tokio::sync::broadcast;
    use url::Url;
    use uwa_core::NetworkEvent;

    /// Records every `eval` and `eval_early` call.
    struct RecordingPage {
        early: Mutex<Vec<String>>,
        immediate: Mutex<Vec<String>>,
    }

    impl RecordingPage {
        fn new() -> Self {
            Self {
                early: Mutex::new(Vec::new()),
                immediate: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl Page for RecordingPage {
        async fn goto(&self, _: &Url) -> Result<()> {
            Ok(())
        }
        async fn url(&self) -> Result<Url> {
            Ok("https://x/".parse().unwrap())
        }
        async fn eval(&self, js: &str) -> Result<Value> {
            self.immediate.lock().unwrap().push(js.into());
            Ok(Value::Null)
        }
        async fn eval_early(&self, js: &str) -> Result<()> {
            self.early.lock().unwrap().push(js.into());
            Ok(())
        }
        async fn wait_for_selector(&self, _: &str, _: Duration) -> Result<()> {
            Ok(())
        }
        async fn html(&self) -> Result<String> {
            Ok(String::new())
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

    #[tokio::test]
    async fn applies_default_pack_via_eval_early() {
        let p = RecordingPage::new();
        apply_pack(&p, &crate::builtin::default_pack())
            .await
            .unwrap();
        assert_eq!(p.early.lock().unwrap().len(), 3);
        assert!(p.immediate.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn current_document_uses_eval() {
        let p = RecordingPage::new();
        let pack = StealthPack::new().add(StealthScript::current_document("x", "1"));
        apply_pack(&p, &pack).await.unwrap();
        assert!(p.early.lock().unwrap().is_empty());
        assert_eq!(p.immediate.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn wrapping_adds_try_catch_and_iife() {
        let p = RecordingPage::new();
        let pack = StealthPack::new().add(StealthScript::new("x", "let a = 1;"));
        apply_pack(&p, &pack).await.unwrap();
        let js = &p.early.lock().unwrap()[0];
        assert!(js.starts_with("(function(){try{"));
        assert!(js.ends_with("}catch(_e){}})();"));
        assert!(js.contains("let a = 1;"));
    }

    #[tokio::test]
    async fn error_propagates_with_script_name() {
        struct FailPage;
        #[async_trait]
        impl Page for FailPage {
            async fn goto(&self, _: &Url) -> Result<()> {
                Ok(())
            }
            async fn url(&self) -> Result<Url> {
                Ok("https://x/".parse().unwrap())
            }
            async fn eval(&self, _: &str) -> Result<Value> {
                Ok(Value::Null)
            }
            async fn eval_early(&self, _: &str) -> Result<()> {
                Err(UwaError::Transport("boom".into()))
            }
            async fn wait_for_selector(&self, _: &str, _: Duration) -> Result<()> {
                Ok(())
            }
            async fn html(&self) -> Result<String> {
                Ok(String::new())
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

        let p = FailPage;
        let pack = StealthPack::new().add(StealthScript::new("my-script", "1"));
        let err = apply_pack(&p, &pack).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("my-script"), "got: {msg}");
    }

    #[tokio::test]
    async fn empty_pack_is_noop() {
        let p = RecordingPage::new();
        apply_pack(&p, &StealthPack::new()).await.unwrap();
        assert!(p.early.lock().unwrap().is_empty());
        assert!(p.immediate.lock().unwrap().is_empty());
    }
}
