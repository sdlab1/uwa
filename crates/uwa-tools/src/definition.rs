//! Normalized tool definition + conversion from OpenAI `tools: [...]` JSON.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uwa_core::UwaError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: Value,
}

impl ToolDefinition {
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }

    /// OpenAI shape: `[{ "type":"function", "function": {name, description, parameters} }]`
    pub fn from_openai_array(arr: &[Value]) -> Result<Vec<Self>, UwaError> {
        let mut out = Vec::with_capacity(arr.len());
        for (i, t) in arr.iter().enumerate() {
            let kind = t.get("type").and_then(Value::as_str).unwrap_or("function");
            if kind != "function" {
                return Err(UwaError::BadRequest(format!(
                    "tools[{i}].type=`{kind}` is not supported (only `function`)"
                )));
            }
            let f = t
                .get("function")
                .ok_or_else(|| UwaError::BadRequest(format!("tools[{i}].function is missing")))?;
            let name = f.get("name").and_then(Value::as_str).ok_or_else(|| {
                UwaError::BadRequest(format!("tools[{i}].function.name is missing"))
            })?;
            let description = f
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let parameters = f
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({"type":"object","properties":{}}));
            out.push(ToolDefinition {
                name: name.to_string(),
                description,
                parameters,
            });
        }
        Ok(out)
    }
}
