//! Streaming requests must also honour `X-UWA-Session`.
//!
//! The Svelte-стриминг lesson says: SSE bodies do not terminate through
//! axum-test's reader; here we only assert the status and the session
//! table, so a plain status check is enough.

use serde_json::{json, Value};
use std::sync::Arc;
use uwa_testkit::{config::TEST_AUTH_HEADER, AppBuilder, MockProvider, MockTransport};

#[tokio::test]
async fn streaming_uses_the_profile_header() {
    let cfg = Arc::new(
        uwa_config::Config::load_from_str(
            r####"
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

            [profiles.work]
            label = "Work"
            "####,
        )
        .unwrap_or_else(|e| unreachable!("test config must parse: {e}")),
    );

    let app = AppBuilder::new()
        .with_config(cfg)
        .with_transport(Arc::new(MockTransport::with_n_tabs(1).with_profiles()))
        .with_provider(Arc::new(
            MockProvider::new("chatgpt").with_answer("streamed"),
        ))
        .with_sessions(Arc::new(uwa_session::SessionManager::new(
            uwa_session::SessionCfg::default(),
        )))
        .build();

    let r = app
        .server
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .add_header("x-uwa-session", "work")
        .json(&json!({
            "model": "gpt-4o",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;
    r.assert_status_ok();

    // The session endpoint must show a `work` session.
    let list = app
        .server
        .get("/admin/sessions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    list.assert_status_ok();
    let v: Value = list.json();
    let names: Vec<String> = v["sessions"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(|s| {
            s["profile"]
                .as_str()
                .map(|p| p.to_string())
                .or_else(|| Some("default".into()))
        })
        .collect();
    assert!(names.contains(&"work".to_string()), "{names:?}");
}
