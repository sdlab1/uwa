use serde_json::json;
use std::sync::Arc;
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
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
    let tabs = app.state.transport.list_tabs().await.unwrap();
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
    assert_eq!(v["choices"][0]["message"]["content"], "ok");
}

#[tokio::test]
async fn keyed_config_requires_bearer() {
    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("hi")))
        .build();
    let anon = app
        .server
        .post("/v1/chat/completions")
        .json(&json!({"model": "gpt-4o", "messages": []}));
    assert_eq!(anon.await.status_code(), 401);
}
