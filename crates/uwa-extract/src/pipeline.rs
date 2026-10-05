//! Orchestrates network-first extraction with DOM fallback.
//!
//! Decision tree (matches the architecture diagram):
//!
//! 1. If `strategy = DomOnly` → skip network entirely.
//! 2. If network rules match and events flow → use network deltas.
//! 3. Otherwise → fall back to DOM after the finisher says "done".
//!
//! The pipeline never blocks on the network indefinitely: it races the
//! `NetExtractor` stream against the `Finisher`.

use crate::dom::DomExtractor;
use crate::finisher::{FinishSignal, Finisher};
use crate::net::{DefaultNetExtractor, NetDelta, NetExtractor, NetRules};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_stream::StreamExt;
use uwa_config::ExtractionStrategy;
use uwa_core::{Page, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionSource {
    Network,
    Dom,
}

#[derive(Debug, Clone)]
pub struct ExtractionOutcome {
    pub text: String,
    pub source: ExtractionSource,
    pub finish: FinishSignal,
}

/// Everything the pipeline needs, independent of any site. Provided by config.
#[derive(Debug, Clone)]
pub struct PipelineCfg {
    pub strategy: ExtractionStrategy,
    pub net: Option<NetRules>,
    pub dom: DomExtractor,
    pub finisher: crate::finisher::FinisherCfg,
}

pub struct ExtractionPipeline {
    net: Arc<dyn NetExtractor>,
}

impl Default for ExtractionPipeline {
    fn default() -> Self {
        Self {
            net: Arc::new(DefaultNetExtractor),
        }
    }
}

impl ExtractionPipeline {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_net(net: Arc<dyn NetExtractor>) -> Self {
        Self { net }
    }

    pub async fn run(&self, page: &dyn Page, cfg: &PipelineCfg) -> Result<ExtractionOutcome> {
        // 1. DOM-only path.
        if matches!(cfg.strategy, ExtractionStrategy::DomOnly) || cfg.net.is_none() {
            let finisher = Finisher::new(cfg.finisher.clone());
            let finish = finisher.wait(page).await?;
            let text = cfg.dom.wait_and_extract(page).await?;
            return Ok(ExtractionOutcome {
                text,
                source: ExtractionSource::Dom,
                finish,
            });
        }

        // 2. Network-first: race the net stream against a wall-clock bound.
        let rules = cfg.net.as_ref().unwrap();
        let mut stream = self.net.deltas(page, rules).await?;
        let finisher = Finisher::new(cfg.finisher.clone());

        // Drain the network stream until it idles/ends. `select!` (not
        // `timeout`) so the stream future is dropped — and its borrow of
        // `text` released — as soon as either branch completes.
        let mut text = String::new();
        let mut got_any = false;
        let max_wait = cfg.finisher.max_wait;
        let net_fut = async {
            while let Some(NetDelta(s)) = stream.next().await {
                if !s.is_empty() {
                    text.push_str(&s);
                    got_any = true;
                }
            }
        };
        let timed_out = tokio::select! {
            _ = net_fut => false,
            _ = tokio::time::sleep(max_wait) => {
                tracing::warn!("network stream idle timeout");
                true
            }
        };

        // 3. If network produced nothing (or we hit the bound) → DOM fallback.
        if !got_any || timed_out {
            let finish = finisher.wait(page).await.unwrap_or(FinishSignal::Timeout);
            let dom_text = cfg.dom.wait_and_extract(page).await?;
            if dom_text.is_empty() && got_any {
                return Ok(ExtractionOutcome {
                    text,
                    source: ExtractionSource::Network,
                    finish,
                });
            }
            return Ok(ExtractionOutcome {
                text: dom_text,
                source: ExtractionSource::Dom,
                finish,
            });
        }

        // 4. Network produced text: still run the finisher to know *why* we stopped.
        let finish = finisher.wait(page).await.unwrap_or(FinishSignal::Timeout);
        Ok(ExtractionOutcome {
            text,
            source: ExtractionSource::Network,
            finish,
        })
    }
}
