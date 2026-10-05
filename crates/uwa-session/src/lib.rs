//! # uwa-session
//!
//! Conversation-scoped browser tab leasing with idle eviction.
//!
//! ## Passport (public API)
//! - [`conversation_id`] — stable id from the first messages of a thread
//! - [`SessionManager`], [`SessionCfg`], [`SessionHandle`], [`SessionInfo`]

mod hash;
mod manager;

pub use hash::conversation_id;
pub use manager::{SessionCfg, SessionHandle, SessionInfo, SessionManager};
