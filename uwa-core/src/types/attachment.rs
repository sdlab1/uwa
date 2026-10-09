//! Multimodal attachment resolved to a local file, ready to be attached
//! to a web input. Lives in `uwa-core` (not `uwa-providers`) so the
//! `SiteProvider` trait can reference it without a circular dependency.

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Attachment {
    /// Local temp-file path with the decoded bytes.
    pub path: PathBuf,
    /// File name (also the base of `path`).
    pub name: String,
    /// Best-effort MIME type (`image/png`, ...).
    pub mime: String,
    /// Byte size.
    pub size: u64,
}
