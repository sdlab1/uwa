//! The `media.audio_capture_enabled` provider flag must drive the capture
//! hook: off → no injection; on → start before send, stop after extract.
//!
//! Uses a real `GenericProvider` (the flag lives in its send/extract path —
//! a MockProvider would test nothing).

use serde_json::{json, Value};
use std::sync::Arc;
use uwa_core::TabId;
use uwa_extract::ExtractionPipeline;
use uwa_providers::GenericProvider;
use uwa_testkit::{config::TEST_AUTH_HEADER, AppBuilder, MockPage, MockTransport};

/// A page that satisfies the whole GenericProvider send+extract flow.
/// The audio hooks answer via distinctive substrings: the start script
/// patches `HTMLMediaElement.prototype.play`; the stop script reads the
/// `not-started` sentinel.
fn happy_page(with_audio: bool) -> MockPage {
    let p = MockPage::new()
        .expect_default(Value::Null)
        .expect_seq("!!document.querySelector", vec![json!(true)])
        .expect_seq("el.focus()", vec![json!({ "ok": true })])
        .expect_seq("el.disabled", vec![json!(true)])
        .expect_seq("el.click()", vec![json!(true)])
        .expect_seq("el.value !== undefined", vec![json!(true)])
        .with_html("<div data-role='assistant'>the answer</div>");
    let _ = with_audio;
    p
}

fn keyed_config_with_media(audio: bool) -> Arc<uwa_config::Config> {
    let media = if audio {
        "\n            [providers.chatgpt.media]\n            audio_capture_enabled = true\n"
    } else {
        ""
    };
    Arc::new(
        uwa_config::Config::load_from_str(&format!(
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
            stop_button = "button.stop"
            assistant_message = "[data-role=assistant]"
            [providers.chatgpt.finisher]
            dom_stable_ms = 30
            poll_ms = 10
            min_wait_ms = 30
            max_wait_ms = 2000
            {media}
            "##
        ))
        .unwrap(),
    )
}

/// Server wired with a **real** GenericProvider over the given page.
/// Returns the server plus the page handle for log assertions.
fn server(cfg: Arc<uwa_config::Config>, page: MockPage) -> axum_test::TestServer {
    let provider_cfg = cfg.providers.get("chatgpt").unwrap().clone();
    let provider = GenericProvider::new(provider_cfg, Arc::new(ExtractionPipeline::new()));
    let t = MockTransport::new().with_page(TabId::from_raw("t1"), Arc::new(page));
    AppBuilder::new()
        .with_config(cfg)
        .with_transport(Arc::new(t))
        .with_provider(Arc::new(provider))
        .build()
        .server
}

async fn chat(s: &axum_test::TestServer) -> serde_json::Value {
    let r = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;
    r.assert_status_ok();
    r.json()
}

#[tokio::test]
async fn audio_capture_flag_off_no_injection() {
    let page = happy_page(false);
    let s = server(keyed_config_with_media(false), page.clone());
    let v = chat(&s).await;
    assert_eq!(v["choices"][0]["message"]["content"], "the answer");

    // No audio hook of any kind reached the page.
    assert!(
        !page.log().iter().any(|e| e.starts_with("audio_capture:")),
        "flag off must not start/stop audio capture: {:?}",
        page.log()
    );
}

#[tokio::test]
async fn audio_capture_flag_on_injects_start_and_stop() {
    let page = happy_page(true);
    let s = server(keyed_config_with_media(true), page.clone());
    let v = chat(&s).await;
    assert_eq!(v["choices"][0]["message"]["content"], "the answer");

    let log = page.log();
    let pos_start = log
        .iter()
        .position(|e| e == "audio_capture:start")
        .expect("audio capture started");
    let pos_stop = log
        .iter()
        .position(|e| e == "audio_capture:stop")
        .expect("audio capture stopped after extraction");
    assert!(pos_start < pos_stop, "start precedes stop: {log:?}");
}
