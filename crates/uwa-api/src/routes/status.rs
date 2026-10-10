use axum::extract::State;
use axum::Json;
use serde_json::{json, Map, Value};

use crate::state::AppState;

/// Per-provider config surface for the UI: URL patterns, selectors (so the
/// setup wizard can probe login state), capabilities, extraction strategy.
pub async fn provider_status(State(state): State<AppState>) -> Json<Value> {
    let mut out = Map::new();
    for (name, cfg) in &state.config.providers {
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
