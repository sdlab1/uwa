//! Audio + video capture via JS injection.
//!
//! ## Audio
//!
//! Patches `HTMLMediaElement.prototype.play` so any `<audio>`/`<video>`
//! that starts playing gets piped through `MediaStreamAudioDestinationNode`
//! → `MediaRecorder`. Chunks accumulate in `window.__uwaAudio.chunks` as
//! base64; Rust polls them on stop.
//!
//! Requires Chrome flag `--autoplay-policy=no-user-gesture-required`
//! (added automatically to nodriver `extra_args` when audio capture is
//! enabled for any provider — see `uwa-bin` wiring).
//!
//! ## Video
//!
//! `scan_media` walks `<video>` and `<audio>` tags, resolves
//! `src`/`currentSrc`, and returns URLs. Detection only — UWA never
//! auto-downloads. `fetch_blob` can pull a `blob:` URL from inside the
//! page when a caller explicitly asks.

use uwa_core::Result;

pub async fn start_capture(page: &dyn uwa_core::Page) -> Result<()> {
    let js = include_str!("js/audio_capture_start.js");
    let v = page.eval(js).await?;
    if v.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(uwa_core::UwaError::Transport(format!(
            "audio capture start: {v:?}"
        )));
    }
    Ok(())
}

pub async fn stop_capture(page: &dyn uwa_core::Page) -> Result<Option<Vec<u8>>> {
    let js = include_str!("js/audio_capture_stop.js");
    let v = page.eval(js).await?;
    let Some(chunks) = v.get("chunks").and_then(serde_json::Value::as_array) else {
        return Ok(None);
    };
    if chunks.is_empty() {
        return Ok(None);
    }
    // Concatenate all base64 chunks.
    use base64::Engine;
    let mut buf: Vec<u8> = Vec::new();
    for c in chunks {
        let Some(s) = c.as_str() else { continue };
        if let Ok(b) = base64::engine::general_purpose::STANDARD.decode(s) {
            buf.extend_from_slice(&b);
        }
    }
    if buf.is_empty() {
        return Ok(None);
    }
    Ok(Some(buf))
}

pub async fn scan_media(page: &dyn uwa_core::Page) -> Result<Vec<uwa_core::MediaResource>> {
    let js = include_str!("js/media_scan.js");
    let v = page.eval(js).await?;
    let Some(arr) = v.as_array() else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let kind = match item.get("kind").and_then(serde_json::Value::as_str) {
            Some("video") => uwa_core::MediaSourceKind::Video,
            Some("audio") => uwa_core::MediaSourceKind::Audio,
            _ => continue,
        };
        let Some(url) = item.get("src").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if url.is_empty() {
            continue;
        }
        out.push(uwa_core::MediaResource {
            kind,
            url: url.to_string(),
            mime: item
                .get("mime")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            duration_secs: item.get("duration").and_then(serde_json::Value::as_f64),
        });
    }
    Ok(out)
}

/// Fetch a media URL from inside the page and return it as bytes.
/// Works for `blob:` URLs, which aren't reachable from outside the page.
pub async fn fetch_blob(page: &dyn uwa_core::Page, url: &str) -> Result<Vec<u8>> {
    let js = format!(
        r#"(async () => {{
            try {{
                const r = await fetch({u});
                const buf = await r.arrayBuffer();
                const bytes = new Uint8Array(buf);
                let bin = '';
                const step = 8192;
                for (let i = 0; i < bytes.length; i += step) {{
                    bin += String.fromCharCode.apply(null, bytes.subarray(i, Math.min(i + step, bytes.length)));
                }}
                return {{ ok: true, b64: btoa(bin) }};
            }} catch (e) {{
                return {{ ok: false, error: String(e) }};
            }}
        }})()"#,
        u = serde_json::to_string(url).unwrap_or_else(|_| format!("{url:?}")),
    );
    let v = page.eval(&js).await?;
    if v.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        let err = v
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        return Err(uwa_core::UwaError::Transport(format!("fetch blob: {err}")));
    }
    let b64 = v
        .get("b64")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| uwa_core::UwaError::Transport("fetch blob: no b64".into()))?;
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| uwa_core::UwaError::Transport(format!("decode b64: {e}")))
}

use uwa_core::types::media::{MediaAsset, MediaKind};

const COLLECT_TEMPLATE: &str = include_str!("js/media_collect.js");

/// Collect media inside the last assistant message.
///
/// `blob:` URLs are returned verbatim; the caller decides whether to
/// fetch them via [`fetch_blob`].
pub async fn collect_media(
    page: &dyn uwa_core::Page,
    container_selector: &str,
    max_assets: usize,
    max_bytes: u64,
) -> Result<Vec<MediaAsset>> {
    let js = format!(
        "{}({}, {}, {})",
        COLLECT_TEMPLATE.trim_end_matches(';'),
        serde_json::to_string(container_selector).unwrap_or_else(|_| "\"\"".into()),
        max_assets.min(64),
        max_bytes,
    );
    let v = page.eval(&js).await?;
    let Some(arr) = v.as_array() else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(kind) = item.get("kind").and_then(|x| x.as_str()) else {
            continue;
        };
        let kind = match kind {
            "image" => MediaKind::Image,
            "audio" => MediaKind::Audio,
            "video" => MediaKind::Video,
            "canvas" => MediaKind::Canvas,
            _ => continue,
        };
        let Some(url) = item.get("url").and_then(|x| x.as_str()) else {
            continue;
        };
        if url.is_empty() {
            continue;
        }
        out.push(MediaAsset {
            kind,
            url: url.to_string(),
            mime: item
                .get("mime")
                .and_then(|x| x.as_str())
                .map(str::to_string),
            size_bytes: item.get("size_bytes").and_then(|x| x.as_u64()),
            local_path: None,
            alt: item.get("alt").and_then(|x| x.as_str()).map(str::to_string),
            source: item
                .get("source")
                .and_then(|x| x.as_str())
                .map(str::to_string),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uwa_testkit::MockPage;

    #[tokio::test]
    async fn start_reports_ok() {
        let p = MockPage::new().expect_default(json!({"ok": true}));
        start_capture(&p)
            .await
            .unwrap_or_else(|e| unreachable!("{e}"));
    }

    #[tokio::test]
    async fn start_failure_is_error() {
        let p = MockPage::new().expect_default(json!({"ok": false}));
        assert!(start_capture(&p).await.is_err());
    }

    #[tokio::test]
    async fn stop_returns_bytes() {
        // Base64("Hi") = "SGk=", Base64("!") = "IQ==".
        let p = MockPage::new().expect_default(json!({
            "ok": true,
            "chunks": ["SGk=", "IQ=="]
        }));
        let bytes = stop_capture(&p)
            .await
            .unwrap_or_else(|e| unreachable!("{e}"))
            .unwrap_or_else(|| unreachable!());
        assert_eq!(bytes, b"Hi!");
    }

    #[tokio::test]
    async fn stop_empty_returns_none() {
        let p = MockPage::new().expect_default(json!({"ok": true, "chunks": []}));
        assert!(stop_capture(&p)
            .await
            .unwrap_or_else(|e| unreachable!("{e}"))
            .is_none());
    }

    #[tokio::test]
    async fn stop_not_started_returns_none() {
        // No "chunks" array at all.
        let p = MockPage::new().expect_default(json!({"ok": false}));
        assert!(stop_capture(&p)
            .await
            .unwrap_or_else(|e| unreachable!("{e}"))
            .is_none());
    }

    #[tokio::test]
    async fn scan_media_parses_entries() {
        let p = MockPage::new().expect_default(json!([
            {"kind": "audio", "src": "blob:https://x/1", "mime": "audio/webm", "duration": 3.5},
            {"kind": "video", "src": "https://x/v.mp4", "duration": 10.0},
            {"kind": "unknown", "src": "y"},
        ]));
        let media = scan_media(&p).await.unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(media.len(), 2);
        let m0 = media.first().unwrap_or_else(|| unreachable!());
        assert_eq!(m0.kind, uwa_core::MediaSourceKind::Audio);
        assert_eq!(m0.url, "blob:https://x/1");
        assert_eq!(m0.mime.as_deref(), Some("audio/webm"));
        let m1 = media.get(1).unwrap_or_else(|| unreachable!());
        assert_eq!(m1.kind, uwa_core::MediaSourceKind::Video);
        assert_eq!(m1.duration_secs, Some(10.0));
    }

    #[tokio::test]
    async fn scan_media_non_array_is_empty() {
        let p = MockPage::new().expect_default(json!({"ok": true}));
        assert!(scan_media(&p)
            .await
            .unwrap_or_else(|e| unreachable!("{e}"))
            .is_empty());
    }
}
