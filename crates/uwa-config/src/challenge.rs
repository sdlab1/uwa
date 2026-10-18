//! Challenge detection config.
//!
//! Detection is always on unless explicitly disabled — the alternative
//! is reading "selector not found" tea leaves. The knobs below govern
//! waiting, not noticing.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengeCfg {
    /// Run the challenge gate in the pipeline. `false` restores the
    /// pre-Session-20 behaviour: a challenge surfaces as a generic
    /// ready-wait timeout.
    #[serde(default = "default_detect")]
    pub detect: bool,

    /// How long a freshly detected challenge may take to clear on its
    /// own. Managed challenges in a real browser pass in 3-6 s; a slow
    /// Turnstile can take longer. `0` = never wait — one look, then
    /// fail immediately (useful for headless CI where nobody can
    /// solve anything).
    #[serde(default = "default_auto_wait_ms")]
    pub auto_wait_ms: u64,

    /// Poll interval while waiting for the challenge to clear.
    #[serde(default = "default_poll_ms")]
    pub poll_ms: u64,
}

fn default_detect() -> bool {
    true
}
fn default_auto_wait_ms() -> u64 {
    15_000
}
fn default_poll_ms() -> u64 {
    500
}

impl Default for ChallengeCfg {
    fn default() -> Self {
        Self {
            detect: default_detect(),
            auto_wait_ms: default_auto_wait_ms(),
            poll_ms: default_poll_ms(),
        }
    }
}

impl ChallengeCfg {
    pub fn validate(&self) -> Result<(), String> {
        if !self.detect {
            return Ok(());
        }
        if self.poll_ms < 50 {
            return Err("challenge.poll_ms must be >= 50".into());
        }
        if self.poll_ms > 5_000 {
            return Err("challenge.poll_ms must be <= 5000".into());
        }
        if self.auto_wait_ms > 120_000 {
            return Err("challenge.auto_wait_ms must not exceed 120000".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_detection_on() {
        let c = ChallengeCfg::default();
        assert!(c.detect);
        assert_eq!(c.auto_wait_ms, 15_000);
        assert_eq!(c.poll_ms, 500);
    }

    #[test]
    fn disabled_config_skips_validation() {
        let c = ChallengeCfg {
            detect: false,
            auto_wait_ms: 0,
            poll_ms: 0,
        };
        assert!(c.validate().is_ok());
    }

    #[test]
    fn poll_bounds_are_enforced() {
        assert!(ChallengeCfg {
            detect: true,
            poll_ms: 10,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(ChallengeCfg {
            detect: true,
            poll_ms: 6_000,
            ..Default::default()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn auto_wait_cap_is_enforced() {
        assert!(ChallengeCfg {
            detect: true,
            auto_wait_ms: 120_001,
            ..Default::default()
        }
        .validate()
        .is_err());
    }
}
