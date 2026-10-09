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
        .expect("spawn");
    let stdout = child.stdout.take().unwrap();
    let mut lines = tokio::io::BufReader::new(stdout).lines();
    let mut url = None;
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(u) = line.strip_prefix("URL=") {
            url = Some(u.to_string());
            break;
        }
    }
    (child, url.expect("URL printed"))
}

#[tokio::test]
async fn serves_chat_fixture() {
    let (_child, url) = spawn().await;
    let body = reqwest::get(format!("{url}/fake/chat"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(body.contains("prompt-textarea"));
    assert!(body.contains("data-testid=\"send-button\""));
    assert!(body.contains("data-message-author-role"));
}

#[tokio::test]
async fn serves_claude_and_gemini_fixtures() {
    let (_child, url) = spawn().await;
    let claude = reqwest::get(format!("{url}/fake/claude"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(claude.contains("contenteditable"));
    assert!(claude.contains("assistant-message"));

    let gemini = reqwest::get(format!("{url}/fake/gemini"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(gemini.contains("ql-editor"));
    assert!(gemini.contains("model-response"));
}

#[tokio::test]
async fn serves_index() {
    let (_child, url) = spawn().await;
    let body = reqwest::get(&url).await.unwrap().text().await.unwrap();
    assert!(body.contains("uwa fixture server"));
}
