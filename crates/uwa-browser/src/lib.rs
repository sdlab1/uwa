//! # uwa-browser
//!
//! Chromium/CDP layer: connect to an external browser, own a pool of tabs,
//! expose them as `uwa_core::{Transport, Page}`.
//!
//! ## Passport (public API)
//! - `CdpTransport::connect(ws_url, idle_ttl)` — connect to a running Chromium
//! - `CdpTransport::pool() -> Arc<TabPool>`
//! - `TabPool` — round-robin, per-tab mutex, health, LRU/TTL eviction

pub mod page;
pub mod tabpool;
pub mod transport;

pub use tabpool::{TabGuard, TabPool};
pub use transport::CdpTransport;
