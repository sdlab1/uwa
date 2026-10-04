use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::state::AppState;

pub async fn healthz() -> Json<Value> {
    Json(json!({"status":"ok"}))
}

pub async fn readyz(State(state): State<AppState>) -> Json<Value> {
    let providers = !state.providers.is_empty();
    Json(json!({
        "status": if providers { "ok" } else { "degraded" },
        "providers_loaded": providers,
        "models": state.config.model_aliases.len(),
    }))
}
