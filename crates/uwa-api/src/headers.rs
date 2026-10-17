//! UWA extension response headers.
//!
//! Every chat-shaped response carries these. They give a client handles
//! it can act on without parsing the body:
//!
//! * `x-uwa-conversation` — the [`ConversationId`] the request pinned
//!   to. Stable across the turns of one conversation; usable as the key
//!   for `DELETE /admin/sessions/:id`.
//! * `x-uwa-retry-count` — how many corrective sends the pipeline issued
//!   (tool-schema retries, JSON-mode retries). `0` on the common path.
//! * `x-uwa-cache` — `hit` / `miss`. Always `miss` today; the header is
//!   part of the contract so clients can switch on it when caching ships.
//! * `x-uwa-tab` — the tab the request ran on. Feeds the dashboard's
//!   "jump to events for this request" flow.
//!
//! **Streaming caveat.** On SSE responses only `x-uwa-conversation` and
//! `x-uwa-cache` are set as HTTP headers — retries and the tab aren't
//! known until the stream has already started, and you can't set HTTP
//! headers after the body begins. They arrive instead in the final SSE
//! chunk under a `uwa_meta` field. See `docs/HEADERS.md`.

use axum::http::{HeaderName, HeaderValue};
use axum::response::Response;

use uwa_core::ConversationId;

pub const CONVERSATION: HeaderName = HeaderName::from_static("x-uwa-conversation");
pub const RETRY_COUNT: HeaderName = HeaderName::from_static("x-uwa-retry-count");
pub const CACHE: HeaderName = HeaderName::from_static("x-uwa-cache");
pub const TAB: HeaderName = HeaderName::from_static("x-uwa-tab");

pub const CACHE_HIT: &str = "hit";
pub const CACHE_MISS: &str = "miss";

/// Everything the HTTP layer needs to stamp onto a response.
#[derive(Debug, Clone)]
pub struct UwaMeta {
    pub conversation: ConversationId,
    pub retry_count: u32,
    pub cache: &'static str,
    pub tab: Option<String>,
}

impl UwaMeta {
    /// Stream-time metadata: only what's known before the pipeline runs.
    /// Retry count and tab come later in the SSE stream.
    pub fn stream_prefix(conversation: ConversationId) -> Self {
        Self {
            conversation,
            retry_count: 0,
            cache: CACHE_MISS,
            tab: None,
        }
    }

    /// Non-stream final metadata — everything set.
    pub fn complete(
        conversation: ConversationId,
        retry_count: u32,
        tab: Option<String>,
    ) -> Self {
        Self {
            conversation,
            retry_count,
            cache: CACHE_MISS,
            tab,
        }
    }

    /// The JSON object that rides in the final SSE chunk's `uwa_meta`
    /// field. Only the fields a stream can't get from headers.
    pub fn to_stream_json(&self) -> serde_json::Value {
        serde_json::json!({
            "retry_count": self.retry_count,
            "tab": self.tab,
        })
    }
}

/// Attach headers. Values that can't be encoded are silently skipped — a
/// malformed id is not worth failing the response over.
pub fn attach(mut resp: Response, meta: &UwaMeta) -> Response {
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(meta.conversation.as_str()) {
        h.insert(CONVERSATION, v);
    }
    if let Ok(v) = HeaderValue::from_str(&meta.retry_count.to_string()) {
        h.insert(RETRY_COUNT, v);
    }
    if let Ok(v) = HeaderValue::from_str(meta.cache) {
        h.insert(CACHE, v);
    }
    if let Some(t) = &meta.tab {
        if let Ok(v) = HeaderValue::from_str(t) {
            h.insert(TAB, v);
        }
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;

    fn empty_response() -> Response {
        Response::builder()
            .status(200)
            .body(Body::empty())
            .unwrap_or_else(|_| Response::new(Body::empty()))
    }

    fn header(h: &axum::http::HeaderMap, name: HeaderName) -> Option<String> {
        match h.get(name) {
            Some(v) => v.to_str().ok().map(|s| s.to_string()),
            None => None,
        }
    }

    #[test]
    fn attach_sets_every_header_when_present() {
        let meta = UwaMeta::complete(
            ConversationId::from_raw("conv_abc"),
            2,
            Some("tab_xyz".into()),
        );
        let r = attach(empty_response(), &meta);
        let h = r.headers();
        assert_eq!(header(&h, CONVERSATION).as_deref(), Some("conv_abc"));
        assert_eq!(header(&h, RETRY_COUNT).as_deref(), Some("2"));
        assert_eq!(header(&h, CACHE).as_deref(), Some("miss"));
        assert_eq!(header(&h, TAB).as_deref(), Some("tab_xyz"));
    }

    #[test]
    fn attach_skips_tab_when_absent() {
        let meta = UwaMeta::complete(ConversationId::from_raw("conv_a"), 0, None);
        let r = attach(empty_response(), &meta);
        assert!(r.headers().get(TAB).is_none());
        assert_eq!(header(&r.headers(), RETRY_COUNT).as_deref(), Some("0"));
    }

    #[test]
    fn stream_prefix_omits_retry_and_tab() {
        let m = UwaMeta::stream_prefix(ConversationId::from_raw("conv_s"));
        let r = attach(empty_response(), &m);
        assert_eq!(header(&r.headers(), CONVERSATION).as_deref(), Some("conv_s"));
        assert_eq!(header(&r.headers(), CACHE).as_deref(), Some("miss"));
        assert!(r.headers().get(TAB).is_none());
    }

    #[test]
    fn stream_json_has_only_the_stream_specific_fields() {
        let m = UwaMeta {
            conversation: ConversationId::from_raw("conv_s"),
            retry_count: 3,
            cache: CACHE_MISS,
            tab: Some("tab_z".into()),
        };
        let v = m.to_stream_json();
        assert_eq!(
            v.get("retry_count").and_then(|v| v.as_u64()).unwrap_or(999),
            3
        );
        assert_eq!(
            v.get("tab").and_then(|v| v.as_str()).unwrap_or(""),
            "tab_z"
        );
        assert!(v.get("conversation").is_none());
        assert!(v.get("cache").is_none());
    }
}
