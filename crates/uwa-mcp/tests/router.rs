use serde_json::json;
use std::sync::Arc;
use uwa_core::UwaError;
use uwa_mcp::ToolRouter;
use uwa_testkit::MockToolProvider;

#[tokio::test]
async fn unique_names_stay_bare() {
    let mut r = ToolRouter::new();
    r.register(Arc::new(
        MockToolProvider::new("a").with_tool("foo").with_tool("bar"),
    ));
    r.register(Arc::new(MockToolProvider::new("b").with_tool("baz")));
    let defs = r.all_definitions().await.unwrap();
    let names: Vec<_> = defs.iter().map(|d| d.name.clone()).collect();
    assert!(names.contains(&"foo".to_string()));
    assert!(names.contains(&"baz".to_string()));
}

#[tokio::test]
async fn collisions_get_prefixed() {
    let mut r = ToolRouter::new();
    r.register(Arc::new(MockToolProvider::new("a").with_tool("shared")));
    r.register(Arc::new(MockToolProvider::new("b").with_tool("shared")));
    let defs = r.all_definitions().await.unwrap();
    let names: Vec<_> = defs.iter().map(|d| d.name.clone()).collect();
    assert!(names.contains(&"a__shared".to_string()));
    assert!(names.contains(&"b__shared".to_string()));
}

#[tokio::test]
async fn dispatch_routes_to_correct_provider() {
    let mut r = ToolRouter::new();
    r.register(Arc::new(MockToolProvider::new("a").with_tool("shared")));
    r.register(Arc::new(
        MockToolProvider::new("b")
            .with_tool("shared")
            .with_reply("shared", r#"b::shared::{"x":1}"#),
    ));
    let out = r.dispatch("b__shared", json!({"x": 1})).await.unwrap();
    assert!(out.starts_with("b::shared"));
}

#[tokio::test]
async fn dispatch_unknown_tool_errors() {
    let mut r = ToolRouter::new();
    r.register(Arc::new(MockToolProvider::new("a").with_tool("foo")));
    let err = r.dispatch("nope", json!({})).await.unwrap_err();
    assert!(matches!(err, UwaError::BadRequest(_)));
}
