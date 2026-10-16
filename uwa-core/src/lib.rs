//! # uwa-core
//!
//! Foundation crate: types, errors, IDs and the traits that every other
//! crate in the workspace depends on.
//!
//! ## Passport (public API)
//! - [`error::UwaError`], [`error::Result`]
//! - [`ids::{RequestId, SessionId, TabId, ConversationId}`]
//! - [`types::openai`], [`types::anthropic`], [`types::media`]
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
pub mod workflow;

pub use error::{ModelNotFound, Result, UwaError};
pub use ids::{ConversationId, RequestId, SessionId, TabId};
pub use net::{ExtractionStrategy, FinisherTuning, NetDecoder, NetRules};
pub use traits::{
    Capabilities, MediaResource, MediaSourceKind, NetworkEvent, Page, ResponseStream, SiteProvider,
    TappedEvent, ToolSpec, Transport,
};
pub use types::media::{MediaAsset, MediaKind, MediaSummary};
