//! Admin endpoints: history, stats, selector test.

use crate::error::ApiResult;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uwa_core::UwaError;
use uwa_history::{Stats, StatsWindow};

// ---------- history ----------

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub provider: Option<String>,
    /// `success` | `error` | `pending`
    #[serde(default)]
    pub status: Option<String>,
}

fn default_limit() -> usize {
    50
}

pub async fn history(
    State(state): State<AppState>,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let Some(store) = &state.runtime.history else {
        return Err(UwaError::Internal("history store not configured".into()).into());
    };

    let limit = q.limit.min(500);
    // Both filters may be combined; previously `?provider=x&status=error`
    // silently ignored the status.
    let status = match q.status.as_deref() {
        None => None,
        Some("success") => Some(uwa_history::RequestStatus::Success),
        Some("error") => Some(uwa_history::RequestStatus::Error),
        Some("pending") => Some(uwa_history::RequestStatus::Pending),
        Some(other) => {
            return Err(UwaError::BadRequest(format!("bad status `{other}`")).into());
        }
    };
    let records = store.query(q.provider.as_deref(), status, limit).await;

    let count = records.len();
    Ok(Json(serde_json::json!({
        "count": count,
        "buffer_size": store.len().await,
        "records": records,
    })))
}

pub async fn history_record(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let Some(store) = &state.runtime.history else {
        return Err(UwaError::Internal("history store not configured".into()).into());
    };
    let recent = store.recent(1000).await;
    let r = recent.into_iter().find(|r| r.id == id);
    match r {
        Some(rec) => Ok(Json(
            serde_json::to_value(rec).unwrap_or(serde_json::Value::Null),
        )),
        None => Err(UwaError::BadRequest(format!("no record `{id}`")).into()),
    }
}

// ---------- stats ----------

#[derive(Debug, Deserialize)]
pub struct StatsQuery {
    #[serde(default)]
    pub window: Option<String>,
}

pub async fn stats(
    State(state): State<AppState>,
    Query(q): Query<StatsQuery>,
) -> ApiResult<Json<Stats>> {
    let Some(store) = &state.runtime.history else {
        return Err(UwaError::Internal("history store not configured".into()).into());
    };
    let window = match q.window.as_deref() {
        Some("last50") => StatsWindow::Last50,
        Some("last200") => StatsWindow::Last200,
        _ => StatsWindow::All,
    };
    let records = store.recent(window.take()).await;
    let s = uwa_history::stats::compute(&records, window);
    Ok(Json(s))
}

// ---------- selector test ----------

#[derive(Debug, Deserialize)]
pub struct SelectorTestRequest {
    pub provider: String,
    #[serde(default)]
    pub tab_id: Option<String>,
    pub selector: String,
    /// Which `Selector` field this probe stands for. Used to look up
    /// fallback candidates from the registry. One of: `input`,
    /// `send_button`, `stop_button`, `assistant_message`. Default `input`.
    #[serde(default)]
    pub field: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct SelectorTestResponse {
    pub matched: u32,
    pub first_text: Option<String>,
    pub duration_ms: u64,
    pub error: Option<String>,
    /// Registry-provided fallback candidates. Populated only when
    /// `matched == 0` and the provider has alternates for the field.
    /// Empty otherwise — the UI keeps the response compact on the happy
    /// path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallbacks: Vec<FallbackCandidate>,
}

#[derive(Debug, serde::Serialize)]
pub struct FallbackCandidate {
    pub selector: String,
    pub matched: u32,
    pub first_text: Option<String>,
}

/// Run one selector probe against the page. Returns (matched, first_text)
/// or an error when the selector itself is invalid.
async fn probe(
    page: &dyn uwa_core::Page,
    selector: &str,
) -> Result<(u32, Option<String>), UwaError> {
    let selector_json =
        serde_json::to_string(selector).map_err(|e| UwaError::Internal(e.to_string()))?;
    let js = format!(
        r#"(function() {{
            try {{
                const els = document.querySelectorAll({sel});
                const first = els[0];
                return {{
                    ok: true,
                    matched: els.length,
                    first_text: first ? (first.innerText || first.textContent || '').slice(0, 200) : null
                }};
            }} catch (e) {{
                return {{ ok: false, error: String(e) }};
            }}
        }})()"#,
        sel = selector_json,
    );
    let v = page.eval(&js).await?;
    if v.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        let err = v
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        return Err(UwaError::BadRequest(format!("selector: {err}")));
    }
    let n = v
        .get("matched")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32;
    let text = v
        .get("first_text")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    Ok((n, text))
}

pub async fn selector_test(
    State(state): State<AppState>,
    Json(req): Json<SelectorTestRequest>,
) -> ApiResult<Json<SelectorTestResponse>> {
    let start = std::time::Instant::now();

    // Verify the provider exists.
    let cfg = state.config.clone();
    let Some(_provider) = cfg.providers.get(&req.provider) else {
        return Err(UwaError::UnknownModel(uwa_core::ModelNotFound::new(
            req.provider.clone(),
            vec![],
        ))
        .into());
    };

    // Pick a tab: explicit or first available.
    let tabs = state.transport.list_tabs().await?;
    // An explicit tab must be one the transport actually knows. Taking any
    // caller-supplied id would let this endpoint read page text from tabs it
    // never listed.
    let tab = match &req.tab_id {
        Some(id) => tabs
            .into_iter()
            .find(|t| t.as_str() == id)
            .ok_or_else(|| UwaError::TabNotFound(id.clone()))?,
        None => tabs
            .into_iter()
            .next()
            .ok_or_else(|| UwaError::Internal("no tabs".into()))?,
    };
    let page = state.transport.page(&tab).await?;

    let (matched, first_text, error) = match probe(page.as_ref(), &req.selector).await {
        Ok((n, text)) => (n, text, None),
        Err(e) => (0, None, Some(e.to_string())),
    };

    let duration_ms = start.elapsed().as_millis() as u64;

    let registry = uwa_config::Registry::load();
    // The `field` the user is testing has to be named explicitly so we
    // know which fallback list to use. Default is "input" — it's the one
    // users test most.
    let field = req.field.as_deref().unwrap_or("input");
    let fallbacks: Vec<FallbackCandidate> = if matched == 0 {
        let mut out = Vec::new();
        for candidate in registry.alternates_for(&req.provider, field) {
            if candidate == req.selector {
                continue; // same selector, already failed
            }
            if let Ok((n, text)) = probe(page.as_ref(), &candidate).await {
                if n > 0 {
                    out.push(FallbackCandidate {
                        selector: candidate,
                        matched: n,
                        first_text: text,
                    });
                }
            }
            if out.len() >= 3 {
                break;
            }
        }
        out
    } else {
        Vec::new()
    };

    Ok(Json(SelectorTestResponse {
        matched,
        first_text,
        duration_ms,
        error,
        fallbacks,
    }))
}

// ---------- sessions ----------

pub async fn sessions(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let Some(sm) = &state.runtime.sessions else {
        return Err(UwaError::Internal("session manager not configured".into()).into());
    };
    let list = sm.list().await;
    let now = std::time::Instant::now();
    let out: Vec<serde_json::Value> = list
        .iter()
        .map(|s| {
            serde_json::json!({
                "conversation": s.conversation.as_str(),
                "profile": s.profile.as_str(),
                "tab": s.tab.as_str(),
                "age_secs": now.duration_since(s.created).as_secs(),
                "idle_secs": s.idle.as_secs(),
                "holders": s.holders,
                "generation": s.generation,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "sessions": out })))
}

pub async fn drop_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let Some(sm) = &state.runtime.sessions else {
        return Err(UwaError::Internal("session manager not configured".into()).into());
    };
    let cid = uwa_core::ConversationId::from_raw(id);
    let removed = sm.remove(&cid);
    Ok(Json(serde_json::json!({ "removed": removed.is_some() })))
}

pub async fn recover_sessions(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let Some(sm) = &state.runtime.sessions else {
        return Err(UwaError::Internal("session manager not configured".into()).into());
    };
    let dropped = sm.recover(state.transport.as_ref()).await;
    Ok(Json(serde_json::json!({
        "dropped_tabs": dropped.iter().map(|t| t.as_str()).collect::<Vec<_>>(),
    })))
}

// ---------- log stream (SSE) ----------

pub async fn log_stream(State(state): State<AppState>) -> axum::response::Response {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use axum::response::IntoResponse;
    // channel registered in `metrics::LOG_TX` — set up on first call.
    static ONCE: std::sync::OnceLock<tokio::sync::broadcast::Sender<String>> =
        std::sync::OnceLock::new();
    let tx = ONCE.get_or_init(|| {
        let (tx, _rx) = tokio::sync::broadcast::channel::<String>(256);
        install_tracing_bridge(tx.clone());
        tx
    });
    let _ = state; // keep the extractor for consistency

    let rx = tx.subscribe();
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(line) => {
                    return Some((
                        Ok::<_, std::convert::Infallible>(Event::default().data(line)),
                        rx,
                    ))
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Install a one-time tracing layer that forwards formatted events to the
/// broadcast channel. If another layer is already installed, skip.
fn install_tracing_bridge(tx: tokio::sync::broadcast::Sender<String>) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    struct Bridge {
        tx: tokio::sync::broadcast::Sender<String>,
    }
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Bridge {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let meta = event.metadata();
            let mut visitor = FieldVisitor(String::with_capacity(64));
            event.record(&mut visitor);
            let line = format!(
                "{{\"level\":\"{}\",\"target\":\"{}\",\"message\":\"{}\"}}",
                meta.level(),
                meta.target(),
                visitor.0.replace('"', "\\\""),
            );
            let _ = self.tx.send(line);
        }
    }
    struct FieldVisitor(String);
    impl tracing::field::Visit for FieldVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.0.push_str(&format!("{:?}", value));
            }
        }
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            if field.name() == "message" {
                self.0.push_str(value);
            }
        }
    }

    // Attach the bridge lazily; if a global default is already set this is a
    // no-op (the UI falls back to "no stream available").
    let _ = tracing_subscriber::registry()
        .with(Bridge { tx })
        .try_init();
}

// ---------- selector auto-generation ----------

#[derive(Debug, Deserialize)]
pub struct GenerateRequest {
    pub provider: String,
    #[serde(default)]
    pub tab_id: Option<String>,
}

pub async fn selector_generate(
    State(state): State<AppState>,
    Json(req): Json<GenerateRequest>,
) -> ApiResult<Json<uwa_providers::autogen::PageAnalysis>> {
    // Verify the provider exists.
    let cfg = state.config.clone();
    let Some(_provider) = cfg.providers.get(&req.provider) else {
        return Err(UwaError::UnknownModel(uwa_core::ModelNotFound::new(
            req.provider.clone(),
            vec![],
        ))
        .into());
    };

    let tabs = state.transport.list_tabs().await?;
    // An explicit tab must be one the transport actually knows. Taking any
    // caller-supplied id would let this endpoint read page text from tabs it
    // never listed.
    let tab = match &req.tab_id {
        Some(id) => tabs
            .into_iter()
            .find(|t| t.as_str() == id)
            .ok_or_else(|| UwaError::TabNotFound(id.clone()))?,
        None => tabs
            .into_iter()
            .next()
            .ok_or_else(|| UwaError::Internal("no tabs".into()))?,
    };
    let page = state.transport.page(&tab).await?;
    let analysis = uwa_providers::autogen::analyze(page.as_ref()).await?;
    Ok(Json(analysis))
}

#[derive(Debug, Deserialize)]
pub struct ApplySelectorsRequest {
    pub provider: String,
    #[serde(default)]
    pub input: Option<String>,
    #[serde(default)]
    pub send_button: Option<String>,
    #[serde(default)]
    pub assistant_message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ApplySelectorsResponse {
    /// Always `false`: this endpoint never mutates the running config.
    pub applied: bool,
    pub diff: serde_json::Value,
    /// Human-readable next step.
    pub hint: &'static str,
}

pub async fn selector_apply(
    State(state): State<AppState>,
    Json(req): Json<ApplySelectorsRequest>,
) -> ApiResult<Json<ApplySelectorsResponse>> {
    let cfg = state.config.clone();
    let p = cfg.providers.get(&req.provider).ok_or_else(|| {
        UwaError::UnknownModel(uwa_core::ModelNotFound::new(req.provider.clone(), vec![]))
    })?;

    let mut diff = serde_json::Map::new();
    if let Some(v) = &req.input {
        if p.selectors.input.as_deref() != Some(v.as_str()) {
            diff.insert(
                "input".into(),
                serde_json::json!({ "old": p.selectors.input, "new": v }),
            );
        }
    }
    if let Some(v) = &req.send_button {
        if p.selectors.send_button.as_deref() != Some(v.as_str()) {
            diff.insert(
                "send_button".into(),
                serde_json::json!({ "old": p.selectors.send_button, "new": v }),
            );
        }
    }
    if let Some(v) = &req.assistant_message {
        if p.selectors.assistant_message.as_deref() != Some(v.as_str()) {
            diff.insert(
                "assistant_message".into(),
                serde_json::json!({ "old": p.selectors.assistant_message, "new": v }),
            );
        }
    }

    Ok(Json(ApplySelectorsResponse {
        applied: false,
        diff: serde_json::Value::Object(diff),
        hint: "Dry-run. Edit uwa.toml and restart uwa to apply selector changes.",
    }))
}

/// `GET /admin/conformance` — the last persisted conformance report.
///
/// The CLI writes to `<uwa_home>/conformance/latest.json`; this reads it
/// back. When the file is absent (a fresh install that never ran the
/// suite), returns an empty report shape so the UI can render a hint
/// instead of a 404.
pub async fn conformance() -> ApiResult<Json<serde_json::Value>> {
    let path = uwa_home_file("conformance/latest.json");
    match tokio::fs::read_to_string(&path).await {
        Ok(raw) => {
            let v: serde_json::Value = serde_json::from_str(&raw)
                .map_err(|e| UwaError::Internal(format!("conformance json: {e}")))?;
            Ok(Json(v))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Json(serde_json::json!({
            "generated_at": serde_json::Value::Null,
            "uwa_version": serde_json::Value::Null,
            "scenarios_total": 0,
            "runs": [],
            "hint": "run `uwa conformance run --provider <name>` to populate this report"
        }))),
        Err(e) => Err(UwaError::Internal(format!("read conformance report: {e}")).into()),
    }
}

/// Returns a JSON-serializable representation of the provider registry.
/// Used by the `/admin/providers/registry` endpoint.
pub async fn provider_registry() -> ApiResult<Json<serde_json::Value>> {
    let registry = uwa_config::Registry::load();
    let manifest = registry.manifest().map(|m| {
        serde_json::json!({
            "registry_version": m.registry_version,
            "generated_at": m.generated_at,
            "min_uwa_version": m.min_uwa_version,
        })
    });
    let cache = registry
        .cache_dir()
        .map(|p| p.to_string_lossy().to_string());

    let mut entries = serde_json::Map::new();
    for name in registry.names() {
        if let Some(e) = registry.get(&name) {
            entries.insert(
                name.clone(),
                serde_json::json!({
                    "version": e.meta.version,
                    "updated_at": e.meta.updated_at,
                }),
            );
        }
    }

    Ok(Json(serde_json::json!({
        "manifest": manifest,
        "cache_dir": cache,
        "entries": entries,
    })))
}

fn uwa_home_file(rel: &str) -> std::path::PathBuf {
    let home = std::env::var("UWA_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .join(".uwa")
        });
    home.join(rel)
}

// ---------- profiles ----------

#[derive(Debug, serde::Serialize)]
pub struct ProfileSummary {
    pub name: String,
    pub label: String,
    pub notes: String,
    pub tabs: Vec<String>,
    /// Whether at least one tab currently belongs to the profile.
    pub active: bool,
    /// Whether the profile is a real (non-default) context the transport
    /// knows how to open tabs in.
    pub named: bool,
}

/// `GET /admin/profiles` — configured profiles plus their live tab counts.
pub async fn list_profiles(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let mut out: Vec<ProfileSummary> = Vec::new();

    // Default profile is always present, whether or not it's declared.
    let default_tabs: Vec<String> = state
        .transport
        .list_tabs_in(uwa_session::ProfileName::DEFAULT)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|t| t.to_string())
        .collect();
    out.push(ProfileSummary {
        name: uwa_session::ProfileName::DEFAULT.into(),
        label: "Default".into(),
        notes: "The context Chromium was launched with; your normal logins.".into(),
        active: !default_tabs.is_empty(),
        named: false,
        tabs: default_tabs,
    });

    // Configured profiles, sorted for a stable dashboard order.
    let mut names: Vec<&String> = state.config.profiles.keys().collect();
    names.sort();
    for name in names {
        let Some(cfg) = state.config.profiles.get(name) else {
            continue;
        };
        let tabs: Vec<String> = state
            .transport
            .list_tabs_in(name)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|t| t.to_string())
            .collect();
        out.push(ProfileSummary {
            name: name.clone(),
            label: if cfg.label.is_empty() {
                name.clone()
            } else {
                cfg.label.clone()
            },
            notes: cfg.notes.clone(),
            active: !tabs.is_empty(),
            named: true,
            tabs,
        });
    }

    Ok(Json(serde_json::json!({
        "supports_profiles": state.transport.supports_profiles(),
        "profiles": out,
    })))
}

#[derive(Debug, serde::Deserialize)]
pub struct OpenProfileRequest {
    /// URL to open. Required — the endpoint deliberately does not default
    /// to a specific provider.
    pub url: String,
    /// If set, an existing tab with this URL prefix is reused instead of
    /// opening a new one.
    #[serde(default)]
    pub reuse_if_url_starts_with: Option<String>,
}

/// `POST /admin/profiles/:name/open` — open a URL inside the profile's
/// context. This is how the dashboard lets a user log in to a site under
/// the profile without touching the OS browser.
pub async fn open_profile_tab(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<OpenProfileRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let is_default = name == uwa_session::ProfileName::DEFAULT;
    if !is_default && !state.config.profiles.contains_key(&name) {
        return Err(UwaError::BadRequest(format!("unknown profile `{name}`")).into());
    }
    if !is_default && !state.transport.supports_profiles() {
        return Err(
            UwaError::Config("this transport does not support named profiles".into()).into(),
        );
    }

    // Reuse an existing tab if the caller asked and one matches.
    if let Some(prefix) = &req.reuse_if_url_starts_with {
        for tab in state
            .transport
            .list_tabs_in(&name)
            .await
            .unwrap_or_default()
        {
            if let Ok(page) = state.transport.page(&tab).await {
                if let Ok(url) = page.url().await {
                    if url.as_str().starts_with(prefix) {
                        return Ok(Json(serde_json::json!({
                            "tab": tab.to_string(),
                            "reused": true,
                        })));
                    }
                }
            }
        }
    }

    let tab = if is_default {
        // For the default profile, fall back to an existing tab: the
        // browser's own context already has whatever the user opened at
        // launch, and a transport without profile support cannot open one.
        let existing = state
            .transport
            .list_tabs_in(uwa_session::ProfileName::DEFAULT)
            .await
            .unwrap_or_default()
            .into_iter()
            .next();
        match existing {
            Some(t) => {
                // Navigate the existing tab to the requested URL.
                if let Ok(page) = state.transport.page(&t).await {
                    if let Ok(u) = url::Url::parse(&req.url) {
                        let _ = page.goto(&u).await;
                    }
                }
                t
            }
            None => {
                return Err(UwaError::Unavailable(
                    "no tabs in the default context; open one in Chromium first".into(),
                )
                .into())
            }
        }
    } else {
        state.transport.open_tab(&name, &req.url).await?
    };

    Ok(Json(serde_json::json!({
        "tab": tab.to_string(),
        "reused": false,
    })))
}

/// `POST /admin/profiles/:name/reset` — close the profile's browser
/// context. Every tab in it is dropped and its cookies are discarded;
/// the next request that uses the profile starts from a clean context.
pub async fn reset_profile(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    if name == uwa_session::ProfileName::DEFAULT {
        return Err(UwaError::BadRequest(
            "the default profile cannot be reset through the API; \
             restart Chromium with a fresh user-data-dir instead"
                .into(),
        )
        .into());
    }
    if !state.config.profiles.contains_key(&name) {
        return Err(UwaError::BadRequest(format!("unknown profile `{name}`")).into());
    }

    let dropped = state.transport.close_profile(&name).await?;

    // Also drop any sessions that were pinned to tabs in this profile.
    if let Some(sm) = &state.runtime.sessions {
        let sessions = sm.list().await;
        for s in sessions {
            if s.profile.as_str() == name {
                let _ = sm.remove_for(&s.conversation, &s.profile);
            }
        }
    }

    Ok(Json(serde_json::json!({
        "dropped_tabs": dropped.iter().map(|t| t.as_str()).collect::<Vec<_>>(),
    })))
}

// ---------- recorder ----------

#[derive(Debug, serde::Deserialize)]
pub struct RecorderStartRequest {
    /// Provider whose tab should be recorded.
    pub provider: String,
    /// Explicit tab. When absent, the first tab matching the provider's
    /// URL patterns is used.
    #[serde(default)]
    pub tab_id: Option<String>,
}

/// `GET /admin/recorder/status` — one recording at a time; when none is
/// in flight the shape is `{ active: false }`.
pub async fn recorder_status(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let slot = state.runtime.recorder.lock().await;
    match slot.as_ref() {
        Some(r) => {
            let st = r.status().await;
            Ok(Json(serde_json::to_value(st).unwrap_or_else(
                |_| serde_json::json!({ "active": false }),
            )))
        }
        None => Ok(Json(serde_json::json!({ "active": false }))),
    }
}

/// `POST /admin/recorder/start` — begin observing a provider's tab.
/// Rejects a second concurrent recording with a message naming the
/// in-flight one.
pub async fn recorder_start(
    State(state): State<AppState>,
    Json(req): Json<RecorderStartRequest>,
) -> ApiResult<Json<crate::recorder::RecorderStatus>> {
    {
        let slot = state.runtime.recorder.lock().await;
        if let Some(existing) = slot.as_ref() {
            let st = existing.status().await;
            return Err(UwaError::BadRequest(format!(
                "recording already in progress (id `{}`, {} events); \
                 stop it before starting a new one",
                st.id, st.events
            ))
            .into());
        }
    }

    let provider_cfg = state
        .config
        .providers
        .get(&req.provider)
        .cloned()
        .ok_or_else(|| {
            UwaError::UnknownModel(uwa_core::ModelNotFound::new(
                req.provider.clone(),
                state.config.available_models(),
            ))
        })?;

    let tab = if let Some(id) = req.tab_id.clone() {
        uwa_core::TabId::from_raw(id)
    } else {
        let tabs = state.transport.list_tabs().await?;
        let mut picked: Option<uwa_core::TabId> = None;
        for t in &tabs {
            if let Ok(page) = state.transport.page(t).await {
                if let Ok(url) = page.url().await {
                    if provider_cfg
                        .url_patterns
                        .iter()
                        .any(|pat| url_glob_matches(pat, &url))
                    {
                        picked = Some(t.clone());
                        break;
                    }
                }
            }
        }
        picked.ok_or_else(|| {
            UwaError::Unavailable(format!(
                "no open tab matches provider `{}`; open the site in Chromium first",
                req.provider
            ))
        })?
    };

    let rec = crate::recorder::Recorder::start(state.clone(), req.provider.clone(), tab).await?;
    let st = rec.status().await;
    let mut slot = state.runtime.recorder.lock().await;
    *slot = Some(rec);
    Ok(Json(st))
}

#[derive(Debug, serde::Serialize)]
pub struct RecorderStopResponse {
    pub provider: String,
    pub event_count: usize,
    pub warnings: Vec<String>,
    pub workflow: uwa_core::workflow::Workflow,
    pub toml_snippet: String,
}

/// `POST /admin/recorder/stop` — end the recording, synthesise the
/// proposal, and render the TOML. The recorder slot is freed whether or
/// not the proposal succeeds.
pub async fn recorder_stop(State(state): State<AppState>) -> ApiResult<Json<RecorderStopResponse>> {
    let rec = {
        let mut slot = state.runtime.recorder.lock().await;
        slot.take()
    };
    let Some(rec) = rec else {
        return Err(UwaError::BadRequest("no recording in progress".into()).into());
    };

    let provider = rec.provider.clone();
    let trace = rec.stop().await;

    let proposal = match state.config.providers.get(&provider) {
        Some(p) => uwa_providers::workflow::propose_workflow(&trace, &p.selectors),
        None => uwa_providers::workflow::ProposedWorkflow {
            workflow: uwa_core::workflow::Workflow::default(),
            warnings: vec![format!(
                "provider `{provider}` disappeared from the config mid-recording"
            )],
            event_count: trace.events.len(),
        },
    };

    let toml_snippet =
        uwa_providers::workflow::workflow_to_toml_snippet(&provider, &proposal.workflow);

    Ok(Json(RecorderStopResponse {
        provider,
        event_count: proposal.event_count,
        warnings: proposal.warnings,
        workflow: proposal.workflow,
        toml_snippet,
    }))
}

/// Same glob as `uwa-config::url_matches`, duplicated here to avoid a
/// public re-export from `uwa-config` just for one call.
fn url_glob_matches(pattern: &str, url: &url::Url) -> bool {
    let u = url.as_str();
    match pattern.split_once('*') {
        None => pattern == u,
        Some((head, tail)) => u.starts_with(head) && (tail.is_empty() || u.ends_with(tail)),
    }
}
