//! UWA_CHROMIUM=1 cargo test -p uwa-browser --test media_scan_e2e -- --ignored

#[tokio::test]
async fn scan_media_detects_video_tag() {
    if std::env::var("UWA_CHROMIUM").is_err() {
        return;
    }
    let url = uwa_testkit::chromium_ws_url();
    let t = uwa_browser::CdpTransport::connect(&url, std::time::Duration::from_secs(60), None)
        .await
        .unwrap_or_else(|_| unreachable!());

    use uwa_core::Transport;
    let tabs = t.list_tabs().await.unwrap_or_else(|_| unreachable!());
    let page = t
        .page(tabs.first().unwrap_or_else(|| unreachable!()))
        .await
        .unwrap_or_else(|_| unreachable!());
    page.goto(
        &"data:text/html,<video src='https://example.com/v.mp4' controls></video>"
            .parse()
            .unwrap_or_else(|_| unreachable!()),
    )
    .await
    .unwrap_or_else(|_| unreachable!());
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let media = page.scan_media().await.unwrap_or_else(|_| unreachable!());
    assert_eq!(media.len(), 1, "one video element expected: {media:?}");
    let m0 = media.first().unwrap_or_else(|| unreachable!());
    assert_eq!(m0.kind, uwa_core::MediaSourceKind::Video);
    assert!(m0.url.contains("v.mp4"), "url: {}", m0.url);
}

#[tokio::test]
async fn audio_capture_roundtrip_on_local_page() {
    if std::env::var("UWA_CHROMIUM").is_err() {
        return;
    }
    let url = uwa_testkit::chromium_ws_url();
    let t = uwa_browser::CdpTransport::connect(&url, std::time::Duration::from_secs(60), None)
        .await
        .unwrap_or_else(|_| unreachable!());

    use uwa_core::Transport;
    let tabs = t.list_tabs().await.unwrap_or_else(|_| unreachable!());
    let page = t
        .page(tabs.first().unwrap_or_else(|| unreachable!()))
        .await
        .unwrap_or_else(|_| unreachable!());
    page.goto(
        &"http://127.0.0.1:1/"
            .parse()
            .unwrap_or_else(|_| unreachable!()),
    )
    .await
    .ok();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Start must succeed and be idempotent; stop with nothing played
    // returns None (empty chunks).
    page.start_audio_capture()
        .await
        .unwrap_or_else(|_| unreachable!());
    page.start_audio_capture()
        .await
        .unwrap_or_else(|_| unreachable!()); // already_running
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let blob = page
        .stop_audio_capture()
        .await
        .unwrap_or_else(|_| unreachable!());
    assert!(
        blob.is_none(),
        "no playback happened, expect no bytes: {blob:?}"
    );
}
