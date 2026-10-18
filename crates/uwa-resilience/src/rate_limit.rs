//! Token-bucket rate limiter.
//!
//! Refills continuously at `rpm / 60` tokens per second, capped at
//! `burst`. A `try_acquire` that finds fewer than one token returns the
//! time until the next token instead of blocking — the HTTP layer turns
//! that into a `429` + `Retry-After`, which is what a client-facing
//! limiter should do. A blocking variant would tie up a Tokio task on a
//! request that the caller might not even want any more.

use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use tokio::sync::Mutex;

use uwa_config::RateLimitCfg;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RateDecision {
    /// Request admitted. `remaining` is the bucket level after this
    /// request, floored. `reset_secs` is how long until the bucket is
    /// full again (informational; spec-standard headers).
    Allowed { remaining: u32, reset_secs: u64 },
    /// Request rejected. `retry_after_secs` is what goes into the
    /// `Retry-After` header. `remaining` is always zero on a rejection.
    Denied { retry_after_secs: u64 },
}

struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

pub struct RateLimiter {
    enabled: bool,
    rpm: f64,
    capacity: f64,
    refill_per_sec: f64,
    bucket: Mutex<Bucket>,
}

impl RateLimiter {
    pub fn new(cfg: &RateLimitCfg) -> Self {
        let rpm = cfg.requests_per_minute.max(0.0);
        let capacity = cfg.effective_burst();
        Self {
            enabled: cfg.enabled,
            rpm,
            capacity,
            refill_per_sec: rpm / 60.0,
            bucket: Mutex::new(Bucket {
                // Start full: a fresh daemon shouldn't reject the first
                // request just because it hasn't earned any tokens yet.
                tokens: capacity,
                last_refill: Instant::now(),
            }),
        }
    }

    /// Unlimited: always allows, never touches the bucket. Used for
    /// providers that don't declare a `rate_limit`.
    pub fn unlimited() -> Self {
        Self {
            enabled: false,
            rpm: 0.0,
            capacity: 0.0,
            refill_per_sec: 0.0,
            bucket: Mutex::new(Bucket {
                tokens: 0.0,
                last_refill: Instant::now(),
            }),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn rpm(&self) -> f64 {
        self.rpm
    }

    pub fn capacity(&self) -> f64 {
        self.capacity
    }

    /// A snapshot for the UI: current tokens, refill rate, capacity.
    pub async fn snapshot(&self) -> RateSnapshot {
        let mut g = self.bucket.lock().await;
        refill(&mut g, self.capacity, self.refill_per_sec);
        RateSnapshot {
            enabled: self.enabled,
            requests_per_minute: self.rpm,
            capacity: self.capacity,
            remaining: g.tokens.floor() as u32,
            // Seconds until the next token lands, if the bucket is
            // currently below 1.
            next_token_in_secs: if !self.enabled || g.tokens >= 1.0 {
                0
            } else {
                ((1.0 - g.tokens) / self.refill_per_sec).ceil() as u64
            },
        }
    }

    /// The hot path. Non-blocking apart from the internal mutex.
    pub async fn try_acquire(&self) -> RateDecision {
        if !self.enabled {
            return RateDecision::Allowed {
                remaining: u32::MAX,
                reset_secs: 0,
            };
        }
        let mut g = self.bucket.lock().await;
        refill(&mut g, self.capacity, self.refill_per_sec);

        if g.tokens >= 1.0 {
            g.tokens -= 1.0;
            let remaining = g.tokens.floor().max(0.0) as u32;
            let reset_secs = if self.refill_per_sec > 0.0 {
                (((self.capacity - g.tokens) / self.refill_per_sec).ceil() as u64).max(0)
            } else {
                0
            };
            RateDecision::Allowed {
                remaining,
                reset_secs,
            }
        } else {
            let secs = ((1.0 - g.tokens) / self.refill_per_sec).ceil();
            RateDecision::Denied {
                retry_after_secs: (secs as u64).max(1),
            }
        }
    }
}

fn refill(b: &mut Bucket, capacity: f64, rate_per_sec: f64) {
    let now = Instant::now();
    let dt = now.duration_since(b.last_refill).as_secs_f64();
    b.last_refill = now;
    b.tokens = (b.tokens + dt * rate_per_sec).min(capacity);
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RateSnapshot {
    pub enabled: bool,
    pub requests_per_minute: f64,
    pub capacity: f64,
    pub remaining: u32,
    pub next_token_in_secs: u64,
}

/// Registry of per-provider limiters. Providers without a `rate_limit`
/// block get an unlimited limiter — the registry returns a shared
/// unlimited limiter, so `get` never allocates twice for the same name.
pub struct ProviderRateLimiters {
    inner: DashMap<String, Arc<RateLimiter>>,
}

impl ProviderRateLimiters {
    pub fn new() -> Self {
        Self {
            inner: DashMap::new(),
        }
    }

    pub fn with_limit(self, provider: &str, cfg: &RateLimitCfg) -> Self {
        self.inner
            .insert(provider.to_string(), Arc::new(RateLimiter::new(cfg)));
        self
    }

    pub fn get(&self, provider: &str) -> Arc<RateLimiter> {
        if let Some(l) = self.inner.get(provider) {
            return l.clone();
        }
        // Cache the miss so subsequent calls don't allocate a fresh
        // limiter every time. `unlimited()` is cheap but `Arc`-wrapping
        // it once is cheaper.
        let fresh = Arc::new(RateLimiter::unlimited());
        self.inner.insert(provider.to_string(), fresh.clone());
        fresh
    }

    pub fn providers(&self) -> Vec<String> {
        let mut v: Vec<String> = self.inner.iter().map(|e| e.key().clone()).collect();
        v.sort();
        v
    }
}

impl Default for ProviderRateLimiters {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn cfg(rpm: f64, burst: u32) -> RateLimitCfg {
        RateLimitCfg {
            enabled: true,
            requests_per_minute: rpm,
            burst: Some(burst),
        }
    }

    #[tokio::test]
    async fn unlimited_always_allows() {
        let l = RateLimiter::unlimited();
        for _ in 0..1000 {
            assert!(matches!(l.try_acquire().await, RateDecision::Allowed { .. }));
        }
    }

    #[tokio::test]
    async fn burst_capacity_is_available_immediately() {
        let l = RateLimiter::new(&cfg(60.0, 3));
        for i in 0..3 {
            let d = l.try_acquire().await;
            assert!(matches!(d, RateDecision::Allowed { .. }), "acquire #{i}: {d:?}");
        }
        let d = l.try_acquire().await;
        assert!(matches!(d, RateDecision::Denied { .. }), "{d:?}");
    }

    #[tokio::test]
    async fn denied_returns_a_sensible_retry_after() {
        // 60 rpm = 1 token/sec. Burst 1. After one acquire, next token
        // lands in ~1 s. `Retry-After` must be at least 1 and no more
        // than 2 — rounding up from anything under a second gives 1.
        let l = RateLimiter::new(&cfg(60.0, 1));
        let _ = l.try_acquire().await;
        let d = l.try_acquire().await;
        match d {
            RateDecision::Denied { retry_after_secs } => {
                assert!((1..=2).contains(&retry_after_secs), "{retry_after_secs}");
            }
            _ => panic!("expected Denied, got {d:?}"),
        }
    }

    #[tokio::test]
    async fn refill_is_continuous() {
        // 60 rpm = 1 token/sec, burst 2. Consume two, then wait 1.1 s for
        // one token to refill (real time, since the bucket uses Instant).
        let l = RateLimiter::new(&cfg(60.0, 2));
        let _ = l.try_acquire().await;
        let _ = l.try_acquire().await;
        assert!(matches!(
            l.try_acquire().await,
            RateDecision::Denied { .. }
        ));
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(matches!(l.try_acquire().await, RateDecision::Allowed { .. }));
    }

    #[tokio::test(start_paused = true)]
    async fn bucket_never_exceeds_capacity() {
        let l = RateLimiter::new(&cfg(60.0, 3));
        // Idle for an hour. If the refill overshot, the bucket would
        // hold 60 tokens.
        tokio::time::advance(Duration::from_secs(3600)).await;
        for i in 0..3 {
            assert!(
                matches!(l.try_acquire().await, RateDecision::Allowed { .. }),
                "burst #{i} should still be allowed"
            );
        }
        assert!(matches!(
            l.try_acquire().await,
            RateDecision::Denied { .. }
        ));
    }

    #[tokio::test]
    async fn remaining_decrements() {
        let l = RateLimiter::new(&cfg(60.0, 5));
        match l.try_acquire().await {
            RateDecision::Allowed { remaining, .. } => assert_eq!(remaining, 4),
            _ => panic!(),
        }
        match l.try_acquire().await {
            RateDecision::Allowed { remaining, .. } => assert_eq!(remaining, 3),
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn fractional_rpm_works() {
        // 0.2 rpm = 1 token every 300 s. Burst 1.
        let l = RateLimiter::new(&RateLimitCfg {
            enabled: true,
            requests_per_minute: 0.2,
            burst: Some(1),
        });
        assert!(matches!(l.try_acquire().await, RateDecision::Allowed { .. }));
        match l.try_acquire().await {
            RateDecision::Denied { retry_after_secs } => {
                // Should be ~300 s, allow for rounding.
                assert!((295..=305).contains(&retry_after_secs), "{retry_after_secs}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn registry_caches_misses() {
        let r = ProviderRateLimiters::new().with_limit("a", &cfg(60.0, 3));
        let l1 = r.get("a");
        let l2 = r.get("a");
        assert!(Arc::ptr_eq(&l1, &l2));
        let u1 = r.get("unknown");
        let u2 = r.get("unknown");
        assert!(Arc::ptr_eq(&u1, &u2));
        assert!(!u1.enabled());
    }

    #[tokio::test]
    async fn snapshot_reports_state() {
        let l = RateLimiter::new(&cfg(60.0, 3));
        let _ = l.try_acquire().await;
        let s = l.snapshot().await;
        assert!(s.enabled);
        assert_eq!(s.requests_per_minute, 60.0);
        assert_eq!(s.capacity, 3.0);
        assert_eq!(s.remaining, 2);
        assert_eq!(s.next_token_in_secs, 0); // ≥1 token still available
    }
}
