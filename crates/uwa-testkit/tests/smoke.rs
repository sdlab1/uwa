use serde_json::json;
use std::sync::Arc;
use uwa_testkit::{
    config::{config_with_key_lan, TEST_AUTH_HEADER},
    AppBuilder, MockPage, MockProvider, MockTransport,
};

#[test]
fn mock_page_scripts_html_and_eval() {
    // MockPage is an async trait impl; the builder methods are enough to
    // verify it assembles (the real exercise happens via the Transport).
    let page = MockPage::new()
        .with_url("https://demo.test/chat")
        .with_html("<p>hello</p>");
    assert!(page.log().is_empty());
}

#[test]
fn mock_transport_creates_the_tabs_it_promised() {
    let t = MockTransport::with_n_tabs(3);
    assert_eq!(t.tab_ids().len(), 3);
}

#[tokio::test]
async fn builder_transport_has_a_tab_to_type_into() {
    let app = AppBuilder::new()
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("hi")))
        .build();
    let tabs = app.state.transport.list_tabs().await.unwrap_or_else(|_| unreachable!());
    assert!(!tabs.is_empty());
}

#[tokio::test]
async fn app_builder_runs_a_chat_turn() {
    let app = AppBuilder::new()
        .with_key_auth()
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("ok")))
        .build();
    let r = app
        .server
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(
        v.get("choices")
            .unwrap_or_else(|| unreachable!())
            .get(0)
            .unwrap_or_else(|| unreachable!())
            .get("message")
            .unwrap_or_else(|| unreachable!())
            .get("content")
            .unwrap_or_else(|| unreachable!()),
        "ok"
    );
}

#[tokio::test]
async fn keyed_config_requires_bearer_off_loopback() {
    // Loopback binds accept any key by design; off-loopback the
    // configured key is enforced.
    let app = AppBuilder::new()
        .with_config(config_with_key_lan())
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("hi")))
        .build();
    let anon = app
        .server
        .post("/v1/chat/completions")
        .json(&json!({"model": "gpt-4o", "messages": []}));
    assert_eq!(anon.await.status_code(), 401);
}
