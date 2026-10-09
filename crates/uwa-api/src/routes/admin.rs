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
