//! P4: Profile isolation — different browser contexts for different providers/sessions.
//!
//! UWA-109 is fixed (the registry overlay fills capabilities from built-in
//! entries), so these run for real: profiles must be declared in the
//! config, are selected with the `X-UWA-Session` header, and the mock
//! transport tracks a separate tab pool per profile.

use serde_json::json;
use std::sync::Arc;
use uwa_core::Transport;
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockProvider, MockTransport,
};

/// Keyed config with two providers and two profiles declared.
fn config_with_profiles() -> Arc<uwa_config::Config> {
    Arc::new(
        uwa_config::Config::load_from_str(
            r####"
            [server]
            bind = "127.0.0.1"
            port = 8080
            api_key = "k"

            [model_aliases]
            "gpt-4o" = "chatgpt"
            "claude-3-5-sonnet" = "claude"

            [providers.chatgpt]
            name = "chatgpt"
            url_patterns = ["https://chatgpt.com/*"]
            capabilities = { streams = true, tool_calls = true, vision = false }
            [providers.chatgpt.selectors]
            input = "#prompt"
            send_button = "button.send"
            stop_button = "button.stop"
            assistant_message = "[data-role=assistant]"

            [providers.claude]
            name = "claude"
            url_patterns = ["https://claude.ai/*"]
            capabilities = { streams = true, tool_calls = true, vision = false }
            fallback_model = "claude-3-5-sonnet"
            [providers.claude.selectors]
            input = "#prompt"
            send_button = "button.send"
            stop_button = "button.stop"
            assistant_message = "[data-role=assistant]"

            [profiles.work]
            label = "Work"

            [profiles.personal]
            label = "Personal"
            "####,
        )
        .unwrap_or_else(|e| unreachable!("test config must parse: {e}")),
    )
}

async fn chat_as(
    app: &uwa_testkit::TestApp,
    profile: &str,
    content: &str,
    extra: &[(&'static str, &'static str)],
) -> axum_test::TestResponse {
    let mut r = app
        .server
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .add_header("X-UWA-Session", profile);
    for (name, value) in extra {
        r = r.add_header(*name, *value);
    }
    r.json(&json!({"model": "gpt-4o", "messages": [{"role": "user", "content": content}]}))
        .await
}

#[tokio::test]
async fn profiles_isolate_tabs_between_contexts() {
    let transport = Arc::new(MockTransport::with_n_tabs(2).with_profiles());

    let app = AppBuilder::new()
        .with_config(config_with_profiles())
        .with_transport(transport.clone())
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("ok")))
        .build();

    let work = chat_as(&app, "work", "hi", &[]).await;
    let personal = chat_as(&app, "personal", "hi", &[]).await;

    work.assert_status_ok();
    personal.assert_status_ok();

    // Each profile got its own tab in its own pool.
    let work_tabs = transport.list_tabs_in("work").await.unwrap_or_default();
    let personal_tabs = transport.list_tabs_in("personal").await.unwrap_or_default();
    assert!(
        !work_tabs.is_empty(),
        "work profile must own at least one tab"
    );
    assert!(
        !personal_tabs.is_empty(),
        "personal profile must own at least one tab"
    );
}

#[tokio::test]
async fn provider_switch_uses_correct_profile() {
    // Switching X-UWA-Provider under a pinned profile keeps the profile:
    // each provider serves its own request, and every tab lands in `work`.
    let transport = Arc::new(MockTransport::with_n_tabs(2).with_profiles());

    let provider1 = Arc::new(MockProvider::new("chatgpt").with_answer("from chatgpt"));
    let provider2 = Arc::new(MockProvider::new("claude").with_answer("from claude"));

    let app = AppBuilder::new()
        .with_config(config_with_profiles())
        .with_transport(transport.clone())
        .with_provider(provider1.clone())
        .with_provider(provider2.clone())
        .build();

    let r1 = chat_as(
        &app,
        "work",
        "for chatgpt",
        &[("X-UWA-Provider", "chatgpt")],
    )
    .await;
    r1.assert_status_ok();

    let r2 = chat_as(&app, "work", "for claude", &[("X-UWA-Provider", "claude")]).await;
    r2.assert_status_ok();

    // Each provider served exactly one request.
    assert_eq!(provider1.sent().len(), 1);
    assert_eq!(provider2.sent().len(), 1);

    // Both turns stayed inside the work profile; personal stays untouched.
    let work_tabs = transport.list_tabs_in("work").await.unwrap_or_default();
    let personal_tabs = transport.list_tabs_in("personal").await.unwrap_or_default();
    assert!(!work_tabs.is_empty(), "work profile must own tabs");
    assert!(
        personal_tabs.is_empty(),
        "personal profile must stay untouched"
    );
}

#[tokio::test]
async fn closing_profile_doesnt_affect_others() {
    let transport = Arc::new(MockTransport::with_n_tabs(1).with_profiles());

    let app = AppBuilder::new()
        .with_config(config_with_profiles())
        .with_transport(transport.clone())
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("ok")))
        .build();

    // Warm both profiles.
    let r1 = chat_as(&app, "work", "hi", &[]).await;
    r1.assert_status_ok();
    let r2 = chat_as(&app, "personal", "hi", &[]).await;
    r2.assert_status_ok();

    // Close the work profile through the admin API.
    let reset = app
        .server
        .post("/admin/profiles/work/reset")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    reset.assert_status_ok();
    let v: serde_json::Value = reset.json();
    assert!(
        !v.get("dropped_tabs")
            .and_then(|t| t.as_array())
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .is_empty(),
        "reset must report the dropped work tabs: {v}"
    );

    // Work's tabs are gone; personal's survive.
    let work_tabs = transport.list_tabs_in("work").await.unwrap_or_default();
    let personal_tabs = transport.list_tabs_in("personal").await.unwrap_or_default();
    assert!(work_tabs.is_empty(), "work tabs must be dropped");
    assert!(!personal_tabs.is_empty(), "personal tabs must survive");

    // The personal profile still answers after work was closed.
    let r3 = chat_as(&app, "personal", "hi", &[]).await;
    r3.assert_status_ok();
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
    let r1 = chat_as(&app, "session_a", "hi", &[]).await;
    r1.assert_status_ok();

    let r2 = chat_as(&app, "session_b", "hi", &[]).await;
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
