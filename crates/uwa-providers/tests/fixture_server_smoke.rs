//! Smoke test: the fixture server serves valid HTML at the expected paths.

#![cfg(feature = "fixture-server")]

use std::process::Stdio;
use tokio::io::AsyncBufReadExt;

async fn spawn() -> (tokio::process::Child, String) {
    let bin = env!("CARGO_BIN_EXE_uwa-fixture-server");
    let mut child = tokio::process::Command::new(bin)
        .arg("--addr")
        .arg("127.0.0.1:0")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap_or_else(|e| unreachable!("spawn fixture server: {e}"));
    let stdout = child
        .stdout
        .take()
        .unwrap_or_else(|| unreachable!("stdout captured"));
    let mut lines = tokio::io::BufReader::new(stdout).lines();
    let mut url = None;
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(u) = line.strip_prefix("URL=") {
            url = Some(u.to_string());
            break;
        }
    }
    let url = url.unwrap_or_else(|| unreachable!("URL printed by fixture server"));
    (child, url)
}

async fn get_text(url: &str, path: &str) -> String {
    let full = format!("{url}{path}");
    reqwest::get(&full)
        .await
        .unwrap_or_else(|e| unreachable!("GET {full}: {e}"))
        .text()
        .await
        .unwrap_or_else(|e| unreachable!("read body of {full}: {e}"))
}

#[tokio::test]
async fn serves_chat_fixture() {
    let (_child, url) = spawn().await;
    let body = get_text(&url, "/fake/chat").await;
    assert!(body.contains("prompt-textarea"));
    assert!(body.contains("data-testid=\"send-button\""));
    assert!(body.contains("data-message-author-role"));
}

#[tokio::test]
async fn serves_claude_and_gemini_fixtures() {
    let (_child, url) = spawn().await;
    let claude = get_text(&url, "/fake/claude").await;
    assert!(claude.contains("contenteditable"));
    assert!(claude.contains("assistant-message"));

    let gemini = get_text(&url, "/fake/gemini").await;
    assert!(gemini.contains("ql-editor"));
    assert!(gemini.contains("model-response"));
}

#[tokio::test]
async fn serves_index() {
    let (_child, url) = spawn().await;
    let body = get_text(&url, "/").await;
    assert!(body.contains("uwa fixture server"));
}
