//! Cross-crate contracts. Everything that touches the browser or a
//! provider MUST go through these traits so we can unit-test with mocks.

use crate::error::{Result, UwaError};
use crate::ids::TabId;
use crate::types::media::MediaAsset;
use async_trait::async_trait;
use futures::stream::Stream;
use serde_json::Value;
use std::pin::Pin;
use url::Url;

/// Streaming response body: a sequence of text chunks. Ends when the
/// provider considers the response complete. The caller accumulates the
/// concatenation as the final text.
pub type ResponseStream = Pin<Box<dyn Stream<Item = Result<String>> + Send>>;

/// A logical browser page we can drive. Abstracts over `chaser-oxide`.
#[async_trait]
pub trait Page: Send + Sync {
    async fn goto(&self, url: &Url) -> Result<()>;
    async fn url(&self) -> Result<Url>;
    /// Evaluate JS in the page and return the JSON-decoded result.
    async fn eval(&self, js: &str) -> Result<Value>;
    /// Wait for a CSS selector to appear; returns Err(Timeout) if not.
    async fn wait_for_selector(&self, selector: &str, timeout: std::time::Duration) -> Result<()>;
    /// Return the current HTML of the page.
    async fn html(&self) -> Result<String>;
    /// Click an element by CSS selector.
    async fn click(&self, selector: &str) -> Result<()>;
    /// Type text into an input matched by CSS selector.
    async fn type_text(&self, selector: &str, text: &str) -> Result<()>;
    /// Subscribe to CDP network events (SSE/JSON bodies).
    async fn network_events(&self) -> Result<tokio::sync::broadcast::Receiver<NetworkEvent>>;
    /// Register a script that runs on every navigation *before* page JS.
    /// Default: no-op (test mocks don't need stealth).
    async fn eval_early(&self, _js: &str) -> Result<()> {
        Ok(())
    }
    /// Return the frame tree as (frame_id, url) pairs, root first.
    /// Default: empty (test mocks don't have frames).
    async fn frame_tree(&self) -> Result<Vec<(String, String)>> {
        Ok(Vec::new())
    }
    /// Evaluate JS inside a specific frame (including cross-origin OOPIFs).
    /// `frame_id` is the CDP frame ID; the implementation routes to the
    /// correct CDP session / execution context.
    /// Default: error (only the CDP transport supports frames).
    async fn eval_in_frame(&self, frame_id: &str, _js: &str) -> Result<Value> {
        Err(crate::error::UwaError::Transport(format!(
            "eval_in_frame(frame `{frame_id}`) requires a real browser transport"
        )))
    }

    /// Start capturing audio from any `<audio>`/`<video>` that plays from
    /// now on. Injects a Web Audio API hook. Default: no-op.
    async fn start_audio_capture(&self) -> Result<()> {
        Ok(())
    }

    /// Stop capturing and return all accumulated audio as a single blob
    /// (webm/opus). Returns `None` if nothing was captured.
    async fn stop_audio_capture(&self) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    /// Scan the page for `<video>` and `<audio>` sources.
    /// Default: empty (mocks have no media).
    async fn scan_media(&self) -> Result<Vec<MediaResource>> {
        Ok(Vec::new())
    }

    /// Collect media assets (images, canvas snapshots, audio, video)
    /// inside the last assistant message, if the selector identifies
    /// one. Returns an empty vec when the container has no media.
    ///
    /// `max_assets` caps the response; a chatty page with hundreds of
    /// avatars shouldn't flood the response. `max_bytes` is a hint for
    /// inline `data:` assets — the implementation may refuse to
    /// materialize one larger than this.
    ///
    /// Default: empty (test mocks, providers without media support).
    async fn collect_media(
        &self,
        _container_selector: &str,
        _max_assets: usize,
        _max_bytes: u64,
    ) -> Result<Vec<MediaAsset>> {
        Ok(Vec::new())
    }
}

/// One detected media element on a page (for scan_media).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MediaResource {
    pub kind: MediaSourceKind,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
}

/// Kind of media element found by `Page::scan_media`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaSourceKind {
    Audio,
    Video,
}

/// A network event relevant to response extraction.
#[derive(Debug, Clone)]
pub enum NetworkEvent {
    ResponseBody {
        /// CDP request id, so a `Finished` for the same request can be told
        /// apart from an unrelated one (favicon, XHR, …).
        request_id: String,
        url: String,
        body: String,
        mime: String,
        /// Cloudflare documents this response header as the definitive
        /// marker for an interstitial challenge: `cf-mitigated: challenge`.
        cf_mitigated_challenge: bool,
    },
    Finished {
        request_id: String,
    },
}

/// Site adapter: everything provider-specific lives behind this trait.
#[async_trait]
pub trait SiteProvider: Send + Sync {
    /// Stable identifier, e.g. `"chatgpt"`.
    fn name(&self) -> &str;

    /// Whether this provider can handle the given page URL.
    fn matches(&self, url: &Url) -> bool;

    /// Send the user's message into the currently-open conversation.
    async fn send_message(&self, page: &dyn Page, text: &str) -> Result<()>;

    /// Multimodal-aware send. Default: ignore attachments, delegate to
    /// [`SiteProvider::send_message`].
    async fn send_multimodal(
        &self,
        page: &dyn Page,
        text: &str,
        attachments: &[crate::types::Attachment],
    ) -> Result<()> {
        let _ = attachments;
        self.send_message(page, text).await
    }

    /// Block until the assistant's answer for the last message is complete.
    /// Returns the extracted, normalized text.
    async fn wait_response(&self, page: &dyn Page) -> Result<String>;

    /// Stream response deltas as they arrive from the browser.
    ///
    /// Default implementation buffers via [`wait_response`](Self::wait_response)
    /// and yields the whole text as one chunk. Providers with a network-first
    /// extraction strategy override this to emit real deltas — enabling
    /// token-by-token SSE at the HTTP edge and true TTFT tracking.
    ///
    /// Media housekeeping (audio stop, video scan) is not run by this path;
    /// call `wait_response` when that side effect is needed.
    async fn wait_response_stream(&self, page: &dyn Page) -> Result<ResponseStream> {
        let text = self.wait_response(page).await?;
        Ok(Box::pin(futures::stream::iter(vec![Ok(text)])))
    }

    /// Best-effort cancellation (click "Stop").
    async fn cancel(&self, page: &dyn Page) -> Result<()>;

    /// Capabilities advertised via `/v1/models`.
    fn capabilities(&self) -> Capabilities;
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Capabilities {
    pub streams: bool,
    pub tool_calls: bool,
    pub vision: bool,
    pub max_context_tokens: Option<u32>,
}

/// Factory that produces `Page`s from a `TabId` (implemented by uwa-browser).
#[async_trait]
pub trait Transport: Send + Sync {
    async fn page(&self, tab: &TabId) -> Result<Box<dyn Page>>;
    async fn list_tabs(&self) -> Result<Vec<TabId>>;
    async fn health(&self, tab: &TabId) -> Result<()>;
    /// Graceful shutdown: close tabs, stop sidecars, release resources.
    /// Default implementation is a no-op for mock/test transports.
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    // ---- profile support (default: unsupported) ----

    /// Whether this transport honours profile names. When `false`, any
    /// non-default profile is rejected at the routing layer before we
    /// get here, so the methods below are never called for a real
    /// non-default profile.
    fn supports_profiles(&self) -> bool {
        false
    }

    /// Open a new tab inside the named profile's browser context and
    /// navigate it to `url`. Called lazily — the first time a session
    /// acquires a profile that has no tabs yet.
    async fn open_tab(&self, _profile: &str, _url: &str) -> Result<TabId> {
        Err(UwaError::Config(
            "this transport does not support browser profiles".into(),
        ))
    }

    /// Which profile owns this tab. `None` means the transport doesn't
    /// track profiles at all (default behaviour).
    async fn tab_profile(&self, _tab: &TabId) -> Result<Option<String>> {
        Ok(None)
    }

    /// Tabs belonging to a profile.
    ///
    /// The default implementation uses `tab_profile`; when the transport
    /// doesn't track profiles, only the literal name `"default"` returns
    /// anything (all tabs), and every other name returns an empty list —
    /// which is exactly the behaviour we want for a transport with no
    /// profile support.
    async fn list_tabs_in(&self, profile: &str) -> Result<Vec<TabId>> {
        if !self.supports_profiles() {
            return if profile == "default" {
                self.list_tabs().await
            } else {
                Ok(Vec::new())
            };
        }
        let all = self.list_tabs().await?;
        let mut out = Vec::new();
        for t in all {
            if self.tab_profile(&t).await?.as_deref() == Some(profile) {
                out.push(t);
            }
        }
        Ok(out)
    }

    /// Drop a profile: close its browser context and remove every tab
    /// that belonged to it. Default: no-op (nothing to drop).
    async fn close_profile(&self, _profile: &str) -> Result<Vec<TabId>> {
        Ok(Vec::new())
    }
}

/// One tool definition, decoupled from MCP/OpenAI formats.
/// `uwa-tools` has the OpenAI <-> this converters.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for arguments.
    pub parameters: Value,
}
