use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use uwa_core::{Page, Result, TabId, Transport, UwaError};
use uwa_mcp::*;

struct T {
    tabs: Vec<TabId>,
}
#[async_trait]
impl Transport for T {
    async fn page(&self, _: &TabId) -> Result<Box<dyn Page>> {
        Err(UwaError::Unavailable("n/a".into()))
    }
    async fn list_tabs(&self) -> Result<Vec<TabId>> {
        Ok(self.tabs.clone())
    }
    async fn health(&self, _: &TabId) -> Result<()> {
        Ok(())
    }
}

fn tabs_handler() -> Arc<WebTabsHandler> {
    Arc::new(WebTabsHandler::new(Arc::new(T {
        tabs: vec![TabId::new()],
    })))
}

#[tokio::test]
async fn tabs_resource_lists_and_reads() {
    let h = tabs_handler();
    let resources = h.resources();
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].uri, "uwa://web/tabs");
    let r = h.read_resource("uwa://web/tabs").await.unwrap();
    assert_eq!(r.contents.len(), 1);
    assert!(r.contents[0].text.as_ref().unwrap().starts_with("["));
}

#[tokio::test]
async fn unknown_resource_errors() {
    let h = tabs_handler();
    assert!(h.read_resource("uwa://bogus").await.is_err());
}

#[tokio::test]
async fn dispatch_routes_resources_list() {
    let srv = Arc::new(McpServer::new("uwa").register(tabs_handler()));
    let req = JsonRpcRequest::new(1, "resources/list", Some(json!({})));
    let resp = srv.dispatch_public(req).await;
    assert!(resp.error.is_none());
    let r: ListResourcesResult = serde_json::from_value(resp.result.unwrap()).unwrap();
    assert_eq!(r.resources.len(), 1);
}

#[tokio::test]
async fn dispatch_routes_prompts_get() {
    let prompts = WebPromptHandler::new(Arc::new(|| vec!["chatgpt".into()]));
    let srv = Arc::new(McpServer::new("uwa").register(Arc::new(prompts)));
    let req = JsonRpcRequest::new(
        2,
        "prompts/get",
        Some(json!({"name": "ask", "arguments": {"provider": "chatgpt", "question": "hi"}})),
    );
    let resp = srv.dispatch_public(req).await;
    assert!(resp.error.is_none(), "{:?}", resp.error);
    let r: GetPromptResult = serde_json::from_value(resp.result.unwrap()).unwrap();
    assert_eq!(r.messages.len(), 1);
}

#[tokio::test]
async fn dispatch_routes_prompts_list() {
    let prompts = WebPromptHandler::new(Arc::new(|| vec!["chatgpt".into(), "claude".into()]));
    let srv = Arc::new(McpServer::new("uwa").register(Arc::new(prompts)));
    let req = JsonRpcRequest::new(3, "prompts/list", Some(json!({})));
    let resp = srv.dispatch_public(req).await;
    assert!(resp.error.is_none());
    let r: ListPromptsResult = serde_json::from_value(resp.result.unwrap()).unwrap();
    assert_eq!(r.prompts.len(), 1);
    assert_eq!(r.prompts[0].name, "ask");
}

#[tokio::test]
async fn capabilities_reflect_handlers() {
    let srv = McpServer::new("uwa")
        .register(tabs_handler())
        .register(Arc::new(WebPromptHandler::new(Arc::new(Vec::new))));
    let caps = srv.server_capabilities();
    assert!(caps.resources.is_some());
    assert!(caps.prompts.is_some());
    assert!(caps.tools.is_some()); // WebTabsHandler exposes tools too
}
