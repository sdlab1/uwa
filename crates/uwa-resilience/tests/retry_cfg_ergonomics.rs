//! `RetryCfg` is `#[non_exhaustive]`: callers outside `uwa-resilience`
//! cannot use struct literals at all (E0639 fires even with
//! `..Default::default()`), so new knobs can never break them. This file
//! pins the two sanctioned construction patterns.

use std::time::Duration;
use uwa_resilience::{retry, RetryCfg};

/// The constructor: attempts override, default backoff schedule.
#[tokio::test(start_paused = true)]
async fn constructor_overrides_attempts_only() {
    let cfg = RetryCfg::new(2);
    assert_eq!(cfg.attempts, 2);
    assert_eq!(cfg.base, Duration::from_millis(100));
    let r: Result<(), uwa_core::UwaError> = retry(&cfg, || async {
        Err(uwa_core::UwaError::Transport("nope".into()))
    })
    .await;
    assert!(r.is_err());
}

/// `Default::default()` plus public-field mutation — the fully custom form.
#[tokio::test(start_paused = true)]
async fn default_then_field_mutation() {
    let mut cfg = RetryCfg::default();
    cfg.attempts = 3;
    cfg.base = Duration::from_millis(10);
    cfg.max = Duration::from_millis(100);
    let r: Result<(), uwa_core::UwaError> = retry(&cfg, || async {
        Err(uwa_core::UwaError::Transport("nope".into()))
    })
    .await;
    assert!(r.is_err());
}
