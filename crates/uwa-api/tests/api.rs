use axum::http::{Method, StatusCode};
use axum_test::TestServer;
use serde_json::{json, Value};
use std::sync::Arc;
use uwa_api::AppState;
use uwa_resilience::circuit::CircuitState;
use uwa_resilience::semaphore::ProviderSemaphores;
use uwa_session::{SessionCfg, SessionManager};
use uwa_testkit::{
    config::{config_with_key, config_with_key_lan, TEST_AUTH_HEADER},
    AppBuilder, MockProvider, MockTransport,
};

/// A running server plus the handles a test may need afterwards.
struct Harness {
    server: TestServer,
    provider: Arc<MockProvider>,
    state: AppState,
}

fn harness_with(answer: &str, sessions: Option<Arc<SessionManager>>) -> Harness {
    let provider = Arc::new(MockProvider::new("chatgpt").with_answer(answer));
    let mut builder = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(provider.clone());
    if let Some(s) = sessions {
        builder = builder.with_sessions(s);
    }
    let app = builder.build();
    Harness {
        server: app.server,
        provider,
        state: app.state,
    }
}

/// Keyed server with one answering `chatgpt` provider.
fn harness(answer: &str) -> Harness {
    harness_with(answer, None)
}

/// Same, but the provider fails every turn.
fn harness_failing() -> Harness {
    let provider = Arc::new(MockProvider::new("chatgpt").with_error("provider exploded"));
    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(provider.clone())
        .build();
    Harness {
        server: app.server,
        provider,
        state: app.state,
    }
}

fn server(answer: &str) -> TestServer {
    harness(answer).server
}

/// Server plus the provider, so a test can read what reached the browser.
fn server_with(answer: &str) -> (TestServer, Arc<MockProvider>) {
    let h = harness(answer);
    (h.server, h.provider)
}

// --- Tests ---

#[tokio::test]
async fn healthz_is_open() {
    let s = server("hi");
    s.get("/healthz").await.assert_status_ok();
}

#[tokio::test]
async fn requires_api_key_off_loopback() {
    // Loopback binds accept any key (see `loopback_accepts_any_key` in
    // responses.rs); off-loopback the configured key is enforced.
    let provider = std::sync::Arc::new(MockProvider::new("chatgpt").with_answer("hi"));
    let app = AppBuilder::new()
        .with_config(config_with_key_lan())
        .with_transport(std::sync::Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(provider.clone())
        .build();
    app.server
        .get("/v1/models")
        .await
        .assert_status_unauthorized();
}

#[tokio::test]
async fn loopback_accepts_any_key_when_configured() {
    // Regression for the #1 wild failure: local clients with placeholder
    // keys. `server()`'s config HAS an api_key — and it must not matter
    // on 127.0.0.1.
    let s = server("hi");
    s.get("/v1/models")
        .add_header("Authorization", "Bearer dummy-key")
        .await
        .assert_status_ok();
}

#[tokio::test]
async fn models_lists_aliases() {
    let s = server("hi");
    let r = s
        .get("/v1/models")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(
        v.get("data")
            .and_then(|d| d.get(0))
            .and_then(|d| d.get("id"))
            .and_then(|i| i.as_str()),
        Some("gpt-4o")
    );
}

#[tokio::test]
async fn chat_unknown_model_400() {
    let s = server("hi");
    let r = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({"model":"nope","messages":[{"role":"user","content":"x"}]}))
        .await;
    r.assert_status_not_found();
    let v: serde_json::Value = r.json();
    assert_eq!(
        v.get("error").and_then(|e| e.get("code")).and_then(|c| c.as_str()),
        Some("model_not_found")
    );
}

#[tokio::test]
async fn chat_plain_text_stop() {
    let s = server("Hello there.");
    let r = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}))
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(
        v.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str()),
        Some("Hello there.")
    );
    assert_eq!(
        v.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason"))
            .and_then(|f| f.as_str()),
        Some("stop")
    );
    assert!(
        v.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("tool_calls"))
            .is_none()
    );
}

#[tokio::test]
async fn chat_emits_tool_calls() {
    let answer = "Let me check.\n<tool_call>{\"name\":\"get_weather\",\"arguments\":{\"city\":\"NYC\"}}</tool_call>";
    let s = server(answer);
    let r = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model":"gpt-4o",
            "messages":[{"role":"user","content":"weather?"}],
            "tools":[{"type":"function","function":{
                "name":"get_weather",
                "description":"weather",
                "parameters":{"type":"object","properties":{"city":{"type":"string"}}}
            }}]
        }))
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(
        v.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason"))
            .and_then(|f| f.as_str()),
        Some("tool_calls")
    );
    let tc = v
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("tool_calls"))
        .and_then(|t| t.get(0))
        .unwrap_or_else(|| unreachable!("missing tool_calls[0]"));
    assert_eq!(
        tc.get("type").and_then(|t| t.as_str()).unwrap_or(""),
        "function"
    );
    assert_eq!(
        tc.get("function")
            .and_then(|f| f.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or(""),
        "get_weather"
    );
    let args_str = tc
        .get("function")
        .and_then(|f| f.get("arguments"))
        .and_then(|a| a.as_str())
        .unwrap_or_default();
    let args: serde_json::Value =
        serde_json::from_str(args_str).unwrap_or_else(|e| unreachable!("bad args: {e}"));
    assert_eq!(args, json!({"city":"NYC"}));
    assert_eq!(
        v.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str()),
        Some("Let me check.")
    );
}

#[tokio::test]
async fn streaming_ends_with_done_and_finish_reason() {
    // `stream: true` answers with chat.completion.chunk events instead of a
    // single JSON body, and closes with `data: [DONE]`.
    let answer =
        "hi <tool_call>{\"name\":\"get_weather\",\"arguments\":{\"city\":\"X\"}}</tool_call>";
    let s = server(answer);
    let r = s.post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model":"gpt-4o",
            "stream": true,
            "messages":[{"role":"user","content":"weather"}],
            "tools":[{"type":"function","function":{"name":"get_weather","parameters":{"type":"object"}}}]
        }))
        .await;
    r.assert_status_ok();
    let ct = r
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(ct.contains("text/event-stream"), "content-type: {ct}");

    let body = r.text();
    let frames: Vec<&str> = body.split("data: ").skip(1).collect();

    assert!(frames.len() > 2, "too few chunks: {body}");
    assert_eq!(
        frames.last().copied().unwrap_or_default().trim(),
        "[DONE]"
    );

    // The frame before [DONE] carries the finish reason.
    let last: serde_json::Value = frames
        .get(frames.len() - 2)
        .map(|f| f.trim())
        .and_then(|f| serde_json::from_str(f).ok())
        .unwrap_or_else(|| unreachable!("missing finish frame"));
    assert_eq!(
        last.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason"))
            .and_then(|f| f.as_str()),
        Some("tool_calls")
    );

    // Tool name arrives first, then the arguments are streamed in chunks.
    let mut args = String::new();
    let end = frames.len().saturating_sub(2);
    for frame in frames.get(..end).unwrap_or(&[]) {
        let ev: serde_json::Value =
            serde_json::from_str(frame.trim()).unwrap_or_else(|e| unreachable!("bad chunk frame: {e}"));
        if let Some(calls) = ev
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("delta"))
            .and_then(|d| d.get("tool_calls"))
            .and_then(|t| t.as_array())
        {
            let call = calls.get(0).unwrap_or_else(|| unreachable!("empty tool_calls array"));
            args.push_str(
                call.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or(""),
            );
            args.push_str(
                call.get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|a| a.as_str())
                    .unwrap_or(""),
            );
        }
    }
    assert_eq!(&args[..11], "get_weather");
    let parsed: serde_json::Value =
        serde_json::from_str(&args[11..]).unwrap_or_else(|e| unreachable!("bad arguments json: {e}"));
    assert_eq!(parsed, json!({"city": "X"}));
}

// --- /v1/messages (Anthropic adapter) ---

#[tokio::test]
async fn messages_returns_anthropic_shape() {
    let (s, provider) = server_with("Hello there.");
    let r = s
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 128,
            "system": [{"type": "text", "text": "be terse"}],
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(
        v.get("type").and_then(|v| v.as_str()).unwrap_or(""),
        "message"
    );
    assert_eq!(
        v.get("role").and_then(|v| v.as_str()).unwrap_or(""),
        "assistant"
    );
    assert_eq!(
        v.get("model").and_then(|v| v.as_str()).unwrap_or(""),
        "gpt-4o"
    );
    assert!(
        v.get("id").and_then(|v| v.as_str()).unwrap_or("").starts_with("msg_")
    );
    assert_eq!(
        v.get("content")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("type"))
            .and_then(|t| t.as_str()),
        Some("text")
    );
    assert_eq!(
        v.get("content")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str()),
        Some("Hello there.")
    );
    assert_eq!(
        v.get("stop_reason").and_then(|v| v.as_str()).unwrap_or(""),
        "end_turn"
    );
    assert!(
        v.get("usage")
            .and_then(|u| u.get("input_tokens"))
            .and_then(|t| t.as_u64())
            .unwrap_or(0)
            >= 1
    );
    assert!(
        v.get("usage")
            .and_then(|u| u.get("output_tokens"))
            .and_then(|t| t.as_u64())
            .unwrap_or(0)
            >= 1
    );

    // The system block must actually reach the browser (it used to be lost).
    let body = provider.sent_last();
    assert!(body.contains("be terse"), "system missing: {body}");
    assert!(body.contains("hi"), "user text missing: {body}");
}

#[tokio::test]
async fn messages_tool_use_block_and_stop_reason() {
    let answer = "Let me check.\n{\"name\":\"get_weather\",\"arguments\":{\"city\":\"NYC\"}}";
    let (s, provider) = server_with(answer);
    let r = s
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": "weather?"}],
            "tools": [{
                "name": "get_weather",
                "description": "weather by city",
                "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}}
            }]
        }))
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(
        v.get("stop_reason").and_then(|v| v.as_str()).unwrap_or(""),
        "tool_use"
    );
    assert_eq!(
        v.get("content")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str()),
        Some("Let me check.")
    );
    let tool = v
        .get("content")
        .and_then(|c| c.get(1))
        .unwrap_or_else(|| unreachable!("missing content[1]"));
    assert_eq!(
        tool.get("type").and_then(|t| t.as_str()).unwrap_or(""),
        "tool_use"
    );
    assert_eq!(
        tool.get("name").and_then(|n| n.as_str()).unwrap_or(""),
        "get_weather"
    );
    assert_eq!(
        tool.get("input")
            .and_then(|i| i.get("city"))
            .and_then(|c| c.as_str())
            .unwrap_or(""),
        "NYC"
    );
    // The Anthropic tool declaration became an OpenAI tool in the prompt.
    assert!(provider.sent_last().contains("get_weather"));
}

#[tokio::test]
async fn messages_tool_result_history_reaches_the_browser() {
    let (s, provider) = server_with("22C");
    let r = s
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 100,
            "messages": [
                {"role": "user", "content": "weather?"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "checking"},
                    {"type": "tool_use", "id": "toolu_1", "name": "get_weather",
                     "input": {"city": "NYC"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "22C"}
                ]}
            ]
        }))
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(
        v.get("stop_reason").and_then(|v| v.as_str()).unwrap_or(""),
        "end_turn"
    );
    assert_eq!(
        v.get("content")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str()),
        Some("22C")
    );

    let body = provider.sent_last();
    assert!(body.contains("toolu_1"), "tool result id missing: {body}");
    assert!(body.contains("22C"), "tool result content missing: {body}");
}

#[tokio::test]
async fn messages_stream_emits_real_deltas() {
    // Real streaming: deltas arrive as the pipeline produces them (here:
    // one buffered chunk from the mock provider — no 24-char re-slicing).
    let answer = "Hello there. This answer is long enough to need several chunks.";
    let (s, _provider) = server_with(answer);
    let r = s
        .post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 100,
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;
    r.assert_status_ok();
    let ct = r
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(ct.contains("text/event-stream"), "content-type: {ct}");

    let body = r.text();
    for event in [
        "message_start",
        "content_block_start",
        "content_block_delta",
        "content_block_stop",
        "message_delta",
        "message_stop",
    ] {
        assert!(body.contains(&format!("event: {event}")), "missing {event}");
    }

    let mut text = String::new();
    let mut delta_count = 0;
    for line in body.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            let ev: Value =
                serde_json::from_str(data).unwrap_or_else(|e| unreachable!("bad event json: {e}"));
            if ev.get("type").and_then(|t| t.as_str()) == Some("content_block_delta")
                && ev.get("delta").and_then(|d| d.get("type")).and_then(|t| t.as_str())
                    == Some("text_delta")
            {
                delta_count += 1;
                text.push_str(
                    ev.get("delta")
                        .and_then(|d| d.get("text"))
                        .and_then(|t| t.as_str())
                        .unwrap_or(""),
                );
            }
        }
    }
    assert!(delta_count >= 1, "no text deltas in: {body}");
    assert_eq!(text, answer);
}

#[tokio::test]
async fn messages_tool_choice_none_drops_client_tools() {
    // Bridge semantics: tools come from the client only. `tool_choice: none`
    // means the client-declared tool must not reach the browser at all.
    let (s, provider) = server_with("ok");

    // Control: with tools, the schema reaches the browser prompt.
    s.post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 50,
            "tools": [{
                "name": "client_search",
                "description": "searches the web",
                "input_schema": {"type": "object", "properties": {}}
            }],
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await
        .assert_status_ok();
    assert!(
        provider.sent_last().contains("client_search"),
        "client tool should be injected by default: {}",
        provider.sent_last()
    );

    // `tool_choice: none` — the tool must not reach the browser.
    s.post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 50,
            "tool_choice": {"type": "none"},
            "tools": [{
                "name": "client_search",
                "description": "searches the web",
                "input_schema": {"type": "object", "properties": {}}
            }],
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await
        .assert_status_ok();
    assert!(
        !provider.sent_last().contains("client_search"),
        "tool_choice=none must not inject tools: {}",
        provider.sent_last()
    );
}

// --- breaker, sessions and runtime wiring ---

async fn chat(s: &TestServer, payload: Value) -> axum_test::TestResponse {
    s.post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&payload)
        .await
}

#[tokio::test]
async fn circuit_opens_after_repeated_provider_failures() {
    let h = harness_failing();
    let payload = json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]});

    for round in 0..5 {
        let r = chat(&h.server, payload.clone()).await;
        r.assert_status(StatusCode::SERVICE_UNAVAILABLE);
        let v: Value = r.json();
        assert!(
            v.get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .is_some_and(|m| m.contains("provider exploded")),
            "round {round}: {v}"
        );
    }
    assert_eq!(h.state.breaker("chatgpt").state(), CircuitState::Open);

    // The breaker now rejects before the provider is even called.
    let r = chat(&h.server, payload).await;
    r.assert_status(StatusCode::SERVICE_UNAVAILABLE);
    let v: Value = r.json();
    assert!(
        v.get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .is_some_and(|m| m.contains("circuit `chatgpt` open")),
        "{v}"
    );
}

#[tokio::test]
async fn sessions_pin_a_conversation_to_one_entry() {
    let sm = Arc::new(SessionManager::new(SessionCfg::default()));
    let h = harness_with("hi", Some(sm.clone()));

    let same = json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "same"}]});
    for _ in 0..2 {
        chat(&h.server, same.clone()).await.assert_status_ok();
    }
    assert_eq!(sm.len(), 1, "identical history must reuse one session");

    let other = json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "other"}]});
    chat(&h.server, other).await.assert_status_ok();
    assert_eq!(sm.len(), 2, "a new conversation gets its own entry");
}

#[tokio::test]
async fn runtime_services_defaults_and_overrides() {
    let h = harness("hi");
    let state = h.state;

    assert_eq!(state.runtime.semaphores.default_limit(), 4);
    assert!(state.runtime.sessions.is_none());
    assert!(state.runtime.history.is_none());

    // One breaker per provider, shared between clones.
    assert!(Arc::ptr_eq(&state.breaker("a"), &state.breaker("a")));
    assert!(!Arc::ptr_eq(&state.breaker("a"), &state.breaker("b")));

    // Builders replace the runtime instead of mutating it.
    let tuned = state
        .clone()
        .with_semaphores(Arc::new(ProviderSemaphores::new(1)));
    assert_eq!(tuned.runtime.semaphores.default_limit(), 1);
    assert_eq!(
        state.runtime.semaphores.default_limit(),
        4,
        "the original state must be untouched"
    );
    assert!(Arc::ptr_eq(&state.breaker("c"), &tuned.breaker("c")));
}

// ---------------------------------------------------------------------------
// Auth + browser-origin hardening (bug-hunt session)
// ---------------------------------------------------------------------------

/// Off-loopback server (key enforced) with one answering provider.
fn lan_server() -> TestServer {
    let provider = Arc::new(MockProvider::new("chatgpt").with_answer("hi"));
    AppBuilder::new()
        .with_config(config_with_key_lan())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(provider)
        .build()
        .server
}

#[tokio::test]
async fn lan_accepts_x_api_key_header() {
    // The Anthropic SDK / Claude Code send `x-api-key`, never `Authorization`.
    // README tells users to set ANTHROPIC_API_KEY; without this they got 401
    // on any non-loopback bind (Docker included).
    lan_server()
        .get("/v1/models")
        .add_header("x-api-key", "k")
        .await
        .assert_status_ok();
}

#[tokio::test]
async fn lan_rejects_wrong_or_missing_keys() {
    let s = lan_server();
    s.get("/v1/models")
        .add_header("x-api-key", "nope")
        .await
        .assert_status_unauthorized();
    s.get("/v1/models")
        .add_header("Authorization", "Bearer nope")
        .await
        .assert_status_unauthorized();
    s.get("/v1/models")
        .add_header("Authorization", "Bearer")
        .await
        .assert_status_unauthorized();
    s.get("/v1/models").await.assert_status_unauthorized();
    // Admin routes sit behind the same single auth layer.
    s.get("/admin/stats").await.assert_status_unauthorized();
}

#[tokio::test]
async fn lan_bearer_scheme_is_case_insensitive() {
    let s = lan_server();
    for v in ["bearer k", "BEARER k", "Bearer   k"] {
        s.get("/v1/models")
            .add_header("Authorization", v)
            .await
            .assert_status_ok();
    }
}

#[tokio::test]
async fn lan_accepts_query_key_once_decoded() {
    let s = lan_server();
    s.get("/v1/models?api_key=k").await.assert_status_ok();
    s.get("/v1/models?x=1&api_key=%6B").await.assert_status_ok();
    s.get("/v1/models?api_key=%256B")
        .await
        .assert_status_unauthorized(); // decoded once: "%6B" != "k"
    s.get("/v1/models?api_key=%zz")
        .await
        .assert_status_unauthorized();
}

#[tokio::test]
async fn loopback_blocks_cross_origin_browser_requests() {
    // A random website can `fetch("http://127.0.0.1:8080/...")`. On a
    // loopback bind there is no key check, so the Origin guard is the only
    // thing between that page and the user's logged-in LLM sessions.
    let h = harness("hi");
    let payload = json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "x"}]});

    let r = h
        .server
        .post("/v1/chat/completions")
        .add_header("Origin", "https://evil.example")
        .json(&payload)
        .await;
    r.assert_status_forbidden();
    assert_eq!(
        r.json::<Value>()
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(|c| c.as_str()),
        Some("forbidden_origin")
    );
    assert!(
        h.provider.sent().is_empty(),
        "a blocked request must never reach the browser"
    );

    for path in ["/admin/history", "/admin/stats", "/admin/sessions"] {
        h.server
            .get(path)
            .add_header("Origin", "https://evil.example")
            .await
            .assert_status_forbidden();
    }

    // The opaque origin (sandboxed iframe, file://) is just as foreign.
    h.server
        .get("/v1/models")
        .add_header("Origin", "null")
        .await
        .assert_status_forbidden();
}

#[tokio::test]
async fn loopback_blocks_dns_rebinding_hosts() {
    let s = server("hi");
    let r = s
        .get("/v1/models")
        .add_header("Host", "rebind.evil.example:8080")
        .await;
    r.assert_status_forbidden();
    assert_eq!(
        r.json::<Value>()
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(|c| c.as_str()),
        Some("forbidden_host")
    );
}

#[tokio::test]
async fn loopback_allows_local_dashboard_and_plain_clients() {
    let s = server("hi");
    // curl / SDKs: no Origin header at all.
    s.get("/v1/models").await.assert_status_ok();
    // The bundled dashboard: same-origin, loopback Host + Origin.
    for origin in ["http://127.0.0.1:8080", "http://localhost:3000"] {
        s.get("/v1/models")
            .add_header("Host", "127.0.0.1:8080")
            .add_header("Origin", origin)
            .await
            .assert_status_ok();
    }
}

#[tokio::test]
async fn cors_preflight_is_not_granted_to_foreign_origins_on_loopback() {
    let s = server("hi");
    let evil = s
        .method(Method::OPTIONS, "/v1/chat/completions")
        .add_header("Origin", "https://evil.example")
        .add_header("Access-Control-Request-Method", "POST")
        .add_header("Access-Control-Request-Headers", "content-type")
        .await;
    assert!(
        evil.headers().get("access-control-allow-origin").is_none(),
        "foreign origin must not be granted CORS on a loopback bind"
    );

    let local = s
        .method(Method::OPTIONS, "/v1/chat/completions")
        .add_header("Origin", "http://localhost:3000")
        .add_header("Access-Control-Request-Method", "POST")
        .add_header("Access-Control-Request-Headers", "content-type")
        .await;
    assert_eq!(
        local
            .headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok()),
        Some("http://localhost:3000")
    );
}

#[tokio::test]
async fn request_id_header_is_sanitized() {
    let s = server("hi");
    let r = s
        .get("/v1/models")
        .add_header("x-request-id", "ok-id_1.2")
        .await;
    r.assert_status_ok();
    // Oversized / odd ids are replaced, never echoed into logs unchecked.
    let long = "a".repeat(5_000);
    s.get("/v1/models")
        .add_header("x-request-id", long.as_str())
        .await
        .assert_status_ok();
    s.get("/v1/models")
        .add_header("x-request-id", "has space")
        .await
        .assert_status_ok();
}

// ---------------------------------------------------------------------------
// Conversation identity across turns
// ---------------------------------------------------------------------------

#[tokio::test]
async fn follow_up_turn_without_system_prompt_stays_on_the_same_session() {
    // Regression: the conversation id hashed the first TWO messages. Turn 1
    // of a system-less client is `[user]`, turn 2 is `[user, assistant, user]`,
    // so the id changed on the second turn: the conversation silently moved
    // to a fresh session/tab and lost its browser-side context.
    let sm = Arc::new(SessionManager::new(SessionCfg::default()));
    let h = harness_with("pong", Some(sm.clone()));

    chat(
        &h.server,
        json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "ping"}]}),
    )
    .await
    .assert_status_ok();
    chat(
        &h.server,
        json!({"model": "gpt-4o", "messages": [
            {"role": "user", "content": "ping"},
            {"role": "assistant", "content": "pong"},
            {"role": "user", "content": "again"},
        ]}),
    )
    .await
    .assert_status_ok();

    assert_eq!(sm.len(), 1, "a follow-up turn must reuse the session");
}
