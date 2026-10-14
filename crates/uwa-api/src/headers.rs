//! HTTP header names and values used throughout the application.

use axum::http::HeaderName;

pub const CACHE: HeaderName = /* "x-uwa-cache" */;
pub const CACHE_HIT: &str = "hit";
pub const CACHE_MISS: &str = "miss";

pub const CHALLENGE: HeaderName = /* "x-uwa-challenge" */;
pub const CHALLENGE_NONE: &str = "none";
pub const CHALLENGE_CLEARED: &str = "cleared";

pub const RATE_LIMIT: HeaderName = /* "x-uwa-rate-limit" */;
pub const RATE_LIMIT_OK: &str = "ok";
pub const RATE_LIMIT_EXCEEDED: &str = "exceeded";
