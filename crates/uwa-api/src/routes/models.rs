use axum::extract::State;
use axum::Json;
use uwa_core::types::openai::{ModelList, ModelObject};

use crate::state::AppState;

pub async fn list_models(State(state): State<AppState>) -> Json<ModelList> {
    let created = 0u64;
    let data = state
        .config
        .model_aliases
        .keys()
        .map(|m| ModelObject {
            id: m.clone(),
            object: "model",
            created,
            owned_by: state
                .config
                .model_aliases
                .get(m)
                .cloned()
                .unwrap_or_else(|| "uwa".into()),
        })
        .collect();
    Json(ModelList {
        object: "list",
        data,
    })
}
