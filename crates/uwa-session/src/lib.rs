//! # uwa-session
//!
//! Conversation-scoped browser tab leasing with idle eviction.
//!
//! ## Passport (public API)
//! - [`conversation_id`] — stable id from the first messages of a thread
//! - [`SessionManager`], [`SessionCfg`], [`SessionHandle`], [`SessionInfo`]
//! - [`pinning`] — what the tab has already seen; delta vs full sends
//!
//! ## Issue tracker (migrated from docs/ISSUES_AND_BRIDGE.md)
//!
//! ### UWA-115 ✅ Slow test binary — closed
//!
//! `cargo test -p uwa-session --lib` runs all 61 tests in ~0.16s
//! (pinning suite 13 tests in ~0.04s). The previously-reported ~140s
//! `max_wait_ms = 1000` mock wait is no longer present; no mock-time
//! compression or shorter wait needed.
//!
//! Full hardening history: see the module docs of `crate` (uwa-api),
//! Part 1 — Issue tracker, UWA-115 entry.

mod hash;
mod manager;
pub mod pinning;
mod profile;

pub use hash::conversation_id;
pub use manager::{SessionCfg, SessionHandle, SessionInfo, SessionManager};
pub use pinning::{
    decide, hash_messages, hash_system, ConversationState, PinDecision, PinningStore,
};
pub use profile::ProfileName;
