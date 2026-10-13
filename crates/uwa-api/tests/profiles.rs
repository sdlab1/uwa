//! Multi-account profiles end-to-end.
//!
//! Driven against `MockTransport::with_profiles()`, so the cookie
//! isolation is simulated: each profile gets its own pool of tabs, and
//! `open_tab` creates them lazily.

use serde_json::{json, Value};
use std::sync::Arc;
use uwa_testkit::{config::TEST_AUTH_HEADER, AppBuilder, MockProvider, MockTransport};

/// Keyed config with two profiles declared.
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
            provider_limits = { chatgpt = 1 }

            [profiles.personal]
            label = "Personal"
            "####,
        )
        .unwrap_or_else(|e| unreachable!("test config must parse: {e}")),
    )
}

fn app_with_profiles() -> axum_test::TestServer {
    uwa_testkit::test_server(
        AppBuilder::new()
            .with_config(config_with_profiles())
            .with_transport(Arc::new(MockTransport::with_n_tabs(1).with_profiles()))
            .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("ok")))
            .with_sessions(Arc::new(uwa_session::SessionManager::new(
                uwa_session::SessionCfg::default(),
            )))
            .build_state(),
    )
}

async fn chat_as(s: &axum_test::TestServer, profile: Option<&str>) -> axum_test::TestResponse {
    let mut r = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER);
    if let Some(p) = profile {
        r = r.add_header("x-uwa-session", p);
    }
    r.json(&json!({
        "model": "gpt-4o",
        "messages": [{"role": "user", "content": "hi"}]
    }))
    .await
}

async fn sessions_list(s: &axum_test::TestServer) -> Value {
    let r = s
        .get("/admin/sessions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
    r.json::<Value>()
}

#[tokio::test]
async fn header_selects_the_profile() {
    let s = app_with_profiles();

    let _ = chat_as(&s, None).await;
    let r = chat_as(&s, Some("work")).await;
    r.assert_status_ok();
    let r = chat_as(&s, Some("personal")).await;
    r.assert_status_ok();

    // Three sessions were created for the same message body: one
    // default, one `work`, one `personal`.
    let v = sessions_list(&s).await;
    let sessions = v["sessions"]
        .as_array()
        .unwrap_or_else(|| unreachable!("sessions must be an array: {v}"));
    let mut profiles: Vec<String> = sessions
        .iter()
        .map(|s| s["profile"].as_str().unwrap_or("default").to_string())
        .collect();
    profiles.sort();
    assert_eq!(
        profiles,
        vec!["default".to_string(), "personal".into(), "work".into()]
    );
}

#[tokio::test]
async fn same_profile_reuses_the_tab() {
    let s = app_with_profiles();
    let r = chat_as(&s, Some("work")).await;
    r.assert_status_ok();
    let r = chat_as(&s, Some("work")).await;
    r.assert_status_ok();

    let v = sessions_list(&s).await;
    let work: Vec<&Value> = v["sessions"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(|s| s["profile"] == "work")
        .collect();
    assert_eq!(work.len(), 1, "same (conversation, profile) must reuse");
}

#[tokio::test]
async fn unknown_profile_is_a_bad_request() {
    let s = app_with_profiles();
    let r = chat_as(&s, Some("nope")).await;
    r.assert_status_bad_request();
    let v: Value = r.json();
    let msg = v["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(msg.contains("unknown profile"), "{msg}");
    assert!(msg.contains("work"), "lists configured profiles: {msg}");
}

#[tokio::test]
async fn profiles_endpoint_lists_everything() {
    let s = app_with_profiles();
    // Do something so a tab exists in `work`.
    let r = chat_as(&s, Some("work")).await;
    r.assert_status_ok();

    let r = s
        .get("/admin/profiles")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(v["supports_profiles"], true);

    let names: Vec<String> = v["profiles"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(|p| p["name"].as_str().map(String::from))
        .collect();
    assert!(names.contains(&"default".to_string()));
    assert!(names.contains(&"work".to_string()));
    assert!(names.contains(&"personal".to_string()));

    let work = v["profiles"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .find(|p| p["name"] == "work")
        .cloned()
        .unwrap_or_else(|| unreachable!("work must be listed"));
    assert_eq!(work["active"], true);
    assert!(!work["tabs"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .is_empty());
    assert_eq!(work["label"], "Work");
}

#[tokio::test]
async fn open_profile_tab_creates_a_new_tab() {
    let s = app_with_profiles();
    let r = s
        .post("/admin/profiles/work/open")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({ "url": "https://chatgpt.com/" }))
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(v["reused"], false);
    assert!(v["tab"].as_str().unwrap_or_default().starts_with("dyn_"));
}

#[tokio::test]
async fn open_tab_in_unknown_profile_is_rejected() {
    let s = app_with_profiles();
    let r = s
        .post("/admin/profiles/nope/open")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({ "url": "https://chatgpt.com/" }))
        .await;
    r.assert_status_bad_request();
}

#[tokio::test]
async fn reset_profile_drops_its_tabs_and_sessions() {
    let s = app_with_profiles();
    let r = chat_as(&s, Some("work")).await;
    r.assert_status_ok();

    let r = s
        .post("/admin/profiles/work/reset")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert!(!v["dropped_tabs"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .is_empty());

    // The work session is gone.
    let v = sessions_list(&s).await;
    let work: Vec<&Value> = v["sessions"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(|s| s["profile"] == "work")
        .collect();
    assert!(work.is_empty(), "work sessions must be dropped: {v}");
}

#[tokio::test]
async fn reset_default_profile_is_rejected() {
    let s = app_with_profiles();
    let r = s
        .post("/admin/profiles/default/reset")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_bad_request();
}

#[tokio::test]
async fn default_profile_reuses_an_existing_tab_and_navigates() {
    let s = app_with_profiles();
    let r = s
        .post("/admin/profiles/default/open")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "url": "https://chatgpt.com/",
            "reuse_if_url_starts_with": "https://chatgpt.com/"
        }))
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    // The mock tab's URL is blank, so the reuse prefix never matches;
    // the handler navigates the existing static tab instead of creating
    // a dynamic one.
    assert_eq!(v["reused"], false);
    assert!(!v["tab"].as_str().unwrap_or_default().starts_with("dyn_"));
}
