//! End-to-end test against the fixture server in a real browser.
//!
//! ```sh
//! UWA_CHROMIUM=1 cargo test -p uwa-providers --features fixture-server \
//!   --test e2e_fixture -- --ignored --test-threads=1
//! ```
//!
//! Requires a Chromium with `--remote-debugging-port=9222` (the CI
//! providers-e2e workflow starts one).

#![cfg(feature = "fixture-server")]

use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncBufReadExt;

struct FixtureServer {
    child: tokio::process::Child,
    base_url: String,
}

impl FixtureServer {
    async fn spawn() -> anyhow::Result<Self> {
        let bin = env!("CARGO_BIN_EXE_uwa-fixture-server");
        let mut child = tokio::process::Command::new(bin)
            .arg("--addr")
            .arg("127.0.0.1:0")
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let stdout = child.stdout.take().unwrap();
        let mut lines = tokio::io::BufReader::new(stdout).lines();
        let mut base_url = None;
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(url) = line.strip_prefix("URL=") {
                base_url = Some(url.to_string());
                break;
            }
        }
        let base_url = base_url.ok_or_else(|| anyhow::anyhow!("no URL printed"))?;
        Ok(Self { child, base_url })
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

async fn run_fixture_flow(fixture_path: &str, selectors_toml: &str) -> String {
    let ws = uwa_testkit::chromium_ws_url();

    let server = FixtureServer::spawn().await.expect("fixture server");
    let url = format!("{}{fixture_path}", server.base_url);

    let transport = uwa_browser::CdpTransport::connect(&ws, Duration::from_secs(60), None)
        .await
        .expect("connect Chromium");

    use uwa_core::Transport;
    let tab = {
        let tabs = transport.list_tabs().await.unwrap();
        tabs.into_iter().next().expect("a tab")
    };
    let page = transport.page(&tab).await.unwrap();
    page.goto(&url.parse().unwrap()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let cfg = uwa_config::Config::load_from_str(&format!(
        r##"
        [server]
        bind = "127.0.0.1"
        port = 8080

        [providers.fake]
        name = "fake"
        url_patterns = ["{base}/*"]
        capabilities = {{ streams = true, tool_calls = false, vision = false }}
        [providers.fake.selectors]
        {selectors_toml}
        [providers.fake.finisher]
        dom_stable_ms = 100
        poll_ms = 50
        min_wait_ms = 50
        max_wait_ms = 5000
        "##,
        base = server.base_url
    ))
    .unwrap();

    let provider_cfg = cfg.providers.get("fake").unwrap().clone();
    let provider = uwa_providers::GenericProvider::from_config(provider_cfg);

    use uwa_core::SiteProvider;
    provider
        .send_message(page.as_ref(), "hello fixture")
        .await
        .unwrap();
    provider.wait_response(page.as_ref()).await.unwrap()
}

#[tokio::test]
#[ignore = "requires Chromium on ws://127.0.0.1:9222"]
async fn full_pipeline_against_chat_fixture() {
    if std::env::var("UWA_CHROMIUM").is_err() {
        return;
    }
    let text = run_fixture_flow(
        "/fake/chat",
        r##"
        input = "#prompt-textarea"
        send_button = "[data-testid=send-button]"
        stop_button = "[data-testid=stop-button]"
        assistant_message = "[data-message-author-role=assistant]"
        "##,
    )
    .await;
    assert!(
        text.contains("FIXTURE_REPLY: hello fixture"),
        "unexpected response: {text}"
    );
}

#[tokio::test]
#[ignore = "requires Chromium on ws://127.0.0.1:9222"]
async fn full_pipeline_against_claude_fixture() {
    if std::env::var("UWA_CHROMIUM").is_err() {
        return;
    }
    let text = run_fixture_flow(
        "/fake/claude",
        r##"
        input = "div[contenteditable=true]"
        send_button = "button[aria-label='Send message']"
        assistant_message = "[data-testid=assistant-message]"
        "##,
    )
    .await;
    assert!(
        text.contains("FIXTURE_REPLY: hello fixture"),
        "unexpected response: {text}"
    );
}

#[tokio::test]
#[ignore = "requires Chromium on ws://127.0.0.1:9222"]
async fn full_pipeline_against_gemini_fixture() {
    if std::env::var("UWA_CHROMIUM").is_err() {
        return;
    }
    let text = run_fixture_flow(
        "/fake/gemini",
        r##"
        input = "div.ql-editor"
        send_button = "button.send-button"
        assistant_message = "model-response"
        "##,
    )
    .await;
    assert!(
        text.contains("FIXTURE_REPLY: hello fixture"),
        "unexpected response: {text}"
    );
}

#[tokio::test]
#[ignore = "requires Chromium on ws://127.0.0.1:9222"]
async fn workflow_clicks_send_instead_of_default_path() {
    if std::env::var("UWA_CHROMIUM").is_err() {
        return;
    }
    let ws = uwa_testkit::chromium_ws_url();
    let server = FixtureServer::spawn().await.expect("fixture server");
    let url = format!("{}/fake/chat", server.base_url);

    let transport = uwa_browser::CdpTransport::connect(&ws, Duration::from_secs(60), None)
        .await
        .expect("connect Chromium");
    use uwa_core::Transport;
    let tab = transport
        .list_tabs()
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("a tab");
    let page = transport.page(&tab).await.unwrap();
    page.goto(&url.parse().unwrap()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // A minimal workflow: fill the input, click send, wait for the
    // assistant container to stabilize.
    let cfg = uwa_config::Config::load_from_str(&format!(
        r##"
        [server]
        bind = "127.0.0.1"
        port = 8080

        [providers.fake]
        name = "fake"
        url_patterns = ["{base}/*"]
        capabilities = {{ streams = true, tool_calls = false, vision = false }}
        [providers.fake.selectors]
        input = "#prompt-textarea"
        send_button = "[data-testid=send-button]"
        assistant_message = "[data-message-author-role=assistant]"
        [providers.fake.finisher]
        dom_stable_ms = 100
        poll_ms = 50
        min_wait_ms = 50
        max_wait_ms = 5000

        [[providers.fake.workflow]]
        action = "fill_input"
        target = "input_box"

        [[providers.fake.workflow]]
        action = "click"
        target = "send_btn"

        [[providers.fake.workflow]]
        action = "stream_wait"
        target = "assistant_message"
        timeout_secs = 10
        "##,
        base = server.base_url
    ))
    .unwrap();

    let provider_cfg = cfg.providers.get("fake").unwrap().clone();
    let provider = uwa_providers::GenericProvider::from_config(provider_cfg);

    use uwa_core::SiteProvider;
    provider
        .send_message(page.as_ref(), "workflow hello")
        .await
        .unwrap();
    let text = provider.wait_response(page.as_ref()).await.unwrap();
    assert!(
        text.contains("FIXTURE_REPLY: workflow hello"),
        "unexpected response: {text}"
    );
}
