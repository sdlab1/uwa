//! Prompt building and validation for `response_format` requests.
//!
//! The whole module is pure: `build_prompt` produces text, `clean`
//! extracts a candidate JSON blob from a possibly-wrapped model reply,
//! and `validate` checks the parsed value against the requested format.
//! Nothing here talks to the browser or the config.

use uwa_core::types::openai::{JsonSchemaRef, ResponseFormat};

/// The instruction appended to the system prompt when the client
/// requested structured output. Kept short: web-UI models already see a
/// lot of text, and every token here competes with the actual task.
pub fn build_prompt(fmt: &ResponseFormat) -> Option<String> {
    match fmt {
        ResponseFormat::Text => None,
        ResponseFormat::JsonObject => Some(
            "You must answer with a single JSON object. No prose, no \
             explanations, no markdown fences — only the raw JSON. Any \
             text outside the JSON object is a protocol violation.\n"
                .into(),
        ),
        ResponseFormat::JsonSchema { json_schema } => Some(build_schema_prompt(json_schema)),
    }
}

fn build_schema_prompt(s: &JsonSchemaRef) -> String {
    let schema_str = serde_json::to_string_pretty(&s.schema).unwrap_or_else(|_| "{}".to_string());
    let desc = s
        .description
        .as_deref()
        .filter(|d| !d.is_empty())
        .map(|d| format!("\nDescription: {d}"))
        .unwrap_or_default();
    format!(
        "You must answer with a single JSON object that strictly \
         conforms to the following JSON Schema. No prose, no \
         explanations, no markdown fences — only the raw JSON.\n\
         \n\
         Schema name: {name}{desc}\n\
         Schema:\n{schema_str}\n",
        name = s.name,
        desc = desc,
        schema_str = schema_str,
    )
}

/// The correction the pipeline sends back when validation fails.
pub fn correction_prompt(error: &str, fmt: &ResponseFormat) -> String {
    let suffix = match fmt {
        ResponseFormat::Text => String::new(),
        ResponseFormat::JsonObject => {
            "Emit only the raw JSON object. No text before or after it.".into()
        }
        ResponseFormat::JsonSchema { json_schema } => {
            let schema_str =
                serde_json::to_string_pretty(&json_schema.schema).unwrap_or_else(|_| "{}".into());
            format!(
                "Re-emit only the raw JSON object, and make sure every \
                 field matches the schema exactly:\n{schema_str}",
            )
        }
    };
    format!(
        "[JSON-mode correction — not from the user]\n\n\
         Your previous answer did not satisfy the requested format:\n\
         {error}\n\n\
         {suffix}\n",
    )
}

/// Extract a JSON blob from a model reply that may have leading prose,
/// ```json fences, or a trailing explanation.
///
/// Returns the *cleaned* string (fences stripped, prose removed) if a
/// value was found. The return value is what the caller should hand
/// back to the client on success.
///
/// This is deliberately permissive about where the JSON sits and strict
/// about what the JSON is: if the extracted substring parses, it's a
/// candidate; the caller decides whether it satisfies the requested
/// schema.
pub fn clean(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("response is empty".into());
    }

    // Fenced path: ```json\n{...}\n``` or ```\n{...}\n```.
    if let Some(body) = strip_fence(trimmed) {
        let b = body.trim();
        if b.is_empty() {
            return Err("fenced block is empty".into());
        }
        serde_json::from_str::<serde_json::Value>(b)
            .map_err(|e| format!("fenced block is not valid JSON: {e}"))?;
        return Ok(b.to_string());
    }

    // Direct parse.
    if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
        return Ok(trimmed.to_string());
    }

    // Fall back: hunt for the first `{` and matching `}` using a
    // char-level scan that respects strings and escapes. This catches
    // "here you go: {...} thanks!" without eating the middle of a
    // string.
    if let Some((start, end)) = first_json_object(trimmed) {
        let candidate = &trimmed[start..end];
        if serde_json::from_str::<serde_json::Value>(candidate).is_ok() {
            return Ok(candidate.to_string());
        }
    }

    Err("no JSON value found in the response".into())
}

fn strip_fence(s: &str) -> Option<&str> {
    let s = s.trim_start();
    if !s.starts_with("```") {
        return None;
    }
    // Skip the opening fence line.
    let after_open = s[3..].trim_start_matches(|c: char| c.is_ascii_alphabetic());
    let after_open = after_open.strip_prefix('\n').unwrap_or(after_open);
    // Find the closing fence.
    let close = after_open.find("\n```")?;
    Some(&after_open[..close])
}

/// Byte range of the first balanced `{...}` block that respects strings.
/// Returns `None` when no opening brace exists. `depth` is a saturating
/// counter: the scan starts at the first `{`, so it never underflows
/// before the matching `}`.
fn first_json_object(s: &str) -> Option<(usize, usize)> {
    let open = s.find('{')?;
    let mut depth: usize = 0;
    let mut in_str = false;
    let mut escape = false;
    for (i, c) in s[open..].char_indices() {
        let abs = open.saturating_add(i);
        if in_str {
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_str = false;
            }
        } else {
            match c {
                '"' => in_str = true,
                '{' => depth = depth.saturating_add(1),
                '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some((open, abs.saturating_add(c.len_utf8())));
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// Validate the cleaned string against the requested format. On success
/// returns the parsed value (for the caller's convenience); on failure
/// returns the error message that goes into the retry prompt and, if
/// retries are exhausted, into the 400.
pub fn validate(cleaned: &str, fmt: &ResponseFormat) -> Result<serde_json::Value, String> {
    let value: serde_json::Value =
        serde_json::from_str(cleaned).map_err(|e| format!("not valid JSON: {e}"))?;

    match fmt {
        ResponseFormat::Text => Ok(value),
        ResponseFormat::JsonObject => {
            if !value.is_object() {
                return Err(format!(
                    "expected a JSON object at the top level, got {}",
                    json_kind(&value)
                ));
            }
            Ok(value)
        }
        ResponseFormat::JsonSchema { json_schema } => {
            // `validate_arguments` is the same validator the tool-call
            // path uses; the shape it expects is exactly a JSON Schema
            // fragment and a candidate value.
            crate::schema::validate_arguments(&value, &json_schema.schema)
                .map_err(|e| format!("schema violation: {e}"))?;
            Ok(value)
        }
    }
}

fn json_kind(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// How many retries a `response_format` request gets, given the
/// provider's `max_tool_retries` and the schema's `strict` bit. The
/// strict path uses the full budget; the lenient path halves it.
pub fn retry_budget(max_tool_retries: u8, fmt: &ResponseFormat) -> usize {
    let base = max_tool_retries as usize;
    match fmt {
        ResponseFormat::JsonSchema { json_schema } if !json_schema.strict => {
            base.checked_div(2).unwrap_or(0)
        }
        _ => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj_fmt() -> ResponseFormat {
        ResponseFormat::JsonObject
    }

    fn schema_fmt(schema: serde_json::Value, strict: bool) -> ResponseFormat {
        ResponseFormat::JsonSchema {
            json_schema: JsonSchemaRef {
                name: "test".into(),
                description: None,
                schema,
                strict,
            },
        }
    }

    // ---- prompt building ----

    #[test]
    fn text_format_produces_no_prompt() {
        assert!(build_prompt(&ResponseFormat::Text).is_none());
    }

    #[test]
    fn json_object_prompt_mentions_protocol() {
        let Some(p) = build_prompt(&obj_fmt()) else {
            unreachable!("json_object must produce a prompt")
        };
        assert!(p.contains("JSON object"));
        assert!(p.contains("No prose"));
    }

    #[test]
    fn json_schema_prompt_embeds_schema() {
        let fmt = schema_fmt(
            json!({"type":"object","properties":{"x":{"type":"string"}}}),
            true,
        );
        let Some(p) = build_prompt(&fmt) else {
            unreachable!("json_schema must produce a prompt")
        };
        assert!(p.contains("test"));
        assert!(p.contains("\"x\""));
        assert!(p.contains("string"));
    }

    // ---- cleaning ----

    #[test]
    fn clean_accepts_bare_object() {
        assert_eq!(clean(r#"{"a":1}"#).unwrap_or_default(), r#"{"a":1}"#);
    }

    #[test]
    fn clean_strips_json_fence() {
        let s = "```json\n{\"a\":1}\n```";
        assert_eq!(clean(s).unwrap_or_default(), r#"{"a":1}"#);
    }

    #[test]
    fn clean_strips_bare_fence() {
        let s = "```\n{\"a\":1}\n```";
        assert_eq!(clean(s).unwrap_or_default(), r#"{"a":1}"#);
    }

    #[test]
    fn clean_extracts_from_prose() {
        let s = "Sure, here you go:\n{\"a\":1}\nHope that helps!";
        assert_eq!(clean(s).unwrap_or_default(), r#"{"a":1}"#);
    }

    #[test]
    fn clean_respects_strings_with_braces() {
        let s = r#"prefix {"note":"a { b } c"} suffix"#;
        assert_eq!(clean(s).unwrap_or_default(), r#"{"note":"a { b } c"}"#);
    }

    #[test]
    fn clean_rejects_empty() {
        assert!(clean("").is_err());
        assert!(clean("   \n  ").is_err());
    }

    #[test]
    fn clean_rejects_no_json() {
        assert!(clean("plain text, no braces").is_err());
    }

    #[test]
    fn clean_rejects_unbalanced_braces() {
        assert!(clean("{\"a\":1").is_err());
    }

    // ---- validation ----

    #[test]
    fn validate_object_requires_object() {
        assert!(validate(r#"{"a":1}"#, &obj_fmt()).is_ok());
        let e = match validate(r#"[1,2,3]"#, &obj_fmt()) {
            Err(e) => e,
            Ok(_) => unreachable!("expected validation error"),
        };
        assert!(e.contains("array"), "{e}");
        let e = match validate(r#""text""#, &obj_fmt()) {
            Err(e) => e,
            Ok(_) => unreachable!("expected validation error"),
        };
        assert!(e.contains("string"), "{e}");
    }

    #[test]
    fn validate_schema_enforces_required() {
        let fmt = schema_fmt(
            json!({
                "type": "object",
                "required": ["city"]
            }),
            true,
        );
        assert!(validate(r#"{"city":"NYC"}"#, &fmt).is_ok());
        let e = match validate(r#"{}"#, &fmt) {
            Err(e) => e,
            Ok(_) => unreachable!("expected validation error"),
        };
        assert!(e.contains("city"), "{e}");
    }

    #[test]
    fn validate_schema_checks_types() {
        let fmt = schema_fmt(
            json!({
                "type": "object",
                "properties": {"n": {"type":"integer"}}
            }),
            true,
        );
        assert!(validate(r#"{"n":3}"#, &fmt).is_ok());
        let e = match validate(r#"{"n":"three"}"#, &fmt) {
            Err(e) => e,
            Ok(_) => unreachable!("expected validation error"),
        };
        assert!(e.contains("integer"), "{e}");
    }

    #[test]
    fn validate_rejects_malformed_json() {
        let e = match validate("{not json}", &obj_fmt()) {
            Err(e) => e,
            Ok(_) => unreachable!("expected validation error"),
        };
        assert!(e.contains("not valid JSON"), "{e}");
    }

    // ---- retry budget ----

    #[test]
    fn retry_budget_uses_full_for_strict() {
        let fmt = schema_fmt(json!({"type":"object"}), true);
        assert_eq!(retry_budget(4, &fmt), 4);
        assert_eq!(retry_budget(4, &obj_fmt()), 4);
    }

    #[test]
    fn retry_budget_halves_for_lenient() {
        let fmt = schema_fmt(json!({"type":"object"}), false);
        assert_eq!(retry_budget(4, &fmt), 2);
        assert_eq!(retry_budget(1, &fmt), 0);
    }

    // ---- correction prompt ----

    #[test]
    fn correction_prompt_names_the_error() {
        let p = correction_prompt("schema violation: missing `city`", &obj_fmt());
        assert!(p.contains("missing"));
        assert!(p.contains("correction"));
    }
}
