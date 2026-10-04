//! Deterministic finish detection.
//!
//! We combine three independent signals and take the first that fires:
//!
//! 1. `stop_button` is absent for two consecutive polls.
//! 2. Network stream emitted `Finished` for the active request.
//! 3. DOM was byte-identical for `dom_stable_for`.
//!
//! Plus two hard bounds:
//! * `min_wait` — never declare finish before this; some UIs show "thinking" placeholder.
//! * `max_wait` — after this, we bail regardless (and let the caller decide).

use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use uwa_core::UwaError;
use uwa_core::{NetworkEvent, Page, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishSignal {
    /// UI clearly stopped streaming.
    Stop,
    /// Hit `max_wait`.
    Timeout,
    /// DOM stopped changing and stayed that way.
    Stable,
    /// Network reported the response is done.
    NetworkDone,
}

#[derive(Debug, Clone)]
pub struct FinisherCfg {
    pub stop_button: Option<String>,
    pub dom_stable_for: Duration,
    pub poll_interval: Duration,
    pub min_wait: Duration,
    pub max_wait: Duration,
}

impl Default for FinisherCfg {
    fn default() -> Self {
        Self {
            stop_button: None,
            dom_stable_for: Duration::from_millis(700),
            poll_interval: Duration::from_millis(150),
            min_wait: Duration::from_millis(500),
            max_wait: Duration::from_secs(120),
        }
    }
}

pub struct Finisher {
    pub cfg: FinisherCfg,
}

impl Finisher {
    pub fn new(cfg: FinisherCfg) -> Self {
        Self { cfg }
    }

    /// Wait for one of the finish signals. Does not extract text.
    pub async fn wait(&self, page: &dyn Page) -> Result<FinishSignal> {
        let start = Instant::now();
        let mut net_rx: Option<broadcast::Receiver<NetworkEvent>> =
            page.network_events().await.ok();
        let mut last_html_hash: Option<u64> = None;
        let mut stable_since: Option<Instant> = None;
        let mut stop_gone_streak = 0u8;

        loop {
            if start.elapsed() >= self.cfg.max_wait {
                return Ok(FinishSignal::Timeout);
            }

            // 1. Network signal (non-blocking peek).
            if let Some(rx) = net_rx.as_mut() {
                loop {
                    match rx.try_recv() {
                        Ok(NetworkEvent::Finished { .. }) => {
                            if start.elapsed() >= self.cfg.min_wait {
                                return Ok(FinishSignal::NetworkDone);
                            }
                        }
                        Ok(_) => continue,
                        Err(broadcast::error::TryRecvError::Empty) => break,
                        Err(_) => {
                            net_rx = None;
                            break;
                        }
                    }
                }
            }

            // 2. Stop button gone for two polls.
            if let Some(sel) = &self.cfg.stop_button {
                let present: bool = page
                    .eval(&format!(
                        "!!document.querySelector({})",
                        serde_json::to_string(sel).unwrap()
                    ))
                    .await
                    .ok()
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if !present {
                    stop_gone_streak = stop_gone_streak.saturating_add(1);
                    if stop_gone_streak >= 2 && start.elapsed() >= self.cfg.min_wait {
                        return Ok(FinishSignal::Stop);
                    }
                } else {
                    stop_gone_streak = 0;
                }
            }

            // 3. DOM stability.
            let html = page
                .html()
                .await
                .map_err(|e| UwaError::Transport(format!("html: {e}")))?;
            let h = hash(&html);
            match last_html_hash {
                Some(p) if p == h => {
                    let since = stable_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= self.cfg.dom_stable_for
                        && start.elapsed() >= self.cfg.min_wait
                    {
                        return Ok(FinishSignal::Stable);
                    }
                }
                _ => {
                    last_html_hash = Some(h);
                    stable_since = None;
                }
            }

            tokio::time::sleep(self.cfg.poll_interval).await;
        }
    }
}

fn hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    // Finisher tests live in tests/finisher.rs — they need a `Page` mock.
}
