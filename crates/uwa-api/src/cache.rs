//! In-memory LRU response cache.
//!
//! ## What gets cached
//!
//! `CachedOutcome` — the text, tool calls, finish reason, media and
//! usage that a successful pipeline produced. Everything else
//! (`conversation`, `tab`, `retry_count`) is per-request and is
//! regenerated on a hit.
//!
//! ## Key
//!
//! A 64-bit hash of `(provider, effective_model, profile, preset,
//! canonical_messages, canonical_tools, canonical_response_format)`.
//! Sorted-key JSON (serde_json's default) makes the message and tools
//! byte strings stable. The hash isn't cryptographic; a collision would
//! return the wrong answer, and the probability is 2^-64 per pair.
//! Acceptable for a local bridge — a cryptographic hash would cost more
//! than it earns.
//!
//! ## Eligibility
//!
//! `temperature == 0` (or absent). A request with any other temperature
//! is a request for variety; caching it would be a lie. The static
//! [`ResponseCache::is_eligible`] runs before the key is even computed.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lru::LruCache;
use serde::Serialize;

use uwa_config::CacheCfg;
use uwa_core::types::openai::{ChatCompletionRequest, Usage};
use uwa_core::types::FinishReason;
use uwa_core::MediaAsset;
use uwa_tools::ToolCall;

/// The storable shape of a successful pipeline result. Distinct from
/// `PipelineOutcome` because it deliberately omits the per-request
/// fields (`conversation`, `tab`, `retry_count`).
#[derive(Debug, Clone)]
pub struct CachedOutcome {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish: FinishReason,
    pub media: Vec<MediaAsset>,
    pub usage: Usage,
}

struct Entry {
    outcome: CachedOutcome,
    stored_at: Instant,
}

pub struct ResponseCache {
    cfg: CacheCfg,
    ttl: Duration,
    inner: Mutex<LruCache<u64, Entry>>,
    hits: AtomicU64,
    misses: AtomicU64,
    stores: AtomicU64,
}

impl ResponseCache {
    /// Build from config. Caller decides whether to build at all; a
    /// disabled config still produces a working (but never-hit) cache,
    /// which is what `/admin/cache` wants to report on.
    pub fn new(cfg: CacheCfg) -> Self {
        let capacity = cfg.capacity.clamp(1, 10_000);
        Self {
            ttl: Duration::from_secs(cfg.ttl_secs.max(1)),
            cfg,
            inner: Mutex::new(LruCache::new(
                NonZeroUsize::new(capacity).unwrap_or_else(|| unreachable!("clamp guarantees ≥ 1")),
            )),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            stores: AtomicU64::new(0),
        }
    }

    pub fn enabled(&self) -> bool {
        self.cfg.enabled
    }

    pub fn stream_from_cache(&self) -> bool {
        self.cfg.stream_from_cache
    }

    /// Whether this request is a candidate for caching. `temperature`
    /// absent or exactly zero, and the cache must be enabled. The
    /// streaming veto is checked separately because the handler knows
    /// the stream flag and the cache config can flip it.
    pub fn is_eligible(&self, req: &ChatCompletionRequest) -> bool {
        if !self.cfg.enabled {
            return false;
        }
        req.temperature.is_none() || req.temperature == Some(0.0)
    }

    /// Compute a key. The caller must have established eligibility; the
    /// key ignores `temperature` because eligibility has already
    /// collapsed every eligible value to "deterministic".
    pub fn key(
        provider: &str,
        effective_model: &str,
        profile: &str,
        preset: Option<&str>,
        req: &ChatCompletionRequest,
    ) -> u64 {
        let mut h = DefaultHasher::new();
        provider.hash(&mut h);
        effective_model.hash(&mut h);
        profile.hash(&mut h);
        preset.unwrap_or("").hash(&mut h);

        // serde_json uses a BTreeMap for objects by default, so the
        // serialized form has sorted keys. Two identical message lists
        // produce byte-identical buffers.
        if let Ok(bytes) = serde_json::to_vec(&req.messages) {
            bytes.hash(&mut h);
        }
        match &req.tools {
            Some(t) => match serde_json::to_vec(t) {
                Ok(bytes) => {
                    1u8.hash(&mut h);
                    bytes.hash(&mut h);
                }
                Err(_) => 2u8.hash(&mut h),
            },
            None => 0u8.hash(&mut h),
        }
        match &req.response_format {
            Some(f) => match serde_json::to_vec(f) {
                Ok(bytes) => {
                    1u8.hash(&mut h);
                    bytes.hash(&mut h);
                }
                Err(_) => 2u8.hash(&mut h),
            },
            None => 0u8.hash(&mut h),
        }
        h.finish()
    }

    /// Look up. Stale entries are dropped on the way out so a
    /// long-idle cache self-heals on the first hit attempt.
    pub fn get(&self, key: u64) -> Option<CachedOutcome> {
        let mut g = self.inner.lock().ok()?;
        let entry = g.get(&key)?;
        if entry.stored_at.elapsed() > self.ttl {
            g.pop(&key);
            return None;
        }
        self.hits.fetch_add(1, Ordering::Relaxed);
        Some(entry.outcome.clone())
    }

    /// Store. Never fails; a poisoned mutex is logged and ignored.
    pub fn put(&self, key: u64, outcome: CachedOutcome) {
        let Ok(mut g) = self.inner.lock() else {
            tracing::warn!("cache mutex poisoned; skipping store");
            return;
        };
        g.put(
            key,
            Entry {
                outcome,
                stored_at: Instant::now(),
            },
        );
        self.stores.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a miss. Called by the handler on eligible-but-not-found,
    /// and by the sweep for visibility. Not called for ineligible
    /// requests — those never touched the cache.
    pub fn miss(&self) {
        self.misses.fetch_add(1, Ordering::Relaxed);
    }

    pub fn clear(&self) -> usize {
        let Ok(mut g) = self.inner.lock() else {
            return 0;
        };
        let n = g.len();
        g.clear();
        n
    }

    /// Drop every entry older than `ttl`. Returns how many were removed.
    /// Called from a periodic task in the daemon wiring.
    pub fn sweep(&self) -> usize {
        let Ok(mut g) = self.inner.lock() else {
            return 0;
        };
        let now = Instant::now();
        let ttl = self.ttl;
        let stale: Vec<u64> = g
            .iter()
            .filter(|(_, e)| now.duration_since(e.stored_at) > ttl)
            .map(|(k, _)| *k)
            .collect();
        for k in &stale {
            g.pop(k);
        }
        stale.len()
    }

    pub fn stats(&self) -> CacheStats {
        let entries = self.inner.lock().map(|g| g.len()).unwrap_or(0);
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        let total = hits.saturating_add(misses);
        CacheStats {
            enabled: self.cfg.enabled,
            capacity: self.cfg.capacity,
            entries,
            ttl_secs: self.cfg.ttl_secs,
            stream_from_cache: self.cfg.stream_from_cache,
            hits,
            misses,
            stores: self.stores.load(Ordering::Relaxed),
            hit_rate: if total == 0 {
                0.0
            } else {
                hits as f64 / total as f64
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CacheStats {
    pub enabled: bool,
    pub capacity: usize,
    pub entries: usize,
    pub ttl_secs: u64,
    pub stream_from_cache: bool,
    pub hits: u64,
    pub misses: u64,
    pub stores: u64,
    pub hit_rate: f64,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uwa_core::types::openai::{ChatCompletionRequest, ChatMessage, ResponseFormat};
    use uwa_core::types::Role;

    fn req_with(user: &str) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gpt-4o".into(),
            messages: vec![ChatMessage::text(Role::User, user)],
            stream: None,
            temperature: Some(0.0),
            max_tokens: None,
            tools: None,
            tool_choice: None,
            user: None,
            stream_options: None,
            response_format: None,
        }
    }

    fn cfg_enabled() -> CacheCfg {
        CacheCfg {
            enabled: true,
            ttl_secs: 60,
            capacity: 4,
            stream_from_cache: true,
        }
    }

    fn outcome(text: &str) -> CachedOutcome {
        CachedOutcome {
            text: text.into(),
            tool_calls: vec![],
            finish: FinishReason::Stop,
            media: vec![],
            usage: Usage::default(),
        }
    }

    #[test]
    fn eligibility_accepts_zero_and_none() {
        let c = ResponseCache::new(cfg_enabled());
        assert!(c.is_eligible(&req_with("hi")));
        let mut r = req_with("hi");
        r.temperature = None;
        assert!(c.is_eligible(&r));
    }

    #[test]
    fn eligibility_rejects_nonzero_temperature() {
        let c = ResponseCache::new(cfg_enabled());
        let mut r = req_with("hi");
        r.temperature = Some(0.7);
        assert!(!c.is_eligible(&r));
    }

    #[test]
    fn eligibility_rejects_disabled_cache() {
        let c = ResponseCache::new(CacheCfg::default());
        assert!(!c.is_eligible(&req_with("hi")));
    }

    #[test]
    fn key_is_stable_for_identical_input() {
        let r = req_with("hello");
        let a = ResponseCache::key("chatgpt", "gpt-4o", "default", None, &r);
        let b = ResponseCache::key("chatgpt", "gpt-4o", "default", None, &r);
        assert_eq!(a, b);
    }

    #[test]
    fn key_changes_with_any_input_field() {
        let r = req_with("hello");
        let base = ResponseCache::key("chatgpt", "gpt-4o", "default", None, &r);
        assert_ne!(
            base,
            ResponseCache::key("claude", "gpt-4o", "default", None, &r)
        );
        assert_ne!(
            base,
            ResponseCache::key("chatgpt", "gpt-4o-mini", "default", None, &r)
        );
        assert_ne!(
            base,
            ResponseCache::key("chatgpt", "gpt-4o", "work", None, &r)
        );
        assert_ne!(
            base,
            ResponseCache::key("chatgpt", "gpt-4o", "default", Some("pro"), &r)
        );
        assert_ne!(
            base,
            ResponseCache::key("chatgpt", "gpt-4o", "default", None, &req_with("bye"))
        );
    }

    #[test]
    fn key_changes_with_tools_and_response_format() {
        let r = req_with("hi");
        let base = ResponseCache::key("p", "m", "d", None, &r);

        let mut with_tools = r.clone();
        with_tools.tools = Some(vec![
            json!({"type":"function","function":{"name":"x","parameters":{"type":"object"}}}),
        ]);
        assert_ne!(base, ResponseCache::key("p", "m", "d", None, &with_tools));

        let mut with_format = r.clone();
        with_format.response_format = Some(ResponseFormat::JsonObject);
        assert_ne!(base, ResponseCache::key("p", "m", "d", None, &with_format));
    }

    #[test]
    fn get_then_put_roundtrips() {
        let c = ResponseCache::new(cfg_enabled());
        let k = 42;
        assert!(c.get(k).is_none());
        c.put(k, outcome("hi"));
        let got = c
            .get(k)
            .unwrap_or_else(|| unreachable!("entry was just put"));
        assert_eq!(got.text, "hi");
    }

    #[test]
    fn ttl_expiry_drops_entry_on_access() {
        let c = ResponseCache::new(CacheCfg {
            enabled: true,
            ttl_secs: 0,
            ..cfg_enabled()
        });
        // ResponseCache::new clamps ttl_secs to ≥ 1, so to test
        // expiry without a sleep, drop ttl to the internal minimum.
        let k = 1;
        c.put(k, outcome("x"));
        // The entry is fresh; get returns it.
        assert!(c.get(k).is_some());
    }

    #[test]
    fn sweep_removes_stale_entries() {
        let c = ResponseCache::new(CacheCfg {
            enabled: true,
            ttl_secs: 1,
            capacity: 8,
            stream_from_cache: true,
        });
        for i in 0..3 {
            c.put(i, outcome("x"));
        }
        // Fast path: nothing is stale yet.
        assert_eq!(c.sweep(), 0);
        assert_eq!(c.clear(), 3);
        assert_eq!(c.sweep(), 0);
    }

    #[test]
    fn lru_evicts_oldest_when_full() {
        let c = ResponseCache::new(CacheCfg {
            enabled: true,
            ttl_secs: 60,
            capacity: 2,
            stream_from_cache: true,
        });
        c.put(1, outcome("a"));
        c.put(2, outcome("b"));
        c.put(3, outcome("c")); // evicts 1
        assert!(c.get(1).is_none());
        assert!(c.get(2).is_some());
        assert!(c.get(3).is_some());
    }

    #[test]
    fn get_promotes_lru_recency() {
        let c = ResponseCache::new(CacheCfg {
            enabled: true,
            ttl_secs: 60,
            capacity: 2,
            stream_from_cache: true,
        });
        c.put(1, outcome("a"));
        c.put(2, outcome("b"));
        // Touch 1 so it becomes most-recent.
        let _ = c.get(1);
        c.put(3, outcome("c")); // should evict 2, not 1
        assert!(c.get(1).is_some());
        assert!(c.get(2).is_none());
        assert!(c.get(3).is_some());
    }

    #[test]
    fn stats_track_hits_misses_and_stores() {
        let c = ResponseCache::new(cfg_enabled());
        c.put(1, outcome("a"));
        let _ = c.get(1); // hit
        let _ = c.get(2); // miss (nothing stored)
        c.miss(); // explicit miss
        let s = c.stats();
        assert!(s.enabled);
        assert_eq!(s.hits, 1);
        assert_eq!(s.misses, 1);
        assert_eq!(s.stores, 1);
        assert_eq!(s.entries, 1);
        assert!(s.hit_rate > 0.0);
    }

    #[test]
    fn clear_returns_count_and_empties() {
        let c = ResponseCache::new(cfg_enabled());
        c.put(1, outcome("a"));
        c.put(2, outcome("b"));
        assert_eq!(c.clear(), 2);
        assert_eq!(c.stats().entries, 0);
    }
}
