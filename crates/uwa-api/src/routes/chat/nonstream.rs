//! Non-streaming JSON response for `/v1/chat/completions`.

use uwa_core::types::openai::MessageContent;
use uwa_core::types::openai::{
    ChatChoice, ChatCompletionResponse, ChatMessage, FunctionCall, ToolCallRef, Usage,
};
use uwa_core::types::{FinishReason, Role};
use uwa_core::RequestId;
use uwa_tools::ToolCall;

pub fn build_non_streaming(
    id: RequestId,
    model: String,
    created: u64,
    text: String,
    calls: Vec<ToolCall>,
    finish: FinishReason,
) -> ChatCompletionResponse {
    let (content, tool_calls) = if calls.is_empty() {
        (
            if text.is_empty() {
                None
            } else {
                Some(MessageContent::Text(text))
            },
            None,
        )
    } else {
        let c = if text.is_empty() {
            None
        } else {
            Some(MessageContent::Text(text))
        };
        (c, Some(calls.iter().map(to_ref).collect::<Vec<_>>()))
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

fn to_ref(c: &ToolCall) -> ToolCallRef {
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

    #[test]
    fn plain_text_stop() {
        let r = build_non_streaming(
            RequestId::from_raw("req_1"),
            "gpt-4o".into(),
            100,
            "hello".into(),
            vec![],
            FinishReason::Stop,
        );
        assert_eq!(r.object, "chat.completion");
        assert_eq!(r.choices[0].finish_reason, Some(FinishReason::Stop));
        assert!(r.choices[0].message.tool_calls.is_none());
    }

    #[test]
    fn tool_calls_path() {
        let calls = vec![ToolCall {
            id: "call_x".into(),
            name: "echo".into(),
            arguments: json!({"x": 1}),
        }];
        let r = build_non_streaming(
            RequestId::from_raw("req_1"),
            "gpt-4o".into(),
            100,
            "".into(),
            calls,
            FinishReason::ToolCalls,
        );
        let tc = r.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].function.name, "echo");
        assert_eq!(tc[0].function.arguments, r#"{"x":1}"#);
    }
}
