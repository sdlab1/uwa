//! Routing hints extracted from paths/headers.
//!
//! The endpoint order in the router is important: `/url/{domain}/v1/...`
//! and `/tab/{id}/v1/...` are merged **before** the plain `/v1/...` routes
//! so that axum's matcher resolves path parameters first.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use uwa_core::{ConversationId, TabId, UwaError};

/// Where to send the request.
#[derive(Debug, Clone, Default)]
pub struct RoutingHint {
    /// Provider name, e.g. "chatgpt". Overrides `model_aliases`.
    pub provider: Option<String>,
    /// Pin to a specific tab. Overrides session routing.
    pub tab: Option<TabId>,
    /// Pin a specific conversation id. Overrides the hash-based default.
    pub conversation: Option<ConversationId>,
}

impl RoutingHint {
    pub fn is_empty(&self) -> bool {
        self.provider.is_none() && self.tab.is_none() && self.conversation.is_none()
    }
}

/// Extract a `RoutingHint` from request extensions (set by middleware)
/// or from the `X-UWA-Provider` header.
pub struct HintExtractor(pub RoutingHint);

#[axum::async_trait]
impl<S: Send + Sync> FromRequestParts<S> for HintExtractor {
    type Rejection = crate::error::ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        // Path-provided hints (from middleware) take priority.
        if let Some(hint) = parts.extensions.get::<RoutingHint>() {
            return Ok(HintExtractor(hint.clone()));
        }
        // Fall back to header.
        let provider = parts
            .headers
            .get("x-uwa-provider")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        Ok(HintExtractor(RoutingHint {
            provider,
            tab: None,
            conversation: None,
        }))
    }
}

impl HintExtractor {
    pub fn provider(&self) -> Option<&str> {
        self.0.provider.as_deref()
    }
    pub fn tab(&self) -> Option<&TabId> {
        self.0.tab.as_ref()
    }
    pub fn conversation(&self) -> Option<&ConversationId> {
        self.0.conversation.as_ref()
    }
}

// ---------- path extractors ----------

/// `/url/{domain}/...` → `RoutingHint::provider`.
pub async fn from_url_path(
    axum::extract::Path(domain): axum::extract::Path<String>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let mut hint = req
        .extensions()
        .get::<RoutingHint>()
        .cloned()
        .unwrap_or_default();
    // Match domain → provider by config later; store the raw for now.
    hint.provider = Some(format!("url:{domain}"));
    req.extensions_mut().insert(hint);
    next.run(req).await
}

/// `/tab/{tab_id}/...` → `RoutingHint::tab`.
pub async fn from_tab_path(
    axum::extract::Path(tab_id): axum::extract::Path<String>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let mut hint = req
        .extensions()
        .get::<RoutingHint>()
        .cloned()
        .unwrap_or_default();
    hint.tab = Some(TabId::from_raw(tab_id));
    req.extensions_mut().insert(hint);
    next.run(req).await
}

/// Resolve a raw `url:<domain>` or `tab:<id>` into a concrete provider /
/// tab via config or transport. Called once at the start of `chat_completions`.
pub async fn resolve_hint(
    state: &crate::state::AppState,
    hint: &RoutingHint,
) -> Result<RoutingHint, UwaError> {
    let mut out = hint.clone();
    if let Some(p) = &hint.provider {
        if let Some(domain) = p.strip_prefix("url:") {
            // Find a provider whose URL patterns include the given domain.
            let provider_name = state
                .config
                .providers
                .iter()
                .find(|(_, cfg)| {
                    cfg.url_patterns
                        .iter()
                        .any(|pat| pat.contains(domain))
                })
                .map(|(name, _)| name.clone())
                .ok_or_else(|| {
                    UwaError::NoProviderForUrl(domain.to_string())
                })?;
            out.provider = Some(provider_name);
        }
    }
    Ok(out)
}
