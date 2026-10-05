//! Extraction snapshots: HTML fixture in, assistant text out.
//!
//! The fixtures are the shapes these sites actually render; `insta` owns the
//! expected text (`tests/snapshots/`).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use url::Url;
use uwa_config::{ExtractionStrategy, ProviderCfg, Selectors};
use uwa_core::{Capabilities, Page, Result, SiteProvider, UwaError};
use uwa_extract::ExtractionPipeline;
use uwa_providers::GenericProvider;

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// A page whose DOM never changes: extraction must finish on stability alone.
struct HtmlPage {
    html: String,
}

#[async_trait]
impl Page for HtmlPage {
    async fn goto(&self, _: &Url) -> Result<()> {
        Ok(())
    }
    async fn url(&self) -> Result<Url> {
        Ok("https://demo.test/chat".parse().expect("static url"))
    }
    async fn eval(&self, _: &str) -> Result<Value> {
        Ok(Value::Null)
    }
    async fn wait_for_selector(&self, _: &str, _: Duration) -> Result<()> {
        Ok(())
    }
    async fn html(&self) -> Result<String> {
        Ok(self.html.clone())
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

fn provider() -> GenericProvider {
    let cfg = ProviderCfg {
        name: "demo".into(),
        url_patterns: vec!["https://demo.test/*".into()],
        capabilities: Capabilities::default(),
        selectors: Selectors {
            input: Some("#prompt".into()),
            send_button: Some("#send".into()),
            stop_button: None,
            assistant_message: Some(".assistant".into()),
            conversation_root: None,
        },
        extraction: ExtractionStrategy::DomOnly,
        net: None,
        finisher: uwa_core::FinisherTuning {
            dom_stable_ms: 20,
            poll_ms: 5,
            min_wait_ms: 20,
            max_wait_ms: 1_000,
        },
        selectors_version: None,
    };
    GenericProvider::new(cfg, Arc::new(ExtractionPipeline::new()))
}

async fn extract(fixture_name: &str) -> String {
    let page = HtmlPage {
        html: fixture(fixture_name),
    };
    provider()
        .wait_response(&page)
        .await
        .expect("extraction succeeds")
}

#[tokio::test]
async fn single_assistant_message() {
    insta::assert_snapshot!(extract("claude_simple.html").await);
}

#[tokio::test]
async fn last_of_several_messages_wins() {
    insta::assert_snapshot!(extract("claude_multi.html").await);
}

#[tokio::test]
async fn code_block_is_kept_verbatim() {
    insta::assert_snapshot!(extract("gemini_code.html").await);
}

#[tokio::test]
async fn surrounding_whitespace_is_trimmed() {
    insta::assert_snapshot!(extract("deepseek_whitespace.html").await);
}
