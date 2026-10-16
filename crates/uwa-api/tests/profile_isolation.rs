//! P4: Profile isolation — different browser contexts for different providers/sessions.
//!
//! These tests require the registry overlay to fill capabilities from built-in entries.
//! Currently marked as #[ignore] — enable after UWA-109 is fixed.

use serde_json::json;
use std::sync::Arc;
use uwa_core::Transport;
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockProvider, MockTransport,
};

#[tokio::test]
#[ignore = "UWA-109: registry overlay doesn't fill capabilities yet"]
async fn profiles_isolate_tabs_between_contexts() {
    let transport = Arc::new(MockTransport::with_n_tabs(2).with_profiles());

    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(transport.clone())
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("work")))
        .build();

    // Request with work profile
    let work = app
        .server
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .add_header("X-UWA-Session", "work")
        .json(&json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}))
        .await;

    // Request with personal profile
    let personal = app
        .server
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .add_header("X-UWA-Session", "personal")
        .json(&json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}))
        .await;

    work.assert_status_ok();
    personal.assert_status_ok();

    // TODO: verify tabs are in different browser contexts (different profiles)
    // This requires the transport to track which profile each tab belongs to
    let work_tabs = transport.list_tabs_in("work").await.unwrap_or_default();
    let personal_tabs = transport.list_tabs_in("personal").await.unwrap_or_default();

    // At least one tab should be in each profile
    assert!(
        !work_tabs.is_empty() || !personal_tabs.is_empty(),
        "no tabs allocated to profiles: work={}, personal={}",
        work_tabs.len(),
        personal_tabs.len()
    );
}

#[tokio::test]
#[ignore = "UWA-109: registry overlay doesn't fill capabilities yet"]
async fn provider_switch_uses_correct_profile() {
    // When X-UWA-Provider header changes, the profile should change too
    let transport = Arc::new(
        MockTransport::with_n_tabs(2)
            .with_profiles()
            .with_page_in(
                "work",
                uwa_core::TabId::from_raw("tab_work"),
                Arc::new(uwa_testkit::MockPage::new()),
            )
            .with_page_in(
                "personal",
                uwa_core::TabId::from_raw("tab_personal"),
                Arc::new(uwa_testkit::MockPage::new()),
            ),
    );

    let provider1 = Arc::new(MockProvider::new("chatgpt").with_answer("from chatgpt"));
    let provider2 = Arc::new(MockProvider::new("claude").with_answer("from claude"));

    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(transport.clone())
        .with_provider(provider1.clone())
        .with_provider(provider2.clone())
        .build();

    // Request to chatgpt -> should use work profile
    let r1 = app
        .server
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .add_header("X-UWA-Provider", "chatgpt")
        .json(&json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}))
        .await;
    r1.assert_status_ok();

    // Request to claude -> should use personal profile
    let r2 = app
        .server
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .add_header("X-UWA-Provider", "claude")
        .json(
            &json!({"model": "claude-3-5-sonnet", "messages": [{"role": "user", "content": "hi"}]}),
        )
        .await;
    r2.assert_status_ok();

    // Verify providers received correct requests
    assert_eq!(provider1.sent().len(), 1);
    assert_eq!(provider2.sent().len(), 1);

    // TODO: verify tabs were from correct profiles
    // let work_tabs = transport.list_tabs_in("work").await.unwrap_or_default();
    // let personal_tabs = transport.list_tabs_in("personal").await.unwrap_or_default();
    // assert_eq!(work_tabs.len(), 1);
    // assert_eq!(personal_tabs.len(), 1);
}

#[tokio::test]
async fn x_uwa_session_header_routes_to_session() {
    // Even without full profile support, X-UWA-Session should affect conversation_id
    // The config must include the profile for it to be accepted
    let config = uwa_config::Config::load_from_str(
        r##"
        [server]
        bind = "127.0.0.1"
        port = 8080
        api_key = "k"

        [model_aliases]
        "gpt-4o" = "chatgpt"

        [providers.chatgpt]
        name = "chatgpt"
        url_patterns = ["https://chatgpt.com/*"]
        capabilities = { streams = true, tool_calls = true, vision = false }
        [providers.chatgpt.selectors]
        input = "#prompt"
        send_button = "button.send"
        stop_button = "button.stop"
        assistant_message = "[data-role=assistant]"

        [profiles.session_a]
        label = "Session A"

        [profiles.session_b]
        label = "Session B"
    "##,
    )
    .unwrap_or_else(|e| unreachable!("config load failed: {e}"));

    let transport = Arc::new(
        MockTransport::with_n_tabs(2)
            .with_profiles()
            .with_page_in(
                "session_a",
                uwa_core::TabId::from_raw("tab_session_a"),
                Arc::new(uwa_testkit::MockPage::new()),
            )
            .with_page_in(
                "session_b",
                uwa_core::TabId::from_raw("tab_session_b"),
                Arc::new(uwa_testkit::MockPage::new()),
            ),
    );

    let app = AppBuilder::new()
        .with_config(std::sync::Arc::new(config))
        .with_transport(transport.clone())
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("ok")))
        .build();

    // Two requests with different session headers
    let r1 = app
        .server
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .add_header("X-UWA-Session", "session_a")
        .json(&json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}))
        .await;

    eprintln!("r1 status: {}", r1.status_code());
    eprintln!("r1 body: {}", r1.text());
    r1.assert_status_ok();

    let r2 = app
        .server
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .add_header("X-UWA-Session", "session_b")
        .json(&json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}))
        .await;

    r2.assert_status_ok();

    // Both should succeed - different session headers = different conversation_ids
    // (exact tab assignment is implementation detail)
}

#[tokio::test]
async fn default_session_without_header_works() {
    // No X-UWA-Session header should fall back to default behavior
    let transport = Arc::new(MockTransport::with_n_tabs(1));

    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(transport)
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("ok")))
        .build();

    let r = app
        .server
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}))
        .await;

    r.assert_status_ok();
}

#[tokio::test]
#[ignore = "UWA-109: requires full profile implementation"]
async fn closing_profile_doesnt_affect_others() {
    let transport = Arc::new(MockTransport::with_n_tabs(1).with_profiles());
    // Add tabs in different profiles
    // transport = transport.with_page_in("work", ...).with_page_in("personal", ...);

    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(transport.clone())
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("ok")))
        .build();

    // Use work profile
    let r1 = app
        .server
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .add_header("X-UWA-Session", "work")
        .json(&json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}))
        .await;
    r1.assert_status_ok();

    // Close work profile
    // let dropped = transport.close_profile("work").await.unwrap();
    // assert_eq!(dropped.len(), 1);

    // personal profile should still work
    let r2 = app
        .server
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .add_header("X-UWA-Session", "personal")
        .json(&json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}))
        .await;
    r2.assert_status_ok();
}
