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
    let records = if let Some(p) = &q.provider {
        store.by_provider(p, limit).await
    } else if let Some(s) = &q.status {
        let st = match s.as_str() {
            "success" => uwa_history::RequestStatus::Success,
            "error" => uwa_history::RequestStatus::Error,
            "pending" => uwa_history::RequestStatus::Pending,
            _ => {
                return Err(UwaError::BadRequest(format!("bad status `{s}`")).into());
            }
        };
        store.by_status(st, limit).await
    } else {
        store.recent(limit).await
    };

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
}

#[derive(Debug, serde::Serialize)]
pub struct SelectorTestResponse {
    pub matched: u32,
    pub first_text: Option<String>,
    pub duration_ms: u64,
    pub error: Option<String>,
}

pub async fn selector_test(
    State(state): State<AppState>,
    Json(req): Json<SelectorTestRequest>,
) -> ApiResult<Json<SelectorTestResponse>> {
    let start = std::time::Instant::now();

    // Verify the provider exists.
    let cfg = state.config.clone();
    let Some(_provider) = cfg.providers.get(&req.provider) else {
        return Err(UwaError::UnknownModel(req.provider.clone()).into());
    };

    // Pick a tab: explicit or first available.
    let tabs = state.transport.list_tabs().await?;
    let tab = match &req.tab_id {
        Some(id) => uwa_core::TabId::from_raw(id.clone()),
        None => tabs
            .into_iter()
            .next()
            .ok_or_else(|| UwaError::Internal("no tabs".into()))?,
    };
    let page = state.transport.page(&tab).await?;

    let selector_json = serde_json::to_string(&req.selector).unwrap_or_default();
    let js = format!(
        r#"(function() {{
            try {{
                const els = document.querySelectorAll({sel});
                const first = els[0];
                return {{
                    ok: true,
                    matched: els.length,
                    first_text: first ? (first.innerText || first.textContent || '').slice(0, 500) : null
                }};
            }} catch (e) {{
                return {{ ok: false, error: String(e) }};
            }}
        }})()"#,
        sel = selector_json,
    );

    let v = page.eval(&js).await;

    let duration_ms = start.elapsed().as_millis() as u64;

    match v {
        Ok(val) => {
            let matched = val.get("matched").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
            let first_text = val
                .get("first_text")
                .and_then(|x| x.as_str())
                .map(str::to_string);
            let error = if val.get("ok").and_then(|x| x.as_bool()) == Some(true) {
                None
            } else {
                val.get("error")
                    .and_then(|x| x.as_str())
                    .map(str::to_string)
            };
            Ok(Json(SelectorTestResponse {
                matched,
                first_text,
                duration_ms,
                error,
            }))
        }
        Err(e) => Ok(Json(SelectorTestResponse {
            matched: 0,
            first_text: None,
            duration_ms,
            error: Some(e.to_string()),
        })),
    }
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
                "tab": s.tab.as_str(),
                "age_secs": now.duration_since(s.created).as_secs(),
                "idle_secs": s.idle.as_secs(),
                "generation": s.holders,
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
        return Err(UwaError::UnknownModel(req.provider.clone()).into());
    };

    let tabs = state.transport.list_tabs().await?;
    let tab = match &req.tab_id {
        Some(id) => uwa_core::TabId::from_raw(id.clone()),
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
    /// When true: persist to disk. Currently a no-op — the full TOML
    /// round-trip lives in `/admin/config/reload`.
    #[serde(default)]
    pub persist: bool,
}

#[derive(Debug, Serialize)]
pub struct ApplySelectorsResponse {
    pub applied: bool,
    pub diff: serde_json::Value,
    pub persisted_to: Option<String>,
}

pub async fn selector_apply(
    State(state): State<AppState>,
    Json(req): Json<ApplySelectorsRequest>,
) -> ApiResult<Json<ApplySelectorsResponse>> {
    let cfg = state.config.clone();
    let p = cfg
        .providers
        .get(&req.provider)
        .ok_or_else(|| UwaError::UnknownModel(req.provider.clone()))?;

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

    if !req.persist {
        return Ok(Json(ApplySelectorsResponse {
            applied: false,
            diff: serde_json::Value::Object(diff),
            persisted_to: None,
        }));
    }

    // Persist: the full TOML write-back is deferred to
    // `/admin/config/reload`, which takes the whole document.
    tracing::warn!(
        "selector_apply: persist=true is a no-op; use /admin/config/reload with full TOML"
    );

    Ok(Json(ApplySelectorsResponse {
        applied: false,
        diff: serde_json::Value::Object(diff),
        persisted_to: None,
    }))
}
