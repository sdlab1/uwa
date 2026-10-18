//! Rate limiter at the HTTP edge.

use serde_json::json;
use std::sync::Arc;
use uwa_resilience::ProviderRateLimiters;
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

fn jstr<'a>(v: &'a serde_json::Value, key: &str) -> &'a str {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or_else(|| unreachable!("string key {key} in {v}"))
}

fn jarr<'a>(v: &'a serde_json::Value, key: &str) -> &'a [serde_json::Value] {
    v.get(key)
        .and_then(|x| x.as_array())
        .unwrap_or_else(|| unreachable!("array key {key} in {v}"))
}

fn keyed_config_rpm(rpm: f64, burst: u32) -> Arc<uwa_config::Config> {
    Arc::new(ok(uwa_config::Config::load_from_str(&format!(
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
        capabilities = {{ streams = true, tool_calls = false, vision = false }}

        [providers.chatgpt.selectors]
        input = "#prompt"
        send_button = "button.send"
        assistant_message = "[data-role=assistant]"

        [providers.chatgpt.rate_limit]
        enabled = true
        requests_per_minute = {rpm}
        burst = {burst}
        "##
    ))))
}

async fn app_with_limiter(rpm: f64, burst: u32) -> axum_test::TestServer {
    let cfg = keyed_config_rpm(rpm, burst);
    let rl = ProviderRateLimiters::new().with_limit(
        "chatgpt",
        &cfg.providers
            .get("chatgpt")
            .unwrap_or_else(|| unreachable!("chatgpt provider"))
            .rate_limit,
    );
    AppBuilder::new()
        .with_config(cfg)
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("ok")))
        .with_rate_limiters(Arc::new(rl))
        .build()
        .server
}

async fn chat(s: &axum_test::TestServer) -> axum_test::TestResponse {
    s.post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role":"user","content":"hi"}]
        }))
        .await
}

#[tokio::test]
async fn under_budget_sets_rate_headers() {
    let s = app_with_limiter(60.0, 5).await;
    let r = chat(&s).await;
    r.assert_status_ok();
    assert_eq!(hstr(&r, "x-ratelimit-limit"), "60");
    // One consumed of five.
    assert_eq!(hstr(&r, "x-ratelimit-remaining"), "4");
    // Reset header is present (value 0 when bucket is above 1).
    assert!(r.headers().get("x-ratelimit-reset").is_some());
}

#[tokio::test]
async fn exceeding_burst_returns_429_with_retry_after() {
    let s = app_with_limiter(60.0, 2).await;
    chat(&s).await.assert_status_ok();
    chat(&s).await.assert_status_ok();
    let r = chat(&s).await;
    r.assert_status_too_many_requests();

    let ra = hstr(&r, "retry-after");
    let secs: u64 = ok(ra.parse());
    assert!((1..=2).contains(&secs), "{secs}");

    assert_eq!(hstr(&r, "x-ratelimit-limit"), "60");
    assert_eq!(hstr(&r, "x-ratelimit-remaining"), "0");

    let v: serde_json::Value = r.json();
    assert_eq!(jstr(field(&v, "error"), "code"), "rate_limited");
    assert!(
        jstr(field(&v, "error"), "message").contains("retry after"),
        "{v}"
    );
}

#[tokio::test]
async fn unlimited_provider_never_429s() {
    // Config without a rate_limit block: default is unlimited.
    let cfg = Arc::new(ok(uwa_config::Config::load_from_str(
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
        capabilities = { streams = true, tool_calls = false, vision = false }

        [providers.chatgpt.selectors]
        input = "#prompt"
        send_button = "button.send"
        assistant_message = "[data-role=assistant]"
        "##,
    )));
    let s = AppBuilder::new()
        .with_config(cfg)
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("ok")))
        .build()
        .server;
    for _ in 0..5 {
        chat(&s).await.assert_status_ok();
    }
    // No rate headers on an unlimited provider.
    let r = chat(&s).await;
    r.assert_status_ok();
    assert!(r.headers().get("x-ratelimit-limit").is_none());
}

#[tokio::test]
async fn admin_endpoint_reports_enabled_providers() {
    let s = app_with_limiter(20.0, 4).await;
    chat(&s).await;
    let r = s
        .get("/admin/rate-limits")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    let ps = jarr(&v, "providers");
    assert_eq!(ps.len(), 1);
    let p = ps
        .first()
        .unwrap_or_else(|| unreachable!("one provider entry"));
    assert_eq!(jstr(p, "provider"), "chatgpt");
    assert_eq!(field(p, "requests_per_minute").as_f64(), Some(20.0));
    assert_eq!(field(p, "capacity").as_f64(), Some(4.0));
    assert!(field(p, "remaining").as_u64().unwrap_or(0) <= 4);
}

#[tokio::test]
async fn admin_endpoint_skips_unlimited() {
    let cfg = Arc::new(ok(uwa_config::Config::load_from_str(
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
        capabilities = { streams = true, tool_calls = false, vision = false }

        [providers.chatgpt.selectors]
        input = "#prompt"
        send_button = "button.send"
        assistant_message = "[data-role=assistant]"
        "##,
    )));
    let s = AppBuilder::new()
        .with_config(cfg)
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("ok")))
        .build()
        .server;
    // Touch the registry so it caches an unlimited limiter.
    chat(&s).await;
    let r = s
        .get("/admin/rate-limits")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(jarr(&v, "providers").len(), 0);
}

#[tokio::test]
async fn streaming_request_gets_rate_headers() {
    let s = app_with_limiter(60.0, 3).await;
    let r = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "stream": true,
            "messages": [{"role":"user","content":"hi"}]
        }))
        .await;
    r.assert_status_ok();
    assert_eq!(hstr(&r, "x-ratelimit-limit"), "60");
    assert_eq!(hstr(&r, "x-ratelimit-remaining"), "2");
}

#[tokio::test]
async fn messages_endpoint_honours_the_limiter() {
    let s = app_with_limiter(60.0, 1).await;
    let r = s
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 16,
            "messages": [{"role":"user","content":"hi"}]
        }))
        .await;
    r.assert_status_ok();
    assert_eq!(hstr(&r, "x-ratelimit-remaining"), "0");

    let r = s
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 16,
            "messages": [{"role":"user","content":"hi"}]
        }))
        .await;
    r.assert_status_too_many_requests();
}
