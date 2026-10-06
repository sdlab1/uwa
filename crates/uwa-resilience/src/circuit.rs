//! Circuit breaker: stop hammering a provider that is already failing.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use uwa_core::{Result, UwaError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

impl CircuitState {
    pub fn as_str(&self) -> &'static str {
        match self {
            CircuitState::Closed => "closed",
            CircuitState::Open => "open",
            CircuitState::HalfOpen => "half_open",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CircuitCfg {
    /// Failures inside `rolling_window` that trip the breaker.
    pub failure_threshold: usize,
    pub rolling_window: Duration,
    /// How long we stay open before probing again.
    pub cooldown: Duration,
    /// Simultaneous probes allowed while half-open.
    pub half_open_max_calls: usize,
}

impl Default for CircuitCfg {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            rolling_window: Duration::from_secs(30),
            cooldown: Duration::from_secs(15),
            half_open_max_calls: 1,
        }
    }
}

struct Inner {
    state: CircuitState,
    failures: VecDeque<Instant>,
    opened_at: Option<Instant>,
    half_open_in_flight: usize,
}

pub struct CircuitBreaker {
    name: String,
    cfg: CircuitCfg,
    inner: Mutex<Inner>,
}

impl CircuitBreaker {
    pub fn new(name: impl Into<String>, cfg: CircuitCfg) -> Self {
        Self {
            name: name.into(),
            cfg,
            inner: Mutex::new(Inner {
                state: CircuitState::Closed,
                failures: VecDeque::new(),
                opened_at: None,
                half_open_in_flight: 0,
            }),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        let mut g = self.inner.lock().expect("circuit mutex poisoned");
        f(&mut g)
    }

    /// May a call proceed right now?
    pub fn allow(&self) -> Result<()> {
        self.with(|inner| {
            let now = Instant::now();
            match inner.state {
                CircuitState::Closed => Ok(()),
                CircuitState::Open => {
                    let opened = inner.opened_at.unwrap_or(now);
                    if now.duration_since(opened) >= self.cfg.cooldown {
                        inner.state = CircuitState::HalfOpen;
                        inner.half_open_in_flight = 1;
                        Ok(())
                    } else {
                        Err(UwaError::Unavailable(format!(
                            "circuit `{}` open",
                            self.name
                        )))
                    }
                }
                CircuitState::HalfOpen => {
                    if inner.half_open_in_flight < self.cfg.half_open_max_calls {
                        inner.half_open_in_flight += 1;
                        Ok(())
                    } else {
                        Err(UwaError::Unavailable(format!(
                            "circuit `{}` half-open probe in flight",
                            self.name
                        )))
                    }
                }
            }
        })
    }

    pub fn record_success(&self) {
        self.with(|inner| {
            inner.state = CircuitState::Closed;
            inner.failures.clear();
            inner.opened_at = None;
            inner.half_open_in_flight = 0;
        });
    }

    pub fn record_failure(&self) {
        self.with(|inner| {
            let now = Instant::now();
            if inner.state == CircuitState::HalfOpen {
                inner.state = CircuitState::Open;
                inner.opened_at = Some(now);
                inner.half_open_in_flight = 0;
                return;
            }
            inner.failures.push_back(now);
            let cutoff = now
                .checked_sub(self.cfg.rolling_window)
                .unwrap_or_else(Instant::now);
            while inner.failures.front().is_some_and(|t| *t < cutoff) {
                inner.failures.pop_front();
            }
            if inner.failures.len() >= self.cfg.failure_threshold {
                inner.state = CircuitState::Open;
                inner.opened_at = Some(now);
                inner.failures.clear();
            }
        });
    }

    pub fn state(&self) -> CircuitState {
        self.with(|inner| {
            if inner.state == CircuitState::Open {
                if let Some(opened) = inner.opened_at {
                    if opened.elapsed() >= self.cfg.cooldown {
                        inner.state = CircuitState::HalfOpen;
                        inner.half_open_in_flight = 0;
                    }
                }
            }
            inner.state
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cb() -> CircuitBreaker {
        CircuitBreaker::new(
            "chatgpt",
            CircuitCfg {
                failure_threshold: 3,
                rolling_window: Duration::from_secs(60),
                cooldown: Duration::from_millis(50),
                half_open_max_calls: 1,
            },
        )
    }

    #[test]
    fn opens_after_threshold() {
        let cb = cb();
        assert!(cb.allow().is_ok());
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(cb.allow().is_err());
    }

    #[tokio::test]
    async fn cooldown_promotes_to_half_open_then_closed() {
        let cb = cb();
        cb.record_failure();
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(cb.state(), CircuitState::HalfOpen);
        assert!(cb.allow().is_ok());
        assert!(cb.allow().is_err(), "only one probe at a time");
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.allow().is_ok());
    }

    #[tokio::test]
    async fn half_open_failure_reopens() {
        let cb = cb();
        cb.record_failure();
        cb.record_failure();
        cb.record_failure();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(cb.allow().is_ok());
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(cb.allow().is_err());
    }

    #[tokio::test]
    async fn initial_state_is_closed() {
        let cb = CircuitBreaker::new("test", CircuitCfg::default());
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.allow().is_ok());
    }
    #[tokio::test]
    async fn record_success_resets_the_failure_count() {
        let cb = CircuitBreaker::new(
            "test",
            CircuitCfg {
                failure_threshold: 3,
                rolling_window: Duration::from_secs(60),
                cooldown: Duration::from_secs(60),
                half_open_max_calls: 1,
            },
        );
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);

        // A success wipes the slate: two more failures must not be enough to trip.
        cb.record_success();
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
    }

    /// Failures older than `rolling_window` must not count towards the
    /// threshold: two stale ones plus two fresh ones stay below three.
    /// Real time, not virtual: the breaker stamps failures with
    /// `std::time::Instant`.
    #[tokio::test]
    async fn old_failures_leave_the_rolling_window() {
        let cb = CircuitBreaker::new(
            "test",
            CircuitCfg {
                failure_threshold: 3,
                rolling_window: Duration::from_millis(50),
                cooldown: Duration::from_secs(30),
                half_open_max_calls: 1,
            },
        );
        cb.record_failure();
        cb.record_failure();
        tokio::time::sleep(Duration::from_millis(90)).await;
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[tokio::test]
    async fn half_open_allows_multiple_probes_when_configured() {
        let cb = CircuitBreaker::new(
            "test",
            CircuitCfg {
                failure_threshold: 2,
                rolling_window: Duration::from_secs(10),
                cooldown: Duration::from_millis(50),
                half_open_max_calls: 3,
            },
        );
        // Trip the breaker
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
        // Wait cooldown
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(cb.state(), CircuitState::HalfOpen);
        // Allow three probes
        assert!(cb.allow().is_ok());
        assert!(cb.allow().is_ok());
        assert!(cb.allow().is_ok());
        // Fourth should fail
        assert!(cb.allow().is_err());
        // Success on one of them closes the breaker
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.allow().is_ok());
    }
}
