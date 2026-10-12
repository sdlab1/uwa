//! # uwa-session
//!
//! Conversation-scoped browser tab leasing with idle eviction.
//!
//! ## Passport (public API)
//! - [`conversation_id`] — stable id from the first messages of a thread
//! - [`SessionManager`], [`SessionCfg`], [`SessionHandle`], [`SessionInfo`]
//! - [`pinning`] — what the tab has already seen; delta vs full sends

mod hash;
mod manager;
pub mod pinning;

pub use hash::conversation_id;
pub use manager::{SessionCfg, SessionHandle, SessionInfo, SessionManager};
pub use pinning::{
    decide, hash_messages, hash_system, ConversationState, PinDecision, PinningStore,
};
