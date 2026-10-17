use axum_test::TestServer;
use serde_json::json;
use std::sync::Arc;
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockProvider, MockTransport,
};

/// Config with two providers: chatgpt (default alias) and claude.
fn two_provider_config() -> Arc<uwa_config::Config> {
    Arc::new(
        uwa_config::Config::load_from_str(
            r##"
            [server]
            bind = "127.0.0.1"
            port = 8080
            api_key = "k"

            [model_aliases]
            "gpt-4o" = "chatgpt"
            "claude" = "claude"

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
            [providers.claude.selectors]
            input = "#prompt"
            send_button = "button.send"
            stop_button = "button.stop"
            assistant_message = "[data-role=assistant]"
        "##,
        )
        .unwrap_or_else(|e| unreachable!("two-provider config rejected: {e}")),
    )
}

/// Build a server with two providers registered (chatgpt + claude).
fn two_provider_server() -> (TestServer, Arc<MockProvider>, Arc<MockProvider>) {
    let chatgpt = Arc::new(MockProvider::new("chatgpt").with_answer("from chatgpt"));
    let claude = Arc::new(MockProvider::new("claude").with_answer("from claude"));
    let chatgpt_dyn: Arc<dyn uwa_core::traits::SiteProvider> = chatgpt.clone();
    let claude_dyn: Arc<dyn uwa_core::traits::SiteProvider> = claude.clone();
    let app = AppBuilder::new()
        .with_config(two_provider_config())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(chatgpt_dyn)
        .with_provider(claude_dyn)
        .build();
    (app.server, chatgpt, claude)
}

fn server(answer: &str) -> TestServer {
    AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer(answer)))
        .build()
        .server
}

#[tokio::test]
async fn plain_text_end_turn() {
    let r = server("hello")
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(v.get("type").and_then(|v| v.as_str()), Some("message"));
    assert_eq!(v.get("role").and_then(|v| v.as_str()), Some("assistant"));
    assert_eq!(
        v.get("content")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("type"))
            .and_then(|c| c.as_str()),
        Some("text")
    );
    assert_eq!(
        v.get("content")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("text"))
            .and_then(|c| c.as_str()),
        Some("hello")
    );
    assert_eq!(
        v.get("stop_reason").and_then(|v| v.as_str()),
        Some("end_turn")
    );
}

#[tokio::test]
async fn tool_use_block_emitted() {
    let answer = "checking\n<tool_call>{\"name\":\"get_weather\",\"arguments\":{\"city\":\"NYC\"}}</tool_call>";
    let r = server(answer)
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "weather?"}],
            "tools": [{
                "name": "get_weather",
                "description": "w",
                "input_schema": {"type": "object"}
            }]
        }))
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(
        v.get("stop_reason").and_then(|v| v.as_str()),
        Some("tool_use")
    );
    let tool_use = v
        .get("content")
        .and_then(|c| c.as_array())
        .unwrap_or_else(|| unreachable!("content array missing"))
        .iter()
        .find(|c| c.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
        .unwrap_or_else(|| unreachable!("tool_use block not found"));
    assert_eq!(
        tool_use.get("name").and_then(|n| n.as_str()),
        Some("get_weather")
    );
    assert_eq!(
        tool_use
            .get("input")
            .and_then(|i| i.get("city"))
            .and_then(|c| c.as_str()),
        Some("NYC")
    );
}

#[tokio::test]
async fn streaming_shape() {
    let r = server("hi")
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 64,
            "stream": true,
            "messages": [{"role": "user", "content": "x"}]
        }))
        .await;
    r.assert_status_ok();
    let body = r.text();
    assert!(body.contains("message_start"));
    assert!(body.contains("content_block_start"));
    assert!(body.contains("content_block_delta"));
    assert!(body.contains("content_block_stop"));
    assert!(body.contains("message_delta"));
    assert!(body.contains("message_stop"));
}

/// Multi-turn: assistant asked for a tool, user supplies the result.
/// Regression: tool_result messages must be passed through to the provider
/// so the site can see them and close the tool loop.
#[tokio::test]
async fn tool_result_roundtrip() {
    let provider_raw = Arc::new(
        MockProvider::new("chatgpt").with_answers([
            "checking\n<tool_call>{\"name\":\"get_weather\",\"arguments\":{\"city\":\"NYC\"}}</tool_call>",
            "thanks after tool result",
        ]),
    );
    let provider_dyn: Arc<dyn uwa_core::traits::SiteProvider> = provider_raw.clone();
    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(provider_dyn)
        .build();

    // Turn 1: assistant asks for a tool — provider returns a tool_use block.
    let r1 = app
        .server
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "weather?"}],
            "tools": [{
                "name": "get_weather",
                "description": "w",
                "input_schema": {"type": "object"}
            }]
        }))
        .await;
    r1.assert_status_ok();
    let v1: serde_json::Value = r1.json();
    assert_eq!(
        v1.get("stop_reason").and_then(|v| v.as_str()),
        Some("tool_use")
    );
    // The tool_use block must be present.
    assert!(
        v1.get("content").and_then(|c| c.as_array()).is_some(),
        "content array missing: {v1}"
    );

    // Turn 2: client supplies the tool result — provider must receive it.
    let r2 = app
        .server
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 64,
            "messages": [
                {"role": "user", "content": "weather?"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "t1", "name": "w", "input": {"city": "NYC"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "22C"}
                ]}
            ]
        }))
        .await;
    r2.assert_status_ok();
    let v2: serde_json::Value = r2.json();
    assert_eq!(
        v2.get("content")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("text"))
            .and_then(|c| c.as_str()),
        Some("thanks after tool result")
    );

    // The provider's last received prompt must contain the tool result.
    let last = provider_raw.sent_last();
    assert!(
        last.contains("22C"),
        "provider should see the tool result in its prompt: {last}"
    );
}

/// Regression: `X-UWA-Provider` header must select the named provider,
/// not the model-alias table default. With two providers registered
/// (chatgpt + claude), sending `X-UWA-Provider: claude` routes to claude.
#[tokio::test]
async fn x_uwa_provider_routes_to_named_provider() {
    let (server, chatgpt, claude) = two_provider_server();

    // Without the header: alias table picks chatgpt (default for gpt-4o).
    let r_default = server
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;
    r_default.assert_status_ok();
    assert!(
        chatgpt.sent_last().contains("hi"),
        "chatgpt should receive the message by default: {:?}",
        chatgpt.sent()
    );
    assert!(
        claude.sent().is_empty(),
        "claude should not receive the message by default: {:?}",
        claude.sent()
    );

    // With X-UWA-Provider: claude — must route to claude.
    let r_claude = server
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .add_header("X-UWA-Provider", "claude")
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi claude"}]
        }))
        .await;
    r_claude.assert_status_ok();
    assert!(
        claude.sent_last().contains("hi claude"),
        "claude should receive the message with X-UWA-Provider: claude: {:?}",
        claude.sent()
    );
}

/// Regression: `X-UWA-Session: default` header is accepted — `"default"`
/// is always a valid profile (UWA-137).
#[tokio::test]
async fn x_uwa_session_default_profile_is_accepted() {
    let (server, _, _) = two_provider_server();

    let r = server
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .add_header("X-UWA-Session", "default")
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi from default profile"}]
        }))
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(v.get("type").and_then(|v| v.as_str()), Some("message"));
    assert_eq!(
        v.get("content")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("text"))
            .and_then(|c| c.as_str()),
        Some("from chatgpt")
    );
}

/// Regression: unknown `X-UWA-Session` profile returns 400 Bad Request,
/// not a 500 or silent fallback.
#[tokio::test]
async fn x_uwa_session_unknown_profile_is_400() {
    let (server, _, _) = two_provider_server();

    let r = server
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .add_header("X-UWA-Session", "nonexistent")
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;
    assert_eq!(r.status_code(), 400);
}

/// Regression: unknown `X-UWA-Provider` value returns 400 Bad Request
/// (`NoProviderForUrl` maps to 400), not a 500 or silent fallback.
#[tokio::test]
async fn x_uwa_provider_unknown_is_400() {
    let (server, _, _) = two_provider_server();

    let r = server
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .add_header("X-UWA-Provider", "nonexistent")
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;
    assert_eq!(r.status_code(), 400);
}
