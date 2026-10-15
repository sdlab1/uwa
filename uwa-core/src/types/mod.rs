//! Type definitions shared across the workspace.

pub mod anthropic;
pub mod attachment;
pub mod media;
pub mod openai;

pub use anthropic::*;
pub use attachment::Attachment;
pub use media::{MediaAsset, MediaKind, MediaSummary};
pub use openai::*;

// Re-export commonly used types from openai module
pub use openai::{stream_chunks, FinishReason, Role, STREAM_CHUNK_CHARS};
