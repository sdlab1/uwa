//! The doubles have to work together, not only one at a time.

use std::sync::Arc;
use uwa_core::traits::ToolProvider;
use uwa_core::{Page, Transport};
use uwa_testkit::{AppBuilder, MockPage, MockProvider, MockToolProvider, MockTransport};

#[tokio::test]
async fn mock_page_scripts_html_and_eval() {
    let page = MockPage::new()
        .with_html("<html><body>x</body></html>")
        .expect("querySelector(\"#x\")", serde_json::json!(true));

    let hit = page
        .eval("!!document.querySelector(\"#x\")")
        .await
        .expect("scripted answer");
    assert_eq!(hit, serde_json::json!(true));
    assert_eq!(
        page.html().await.expect("html"),
        "<html><body>x</body></html>"
    );
}

#[tokio::test]
async fn app_builder_runs_a_chat_turn() {
    let app = AppBuilder::new()
        .with_provider(Arc::new(
            MockProvider::new("chatgpt").with_answer("hi there"),
        ))
        .build();

    let resp = app
        .server
        .post("/v1/chat/completions")
        .json(&serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "x"}]
        }))
        .await;
    resp.assert_status_ok();

    let body: serde_json::Value = resp.json();
    assert_eq!(
        body["choices"][0]["message"]["content"].as_str(),
        Some("hi there")
    );
    assert_eq!(body["model"].as_str(), Some("gpt-4o"));
}

#[tokio::test]
async fn mock_tool_provider_dispatches() {
    let tp = MockToolProvider::new("ns")
        .with_tool("echo")
        .with_reply("echo", "pong");
    let out = tp
        .call_tool("echo", serde_json::json!({"x": 1}))
        .await
        .expect("reply");
    assert_eq!(out, "pong");
    assert_eq!(tp.list_tools().await.expect("tools")[0].name, "echo");
}

#[tokio::test]
async fn builder_transport_has_a_tab_to_type_into() {
    let app = AppBuilder::new().build();
    let tabs = app.state.transport.list_tabs().await.expect("tabs");
    assert_eq!(tabs.len(), 1, "the pipeline picks the first tab it finds");
    app.server.get("/healthz").await.assert_status_ok();
}

#[tokio::test]
async fn mock_transport_creates_the_tabs_it_promised() {
    let t = MockTransport::with_n_tabs(2);
    let tabs = t.list_tabs().await.expect("tabs");
    assert_eq!(tabs.len(), 2);
    assert!(t.page(&tabs[0]).await.is_ok());
}
