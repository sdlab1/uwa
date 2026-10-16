//! Media assets produced *by* the assistant: images, audio, video,
//! canvas snapshots. Distinct from `Attachment` (input side): attachments
//! are decoded, resolved to temp files, and handed to the provider;
//! `MediaAsset`s are collected from the assistant container after a
//! response, optionally persisted, and returned to the client.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// What kind of asset this is. `Image` and `Canvas` are kept separate
/// because the source URL shape differs: `Canvas` only ever comes from
/// `canvas.toDataURL()` in the page, and a client may want to render it
/// differently from a fetched image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Image,
    Audio,
    Video,
    Canvas,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Canvas => "canvas",
        }
    }

    /// Default file extension for a saved asset when the mime doesn't
    /// resolve one. Chosen to match the browser's own `data:` defaults.
    pub fn default_extension(self) -> &'static str {
        match self {
            Self::Image => "png",
            Self::Audio => "webm",
            Self::Video => "mp4",
            Self::Canvas => "png",
        }
    }
}

/// One asset collected from the response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaAsset {
    pub kind: MediaKind,
    /// `data:`, `blob:`, `http(s):`, or a relative URL served by UWA
    /// when the asset was saved to disk (`/admin/media/<id>/<name>`).
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// Populated when the asset was written to disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_path: Option<PathBuf>,
    /// `alt` text from an `<img>`, or the `<video>`'s `title`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
    /// Free-form source marker: `"img"`, `"canvas"`, `"video"`,
    /// `"video-source"`, `"audio"`. Useful for debugging bad
    /// extractions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Compact form used in history records and stats. Drops `local_path`
/// (filesystem detail) and `size_bytes` when it's an order-of-magnitude
/// estimate from a `data:` URI rather than a real file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaSummary {
    pub kind: MediaKind,
    pub mime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
}

impl From<&MediaAsset> for MediaSummary {
    fn from(a: &MediaAsset) -> Self {
        Self {
            kind: a.kind,
            mime: a.mime.clone(),
            size_bytes: a.size_bytes,
            alt: a.alt.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_kind_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&MediaKind::Image).unwrap_or_else(|_| unreachable!()),
            "\"image\""
        );
        assert_eq!(
            serde_json::to_string(&MediaKind::Canvas).unwrap_or_else(|_| unreachable!()),
            "\"canvas\""
        );
    }

    #[test]
    fn asset_roundtrips() {
        let a = MediaAsset {
            kind: MediaKind::Image,
            url: "/admin/media/abc/pic.png".into(),
            mime: Some("image/png".into()),
            size_bytes: Some(2048),
            local_path: Some(PathBuf::from("/tmp/pic.png")),
            alt: Some("a red square".into()),
            source: Some("img".into()),
        };
        let j = serde_json::to_string(&a).unwrap_or_else(|_| unreachable!());
        let back: MediaAsset = serde_json::from_str(&j).unwrap_or_else(|_| unreachable!());
        assert_eq!(back.kind, MediaKind::Image);
        assert_eq!(back.size_bytes, Some(2048));
    }

    #[test]
    fn summary_drops_local_path() {
        let a = MediaAsset {
            kind: MediaKind::Audio,
            url: "data:audio/webm;base64,AAAA".into(),
            mime: Some("audio/webm".into()),
            size_bytes: Some(3),
            local_path: Some(PathBuf::from("/tmp/x")),
            alt: None,
            source: Some("audio".into()),
        };
        let s: MediaSummary = (&a).into();
        assert_eq!(s.kind, MediaKind::Audio);
        assert_eq!(s.size_bytes, Some(3));
    }
}
