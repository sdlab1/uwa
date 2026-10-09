use serde_json::json;
use std::sync::Arc;
use uwa_history::{HistoryCfg, HistoryStore};
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockProvider, MockTransport,
};

async fn server_with_history() -> axum_test::TestServer {
    let store = Arc::new(
        HistoryStore::new(HistoryCfg::default(), false)
            .await
            .unwrap(),
    );
    AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("hello")))
        .with_history(store)
        .build()
        .server
}

#[tokio::test]
async fn history_starts_empty() {
    let s = server_with_history().await;
    let r = s
        .get("/admin/history")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(v["count"], 0);
}

#[tokio::test]
async fn history_records_after_chat() {
    let s = server_with_history().await;
    // Trigger a chat.
    let _ = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;

    let r = s
        .get("/admin/history")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(v["count"], 1);
    let rec = &v["records"][0];
    assert_eq!(rec["provider"], "chatgpt");
    assert_eq!(rec["status"], "success");
    assert_eq!(rec["response"]["text_preview"], "hello");
}

#[tokio::test]
async fn stats_reflect_requests() {
    let s = server_with_history().await;
    for _ in 0..3 {
        let _ = s
            .post("/v1/chat/completions")
            .add_header("Authorization", TEST_AUTH_HEADER)
            .json(&json!({
                "model": "gpt-4o",
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .await;
    }

    let r = s
        .get("/admin/stats")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(v["total"], 3);
    assert_eq!(v["success"], 3);
}

#[tokio::test]
async fn history_detail_by_id() {
    let s = server_with_history().await;
    let _ = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;

    // Get the list to find the id.
    let r = s
        .get("/admin/history")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    let v: serde_json::Value = r.json();
    let id = v["records"][0]["id"].as_str().unwrap();

    let r2 = s
        .get(&format!("/admin/history/{id}"))
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r2.assert_status_ok();
    let v2: serde_json::Value = r2.json();
    assert_eq!(v2["id"], *id);
}

#[tokio::test]
async fn selector_test_on_live_tab() {
    let s = server_with_history().await;
    let r = s
        .post("/admin/selector-test")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "provider": "chatgpt",
            "selector": "#prompt-textarea"
        }))
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert!(v["duration_ms"].as_u64().is_some());
}
