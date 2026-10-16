//! `/admin/selector-generate` + `/admin/selector-apply` integration tests.

use serde_json::json;
use std::sync::Arc;
use uwa_core::TabId;
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockPage, MockProvider, MockTransport,
};

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
    assert_eq!(v.get("persisted_to"), Some(&serde_json::Value::Null));
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
