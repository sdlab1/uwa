//! Routing hints extracted from paths/headers.
//!
//! The endpoint order in the router is important: `/url/{domain}/v1/...`
//! and `/tab/{id}/v1/...` are merged **before** the plain `/v1/...` routes
//! so that axum's matcher resolves path parameters first.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use uwa_config::GroupCfg;
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
    /// Browser profile name (`X-UWA-Session`). `None` = default context.
    pub profile: Option<String>,
    /// Routing group ID (`/group/{id}/`). Overrides provider selection.
    pub group: Option<String>,
    /// Preset name (`/url/{domain}/{preset}/`). Applied to the resolved provider.
    pub preset: Option<String>,
}

impl RoutingHint {
    pub fn is_empty(&self) -> bool {
        self.provider.is_none()
            && self.tab.is_none()
            && self.conversation.is_none()
            && self.profile.is_none()
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
        let profile = parts
            .headers
            .get("x-uwa-session")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let conversation = parts
            .headers
            .get("x-uwa-conversation")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| ConversationId::from_raw(s.to_string()));
        Ok(HintExtractor(RoutingHint {
            provider,
            tab: None,
            conversation,
            profile,
            group: None,
            preset: None,
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

/// `/group/{id}/...` → `RoutingHint::group`.
pub async fn from_group_path(
    axum::extract::Path(group_id): axum::extract::Path<String>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let mut hint = req
        .extensions()
        .get::<RoutingHint>()
        .cloned()
        .unwrap_or_default();
    hint.group = Some(group_id);
    req.extensions_mut().insert(hint);
    next.run(req).await
}

/// `/url/{domain}/{preset}/...` → `RoutingHint::provider` (raw `url:{domain}`)
/// and `RoutingHint::preset`.
pub async fn from_url_preset_path(
    axum::extract::Path((domain, preset)): axum::extract::Path<(String, String)>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let mut hint = req
        .extensions()
        .get::<RoutingHint>()
        .cloned()
        .unwrap_or_default();
    hint.provider = Some(format!("url:{domain}"));
    hint.preset = Some(preset);
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

    // Routing group takes priority over provider hint.
    if let Some(group_id) = &hint.group {
        let cfg = state.cfg();
        let group = cfg
            .groups
            .get(group_id)
            .ok_or_else(|| UwaError::Config(format!("unknown routing group `{group_id}`")))?;
        let member = select_group_member(group, &cfg.providers, &out.conversation)?;
        out.provider = Some(member.provider.clone());
        out.preset = member.preset.clone().or(out.preset.clone());
    } else if let Some(p) = &hint.provider {
        if let Some(domain) = p.strip_prefix("url:") {
            // Find a provider whose URL patterns include the given domain.
            let provider_name = state
                .cfg()
                .providers
                .iter()
                .find(|(_, cfg)| cfg.url_patterns.iter().any(|pat| pat.contains(domain)))
                .map(|(name, _)| name.clone())
                .ok_or_else(|| UwaError::NoProviderForUrl(domain.to_string()))?;
            out.provider = Some(provider_name);
        }
    }

    // Validate that a named profile exists in the config.
    // `"default"` is always valid (the default browser context); any other
    // name must be declared under `[profiles.<name>]` in the config.
    if let Some(profile_name) = &out.profile {
        if profile_name != "default" {
            let cfg = state.cfg();
            if !cfg.profiles.contains_key(profile_name) {
                let available: Vec<&str> = std::iter::once("default")
                    .chain(cfg.profiles.keys().map(String::as_str))
                    .collect();
                return Err(UwaError::BadRequest(format!(
                    "unknown profile `{profile_name}`. Available profiles: {}",
                    available.join(", ")
                )));
            }
        }
    }

    Ok(out)
}

/// Select a group member based on the group's strategy.
///
/// For `RoundRobin` and `Failover`, this is a simple deterministic pick
/// (first member). For `HashConversation`, the conversation id is hashed
/// to pick a member (sticky routing).
fn select_group_member(
    group: &GroupCfg,
    providers: &std::collections::HashMap<String, uwa_config::ProviderCfg>,
    conversation: &Option<ConversationId>,
) -> Result<uwa_config::GroupMember, UwaError> {
    if group.members.is_empty() {
        return Err(UwaError::Config("group has no members".into()));
    }

    let idx = match group.strategy {
        uwa_config::GroupStrategy::RoundRobin | uwa_config::GroupStrategy::Failover => 0,
        uwa_config::GroupStrategy::HashConversation => {
            let hash: usize = conversation
                .as_ref()
                .map(|c| c.as_str())
                .unwrap_or("")
                .bytes()
                .fold(0usize, |acc, b| {
                    acc.wrapping_add(b as usize).wrapping_mul(31)
                });
            hash.checked_rem(group.members.len()).unwrap_or(0)
        }
    };

    let member = group
        .members
        .get(idx)
        .cloned()
        .ok_or_else(|| UwaError::Config("group member index out of bounds".into()))?;
    // Validate the member provider exists.
    if !providers.contains_key(&member.provider) {
        return Err(UwaError::Config(format!(
            "group member provider `{}` is not defined",
            member.provider
        )));
    }
    Ok(member.clone())
}
