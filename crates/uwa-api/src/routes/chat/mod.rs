//! POST /v1/chat/completions
//!
//! Pipeline:
//!   1. Deserialize `ChatCompletionRequest`.
//!   2. Collect tools: request-local + MCP.
//!   3. Run the browser tool loop (`pipeline`), guarded by a breaker and a
//!      per-provider semaphore.
//!   4. Emit the answer as JSON (`nonstream`) or as SSE chunks (`stream`).

pub mod nonstream;
pub mod pipeline;
pub mod stream;

pub use pipeline::{run_pipeline, run_pipeline_with};

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::time::{SystemTime, UNIX_EPOCH};

use uwa_core::traits::ToolSpec;
use uwa_core::types::openai::ChatCompletionRequest;
use uwa_core::RequestId;
use uwa_tools::{ToolDefinition, ToolParseOutcome};

use crate::error::ApiResult;
use crate::state::AppState;

/// Main chat entrypoint: runs the pipeline and builds the HTTP response.
pub async fn chat_completions(
    State(state): State<AppState>,
    Json(req): Json<ChatCompletionRequest>,
) -> ApiResult<Response> {
    let local_tools = local_tools(&req);
    let (text, calls, finish) = run_pipeline_with(&state, &req, local_tools, true).await?;
    let outcome = ToolParseOutcome { text, calls };
    let id = RequestId::new();
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    if req.stream.unwrap_or(false) {
        Ok(stream::sse(&req.model, id, created, outcome, finish))
    } else {
        Ok(Json(nonstream::build_non_streaming(
            id,
            req.model.clone(),
            created,
            outcome,
            finish,
        ))
        .into_response())
    }
}

/// Tools declared by the request itself; MCP tools are merged by
/// [`run_pipeline_with`].
pub(crate) fn local_tools(req: &ChatCompletionRequest) -> Vec<ToolSpec> {
    match &req.tools {
        Some(arr) => ToolDefinition::from_openai_array(arr)
            .unwrap_or_default()
            .into_iter()
            .map(|t| ToolSpec {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.parameters.clone(),
            })
            .collect(),
        None => Vec::new(),
    }
}
