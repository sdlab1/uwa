//! Exponential backoff with jitter.

use std::future::Future;
use std::time::Duration;
use uwa_core::Result;

#[derive(Debug, Clone)]
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
}
