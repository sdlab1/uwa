//! POST /v1/chat/completions
//!
//! Pipeline:
//!   1. Deserialize `ChatCompletionRequest`.
//!   2. Resolve model -> provider.
//!   3. If `tools` present -> build system prompt injection (or reuse existing).
//!   4. Compose the browser turn (user text + any pending tool responses).
//!   5. Send to browser via provider, wait for the raw answer.
//!   6. Parse tool calls out of the answer.
//!   7. Emit either JSON or SSE, with `finish_reason = tool_calls|stop`.

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::time::{SystemTime, UNIX_EPOCH};

use uwa_core::traits::ToolSpec;
use uwa_core::types::openai::*;
use uwa_core::types::{FinishReason, Role};
use uwa_core::{RequestId, UwaError};
use uwa_tools::{
    build_system_prompt, compose_browser_turn, parse, render_tool_response, ToolDefinition,
};

use crate::error::ApiResult;
use crate::state::AppState;

const MAX_TOOL_ROUNDS: usize = 4;

/// Main chat entrypoint: handles a single chat completion request.
/// Delegates to `run_chat_loop` for tool handling and builds the HTTP response.
pub async fn chat_completions(
    State(state): State<AppState>,
    Json(req): Json<ChatCompletionRequest>,
) -> ApiResult<Response> {
    // Extract local tools from the request (if any)
    let local_tools: Vec<ToolSpec> = match &req.tools {
        Some(arr) => ToolDefinition::from_openai_array(arr)
            .unwrap_or_else(|_| Vec::new())
            .into_iter()
            .map(|t| ToolSpec {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.parameters.clone(),
            })
            .collect(),
        None => Vec::new(),
    };

    // Run the chat loop with tool handling
    let (text, tool_calls, finish_reason) = match run_chat_loop(&state, &req, local_tools).await {
        Ok(result) => result,
        Err(e) => return Err(e.into()),
    };

    // Build response
    let outcome = uwa_tools::ToolParseOutcome {
        text,
        calls: tool_calls,
    };

    // For streaming, we currently fall back to non-streaming due to
    // complexity of streaming with tool calls.
    Ok(Json(build_non_streaming(
        RequestId::new(),
        req.model.clone(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        outcome,
        finish_reason,
    ))
    .into_response())
}

/// Chat pipeline (v2): tools can come from local request AND from MCP.
pub async fn run_chat_loop(
    state: &AppState,
    req: &ChatCompletionRequest,
    local_tools: Vec<ToolSpec>,
) -> uwa_core::Result<(String, Vec<uwa_tools::ToolCall>, FinishReason)> {
    // 1. Gather all tool definitions: local (from request) + remote (MCP).
    let mut all_specs: Vec<ToolSpec> = local_tools;
    if let Some(router) = &state.tool_router {
        all_specs.extend(router.all_definitions().await?);
    }

    // 2. Prepare outbound messages (system injection + history).
    let injected = if !all_specs.is_empty() {
        // adapt ToolSpec -> uwa_tools::ToolDefinition
        let defs: Vec<uwa_tools::ToolDefinition> = all_specs.iter().map(spec_to_def).collect();
        Some(build_system_prompt(&defs))
    } else {
        None
    };

    let provider = state
        .config
        .provider_for_model(&req.model)
        .ok_or_else(|| UwaError::UnknownModel(req.model.clone()))?;
    let site = state.providers.get(&provider.name)?;

    // 3. Multi-round loop.
    let mut conversation: Vec<ChatMessage> = req.messages.clone();
    let known_names: Vec<String> = all_specs.iter().map(|s| s.name.clone()).collect();

    for _round in 0..MAX_TOOL_ROUNDS {
        let body = build_browser_body(injected.as_deref(), &conversation);
        let tab = state
            .transport
            .list_tabs()
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| UwaError::Unavailable("no tabs".into()))?;
        let page = state.transport.page(&tab).await?;
        site.send_message(page.as_ref(), &body).await?;
        let raw = site.wait_response(page.as_ref()).await?;
        let parsed = parse(&raw, &known_names);

        if !parsed.has_calls() {
            return Ok((parsed.text, vec![], FinishReason::Stop));
        }

        // If there's a tool router, execute the tool calls and loop.
        // Otherwise, return the tool calls in the response.
        let router = match &state.tool_router {
            Some(r) => r,
            None => return Ok((parsed.text, parsed.calls.clone(), FinishReason::ToolCalls)),
        };

        // 4. Execute tool calls.
        let mut tool_responses: Vec<String> = Vec::new();
        for call in &parsed.calls {
            let result = router.dispatch(&call.name, call.arguments.clone()).await;
            let body = match result {
                Ok(s) => s,
                Err(e) => format!("error: {e}"),
            };
            tool_responses.push(render_tool_response(&call.id, &body));
        }

        // 5. Append assistant text + tool responses to history and loop.
        conversation.push(ChatMessage {
            role: Role::Assistant,
            content: if parsed.text.is_empty() {
                None
            } else {
                Some(MessageContent::Text(parsed.text))
            },
            name: None,
            tool_call_id: None,
            tool_calls: Some(parsed.calls.iter().map(to_openai_ref).collect()),
        });
        for r in tool_responses {
            conversation.push(ChatMessage::text(Role::Tool, r));
        }
    }

    Err(UwaError::Unavailable(format!(
        "tool loop exceeded {MAX_TOOL_ROUNDS} rounds"
    )))
}

fn build_browser_body(system: Option<&str>, messages: &[ChatMessage]) -> String {
    let mut s = String::new();
    if let Some(sys) = system {
        s.push_str(sys);
        s.push_str("\n\n---\n\n");
    }
    s.push_str(&compose_browser_turn(messages));
    s
}

fn spec_to_def(s: &ToolSpec) -> uwa_tools::ToolDefinition {
    uwa_tools::ToolDefinition::new(s.name.clone(), s.description.clone(), s.parameters.clone())
}

fn to_openai_ref(c: &uwa_tools::ToolCall) -> uwa_core::types::openai::ToolCallRef {
    uwa_core::types::openai::ToolCallRef {
        id: c.id.clone(),
        kind: "function".into(),
        function: uwa_core::types::openai::FunctionCall {
            name: c.name.clone(),
            arguments: serde_json::to_string(&c.arguments).unwrap_or_else(|_| "{}".into()),
        },
    }
}

// Helper to build non-streaming response (copied from original implementation)
fn build_non_streaming(
    id: RequestId,
    model: String,
    created: u64,
    outcome: uwa_tools::ToolParseOutcome,
    finish: FinishReason,
) -> ChatCompletionResponse {
    let (content, tool_calls) = if outcome.has_calls() {
        (
            if outcome.text.is_empty() {
                None
            } else {
                Some(MessageContent::Text(outcome.text))
            },
            Some(
                outcome
                    .calls
                    .into_iter()
                    .map(|tc| ToolCallRef {
                        id: tc.id.clone(),
                        kind: "function".into(),
                        function: FunctionCall {
                            name: tc.name.clone(),
                            arguments: serde_json::to_string(&tc.arguments)
                                .unwrap_or_else(|_| "{}".into()),
                        },
                    })
                    .collect::<Vec<ToolCallRef>>(),
            ),
        )
    } else {
        (Some(MessageContent::Text(outcome.text)), None)
    };
    let message = ChatMessage {
        role: Role::Assistant,
        content,
        name: None,
        tool_call_id: None,
        tool_calls,
    };
    ChatCompletionResponse {
        id: id.to_string(),
        object: "chat.completion",
        created,
        model,
        choices: vec![ChatChoice {
            index: 0,
            message,
            finish_reason: Some(finish),
        }],
        usage: Usage::default(),
    }
}
