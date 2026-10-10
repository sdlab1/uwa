//! Exponential backoff with jitter.

use std::future::Future;
use std::time::Duration;
use uwa_core::Result;

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RetryCfg {
    pub attempts: u32,
    pub base: Duration,
    pub max: Duration,
}

impl Default for RetryCfg {
    fn default() -> Self {
        Self {
            attempts: 3,
            base: Duration::from_millis(100),
            max: Duration::from_secs(2),
        }
    }
}

impl RetryCfg {
    /// Explicit constructor: `attempts` with the default backoff schedule.
    ///
    /// Being `#[non_exhaustive]`, the struct cannot be built with a literal
    /// from other crates (E0639 — even with `..Default::default()`), so this
    /// is the sanctioned entry point. Tune the public fields afterwards:
    ///
    /// ```rust
    /// use std::time::Duration;
    /// use uwa_resilience::RetryCfg;
    ///
    /// let mut cfg = RetryCfg::new(2);
    /// cfg.base = Duration::from_millis(50);
    /// cfg.max = Duration::from_millis(100);
    /// assert_eq!(cfg.attempts, 2);
    /// ```
    pub fn new(attempts: u32) -> Self {
        Self {
            attempts,
            ..Default::default()
        }
    }
}

/// Run `f` up to `cfg.attempts` times, backing off exponentially with jitter.
/// The last error is returned if every attempt fails.
pub async fn retry<T, F, Fut>(cfg: &RetryCfg, mut f: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut last: Option<uwa_core::UwaError> = None;
    for attempt in 0..cfg.attempts.max(1) {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => last = Some(e),
        }
        if attempt + 1 >= cfg.attempts.max(1) {
            break;
        }
        let exp = cfg.base.saturating_mul(1u32 << attempt.min(16));
        let capped = exp.min(cfg.max);
        let jitter = capped.mul_f64(rand::random::<f64>() * 0.3);
        tokio::time::sleep(capped + jitter).await;
    }
    Err(last.expect("retry must attempt at least once"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Instant;

    #[tokio::test]
    async fn retries_until_success() {
        let n = AtomicU32::new(0);
        let cfg = RetryCfg {
            attempts: 5,
            base: Duration::from_millis(1),
            max: Duration::from_millis(5),
        };
        let v = retry(&cfg, || {
            let n = &n;
            async move {
                let i = n.fetch_add(1, Ordering::SeqCst);
                if i < 2 {
                    Err(uwa_core::UwaError::Unavailable("boom".into()))
                } else {
                    Ok(i)
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(v, 2);
    }

    #[tokio::test]
    async fn exhausts_attempts_and_returns_last_error() {
        let cfg = RetryCfg {
            attempts: 3,
            base: Duration::from_millis(1),
            max: Duration::from_millis(2),
        };
        let n = AtomicU32::new(0);
        let err = retry(&cfg, || {
            let n = &n;
            async move {
                n.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(uwa_core::UwaError::Unavailable("nope".into()))
            }
        })
        .await
        .unwrap_err();
        assert!(err.to_string().contains("nope"));
    }

    #[tokio::test]
    async fn success_on_first_attempt_no_sleep() {
        let n = AtomicU32::new(0);
        let cfg = RetryCfg {
            attempts: 5,
            base: Duration::from_millis(100),
            max: Duration::from_millis(500),
        };
        let start = Instant::now();
        let v = retry(&cfg, || {
            let n = &n;
            async move {
                let i = n.fetch_add(1, Ordering::SeqCst);
                Ok(i)
            }
        })
        .await
        .unwrap();
        let elapsed = start.elapsed();
        // Should return quickly, certainly less than 20ms (no sleep)
        assert!(elapsed < Duration::from_millis(20));
        assert_eq!(v, 0);
    }

    /// Virtual time: the runtime auto-advances to each timer deadline, so the
    /// backoff schedule is checked exactly, not by wall clock.
    #[tokio::test(start_paused = true)]
    async fn backs_off_between_attempts_only() {
        let cfg = RetryCfg {
            attempts: 3,
            base: Duration::from_millis(100),
            max: Duration::from_millis(100),
        };
        let start = tokio::time::Instant::now();
        let err = retry(&cfg, || async {
            Err::<(), _>(uwa_core::UwaError::Unavailable("x".into()))
        })
        .await
        .unwrap_err();
        let elapsed = start.elapsed();
        assert!(err.to_string().contains('x'));
        // Two backoffs of 100ms, each plus up to 30% jitter: [200ms, 260ms).
        // A third sleep after the final attempt would push this past 300ms.
        assert!(
            elapsed >= Duration::from_millis(200),
            "two backoffs expected: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(290),
            "no backoff after the final attempt: {elapsed:?}"
        );
    }

    /// The delay doubles per attempt until `max`; with `base` 50ms and
    /// `max` 1s four attempts cannot possibly reach the cap, so a total under
    /// 1s proves the growth starts at `base`.
    #[tokio::test(start_paused = true)]
    async fn backoff_grows_exponentially_from_base() {
        let cfg = RetryCfg {
            attempts: 4,
            base: Duration::from_millis(50),
            max: Duration::from_secs(1),
        };
        let start = tokio::time::Instant::now();
        let _ = retry(&cfg, || async {
            Err::<(), _>(uwa_core::UwaError::Unavailable("x".into()))
        })
        .await;
        // 50 + 100 + 200, each plus up to 30% jitter: [350ms, 455ms).
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    /// `attempts` is a floor of one: a zero must still call the closure once
    /// (returning its error), never zero times and never a panic.
    #[tokio::test(start_paused = true)]
    async fn attempts_below_one_still_call_once() {
        for attempts in [0, 1] {
            let cfg = RetryCfg {
                attempts,
                base: Duration::from_millis(1),
                max: Duration::from_millis(1),
            };
            let calls = AtomicU32::new(0);
            let err = retry(&cfg, || {
                let calls = &calls;
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err::<(), _>(uwa_core::UwaError::Unavailable("x".into()))
                }
            })
            .await
            .unwrap_err();
            assert_eq!(calls.load(Ordering::SeqCst), 1, "attempts={attempts}");
            assert!(err.to_string().contains('x'));
        }
    }

    /// `base` 100ms doubling to 800ms, but `max` clamps every delay after the
    /// first: 100 + 150*3, not 100 + 200 + 400 + 800.
    #[tokio::test(start_paused = true)]
    async fn backoff_is_capped_at_max() {
        let cfg = RetryCfg {
            attempts: 5,
            base: Duration::from_millis(100),
            max: Duration::from_millis(150),
        };
        let start = tokio::time::Instant::now();
        let _ = retry(&cfg, || async {
            Err::<(), _>(uwa_core::UwaError::Unavailable("x".into()))
        })
        .await;
        let elapsed = start.elapsed();
        // 550ms of sleeping, plus up to 30% jitter per delay: [550ms, 715ms).
        assert!(elapsed >= Duration::from_millis(500), "{elapsed:?}");
        assert!(
            elapsed < Duration::from_millis(800),
            "uncapped: {elapsed:?}"
        );
    }
}
