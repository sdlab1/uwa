//! [`ToolProvider`](uwa_core::traits::ToolProvider) mock for `ToolRouter`
//! and pipeline tests.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use uwa_core::traits::{ToolProvider, ToolSpec};
use uwa_core::{Result, UwaError};

/// A namespace-scoped tool provider with scripted replies.
pub struct MockToolProvider {
    namespace: String,
    tools: Vec<ToolSpec>,
    /// tool name → text returned for a successful call
    replies: HashMap<String, String>,
    /// tool name → error message; overrides `replies`
    errors: HashMap<String, String>,
}

impl MockToolProvider {
    pub fn new(ns: &str) -> Self {
        Self {
            namespace: ns.into(),
            tools: Vec::new(),
            replies: HashMap::new(),
            errors: HashMap::new(),
        }
    }

    /// Register a tool with an empty-object schema.
    pub fn with_tool(mut self, name: &str) -> Self {
        self.tools.push(ToolSpec {
            name: name.into(),
            description: format!("mock tool {name}"),
            parameters: json!({"type": "object"}),
        });
        self
    }

    /// Register a fully specified tool.
    pub fn with_spec(mut self, spec: ToolSpec) -> Self {
        self.tools.push(spec);
        self
    }

    pub fn with_reply(mut self, tool: &str, reply: &str) -> Self {
        self.replies.insert(tool.into(), reply.into());
        self
    }

    pub fn with_error(mut self, tool: &str, msg: &str) -> Self {
        self.errors.insert(tool.into(), msg.into());
        self
    }
}

#[async_trait]
impl ToolProvider for MockToolProvider {
    fn namespace(&self) -> &str {
        &self.namespace
    }

    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        Ok(self.tools.clone())
    }

    async fn call_tool(&self, name: &str, args: Value) -> Result<String> {
        if let Some(msg) = self.errors.get(name) {
            return Err(UwaError::BadRequest(msg.clone()));
        }
        if let Some(r) = self.replies.get(name) {
            return Ok(r.clone());
        }
        Ok(serde_json::to_string(&args).unwrap_or_else(|_| args.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lists_only_registered_tools() {
        let tp = MockToolProvider::new("ns")
            .with_tool("echo")
            .with_tool("add");
        let tools = tp.list_tools().await.expect("tools");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "echo");
        assert_eq!(tp.namespace(), "ns");
    }

    #[tokio::test]
    async fn replies_override_the_json_echo() {
        let tp = MockToolProvider::new("").with_reply("echo", "pong");
        assert_eq!(
            tp.call_tool("echo", json!({"x": 1})).await.expect("reply"),
            "pong"
        );
        assert_eq!(
            tp.call_tool("other", json!({"x": 1})).await.expect("echo"),
            r#"{"x":1}"#
        );
    }

    #[tokio::test]
    async fn scripted_errors_surface_as_bad_request() {
        let tp = MockToolProvider::new("").with_error("boom", "nope");
        let err = tp.call_tool("boom", json!({})).await.expect_err("fails");
        assert!(matches!(err, UwaError::BadRequest(_)), "{err:?}");
    }
}
