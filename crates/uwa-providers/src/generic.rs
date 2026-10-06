//! One `SiteProvider` to rule them all.
//!
//! ## Data flow
//!
//! ```text
//! send_message:
//!   1. wait for input selector (10s)
//!   2. inject text (native setter / contenteditable branch)
//!   3. wait for send button to enable (2s cap)
//!   4. click send (retry once on stale element)
//!   5. wait for input cleared (2s cap)
//!   6. wait_for_generation_start (stop button OR assistant element)
//!
//! wait_response:
//!   1. build PipelineCfg from ProviderCfg
//!   2. ExtractionPipeline::run → {text, source, finish}
//!   3. emit `uwa_extraction_total{provider, source}` metric
//! ```
//!
//! ## Audit-relevant points
//!
//! * **A7** — no double `finisher.wait()`: the pipeline calls it internally.
//! * **B1** — `or_else` with `.await` was wrong; we now use `if ...is_err()`.
//! * **D4** — `metrics = "0.23"` is a hard dep; without a recorder it's a no-op.
//! * **D8** — no orphan `Finisher` import; only `Finisher` cfg.

use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tracing;
use uwa_config::ProviderCfg;
use uwa_core::{Capabilities, Page, Result, SiteProvider, UwaError};
use uwa_extract::dom::DomExtractor;
use uwa_extract::finisher::FinisherCfg;
use uwa_extract::pipeline::{ExtractionOutcome, ExtractionPipeline, PipelineCfg};
use url::Url;

use crate::input;

pub struct GenericProvider {
    cfg: ProviderCfg,
    pipeline: Arc<ExtractionPipeline>,
}

impl GenericProvider {
    pub fn new(cfg: ProviderCfg, pipeline: Arc<ExtractionPipeline>) -> Self {
        Self { cfg, pipeline }
    }

    pub fn from_config(cfg: ProviderCfg) -> Self {
        Self::new(cfg, Arc::new(ExtractionPipeline::new()))
    }

    // -------- selector accessors --------

    fn input_selector(&self) -> Result<&str> {
        self.cfg
            .selectors
            .input
            .as_deref()
            .ok_or_else(|| UwaError::Config(format!("provider `{}`: no `input` selector", self.cfg.name)))
    }

    fn send_selector(&self) -> Result<&str> {
        self.cfg
            .selectors
            .send_button
            .as_deref()
            .ok_or_else(|| {
                UwaError::Config(format!("provider `{}`: no `send_button` selector", self.cfg.name))
            })
    }

    // -------- pipeline helpers --------

    fn dom_extractor(&self) -> Result<DomExtractor> {
        let assistant = self
            .cfg
            .selectors
            .assistant_message
            .clone()
            .ok_or_else(|| {
                UwaError::Config(format!(
                    "provider `{}`: no `assistant_message` selector",
                    self.cfg.name
                ))
            })?;
        Ok(DomExtractor {
            assistant_message: assistant,
            stop_button: self.cfg.selectors.stop_button.clone(),
            dom_stable_for: Duration::from_millis(self.cfg.finisher.dom_stable_ms),
            max_wait: Duration::from_millis(self.cfg.finisher.max_wait_ms),
            poll_interval: Duration::from_millis(self.cfg.finisher.poll_ms),
        })
    }

    fn finisher_cfg(&self) -> FinisherCfg {
        FinisherCfg {
            stop_button: self.cfg.selectors.stop_button.clone(),
            dom_stable_for: Duration::from_millis(self.cfg.finisher.dom_stable_ms),
            poll_interval: Duration::from_millis(self.cfg.finisher.poll_ms),
            min_wait: Duration::from_millis(self.cfg.finisher.min_wait_ms),
            max_wait: Duration::from_millis(self.cfg.finisher.max_wait_ms),
        }
    }

    fn pipeline_cfg(&self) -> Result<PipelineCfg> {
        Ok(PipelineCfg {
            strategy: self.cfg.extraction,
            net: self.cfg.net.clone(),
            dom: self.dom_extractor()?,
            finisher: self.finisher_cfg(),
        })
    }

    // -------- generation start --------

    /// Wait until the response begins. Two signals:
    /// 1. the stop button appears (strongest);
    /// 2. an assistant element count > 0 after 400 ms.
    ///
    /// If neither fires within 20 s, we give up and let the finisher decide.
    async fn wait_for_generation_start(&self, page: &dyn Page) -> Result<()> {
        let start = std::time::Instant::now();
        let deadline = Duration::from_secs(20);
        let stop = self.cfg.selectors.stop_button.clone();
        let assistant = self.cfg.selectors.assistant_message.clone().unwrap_or_default();

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
            if start.elapsed() >= deadline {
                tracing::warn!(
                    provider = %self.cfg.name,
                    "generation start not detected within 20s"
                );
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
            .any(|pat| url_matches(pat, url))
    }

    async fn send_message(&self, page: &dyn Page, text: &str) -> Result<()> {
        let input_sel = self.input_selector()?;
        let send_sel = self.send_selector()?;

        input::wait_exists(page, input_sel, Duration::from_secs(10)).await?;
        input::inject_text(page, input_sel, text).await?;

        // Send buttons are often disabled until the input has content.
        let started = std::time::Instant::now();
        while !input::is_enabled(page, send_sel).await.unwrap_or(false) {
            if started.elapsed() > Duration::from_secs(2) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // Click with one retry — element may be re-rendered between query
        // and click (audit B1).
        if input::click_js(page, send_sel).await.is_err() {
            tokio::time::sleep(Duration::from_millis(50)).await;
            input::click_js(page, send_sel).await?;
        }

        // Confirm the click landed: input should be cleared within 2 s.
        let cleared = std::time::Instant::now();
        while !input::is_empty(page, input_sel).await.unwrap_or(true) {
            if cleared.elapsed() > Duration::from_secs(2) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(80)).await;
        }

        self.wait_for_generation_start(page).await
    }

    async fn wait_response(&self, page: &dyn Page) -> Result<String> {
        let cfg = self.pipeline_cfg()?;
        let outcome: ExtractionOutcome = self.pipeline.run(page, &cfg).await?;

        // Metrics: no-op if no recorder is installed.
        metrics::counter!(
            "uwa_extraction_total",
            "provider" => self.cfg.name.clone(),
            "source" => match outcome.source {
                uwa_extract::pipeline::ExtractionSource::Network => "network",
                uwa_extract::pipeline::ExtractionSource::Dom => "dom",
            }
        )
        .increment(1);

        tracing::debug!(
            provider = %self.cfg.name,
            source = ?outcome.source,
            finish = ?outcome.finish,
            "extraction done"
        );
        Ok(outcome.text)
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

/// Same glob semantics as `uwa_config`: a single `*` matches any suffix.
fn url_matches(pattern: &str, url: &Url) -> bool {
    let u = url.as_str();
    match pattern.split_once('*') {
        None => pattern == u,
        Some((head, tail)) => u.starts_with(head) && (tail.is_empty() || u.ends_with(tail)),
    }
}

