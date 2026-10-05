//! The non-streaming answer of `/v1/chat/completions`.

use uwa_core::types::openai::{
    ChatChoice, ChatCompletionResponse, ChatMessage, FunctionCall, MessageContent, ToolCallRef,
    Usage,
};
use uwa_core::types::{FinishReason, Role};
use uwa_core::RequestId;
use uwa_tools::ToolParseOutcome;

/// Build the JSON body for a finished tool loop.
pub fn build_non_streaming(
    id: RequestId,
    model: String,
    created: u64,
    outcome: ToolParseOutcome,
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
