//! Response caching.
//!
//! Opt-in. Off by default. A cache hit returns the previous answer
//! verbatim, so it must only apply to requests that would produce the
//! same answer — hence the `temperature == 0` gate. A request with
//! default sampling bypasses the cache; sampling is a request for
//! variety.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheCfg {
    #[serde(default)]
    pub enabled: bool,

    /// How long an entry lives before it's considered stale.
    #[serde(default = "default_ttl_secs")]
    pub ttl_secs: u64,

    /// LRU capacity. A chatty client can keep up to this many distinct
    /// requests warm.
    #[serde(default = "default_capacity")]
    pub capacity: usize,

    /// Serve `stream: true` requests from the cache. When `false`,
    /// streaming requests always go through the pipeline, so a client
    /// that wants real TTFT numbers gets them. Non-stream clients still
    /// benefit from entries the streaming path stored earlier.
    #[serde(default = "default_stream_from_cache")]
    pub stream_from_cache: bool,
}

fn default_ttl_secs() -> u64 {
    300
}
fn default_capacity() -> usize {
    256
}
fn default_stream_from_cache() -> bool {
    true
}

impl Default for CacheCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            ttl_secs: default_ttl_secs(),
            capacity: default_capacity(),
            stream_from_cache: default_stream_from_cache(),
        }
    }
}

impl CacheCfg {
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        if self.ttl_secs == 0 {
            return Err("cache.ttl_secs must be ≥ 1".into());
        }
        if self.ttl_secs > 86_400 {
            return Err("cache.ttl_secs must not exceed 24h (86400)".into());
        }
        if self.capacity == 0 {
            return Err("cache.capacity must be ≥ 1".into());
        }
        if self.capacity > 10_000 {
            return Err("cache.capacity is unreasonably large (> 10000)".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_by_default() {
        let c = CacheCfg::default();
        assert!(!c.enabled);
        assert_eq!(c.ttl_secs, 300);
        assert_eq!(c.capacity, 256);
        assert!(c.stream_from_cache);
    }

    #[test]
    fn disabled_config_skips_validation() {
        // Zero values are fine when the cache is off — nobody reads them.
        let c = CacheCfg {
            enabled: false,
            ttl_secs: 0,
            capacity: 0,
            stream_from_cache: false,
        };
        assert!(c.validate().is_ok());
    }

    #[test]
    fn enabled_config_validates_ranges() {
        assert!(CacheCfg {
            enabled: true,
            ttl_secs: 0,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(CacheCfg {
            enabled: true,
            ttl_secs: 86_401,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(CacheCfg {
            enabled: true,
            capacity: 0,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(CacheCfg {
            enabled: true,
            capacity: 20_000,
            ..Default::default()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn enabled_default_is_valid() {
        let c = CacheCfg {
            enabled: true,
            ..Default::default()
        };
        assert!(c.validate().is_ok());
    }
}
