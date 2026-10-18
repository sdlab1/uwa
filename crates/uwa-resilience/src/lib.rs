//! # uwa-resilience
//!
//! Per-provider concurrency limits, circuit breakers and retries.
//!
//! ## Passport (public API)
//! - [`semaphore::ProviderSemaphores`]
//! - [`circuit::{CircuitBreaker, CircuitCfg, CircuitState}`]
//! - [`retry::{RetryCfg, retry}`]

pub mod circuit;
pub mod rate_limit;
pub mod retry;
pub mod semaphore;

pub use circuit::{CircuitBreaker, CircuitCfg, CircuitState};
pub use rate_limit::{ProviderRateLimiters, RateDecision, RateLimiter, RateSnapshot};
pub use retry::{retry, RetryCfg};
pub use semaphore::ProviderSemaphores;
