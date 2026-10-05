//! # uwa-core
//!
//! Foundation crate: types, errors, IDs and the traits that every other
//! crate in the workspace depends on.
//!
//! ## Passport (public API)
//! - [`error::UwaError`], [`error::Result`]
//! - [`ids::{RequestId, SessionId, TabId, ConversationId}`]
//! - [`types::openai`], [`types::anthropic`]
//! - [`net::{NetRules, NetDecoder, ExtractionStrategy, FinisherTuning}`]
//! - [`traits::{Page, SiteProvider, Transport, NetworkEvent, Capabilities}`]
//!
//! Nothing in this crate may depend on `axum`, `chromiumoxide`, `reqwest`
//! or any other transport implementation. It is pure contract.

pub mod error;
pub mod ids;
pub mod net;
pub mod traits;
pub mod types;

pub use error::{Result, UwaError};
pub use ids::{ConversationId, RequestId, SessionId, TabId};
pub use net::{ExtractionStrategy, FinisherTuning, NetDecoder, NetRules};
pub use traits::{Capabilities, NetworkEvent, Page, SiteProvider, Transport};
