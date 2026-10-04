//! POST /v1/messages — Anthropic-shaped request in, Anthropic-shaped answer out.
//! MVP: convert to the OpenAI pipeline, then reshape back.

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use uwa_core::types::anthropic::MessagesRequest;
use uwa_core::types::openai::{ChatCompletionRequest, ChatMessage};
use uwa_core::types::Role;

use crate::error::ApiResult;
use crate::state::AppState;

pub async fn messages(
    state: State<AppState>,
    Json(req): Json<MessagesRequest>,
) -> ApiResult<Response> {
    // Convert Anthropic -> OpenAI.
    let mut messages = Vec::new();
    if let Some(s) = req.system {
        messages.push(ChatMessage {
            role: Role::System,
            content: Some(s),
            name: None,
            tool_call_id: None,
            tool_calls: None,
        });
    }
    for m in req.messages {
        let content = match m.content {
            serde_json::Value::String(s) => Some(s),
            serde_json::Value::Array(blocks) => Some(
                blocks
                    .into_iter()
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()).map(str::to_string))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        };
        messages.push(ChatMessage {
            role: match m.role.as_str() {
                "assistant" => Role::Assistant,
                _ => Role::User,
            },
            content,
            name: None,
            tool_call_id: None,
            tool_calls: None,
        });
    }
    let oa_req = ChatCompletionRequest {
        model: req.model.clone(),
        messages,
        stream: None,
        temperature: req.temperature,
        max_tokens: Some(req.max_tokens),
        tools: None,
        tool_choice: None,
        user: None,
    };

    // Drive the same handler.
    let resp = super::chat::chat_completions(state, Json(oa_req)).await?;
    // For MVP return the OpenAI body reshaped. (Non-streaming path only.)
    let _ = json!({});
    Ok(resp.into_response())
}
