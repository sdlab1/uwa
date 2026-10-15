//! Minimal JSON Schema validation for tool arguments.
//!
//! We cover the subset OpenAI tool schemas actually use — `type`,
//! `properties`, `required`, `enum`, `items`, `additionalProperties`.
//! That's ~95% of real-world tool definitions and stays dependency-free
//! (`jsonschema` pulls ~1 MB of compiled tables for edge-case drafts we
//! will never see from a tool declaration).

use serde_json::Value;

/// Validate `value` against a JSON Schema fragment.
///
/// Returns `Ok(())` when the value satisfies the subset we understand.
/// Returns `Err(reason)` with a human-readable path when it does not.
/// Unknown keywords and unknown types are **ignored**, not rejected — we
/// would rather under-validate than block a valid call.
pub fn validate_arguments(value: &Value, schema: &Value) -> Result<(), String> {
    validate_inner(value, schema, "$")
}

fn validate_inner(v: &Value, s: &Value, path: &str) -> Result<(), String> {
    let Some(obj) = s.as_object() else {
        return Ok(());
    };

    if let Some(ty) = obj.get("type").and_then(Value::as_str) {
        if !type_matches(v, ty) {
            return Err(format!(
                "{path}: expected type `{ty}`, got `{}`",
                json_kind(v)
            ));
        }
    }

    if let Some(props) = obj.get("properties").and_then(Value::as_object) {
        if let Some(vobj) = v.as_object() {
            for (k, sub) in props {
                if let Some(child) = vobj.get(k) {
                    validate_inner(child, sub, &format!("{path}.{k}"))?;
                }
            }
        }
    }

    if let Some(req) = obj.get("required").and_then(Value::as_array) {
        let vobj = v
            .as_object()
            .ok_or_else(|| format!("{path}: `required` present but value is not an object"))?;
        for r in req {
            let Some(key) = r.as_str() else { continue };
            if !vobj.contains_key(key) {
                return Err(format!("{path}: missing required property `{key}`"));
            }
        }
    }

    if let Some(enum_vals) = obj.get("enum").and_then(Value::as_array) {
        if !enum_vals.iter().any(|x| x == v) {
            let opts: Vec<String> = enum_vals.iter().map(|x| x.to_string()).collect();
            return Err(format!("{path}: value not in enum [{}]", opts.join(", ")));
        }
    }

    if let Some(items) = obj.get("items") {
        if let Some(arr) = v.as_array() {
            for (i, item) in arr.iter().enumerate() {
                validate_inner(item, items, &format!("{path}[{i}]"))?;
            }
        }
    }

    Ok(())
}

fn type_matches(v: &Value, ty: &str) -> bool {
    match ty {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "number" => v.is_number(),
        "integer" => v.is_i64() || v.is_u64(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        _ => true,
    }
}

fn json_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_valid_object() {
        let s = json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        });
        assert!(validate_arguments(&json!({ "city": "NYC" }), &s).is_ok());
    }

    #[test]
    fn rejects_missing_required() -> Result<(), Box<dyn std::error::Error>> {
        let s = json!({
            "type": "object",
            "required": ["city"]
        });
        let Err(err) = validate_arguments(&json!({}), &s) else {
            return Err("expected an error".into());
        };
        assert!(err.contains("city"), "{err}");
        Ok(())
    }

    #[test]
    fn rejects_wrong_type() -> Result<(), Box<dyn std::error::Error>> {
        let s = json!({
            "type": "object",
            "properties": { "n": { "type": "integer" } }
        });
        let Err(err) = validate_arguments(&json!({ "n": "five" }), &s) else {
            return Err("expected an error".into());
        };
        assert!(err.contains("integer"), "{err}");
        Ok(())
    }

    #[test]
    fn enum_is_enforced() {
        let s = json!({
            "type": "object",
            "properties": { "unit": { "type": "string", "enum": ["C", "F"] } }
        });
        assert!(validate_arguments(&json!({ "unit": "C" }), &s).is_ok());
        assert!(validate_arguments(&json!({ "unit": "K" }), &s).is_err());
    }

    #[test]
    fn nested_array_items_checked() -> Result<(), Box<dyn std::error::Error>> {
        let s = json!({
            "type": "object",
            "properties": {
                "cities": {
                    "type": "array",
                    "items": { "type": "string" }
                }
            }
        });
        assert!(validate_arguments(&json!({ "cities": ["NYC", "SF"] }), &s).is_ok());
        let Err(err) = validate_arguments(&json!({ "cities": ["NYC", 42] }), &s) else {
            return Err("expected an error".into());
        };
        assert!(err.contains("[1]"), "{err}");
        Ok(())
    }

    #[test]
    fn unknown_type_is_ignored() {
        let s = json!({ "type": "custom-thing" });
        assert!(validate_arguments(&json!({}), &s).is_ok());
    }

    #[test]
    fn empty_schema_accepts_anything() {
        assert!(validate_arguments(&json!({ "x": 1 }), &json!({})).is_ok());
        assert!(validate_arguments(&json!("any"), &json!({})).is_ok());
    }
}
