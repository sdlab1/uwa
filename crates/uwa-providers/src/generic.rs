//! The single, config-driven [`SiteProvider`]: every site in
//! `config.example.toml` runs through this one implementation.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use url::Url;
use uwa_config::{url_matches, ProviderCfg};
use uwa_core::{Capabilities, Page, Result, SiteProvider, UwaError};
use uwa_extract::dom::DomExtractor;
use uwa_extract::finisher::FinisherCfg;
use uwa_extract::pipeline::{ExtractionPipeline, ExtractionSource, PipelineCfg};

use crate::input;

/// How long `send_message` waits for the send button to become enabled.
const ENABLE_TIMEOUT: Duration = Duration::from_secs(2);
/// How long we wait for the composer to be cleared after sending.
const CLEAR_TIMEOUT: Duration = Duration::from_secs(2);
/// How long we wait for *any* sign that generation started.
const START_TIMEOUT: Duration = Duration::from_secs(20);

pub struct GenericProvider {
    cfg: ProviderCfg,
    pipeline: Arc<ExtractionPipeline>,
}

impl GenericProvider {
    pub fn new(cfg: ProviderCfg, pipeline: Arc<ExtractionPipeline>) -> Self {
        Self { cfg, pipeline }
    }

    /// The provider's config, as loaded from TOML.
    pub fn cfg(&self) -> &ProviderCfg {
        &self.cfg
    }

    fn selector<'a>(&self, sel: Option<&'a String>) -> Result<&'a str> {
        sel.map(String::as_str).ok_or_else(|| {
            UwaError::Config(format!("{}: missing selector", self.cfg.name))
        })
    }

    fn dom_extractor(&self) -> Result<DomExtractor> {
        let assistant = self.cfg.selectors.assistant_message.as_ref().ok_or_else(|| {
            UwaError::Config(format!(
                "{}: no assistant_message selector",
                self.cfg.name
            ))
        })?;
        let t = &self.cfg.finisher;
        Ok(DomExtractor {
            assistant_message: assistant.clone(),
            stop_button: self.cfg.selectors.stop_button.clone(),
            dom_stable_for: Duration::from_millis(t.dom_stable_ms),
            max_wait: Duration::from_millis(t.max_wait_ms),
            poll_interval: Duration::from_millis(t.poll_ms),
        })
    }

    fn finisher_cfg(&self) -> FinisherCfg {
        let t = &self.cfg.finisher;
        FinisherCfg {
            stop_button: self.cfg.selectors.stop_button.clone(),
            dom_stable_for: Duration::from_millis(t.dom_stable_ms),
            poll_interval: Duration::from_millis(t.poll_ms),
            min_wait: Duration::from_millis(t.min_wait_ms),
            max_wait: Duration::from_millis(t.max_wait_ms),
        }
    }

    /// Block until the site visibly starts answering (stop button appears, or
    /// the first assistant node shows up). Never fails: an empty page is a
    /// legitimate "the site ignored us" outcome the caller must handle.
    async fn wait_for_generation_start(&self, page: &dyn Page) -> Result<()> {
        let start = std::time::Instant::now();
        let stop = self.cfg.selectors.stop_button.clone();
        let assistant = self
            .cfg
            .selectors
            .assistant_message
            .clone()
            .unwrap_or_default();
        loop {
            if let Some(sel) = &stop {
                if input::exists(page, sel).await.unwrap_or(false) {
                    return Ok(());
                }
            }
            if !assistant.is_empty()
                && start.elapsed() >= Duration::from_millis(400)
                && input::count(page, &assistant).await.unwrap_or(0) > 0
            {
                return Ok(());
            }
            if start.elapsed() >= START_TIMEOUT {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(60)).await;
        }
    }
}

#[async_trait]
impl SiteProvider for GenericProvider {
    fn name(&self) -> &str {
        &self.cfg.name
    }

    fn matches(&self, url: &Url) -> bool {
        self.cfg
            .url_patterns
            .iter()
            .any(|p| url_matches(p, url))
    }

    async fn send_message(&self, page: &dyn Page, text: &str) -> Result<()> {
        let input_sel = self.selector(self.cfg.selectors.input.as_ref())?;
        let send_sel = self.selector(self.cfg.selectors.send_button.as_ref())?;

        input::wait_exists(page, input_sel, Duration::from_secs(10)).await?;
        input::inject_text(page, input_sel, text).await?;

        let started = std::time::Instant::now();
        while !input::is_enabled(page, send_sel).await.unwrap_or(false) {
            if started.elapsed() > ENABLE_TIMEOUT {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        if input::click_js(page, send_sel).await.is_err() {
            tokio::time::sleep(Duration::from_millis(50)).await;
            input::click_js(page, send_sel).await?;
        }

        let cleared = std::time::Instant::now();
        while !input::is_empty(page, input_sel).await.unwrap_or(true) {
            if cleared.elapsed() > CLEAR_TIMEOUT {
                break;
            }
            tokio::time::sleep(Duration::from_millis(80)).await;
        }

        self.wait_for_generation_start(page).await
    }

    async fn wait_response(&self, page: &dyn Page) -> Result<String> {
        let cfg = PipelineCfg {
            strategy: self.cfg.extraction,
            net: self.cfg.net.clone(),
            dom: self.dom_extractor()?,
            finisher: self.finisher_cfg(),
        };
        let out = self.pipeline.run(page, &cfg).await?;
        metrics::counter!(
            "uwa_extraction_total",
            "provider" => self.cfg.name.clone(),
            "source" => match out.source {
                ExtractionSource::Network => "network",
                ExtractionSource::Dom => "dom",
            }
        )
        .increment(1);
        Ok(out.text)
    }

    async fn cancel(&self, page: &dyn Page) -> Result<()> {
        if let Some(sel) = &self.cfg.selectors.stop_button {
            let _ = input::click_js(page, sel).await;
        }
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        self.cfg.capabilities.clone()
    }
}
