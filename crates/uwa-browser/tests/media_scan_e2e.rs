//! UWA_CHROMIUM=1 cargo test -p uwa-browser --test media_scan_e2e -- --ignored

#[tokio::test]
#[ignore = "requires Chromium on http://127.0.0.1:9222"]
async fn scan_media_detects_video_tag() {
    if std::env::var("UWA_CHROMIUM").is_err() {
        return;
    }
    let url = std::env::var("UWA_CHROMIUM_WS").unwrap_or_else(|_| "http://127.0.0.1:9222".into());
    let t = uwa_browser::CdpTransport::connect(&url, std::time::Duration::from_secs(60), None)
        .await
        .unwrap();

    use uwa_core::Transport;
    let tabs = t.list_tabs().await.unwrap();
    let page = t.page(&tabs[0]).await.unwrap();
    page.goto(
        &"data:text/html,<video src='https://example.com/v.mp4' controls></video>"
            .parse()
            .unwrap(),
    )
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let media = page.scan_media().await.unwrap();
    assert_eq!(media.len(), 1, "one video element expected: {media:?}");
    assert_eq!(media[0].kind, uwa_core::MediaKind::Video);
    assert!(media[0].url.contains("v.mp4"), "url: {}", media[0].url);
}

#[tokio::test]
#[ignore = "requires Chromium on http://127.0.0.1:9222"]
async fn audio_capture_roundtrip_on_local_page() {
    if std::env::var("UWA_CHROMIUM").is_err() {
        return;
    }
    let url = std::env::var("UWA_CHROMIUM_WS").unwrap_or_else(|_| "http://127.0.0.1:9222".into());
    let t = uwa_browser::CdpTransport::connect(&url, std::time::Duration::from_secs(60), None)
        .await
        .unwrap();

    use uwa_core::Transport;
    let tabs = t.list_tabs().await.unwrap();
    let page = t.page(&tabs[0]).await.unwrap();
    page.goto(&"http://127.0.0.1:1/".parse().unwrap())
        .await
        .ok();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Start must succeed and be idempotent; stop with nothing played
    // returns None (empty chunks).
    page.start_audio_capture().await.unwrap();
    page.start_audio_capture().await.unwrap(); // already_running
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let blob = page.stop_audio_capture().await.unwrap();
    assert!(
        blob.is_none(),
        "no playback happened, expect no bytes: {blob:?}"
    );
}
