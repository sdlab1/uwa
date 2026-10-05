//! Push a [`StealthPack`] into a [`Page`].

use uwa_core::{Page, Result};

use crate::StealthPack;

/// Wrap a script so a syntax error in one payload can't break navigation.
pub fn wrap(js: &str) -> String {
    format!("(function(){{try{{{}}}catch(_e){{}}}})();", js)
}

/// Register every `apply_before_load` script via `Page::eval_early`, then run
/// the remaining ones immediately via `Page::eval`.
pub async fn apply_pack(page: &dyn Page, pack: &StealthPack) -> Result<()> {
    for script in pack.scripts() {
        let wrapped = wrap(&script.js);
        if script.apply_before_load {
            page.eval_early(&wrapped).await?;
        } else {
            page.eval(&wrapped).await?;
        }
    }
    Ok(())
}

/// Helper for callers that want the raw wrapped form.
pub fn wrapped_scripts(pack: &StealthPack) -> Vec<String> {
    pack.scripts().iter().map(|s| wrap(&s.js)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::Value;
    use std::sync::Mutex;
    use std::time::Duration;
    use url::Url;
    use uwa_core::UwaError;

    struct RecordingPage {
        early: Mutex<Vec<String>>,
        live: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Page for RecordingPage {
        async fn goto(&self, _url: &Url) -> Result<()> {
            Ok(())
        }
        async fn url(&self) -> Result<Url> {
            Ok(Url::parse("https://example.com").unwrap())
        }
        async fn eval(&self, js: &str) -> Result<Value> {
            self.live.lock().unwrap().push(js.to_string());
            Ok(Value::Null)
        }
        async fn wait_for_selector(&self, _selector: &str, _timeout: Duration) -> Result<()> {
            Ok(())
        }
        async fn html(&self) -> Result<String> {
            Ok(String::new())
        }
        async fn click(&self, _selector: &str) -> Result<()> {
            Ok(())
        }
        async fn type_text(&self, _selector: &str, _text: &str) -> Result<()> {
            Ok(())
        }
        async fn eval_early(&self, js: &str) -> Result<()> {
            self.early.lock().unwrap().push(js.to_string());
            Ok(())
        }
        async fn network_events(
            &self,
        ) -> Result<tokio::sync::broadcast::Receiver<uwa_core::NetworkEvent>> {
            Err(UwaError::Internal("no network".into()))
        }
    }

    #[tokio::test]
    async fn apply_uses_eval_early_for_before_load_scripts() {
        let page = RecordingPage {
            early: Mutex::new(Vec::new()),
            live: Mutex::new(Vec::new()),
        };
        let pack = crate::StealthPack::new()
            .with_script(crate::StealthScript {
                name: "early".into(),
                js: "a".into(),
                apply_before_load: true,
            })
            .with_script(crate::StealthScript {
                name: "late".into(),
                js: "b".into(),
                apply_before_load: false,
            });
        apply_pack(&page, &pack).await.unwrap();
        assert_eq!(page.early.lock().unwrap().len(), 1);
        assert_eq!(page.live.lock().unwrap().len(), 1);
        assert!(page.early.lock().unwrap()[0].contains("try{a}"));
    }
}
