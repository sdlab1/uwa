use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use uwa_core::traits::{ToolProvider, ToolSpec};
use uwa_core::{Result, UwaError};
use uwa_mcp::ToolRouter;

struct Fake {
    ns: &'static str,
    tools: Vec<&'static str>,
}
#[async_trait]
impl ToolProvider for Fake {
    fn namespace(&self) -> &str {
        self.ns
    }
    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        Ok(self
            .tools
            .iter()
            .map(|n| ToolSpec {
                name: (*n).into(),
                description: "".into(),
                parameters: json!({"type":"object"}),
            })
            .collect())
    }
    async fn call_tool(&self, name: &str, args: Value) -> Result<String> {
        Ok(format!("{}::{}::{}", self.ns, name, args))
    }
}

#[tokio::test]
async fn unique_names_stay_bare() {
    let mut r = ToolRouter::new();
    r.register(Arc::new(Fake {
        ns: "a",
        tools: vec!["foo", "bar"],
    }));
    r.register(Arc::new(Fake {
        ns: "b",
        tools: vec!["baz"],
    }));
    let defs = r.all_definitions().await.unwrap();
    let names: Vec<_> = defs.iter().map(|d| d.name.clone()).collect();
    assert!(names.contains(&"foo".to_string()));
    assert!(names.contains(&"baz".to_string()));
}

#[tokio::test]
async fn collisions_get_prefixed() {
    let mut r = ToolRouter::new();
    r.register(Arc::new(Fake {
        ns: "a",
        tools: vec!["shared"],
    }));
    r.register(Arc::new(Fake {
        ns: "b",
        tools: vec!["shared"],
    }));
    let defs = r.all_definitions().await.unwrap();
    let names: Vec<_> = defs.iter().map(|d| d.name.clone()).collect();
    assert!(names.contains(&"a__shared".to_string()));
    assert!(names.contains(&"b__shared".to_string()));
}

#[tokio::test]
async fn dispatch_routes_to_correct_provider() {
    let mut r = ToolRouter::new();
    r.register(Arc::new(Fake {
        ns: "a",
        tools: vec!["shared"],
    }));
    r.register(Arc::new(Fake {
        ns: "b",
        tools: vec!["shared"],
    }));
    let out = r.dispatch("b__shared", json!({"x": 1})).await.unwrap();
    assert!(out.starts_with("b::shared"));
}

#[tokio::test]
async fn dispatch_unknown_tool_errors() {
    let mut r = ToolRouter::new();
    r.register(Arc::new(Fake {
        ns: "a",
        tools: vec!["foo"],
    }));
    let err = r.dispatch("nope", json!({})).await.unwrap_err();
    assert!(matches!(err, UwaError::BadRequest(_)));
}
