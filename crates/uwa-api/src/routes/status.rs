use axum::extract::State;
use axum::Json;
use serde_json::{json, Map, Value};

use crate::state::AppState;

pub async fn provider_status(State(state): State<AppState>) -> Json<Value> {
    let mut providers_status = Map::new();
    for (name, cfg) in &state.config.providers {
        providers_status.insert(
            name.clone(),
            json!({
                "enabled": true,
                "url_patterns": cfg.url_patterns,
            }),
        );
    }
    Json(Value::Object(providers_status))
}

pub async fn pool_status(State(state): State<AppState>) -> Json<Value> {
    // We clone the tabs vector to avoid holding the lock across the await point.
    let tabs = state.transport.list_tabs().await.unwrap_or_default();
    Json(json!({
        "total_tabs": tabs.len(),
        "tabs": tabs.iter().map(|t| t.as_str()).collect::<Vec<_>>(),
    }))
}
