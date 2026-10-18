use axum::extract::State;
use axum::Json;
use serde_json::{json, Map, Value};

use crate::state::AppState;
use crate::verification::inspect_page;

/// Per-provider config surface for the UI: URL patterns, selectors (so the
/// setup wizard can probe login state), capabilities, extraction strategy.
pub async fn provider_status(State(state): State<AppState>) -> Json<Value> {
    let mut out = Map::new();
    for (name, cfg) in &state.cfg().providers {
        out.insert(
            name.clone(),
            json!({
                "name": cfg.name,
                "enabled": true,
                "url_patterns": cfg.url_patterns,
                "selectors": {
                    "input": cfg.selectors.input,
                    "send_button": cfg.selectors.send_button,
                    "assistant_message": cfg.selectors.assistant_message,
                },
                "capabilities": cfg.capabilities,
                "extraction": cfg.extraction,
                "backend": cfg.backend,
            }),
        );
    }
    Json(Value::Object(out))
}

/// Tab list with URLs. The wizard needs URLs to tell if `chatgpt.com` is
/// open; `/v1/models` alone can't answer that. `page()` uses a non-blocking
/// tab lease, so a busy tab reports an empty URL rather than blocking.
pub async fn pool_status(State(state): State<AppState>) -> Json<Value> {
    let tabs = state.transport.list_tabs().await.unwrap_or_default();
    let mut out = Vec::with_capacity(tabs.len());
    for tab in &tabs {
        let url = match state.transport.page(tab).await {
            Ok(page) => page.url().await.map(|u| u.to_string()).unwrap_or_default(),
            Err(_) => String::new(),
        };
        out.push(json!({ "id": tab.as_str(), "url": url }));
    }
    Json(json!({ "total_tabs": out.len(), "tabs": out }))
}

/// Scan open browser tabs for visible Cloudflare challenge pages. This is a
/// read-only diagnostic endpoint; it never interacts with or solves challenges.
pub async fn verification_status(State(state): State<AppState>) -> Json<Value> {
    let tabs = state.transport.list_tabs().await.unwrap_or_default();
    let mut challenges = Vec::new();
    let mut scan_errors = Vec::new();
    let mut scanned_tabs = 0usize;

    for tab in &tabs {
        let page = match state.transport.page(tab).await {
            Ok(page) => page,
            Err(err) => {
                scan_errors.push(json!({
                    "tab_id": tab.as_str(),
                    "error": err.to_string(),
                }));
                continue;
            }
        };

        let status = match inspect_page(page.as_ref()).await {
            Ok(status) => status,
            Err(err) => {
                scan_errors.push(json!({
                    "tab_id": tab.as_str(),
                    "error": err.to_string(),
                }));
                continue;
            }
        };
        scanned_tabs = scanned_tabs.saturating_add(1);

        if status.challenged {
            let provider = state
                .cfg()
                .providers
                .iter()
                .find(|(_, cfg)| {
                    cfg.url_patterns
                        .iter()
                        .any(|pattern| status.url.contains(pattern.as_str()))
                })
                .map(|(name, _)| name.clone());

            challenges.push(json!({
                "tab_id": tab.as_str(),
                "provider": provider,
                "url": status.url,
                "signals": status.signals,
            }));
        }
    }

    Json(json!({
        "total_tabs": tabs.len(),
        "scanned_tabs": scanned_tabs,
        "challenges": challenges,
        "scan_errors": scan_errors,
    }))
}
