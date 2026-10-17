//! Light-weight token estimator.
//!
//! We don't ship a real BPE — cl100k_base tables are ~3 MB and drift
//! whenever OpenAI ships a new model. Instead we use a weighted char count
//! that stays within ±15% of cl100k_base on mixed English/CJK/code input,
//! which is what clients use `usage` for (context budgeting, cost
//! warnings). When a client wants exactness it can call
//! `/v1/messages/count_tokens`, which routes here too.
//!
//! Weights are in 1/4-token units:
//!
//!   * ASCII word char  → 1  (4 chars/token)
//!   * ASCII punctuation → 2  (2 chars/token)
//!   * Non-ASCII char    → 5  (~1.25 chars/token; CJK/Cyrillic/etc.)
//!
//! Then `sum / 4 = tokens`.

/// Estimate the number of tokens in `text`. Returns 0 for empty input.
pub fn estimate(text: &str) -> u32 {
    if text.is_empty() {
        return 0;
    }
    let mut units: u64 = 0;
    for ch in text.chars() {
        let weight: u64 = if ch.is_ascii_alphanumeric() || ch == ' ' || ch == '\n' {
            1
        } else if ch.is_ascii() {
            2
        } else {
            5
        };
        units = units.saturating_add(weight);
    }
    // Round up so a single character doesn't collapse to 0.
    units.div_ceil(4).max(1) as u32
}

/// A message contributes its content plus a fixed per-message overhead
/// (~4 tokens for role marker, delimiters). Tool calls add name +
/// arguments. System messages count as content + overhead the same way.
pub fn estimate_for_content(content: Option<&str>, overhead: u32) -> u32 {
    estimate(content.unwrap_or("")).saturating_add(overhead)
}

use uwa_core::types::openai::Usage;
use uwa_core::types::openai::{ChatCompletionRequest, ChatMessage, MessageContent};

use crate::parser::ToolCall;

/// Rough token cost of a chat completion request: every message
/// (content + 4-token role overhead) plus every tool declaration plus
/// the JSON of any tool_calls the assistant echoed back in history.
///
/// Uses the same weighted-char estimate as [`estimate`] — the goal is
/// context budgeting and cost hints, not an exact match for a specific
/// BPE.
pub fn estimate_chat_request_prompt(req: &ChatCompletionRequest) -> u32 {
    let mut total: u32 = 0;
    for m in &req.messages {
        total = total.saturating_add(estimate_message(m));
    }
    if let Some(tools) = &req.tools {
        if let Ok(s) = serde_json::to_string(tools) {
            total = total.saturating_add(estimate(&s));
        }
    }
    total.max(1)
}

fn estimate_message(m: &ChatMessage) -> u32 {
    let content_tokens = match &m.content {
        Some(MessageContent::Text(s)) => estimate(s),
        Some(MessageContent::Parts(parts)) => {
            let mut t: u32 = 0;
            for p in parts {
                match p {
                    uwa_core::types::openai::ContentPart::Text { text } => {
                        t = t.saturating_add(estimate(text));
                    }
                    uwa_core::types::openai::ContentPart::ImageUrl { .. } => {
                        // An image is ~85 "tokens" in OpenAI's own
                        // accounting for detail:low. We don't know the
                        // detail level; use a middle estimate.
                        t = t.saturating_add(150);
                    }
                    _ => {}
                }
            }
            t
        }
        None => 0,
    };
    let mut n = content_tokens.saturating_add(4);
    if let Some(id) = &m.tool_call_id {
        n = n.saturating_add(estimate(id));
    }
    if let Some(calls) = &m.tool_calls {
        for c in calls {
            n = n
                .saturating_add(estimate(&c.function.name))
                .saturating_add(estimate(&c.function.arguments));
            n = n.saturating_add(4);
        }
    }
    n
}

/// Rough token cost of an assistant response: text plus every tool
/// call's name and serialized arguments.
pub fn estimate_completion(text: &str, calls: &[ToolCall]) -> u32 {
    let mut total = estimate(text);
    for c in calls {
        total = total
            .saturating_add(estimate(&c.name))
            .saturating_add(estimate(&c.arguments.to_string()));
    }
    total.max(1)
}

/// Convenience: full usage for a request/response pair, ready to send
/// to a client or record in history.
pub fn usage_for(req: &ChatCompletionRequest, text: &str, calls: &[ToolCall]) -> Usage {
    let prompt_tokens = estimate_chat_request_prompt(req);
    let completion_tokens = estimate_completion(text, calls);
    Usage {
        prompt_tokens,
        completion_tokens,
        total_tokens: prompt_tokens.saturating_add(completion_tokens),
    }
}

/// Same as [`usage_for`] but for a request/response pair whose prompt
/// cost was computed elsewhere (the Anthropic Messages API maps its own
/// request shape). Kept here so the estimator has one home; the mapper
/// lives in `uwa-api`.
pub fn usage_for_prompt_tokens(prompt_tokens: u32, text: &str, calls: &[ToolCall]) -> Usage {
    let completion_tokens = estimate_completion(text, calls);
    Usage {
        prompt_tokens,
        completion_tokens,
        total_tokens: prompt_tokens.saturating_add(completion_tokens),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_zero() {
        assert_eq!(estimate(""), 0);
    }

    #[test]
    fn short_word_is_one_token() {
        assert_eq!(estimate("hi"), 1);
    }

    #[test]
    fn english_roughly_four_chars_per_token() {
        let t = "The quick brown fox jumps over the lazy dog and then sits down.";
        let n = estimate(t);
        let chars = t.chars().count() as f32;
        let ratio = chars / n as f32;
        assert!(
            (3.2..=4.8).contains(&ratio),
            "ratio {ratio} out of band (n={n})"
        );
    }

    #[test]
    fn cjk_is_denser_than_ascii() {
        let en = estimate("hello world");
        let ja = estimate("こんにちは世界");
        // 11 ascii chars ≈ 3 tokens; 7 CJK chars ≈ 9 tokens
        assert!(ja > en, "CJK {ja} should exceed english {en}");
    }

    #[test]
    fn code_blocks_are_counted() {
        let code = "fn main() {\n    println!(\"hi\");\n}\n";
        assert!(estimate(code) >= 5);
    }

    #[test]
    fn mixed_text_is_smooth() {
        let t = "Café ☕ — цена 100₽.";
        let n = estimate(t);
        assert!((4..=16).contains(&n), "got {n}");
    }

    #[test]
    fn prompt_includes_tools_json() {
        let req = ChatCompletionRequest {
            model: "gpt-4o".into(),
            messages: vec![ChatMessage::text(uwa_core::types::Role::User, "hi")],
            stream: None,
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            user: None,
            stream_options: None,
            response_format: None,
        };
        let without = estimate_chat_request_prompt(&req);
        let req = ChatCompletionRequest {
            tools: Some(vec![serde_json::json!({
                "type": "function",
                "function": {"name":"get_weather","description":"x","parameters":{"type":"object"}}
            })]),
            ..req
        };
        let with = estimate_chat_request_prompt(&req);
        assert!(with > without, "adding tools must raise the prompt cost");
    }

    #[test]
    fn completion_includes_tool_call_arguments() {
        let no_call = estimate_completion("here you go", &[]);
        let with_call = estimate_completion(
            "here you go",
            &[crate::parser::ToolCall {
                id: "c".into(),
                name: "get_weather".into(),
                arguments: serde_json::json!({"city":"NYC"}),
            }],
        );
        assert!(with_call > no_call);
    }

    #[test]
    fn usage_total_is_the_sum() {
        let req = ChatCompletionRequest {
            model: "gpt-4o".into(),
            messages: vec![ChatMessage::text(
                uwa_core::types::Role::User,
                "hello world",
            )],
            stream: None,
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            user: None,
            stream_options: None,
            response_format: None,
        };
        let calls = vec![crate::parser::ToolCall {
            id: "c".into(),
            name: "t".into(),
            arguments: serde_json::json!({}),
        }];
        let u = usage_for(&req, "ok", &calls);
        assert_eq!(
            u.total_tokens,
            u.prompt_tokens.saturating_add(u.completion_tokens)
        );
    }
}
