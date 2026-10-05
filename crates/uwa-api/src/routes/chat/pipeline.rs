//! The browser tool loop.
//!
//! One request costs one circuit-breaker check and one semaphore permit, then
//! up to [`MAX_TOOL_ROUNDS`] send/wait/parse rounds against the site
//! provider.

use std::time::Duration;

use uwa_core::traits::ToolSpec;
use uwa_core::types::openai::{
    ChatCompletionRequest, ChatMessage, FunctionCall, MessageContent, ToolCallRef,
};
use uwa_core::types::{FinishReason, Role};
use uwa_core::UwaError;
use uwa_tools::{
    build_system_prompt, compose_browser_turn, parse, render_tool_response, ToolCall,
    ToolDefinition,
};

use crate::state::AppState;

const MAX_TOOL_ROUNDS: usize = 4;

/// Guarded request: breaker and semaphore cover the whole call, not the
/// individual rounds.
pub async fn run_pipeline(
    state: &AppState,
    req: &ChatCompletionRequest,
    all_specs: &[ToolSpec],
) -> Result<(String, Vec<ToolCall>, FinishReason), UwaError> {
    let provider_cfg = state
        .config
        .provider_for_model(&req.model)
        .ok_or_else(|| UwaError::UnknownModel(req.model.clone()))?;

    let breaker = state.breaker(&provider_cfg.name);
    breaker.allow()?;
    let _permit = state.runtime.semaphores.acquire(&provider_cfg.name).await?;

    let result = run_pipeline_inner(state, req, all_specs).await;

    match &result {
        Ok(_) => breaker.record_success(),
        Err(_) => breaker.record_failure(),
    }
    crate::metrics::circuit_state(&provider_cfg.name, breaker.state().as_str());
    result
}

/// Merge local request tools with the MCP tools (unless `include_remote` is
/// off — that is what `tool_choice: "none"` asks for) and run
/// [`run_pipeline`].
pub async fn run_pipeline_with(
    state: &AppState,
    req: &ChatCompletionRequest,
    local_tools: Vec<ToolSpec>,
    include_remote: bool,
) -> Result<(String, Vec<ToolCall>, FinishReason), UwaError> {
    let mut all_specs = local_tools;
    if include_remote {
        if let Some(router) = &state.runtime.tool_router {
            all_specs.extend(router.all_definitions().await?);
        }
    }
    run_pipeline(state, req, &all_specs).await
}

async fn run_pipeline_inner(
    state: &AppState,
    req: &ChatCompletionRequest,
    all_specs: &[ToolSpec],
) -> Result<(String, Vec<ToolCall>, FinishReason), UwaError> {
    let provider_cfg = state
        .config
        .provider_for_model(&req.model)
        .ok_or_else(|| UwaError::UnknownModel(req.model.clone()))?;
    let site = state.providers.get(&provider_cfg.name)?;

    let system_injection = if all_specs.is_empty() {
        None
    } else {
        let defs: Vec<ToolDefinition> = all_specs.iter().map(spec_to_def).collect();
        Some(build_system_prompt(&defs))
    };

    // Pin the conversation to one tab when sessions are enabled.
    let cid = uwa_session::conversation_id(&req.messages);
    let session_handle = match &state.runtime.sessions {
        Some(sm) => Some(sm.acquire(&cid, state.transport.as_ref()).await?),
        None => None,
    };

    let mut conversation = req.messages.clone();
    let known_names: Vec<String> = all_specs.iter().map(|s| s.name.clone()).collect();
    let send_timeout = Duration::from_millis(state.config.server.request_timeout_ms.max(1_000));

    for round in 0..MAX_TOOL_ROUNDS {
        let tab = match &session_handle {
            Some(h) => h.tab.clone(),
            None => state
                .transport
                .list_tabs()
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| UwaError::Unavailable("no browser tabs available".into()))?,
        };
        let page = state.transport.page(&tab).await?;

        // System prompt only in the first round (audit A2): later rounds run
        // in a browser context that already has it.
        let body = build_browser_body(system_injection.as_deref(), &conversation, round == 0);

        tokio::time::timeout(send_timeout, site.send_message(page.as_ref(), &body))
            .await
            .map_err(|_| UwaError::Timeout(send_timeout))??;
        let raw = site.wait_response(page.as_ref()).await?;

        if let Some(h) = &session_handle {
            h.touch().await;
        }

        let parsed = parse(&raw, &known_names);
        if !parsed.has_calls() {
            return Ok((parsed.text, vec![], FinishReason::Stop));
        }

        // No router: the client executes the tools — hand the calls back,
        // which is exactly what the OpenAI contract asks for.
        let Some(router) = &state.runtime.tool_router else {
            return Ok((parsed.text, parsed.calls.clone(), FinishReason::ToolCalls));
        };

        let mut responses = Vec::with_capacity(parsed.calls.len());
        for call in &parsed.calls {
            let body = match router.dispatch(&call.name, call.arguments.clone()).await {
                Ok(s) => s,
                Err(e) => format!("error: {e}"),
            };
            responses.push(render_tool_response(&call.id, &body));
        }

        conversation.push(ChatMessage {
            role: Role::Assistant,
            content: if parsed.text.is_empty() {
                None
            } else {
                Some(MessageContent::Text(parsed.text.clone()))
            },
            name: None,
            tool_call_id: None,
            tool_calls: Some(parsed.calls.iter().map(to_openai_ref).collect()),
        });
        for (call, body) in parsed.calls.iter().zip(responses) {
            conversation.push(ChatMessage {
                role: Role::Tool,
                content: Some(MessageContent::Text(body)),
                name: None,
                tool_call_id: Some(call.id.clone()),
                tool_calls: None,
            });
        }
    }

    Err(UwaError::Unavailable(format!(
        "tool loop exceeded {MAX_TOOL_ROUNDS} rounds"
    )))
}

/// Compose what gets typed into the browser: the tool protocol prompt (first
/// round only) followed by the visible turn.
fn build_browser_body(system: Option<&str>, messages: &[ChatMessage], first_round: bool) -> String {
    let mut s = String::new();
    if first_round {
        if let Some(sys) = system {
            s.push_str(sys);
            s.push_str("\n\n---\n\n");
        }
        s.push_str(&compose_browser_turn(messages));
    } else {
        // Everything the browser already knows: the system message itself.
        let turn: Vec<ChatMessage> = messages
            .iter()
            .filter(|m| m.role != Role::System)
            .cloned()
            .collect();
        s.push_str(&compose_browser_turn(&turn));
    }
    s
}

fn spec_to_def(s: &ToolSpec) -> ToolDefinition {
    ToolDefinition::new(s.name.clone(), s.description.clone(), s.parameters.clone())
}

fn to_openai_ref(c: &ToolCall) -> ToolCallRef {
    ToolCallRef {
        id: c.id.clone(),
        kind: "function".into(),
        function: FunctionCall {
            name: c.name.clone(),
            arguments: serde_json::to_string(&c.arguments).unwrap_or_else(|_| "{}".into()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn msg(role: Role, text: &str) -> ChatMessage {
        ChatMessage::text(role, text)
    }

    #[test]
    fn system_prompt_only_goes_out_with_the_first_round() {
        let messages = vec![msg(Role::System, "be terse"), msg(Role::User, "hi")];
        let first = build_browser_body(Some("PROTO"), &messages, true);
        assert!(first.starts_with("PROTO"), "{first}");
        assert!(first.contains("be terse"), "{first}");
        assert!(first.contains("hi"), "{first}");

        let second = build_browser_body(Some("PROTO"), &messages, false);
        assert!(!second.contains("PROTO"), "re-injected: {second}");
        assert!(!second.contains("be terse"), "system repeated: {second}");
        assert!(second.contains("hi"), "{second}");
    }

    #[test]
    fn later_rounds_keep_tool_results() {
        let messages = vec![
            msg(Role::System, "sys"),
            msg(Role::User, "weather?"),
            ChatMessage {
                role: Role::Tool,
                content: Some(MessageContent::Text("22C".into())),
                name: None,
                tool_call_id: Some("toolu_1".into()),
                tool_calls: None,
            },
        ];
        let body = build_browser_body(Some("PROTO"), &messages, false);
        assert!(body.contains("toolu_1"), "{body}");
        assert!(body.contains("22C"), "{body}");
        assert!(!body.contains("PROTO"), "{body}");
    }

    #[test]
    fn specs_become_prompt_definitions() {
        let spec = ToolSpec {
            name: "get_weather".into(),
            description: "weather by city".into(),
            parameters: json!({"type": "object"}),
        };
        let prompt = build_system_prompt(&[spec_to_def(&spec)]);
        assert!(prompt.contains("get_weather"));
    }
}
