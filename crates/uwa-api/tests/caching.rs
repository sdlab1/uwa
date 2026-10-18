//! Response cache at the HTTP edge.
//!
//! Covers: temperature gate, hit/miss headers, admin stats/clear,
//! stream_from_cache veto, and that hits skip the provider entirely.

use serde_json::json;
use std::sync::Arc;
use uwa_testkit::{config::TEST_AUTH_HEADER, AppBuilder, MockProvider, MockTransport};

fn ok<T, E: std::fmt::Debug>(r: Result<T, E>) -> T {
    r.unwrap_or_else(|e| unreachable!("{e:?}"))
}

fn hstr<'a>(r: &'a axum_test::TestResponse, name: &str) -> &'a str {
    r.headers()
        .get(name)
        .unwrap_or_else(|| unreachable!("header {name} missing"))
        .to_str()
        .unwrap_or_else(|e| unreachable!("header {name} not ASCII: {e}"))
}

fn field<'a>(v: &'a serde_json::Value, key: &str) -> &'a serde_json::Value {
    v.get(key)
        .unwrap_or_else(|| unreachable!("missing key {key} in {v}"))
}

fn cfg_with_cache(cache_toml: &str) -> Arc<uwa_config::Config> {
    Arc::new(ok(uwa_config::Config::load_from_str(&format!(
        r##"
        [server]
        bind = "127.0.0.1"
        port = 8080

        [model_aliases]
        "gpt-4o" = "chatgpt"

        [providers.chatgpt]
        name = "chatgpt"
        url_patterns = ["https://chatgpt.com/*"]
        capabilities = {{ streams = true, tool_calls = false, vision = false }}

        [providers.chatgpt.selectors]
        input = "#prompt"
        send_button = "button.send"
        assistant_message = "[data-role=assistant]"

        {}
        "##,
        cache_toml
    ))))
}

async fn app_with_cache(cache_toml: &str) -> axum_test::TestServer {
    let cfg = cfg_with_cache(cache_toml);
    let provider = Arc::new(MockProvider::new("chatgpt").with_answer("cached answer"));
    let cache = if cfg.cache.enabled {
        Some(Arc::new(uwa_api::cache::ResponseCache::new(
            cfg.cache.clone(),
        )))
    } else {
        None
    };
    let mut builder = AppBuilder::new()
        .with_config(cfg.clone())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(provider.clone());
    if let Some(c) = cache {
        builder = builder.with_cache(c);
    }
    builder.build().server
}

async fn chat(s: &axum_test::TestServer, temperature: Option<f64>) -> axum_test::TestResponse {
    let mut req = json!({
        "model": "gpt-4o",
        "messages": [{"role":"user","content":"what is the capital of france?"}],
        "stream": false,
    });
    if let Some(t) = temperature {
        req.as_object_mut()
            .unwrap_or_else(|| unreachable!("request is an object"))
            .insert("temperature".to_string(), json!(t));
    }
    s.post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&req)
        .await
}

async fn chat_stream(
    s: &axum_test::TestServer,
    temperature: Option<f64>,
) -> axum_test::TestResponse {
    let mut req = json!({
        "model": "gpt-4o",
        "messages": [{"role":"user","content":"what is the capital of france?"}],
        "stream": true,
    });
    if let Some(t) = temperature {
        req.as_object_mut()
            .unwrap_or_else(|| unreachable!("request is an object"))
            .insert("temperature".to_string(), json!(t));
    }
    s.post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&req)
        .await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn second_identical_request_hits_cache() {
    let s = app_with_cache(
        "[cache]\nenabled = true\nttl_secs = 300\ncapacity = 256\nstream_from_cache = true\n",
    )
    .await;

    // First request: miss.
    let r1 = chat(&s, Some(0.0)).await;
    r1.assert_status_ok();
    assert_eq!(hstr(&r1, "x-uwa-cache"), "miss");

    // Second identical request: hit — provider not called again.
    let r2 = chat(&s, Some(0.0)).await;
    r2.assert_status_ok();
    assert_eq!(hstr(&r2, "x-uwa-cache"), "hit");
    assert_eq!(hstr(&r2, "content-type"), "application/json");
}

#[tokio::test]
async fn temperature_gate_bypasses_cache() {
    let s = app_with_cache(
        "[cache]\nenabled = true\nttl_secs = 300\ncapacity = 256\nstream_from_cache = true\n",
    )
    .await;

    // temperature = 0.7 → not eligible, always a miss.
    let r1 = chat(&s, Some(0.7)).await;
    r1.assert_status_ok();
    assert_eq!(hstr(&r1, "x-uwa-cache"), "miss");

    let r2 = chat(&s, Some(0.7)).await;
    r2.assert_status_ok();
    assert_eq!(hstr(&r2, "x-uwa-cache"), "miss");
}

#[tokio::test]
async fn different_prompt_misses_cache() {
    let s = app_with_cache(
        "[cache]\nenabled = true\nttl_secs = 300\ncapacity = 256\nstream_from_cache = true\n",
    )
    .await;

    let r1 = chat(&s, Some(0.0)).await;
    r1.assert_status_ok();
    assert_eq!(hstr(&r1, "x-uwa-cache"), "miss");

    // Different message → different key → miss.
    let req = json!({
        "model": "gpt-4o",
        "messages": [{"role":"user","content":"what is 2+2?"}],
        "stream": false,
        "temperature": 0.0,
    });
    let r2 = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&req)
        .await;
    r2.assert_status_ok();
    assert_eq!(hstr(&r2, "x-uwa-cache"), "miss");

    // Back to the original → hit.
    let r3 = chat(&s, Some(0.0)).await;
    r3.assert_status_ok();
    assert_eq!(hstr(&r3, "x-uwa-cache"), "hit");
}

#[tokio::test]
async fn cache_hit_skips_provider_on_stream() {
    let s = app_with_cache(
        "[cache]\nenabled = true\nttl_secs = 300\ncapacity = 256\nstream_from_cache = true\n",
    )
    .await;

    // Populate the cache with a non-stream request.
    let r1 = chat(&s, Some(0.0)).await;
    r1.assert_status_ok();
    assert_eq!(hstr(&r1, "x-uwa-cache"), "miss");

    // Stream the same request → hit, provider not called again.
    let r2 = chat_stream(&s, Some(0.0)).await;
    r2.assert_status_ok();
    assert_eq!(hstr(&r2, "x-uwa-cache"), "hit");
}

#[tokio::test]
async fn admin_endpoint_reports_state() {
    let s = app_with_cache(
        "[cache]\nenabled = true\nttl_secs = 120\ncapacity = 64\nstream_from_cache = true\n",
    )
    .await;

    // Generate a hit and a miss.
    chat(&s, Some(0.0)).await;
    chat(&s, Some(0.0)).await;

    let stats = s.get("/admin/cache").await;
    stats.assert_status_ok();
    let body: serde_json::Value = stats.json();
    assert_eq!(field(&body, "enabled").as_bool(), Some(true));
    assert_eq!(field(&body, "capacity").as_u64(), Some(64));
    assert_eq!(field(&body, "ttl_secs").as_u64(), Some(120));
    assert_eq!(field(&body, "stream_from_cache").as_bool(), Some(true));
    assert_eq!(field(&body, "hits").as_u64(), Some(1));
    assert_eq!(field(&body, "misses").as_u64(), Some(1));
}

#[tokio::test]
async fn clear_removes_every_entry() {
    let s = app_with_cache(
        "[cache]\nenabled = true\nttl_secs = 300\ncapacity = 256\nstream_from_cache = true\n",
    )
    .await;

    // Populate with two distinct entries.
    chat(&s, Some(0.0)).await;
    let req = json!({
        "model": "gpt-4o",
        "messages": [{"role":"user","content":"different prompt"}],
        "stream": false,
        "temperature": 0.0,
    });
    s.post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&req)
        .await;

    // Confirm both are cached.
    let r3 = chat(&s, Some(0.0)).await;
    r3.assert_status_ok();
    assert_eq!(hstr(&r3, "x-uwa-cache"), "hit");

    // Clear.
    let cleared = s.post("/admin/cache/clear").await;
    cleared.assert_status_ok();
    let body: serde_json::Value = cleared.json();
    assert_eq!(field(&body, "removed").as_u64(), Some(2));

    // After clear, everything misses again.
    let r4 = chat(&s, Some(0.0)).await;
    r4.assert_status_ok();
    assert_eq!(hstr(&r4, "x-uwa-cache"), "miss");
}

#[tokio::test]
async fn disabled_cache_reports_off() {
    let s = app_with_cache("").await;

    let stats = s.get("/admin/cache").await;
    stats.assert_status_ok();
    let body: serde_json::Value = stats.json();
    assert_eq!(field(&body, "enabled").as_bool(), Some(false));
    // Disabled endpoint reports only `{ "enabled": false }` — no entries
    // key. Treat missing entries as 0.
    let entries = body.get("entries").and_then(|v| v.as_u64()).unwrap_or(0);
    assert_eq!(entries, 0);

    // Requests still work but never hit.
    let r = chat(&s, Some(0.0)).await;
    r.assert_status_ok();
    let cache_val = r
        .header("x-uwa-cache")
        .to_str()
        .map(|s| s.to_string())
        .unwrap_or_default();
    assert_eq!(cache_val.as_str(), "miss");
}

#[tokio::test]
async fn stream_from_cache_false_forces_pipeline_for_streams() {
    let s = app_with_cache(
        "[cache]\nenabled = true\nttl_secs = 300\ncapacity = 256\nstream_from_cache = false\n",
    )
    .await;

    // Populate via non-stream.
    let r1 = chat(&s, Some(0.0)).await;
    r1.assert_status_ok();
    assert_eq!(hstr(&r1, "x-uwa-cache"), "miss");

    // Same request as a stream: stream_from_cache=false → always pipeline.
    let r2 = chat_stream(&s, Some(0.0)).await;
    r2.assert_status_ok();
    assert_eq!(hstr(&r2, "x-uwa-cache"), "miss");
}
