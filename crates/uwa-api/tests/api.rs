use axum::http::StatusCode;
use axum_test::TestServer;
use serde_json::{json, Value};
use std::sync::Arc;
use uwa_api::AppState;
use uwa_core::traits::ToolSpec;
use uwa_mcp::ToolRouter;
use uwa_resilience::circuit::CircuitState;
use uwa_resilience::semaphore::ProviderSemaphores;
use uwa_session::{SessionCfg, SessionManager};
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockProvider, MockToolProvider, MockTransport,
};

/// A running server plus the handles a test may need afterwards.
struct Harness {
    server: TestServer,
    provider: Arc<MockProvider>,
    state: AppState,
}

fn harness_with(
    answer: &str,
    tool_router: Option<Arc<ToolRouter>>,
    sessions: Option<Arc<SessionManager>>,
) -> Harness {
    let provider = Arc::new(MockProvider::new("chatgpt").with_answer(answer));
    let mut builder = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(provider.clone());
    if let Some(r) = tool_router {
        builder = builder.with_tool_router(r);
    }
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
    harness_with(answer, None, None)
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
fn server_with(
    answer: &str,
    tool_router: Option<Arc<ToolRouter>>,
) -> (TestServer, Arc<MockProvider>) {
    let h = harness_with(answer, tool_router, None);
    (h.server, h.provider)
}

// --- Tests ---

#[tokio::test]
async fn healthz_is_open() {
    let s = server("hi");
    s.get("/healthz").await.assert_status_ok();
}

#[tokio::test]
async fn requires_api_key_when_configured() {
    let s = server("hi");
    s.get("/v1/models").await.assert_status_unauthorized();
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
    assert_eq!(v["data"][0]["id"], "gpt-4o");
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
    assert_eq!(v["error"]["code"], "model_not_found");
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
    assert_eq!(v["choices"][0]["message"]["content"], "Hello there.");
    assert_eq!(v["choices"][0]["finish_reason"], "stop");
    assert!(v["choices"][0]["message"].get("tool_calls").is_none());
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
    assert_eq!(v["choices"][0]["finish_reason"], "tool_calls");
    let tc = &v["choices"][0]["message"]["tool_calls"][0];
    assert_eq!(tc["type"], "function");
    assert_eq!(tc["function"]["name"], "get_weather");
    let args: serde_json::Value =
        serde_json::from_str(tc["function"]["arguments"].as_str().unwrap()).unwrap();
    assert_eq!(args, json!({"city":"NYC"}));
    assert_eq!(v["choices"][0]["message"]["content"], "Let me check.");
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
        .expect("content-type")
        .to_str()
        .unwrap()
        .to_string();
    assert!(ct.contains("text/event-stream"), "content-type: {ct}");

    let body = r.text();
    let frames: Vec<&str> = body.split("data: ").skip(1).collect();
    assert!(frames.len() > 2, "too few chunks: {body}");
    assert_eq!(frames.last().unwrap().trim(), "[DONE]");

    // The frame before [DONE] carries the finish reason.
    let last: serde_json::Value =
        serde_json::from_str(frames[frames.len() - 2].trim()).expect("finish frame");
    assert_eq!(last["choices"][0]["finish_reason"], "tool_calls");

    // Tool name arrives first, then the arguments are streamed in chunks.
    let mut args = String::new();
    for frame in &frames[..frames.len() - 2] {
        let ev: serde_json::Value = serde_json::from_str(frame.trim()).expect("chunk frame");
        if let Some(calls) = ev["choices"][0]["delta"]["tool_calls"].as_array() {
            args.push_str(calls[0]["function"]["name"].as_str().unwrap_or(""));
            args.push_str(calls[0]["function"]["arguments"].as_str().unwrap_or(""));
        }
    }
    assert_eq!(&args[..11], "get_weather");
    let parsed: serde_json::Value = serde_json::from_str(&args[11..]).expect("arguments json");
    assert_eq!(parsed, json!({"city": "X"}));
}

// --- /v1/messages (Anthropic adapter) ---

#[tokio::test]
async fn messages_returns_anthropic_shape() {
    let (s, provider) = server_with("Hello there.", None);
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
    assert_eq!(v["type"], "message");
    assert_eq!(v["role"], "assistant");
    assert_eq!(v["model"], "gpt-4o");
    assert!(v["id"].as_str().unwrap().starts_with("msg_"));
    assert_eq!(v["content"][0]["type"], "text");
    assert_eq!(v["content"][0]["text"], "Hello there.");
    assert_eq!(v["stop_reason"], "end_turn");
    assert!(v["usage"]["input_tokens"].as_u64().unwrap() >= 1);
    assert!(v["usage"]["output_tokens"].as_u64().unwrap() >= 1);

    // The system block must actually reach the browser (it used to be lost).
    let body = provider.sent_last();
    assert!(body.contains("be terse"), "system missing: {body}");
    assert!(body.contains("hi"), "user text missing: {body}");
}

#[tokio::test]
async fn messages_tool_use_block_and_stop_reason() {
    let answer = "Let me check.\n{\"name\":\"get_weather\",\"arguments\":{\"city\":\"NYC\"}}";
    let (s, provider) = server_with(answer, None);
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
    assert_eq!(v["stop_reason"], "tool_use");
    assert_eq!(v["content"][0]["text"], "Let me check.");
    let tool = &v["content"][1];
    assert_eq!(tool["type"], "tool_use");
    assert_eq!(tool["name"], "get_weather");
    assert_eq!(tool["input"]["city"], "NYC");
    // The Anthropic tool declaration became an OpenAI tool in the prompt.
    assert!(provider.sent_last().contains("get_weather"));
}

#[tokio::test]
async fn messages_tool_result_history_reaches_the_browser() {
    let (s, provider) = server_with("22C", None);
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
    assert_eq!(v["stop_reason"], "end_turn");
    assert_eq!(v["content"][0]["text"], "22C");

    let body = provider.sent_last();
    assert!(body.contains("toolu_1"), "tool result id missing: {body}");
    assert!(body.contains("22C"), "tool result content missing: {body}");
}

#[tokio::test]
async fn messages_stream_is_sse_of_bounded_chunks() {
    let answer = "Hello there. This answer is long enough to need several chunks.";
    let (s, _provider) = server_with(answer, None);
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
        .expect("content-type")
        .to_str()
        .unwrap()
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
    for line in body.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            let ev: Value = serde_json::from_str(data).expect("valid event json");
            if ev["type"] == "content_block_delta" && ev["delta"]["type"] == "text_delta" {
                let chunk = ev["delta"]["text"].as_str().unwrap();
                assert!(
                    chunk.chars().count() <= 24,
                    "chunk longer than 24 chars: {chunk}"
                );
                text.push_str(chunk);
            }
        }
    }
    assert_eq!(text, answer);
}

#[tokio::test]
async fn messages_tool_choice_none_drops_mcp_tools() {
    let mut router = ToolRouter::new();
    router.register(Arc::new(MockToolProvider::new("stub").with_spec(
        ToolSpec {
            name: "remote_search".into(),
            description: "searches the web".into(),
            parameters: json!({"type": "object", "properties": {}}),
        },
    )));
    let (s, provider) = server_with("ok", Some(Arc::new(router)));

    // Control: without tool_choice the MCP tool is injected into the prompt.
    s.post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 50,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await
        .assert_status_ok();
    assert!(
        provider.sent_last().contains("remote_search"),
        "MCP tool should be injected by default: {}",
        provider.sent_last()
    );

    // `tool_choice: none` — the MCP tool must not reach the browser at all.
    s.post("/v1/messages")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "max_tokens": 50,
            "tool_choice": {"type": "none"},
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await
        .assert_status_ok();
    assert!(
        !provider.sent_last().contains("remote_search"),
        "tool_choice=none must not inject MCP tools: {}",
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
            v["error"]["message"]
                .as_str()
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
        v["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("circuit `chatgpt` open")),
        "{v}"
    );
}

#[tokio::test]
async fn sessions_pin_a_conversation_to_one_entry() {
    let sm = Arc::new(SessionManager::new(SessionCfg::default()));
    let h = harness_with("hi", None, Some(sm.clone()));

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
    assert!(state.runtime.tool_router.is_none());
    assert!(state.runtime.sessions.is_none());

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
