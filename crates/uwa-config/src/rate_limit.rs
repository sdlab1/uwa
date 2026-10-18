//! Per-provider request-rate budget.
//!
//! Distinct from `[providers.X].max_concurrency`, which caps **in-flight**
//! requests. This caps **total requests over time** — a burst of five
//! concurrent requests is fine if the hourly budget allows it; the
//! bucket stops the sixth request a second later when the budget is
//! spent.
//!
//! Both are needed: concurrency protects a single tab from being driven
//! in parallel; the rate limit protects an account from looking
//! robotic to the site.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitCfg {
    #[serde(default)]
    pub enabled: bool,

    /// Refill rate. `f64` on purpose: a site that allows one request per
    /// five minutes is `0.2`.
    #[serde(default = "default_rpm")]
    pub requests_per_minute: f64,

    /// Bucket capacity. A short burst up to this size is allowed before
    /// the limiter starts enforcing the refill rate. `None` means
    /// `max(1, ceil(rpm / 4))` — a quarter-minute of headroom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burst: Option<u32>,
}

fn default_rpm() -> f64 {
    60.0
}

impl Default for RateLimitCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            requests_per_minute: default_rpm(),
            burst: None,
        }
    }
}

impl RateLimitCfg {
    pub fn validate(&self) -> Result<(), String> {
        if self.enabled {
            if !(self.requests_per_minute.is_finite() && self.requests_per_minute > 0.0) {
                return Err(format!(
                    "rate_limit.requests_per_minute must be positive and finite, got {}",
                    self.requests_per_minute
                ));
            }
            if self.requests_per_minute > 100_000.0 {
                return Err("rate_limit.requests_per_minute is unreasonably large".into());
            }
            if let Some(b) = self.burst {
                if b == 0 {
                    return Err("rate_limit.burst must be ≥ 1 when set".into());
                }
            }
        }
        Ok(())
    }

    /// Bucket capacity, resolving the default.
    pub fn effective_burst(&self) -> f64 {
        self.burst
            .map(|b| b as f64)
            .unwrap_or_else(|| ((self.requests_per_minute / 4.0).ceil()).max(1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_by_default() {
        assert!(!RateLimitCfg::default().enabled);
    }

    #[test]
    fn burst_defaults_to_quarter_minute() {
        let c = RateLimitCfg {
            enabled: true,
            requests_per_minute: 40.0,
            burst: None,
        };
        assert_eq!(c.effective_burst(), 10.0);
        let c = RateLimitCfg {
            enabled: true,
            requests_per_minute: 2.0,
            burst: None,
        };
        assert_eq!(c.effective_burst(), 1.0);
    }

    #[test]
    fn explicit_burst_wins() {
        let c = RateLimitCfg {
            enabled: true,
            requests_per_minute: 40.0,
            burst: Some(2),
        };
        assert_eq!(c.effective_burst(), 2.0);
    }

    #[test]
    fn fractional_rpm_is_accepted() {
        let c = RateLimitCfg {
            enabled: true,
            requests_per_minute: 0.2, // one request every five minutes
            burst: None,
        };
        assert!(c.validate().is_ok());
    }

    #[test]
    fn validate_rejects_bad_values() {
        assert!(RateLimitCfg {
            enabled: true,
            requests_per_minute: 0.0,
            burst: None
        }
        .validate()
        .is_err());
        assert!(RateLimitCfg {
            enabled: true,
            requests_per_minute: -1.0,
            burst: None
        }
        .validate()
        .is_err());
        assert!(RateLimitCfg {
            enabled: true,
            requests_per_minute: 60.0,
            burst: Some(0)
        }
        .validate()
        .is_err());
    }
}
