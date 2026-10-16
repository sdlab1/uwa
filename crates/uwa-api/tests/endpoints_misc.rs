//! Merged small endpoint integration tests.
//!
//! Each section used to be its own file (own test binary, own link of the
//! whole dependency graph). On a 2-core box those 6 extra binaries cost
//! more link time than they cost to run, so they share one binary here.
//! libtest runs the tests in parallel threads inside it, so nothing is
//! lost on the parallelism front.
//!
//! Sources (all tests verbatim, helpers renamed to avoid collisions):
//!   routing.rs, messages_count_tokens.rs, chat_pipeline.rs,
//!   profiles_stream.rs, multimodal.rs, ui.rs, selector_generate.rs

use axum_test::TestServer;
use serde_json::{json, Value};
use std::sync::Arc;
use uwa_core::TabId;
use uwa_resilience::semaphore::ProviderSemaphores;
use uwa_testkit::{
    config::{config_with_key, config_with_key_lan, TEST_AUTH_HEADER},
    AppBuilder, MockPage, MockProvider, MockTransport,
};

// === helpers ===

fn server() -> TestServer {
    AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("hi")))
        .build()
        .server
}

/// `messages_count_tokens.rs` had its own identically-named helper.
fn count_tokens_server() -> TestServer {
    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("unused")))
        .build();
    app.server
}

/// `chat_pipeline.rs`: end-to-end pipeline with a semaphore grant pool.
fn server_with(answer: &str) -> TestServer {
    AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer(answer)))
        .with_semaphores(Arc::new(ProviderSemaphores::new(4)))
        .build()
        .server
}

async fn count(s: &TestServer, body: Value) -> u32 {
    let r = s
        .post("/v1/messages/count_tokens")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&body)
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    v.get("input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32
}

// === routing ===

#[tokio::test]
async fn header_override_routes_to_provider() {
    let r = server()
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .add_header("X-UWA-Provider", "chatgpt")
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "x"}]
        }))
        .await;
    r.assert_status_ok();
}

#[tokio::test]
async fn url_domain_path_resolves_provider() {
    // config has chatgpt with pattern `https://chatgpt.com/*`
    let r = server()
        .post("/url/chatgpt.com/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "x"}]
        }))
        .await;
    if !r.status_code().is_success() {
        eprintln!(
            "Request failed with status: {}; body: {}",
            r.status_code(),
            r.text()
        );
    }
    r.assert_status_ok();
}

#[tokio::test]
async fn url_unknown_domain_400() {
    let r = server()
        .post("/url/nowhere.example/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "x"}]
        }))
        .await;
    r.assert_status_bad_request();
}

#[tokio::test]
async fn tab_path_pins_tab() {
    let r = server()
        .post("/tab/tab_abc/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "x"}]
        }))
        .await;
    // We didn't add a tab with id "tab_abc" to the mock transport, so page()
    // errors with TabNotFound (503). We only check routing reached the handler.
    assert!(r.status_code().is_client_error() || r.status_code().is_server_error());
}

#[tokio::test]
async fn provider_status_ok() {
    let r = server()
        .get("/v1/provider/status")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
}

#[tokio::test]
async fn pool_status_ok() {
    let r = server()
        .get("/api/pool/status")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
}

// === POST /v1/messages/count_tokens ===

#[tokio::test]
async fn counts_a_plain_prompt() {
    let n = count(
        &count_tokens_server(),
        json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hello"}],
        }),
    )
    .await;
    // Weighted estimator: "hello" (5 word chars) → 2 tokens, plus the
    // fixed 4-token per-message overhead.
    assert_eq!(n, 6);
}

#[tokio::test]
async fn counts_content_blocks_and_system() {
    let s = count_tokens_server();
    let bare = count(
        &s,
        json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "hello"}]}),
    )
    .await;
    let loaded = count(
        &s,
        json!({
            "model": "gpt-4o",
            "system": "You are helpful.",
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "hello"}]
            }],
        }),
    )
    .await;
    assert!(loaded > bare, "{loaded} should exceed {bare}");
}

#[tokio::test]
async fn counts_longer_prompts_higher() {
    let s = count_tokens_server();
    let short = count(
        &s,
        json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}),
    )
    .await;
    let long = count(
        &s,
        json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi".repeat(100)}]}),
    )
    .await;
    assert!(long > short, "{long} should exceed {short}");
}

#[tokio::test]
async fn missing_messages_default_to_empty() {
    let n = count(&count_tokens_server(), json!({"model": "gpt-4o"})).await;
    // The endpoint clamps to ≥ 1 so a zero-cost estimate never rounds
    // a non-empty exchange down to nothing.
    assert_eq!(n, 1);
}

#[tokio::test]
async fn needs_the_api_key_off_loopback() {
    // Loopback binds accept any key; off-loopback the key is enforced.
    // The builder lets a LAN-shaped config through, so this works from a
    // dual-stack loopback address too (127.0.0.2 is also loopback).
    let app = AppBuilder::new()
        .with_config(config_with_key_lan())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("unused")))
        .build();
    app.server
        .post("/v1/messages/count_tokens")
        .json(&json!({"model": "gpt-4o", "messages": []}))
        .await
        .assert_status_unauthorized();
}

// === chat pipeline ===

#[tokio::test]
async fn plain_chat_stop() {
    let r = server_with("hello")
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
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason"))
            .and_then(|f| f.as_str()),
        Some("stop")
    );
    assert_eq!(
        v.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str()),
        Some("hello")
    );
}

// === profiles_stream ===
// Streaming requests must also honour `X-UWA-Session`.
//
// The Svelte-стриминг lesson says: SSE bodies do not terminate through
// axum-test's reader; here we only assert the status and the session
// table, so a plain status check is enough.

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
    let names: Vec<String> = v
        .get("sessions")
        .and_then(|s| s.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    s.get("profile")
                        .and_then(|p| p.as_str())
                        .map(|p| p.to_string())
                        .or_else(|| Some("default".into()))
                })
                .collect()
        })
        .unwrap_or_default();
    assert!(names.contains(&"work".to_string()), "{names:?}");
}

// === multimodal ===
// Multimodal bridge: images in the client request decode into temp
// files; the pipeline attaches them and cleans up afterwards.

#[tokio::test]
async fn text_only_request_ignores_multimodal() {
    let s = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("ok")))
        .build()
        .server;

    let r = s
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
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str()),
        Some("ok")
    );
}

#[tokio::test]
async fn image_attachment_decoded_but_not_fails_pipeline() {
    // 1x1 red PNG.
    let png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
    let s = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(
            MockProvider::new("chatgpt").with_answer("a red pixel"),
        ))
        .build()
        .server;

    let r = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "what is this?"},
                    {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{png_b64}")}}
                ]
            }]
        }))
        .await;
    // Should succeed end-to-end: the mock provider ignores attachments.
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(
        v.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str()),
        Some("a red pixel")
    );
}

#[tokio::test]
async fn broken_image_does_not_fail_chat() {
    let s = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("fine")))
        .build()
        .server;

    let r = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "hi"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,@@not-base64@@"}}
                ]
            }]
        }))
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(
        v.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str()),
        Some("fine")
    );
}

// === ui ===
// UI serving + richer status endpoints.
//
// `todo/ui.md` added: `GET /` (index.html), `/static/*` assets, URLs in
// `/api/pool/status`, and selectors in `/v1/provider/status`.

#[tokio::test]
async fn index_served_when_built() {
    // The built UI lives in `static/` next to the crate. Skip when it
    // hasn't been built yet (verify.sh runs build.sh first).
    let index = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("static/index.html");
    if !index.exists() {
        return;
    }
    let s = server();
    let r = s.get("/").await;
    assert_eq!(r.status_code(), 200);
    let body = r.text();
    assert!(body.contains("uwa"), "index.html should mention uwa");
}

#[tokio::test]
async fn pool_status_has_urls() {
    let s = server();
    let r = s
        .get("/api/pool/status")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    assert_eq!(r.status_code(), 200);
    let v: serde_json::Value = r.json();
    assert!(
        v.get("total_tabs").map(|v| v.is_number()).unwrap_or(false),
        "total_tabs must stay"
    );
    let tabs = v
        .get("tabs")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| unreachable!("tabs array missing"));
    assert_eq!(tabs.len(), 1);
    // MockTab reports a URL (about:blank default); the shape is what matters.
    assert!(
        tabs.first().and_then(|t| t.get("id")).map(|v| v.is_string()).unwrap_or(false),
        "tab entry has id"
    );
    assert!(
        tabs.first().and_then(|t| t.get("url")).map(|v| v.is_string()).unwrap_or(false),
        "tab entry has url"
    );
}

#[tokio::test]
async fn provider_status_has_selectors_and_caps() {
    let s = server();
    let r = s
        .get("/v1/provider/status")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    assert_eq!(r.status_code(), 200);
    let v: serde_json::Value = r.json();
    let p = v.get("chatgpt").unwrap_or_else(|| unreachable!("chatgpt provider present"));
    assert!(p.is_object(), "chatgpt provider present");
    assert!(
        p.get("url_patterns").map(|v| v.is_array()).unwrap_or(false)
    );
    assert!(
        p.get("selectors").map(|v| v.is_object()).unwrap_or(false),
        "selectors block for the wizard"
    );
    assert!(
        p.get("selectors")
            .and_then(|s| s.get("input"))
            .map(|v| v.is_null() || v.is_string())
            .unwrap_or(false)
    );
    assert!(
        p.get("capabilities").map(|v| v.is_object()).unwrap_or(false),
        "capabilities block"
    );
}

#[tokio::test]
async fn admin_sessions_shape() {
    let s = server();
    let r = s
        .get("/admin/sessions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    // No session manager in the default testkit builder → 500 with a clear
    // message, or 200 with an empty list if one is wired. Either way it
    // must be JSON, not a panic.
    let body = r.text();
    assert!(
        body.contains("session") || r.status_code().is_success(),
        "unexpected body: {body}"
    );
}

#[tokio::test]
async fn log_stream_is_sse() {
    // SSE never terminates by design (live tail), so we must not await the
    // body — the axum-test harness would hang. Call the handler directly
    // and inspect the response headers, then drop the stream.
    let provider = Arc::new(MockProvider::new("chatgpt").with_answer("hi"));
    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(provider)
        .build();
    let resp = uwa_api::routes::admin::log_stream(axum::extract::State(app.state)).await;
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.starts_with("text/event-stream"),
        "log stream must be SSE, got {ct}"
    );
}

// === selector_generate ===
// `/admin/selector-generate` + `/admin/selector-apply` integration tests.

#[tokio::test]
async fn generate_returns_candidates() {
    let page = MockPage::new().expect(
        "candidates",
        json!({
            "inputs": [
                {"selector": "#p", "evidence": "textarea", "score": 0.9, "tag": "textarea"}
            ],
            "sendButtons": [
                {"selector": "button[data-testid=send]", "evidence": "label keyword", "score": 0.7, "tag": "button"}
            ],
            "assistantContainers": []
        }),
    );
    let tab = TabId::from_raw("t1");
    let t = MockTransport::new().with_page(tab, Arc::new(page));

    let s = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(t))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("x")))
        .build()
        .server;

    let r = s
        .post("/admin/selector-generate")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({ "provider": "chatgpt", "tab_id": "t1" }))
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(
        v.get("inputs").and_then(|i| i.get(0)).and_then(|i| i.get("selector")).and_then(|s| s.as_str()),
        Some("#p")
    );
    assert_eq!(
        v.get("send_buttons")
            .and_then(|s| s.get(0))
            .and_then(|s| s.get("selector"))
            .and_then(|s| s.as_str()),
        Some("button[data-testid=send]")
    );
}

#[tokio::test]
async fn generate_unknown_provider_404() {
    let s = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("x")))
        .build()
        .server;

    let r = s
        .post("/admin/selector-generate")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({ "provider": "nope" }))
        .await;
    r.assert_status_not_found();
}

#[tokio::test]
async fn apply_dry_run_returns_diff() {
    let s = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("x")))
        .build()
        .server;

    let r = s
        .post("/admin/selector-apply")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "provider": "chatgpt",
            "input": "#new-input",
            "persist": false
        }))
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(v.get("applied").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        v.get("diff")
            .and_then(|d| d.get("input"))
            .and_then(|i| i.get("new"))
            .and_then(|n| n.as_str()),
        Some("#new-input")
    );
    // `persisted_to` used to come back as JSON null on dry-run; the
    // selector-apply response was narrowed to {applied, diff, hint}
    // (see 73fa6a2), and dry-run never persists anything anyway —
    // `applied: false` above is the real assertion.
}

#[tokio::test]
async fn apply_same_selector_yields_empty_diff() {
    let cfg = config_with_key();
    let current = cfg
        .providers
        .get("chatgpt")
        .unwrap_or_else(|| unreachable!("chatgpt provider not found"))
        .selectors
        .input
        .clone();
    let s = AppBuilder::new()
        .with_config(cfg)
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("x")))
        .build()
        .server;

    let r = s
        .post("/admin/selector-apply")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "provider": "chatgpt",
            "input": current,
        }))
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(v.get("diff"), Some(&serde_json::json!({})));
}
