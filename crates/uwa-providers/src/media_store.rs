//! Persist response media to disk and (optionally) fetch `blob:` /
//! `data:` assets. Also decides the UWA-served URL.
//!
//! Files land under `<UWA_HOME>/media/YYYY-MM-DD/` and are served at
//! `/admin/media/<day>/<name>` — a static-ish path handled by the API
//! crate. We don't use `tower_http::services::ServeDir` for these; a
//! dedicated handler lets us cap types and add auth.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use uwa_core::types::media::{MediaAsset, MediaKind};
use uwa_core::{Page, Result, UwaError};

/// Where a saved asset will be served from. Served relative to the
/// daemon's own origin, so the client appends nothing.
pub const URL_PREFIX: &str = "/admin/media";

/// What to do with an asset whose URL is a `data:` URI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataUriMode {
    /// Save the bytes to disk, expose a served URL.
    Save,
    /// Leave the `data:` URI in place. Useful when the caller wants to
    /// embed the image in a client that doesn't fetch URLs.
    Inline,
}

#[derive(Debug, Clone)]
pub struct StoreCfg {
    pub root: PathBuf,
    pub max_bytes: u64,
    pub data_uri: DataUriMode,
    /// Download `http(s):` sources to disk. When `false`, the URL is
    /// passed through unchanged — cheaper, and clients that can render
    /// remote images are happy.
    pub download_remote: bool,
    /// Fetch `blob:` URLs from inside the page and save the bytes.
    /// When `false`, blob URLs are dropped (they're unreachable from
    /// the client's side of the wire).
    pub fetch_blobs: bool,
    /// Save `<audio>` assets. Video is always pass-through because of
    /// size, images are always saved when they're inline.
    pub save_audio: bool,
}

impl Default for StoreCfg {
    fn default() -> Self {
        Self {
            root: default_root(),
            max_bytes: 20 * 1024 * 1024,
            data_uri: DataUriMode::Save,
            download_remote: false,
            fetch_blobs: true,
            save_audio: true,
        }
    }
}

pub fn default_root() -> PathBuf {
    if let Ok(v) = std::env::var("UWA_HOME") {
        return PathBuf::from(v).join("media");
    }
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    home.join(".uwa").join("media")
}

/// Process a list of assets in place. Failures are logged and the asset
/// is kept (with its original URL) so the client at least sees a link.
pub async fn persist(page: &dyn Page, assets: &mut [MediaAsset], cfg: &StoreCfg) -> usize {
    let mut saved = 0usize;
    for asset in assets.iter_mut() {
        if let Err(e) = persist_one(page, asset, cfg).await {
            tracing::warn!(url = %asset.url, "media persist: {e}");
            continue;
        }
        if asset.local_path.is_some() {
            saved = saved.saturating_add(1);
        }
    }
    saved
}

async fn persist_one(page: &dyn Page, asset: &mut MediaAsset, cfg: &StoreCfg) -> Result<()> {
    // `data:` URI — decode inline.
    let is_data_uri = asset.url.starts_with("data:");
    if is_data_uri {
        let rest: String = asset.url.trim_start_matches("data:").to_string();
        return handle_data_uri(asset, &rest, cfg).await;
    }

    // `blob:` — fetch from the page, then save bytes.
    if asset.url.starts_with("blob:") {
        if !cfg.fetch_blobs {
            return Ok(());
        }
        let bytes = fetch_blob_from_page(page, &asset.url)
            .await
            .map_err(|e| UwaError::Transport(format!("fetch blob: {e}")))?;
        if bytes.len() as u64 > cfg.max_bytes {
            return Err(UwaError::BadRequest(format!(
                "blob is {} bytes, exceeds {}",
                bytes.len(),
                cfg.max_bytes
            )));
        }
        let mime = asset.mime.clone().unwrap_or_else(|| mime_for(asset.kind));
        return write_to_disk(asset, &bytes, &mime, cfg);
    }

    // `http(s):` — optionally download.
    if asset.url.starts_with("http://") || asset.url.starts_with("https://") {
        if !cfg.download_remote {
            return Ok(());
        }
        let resp = reqwest::get(&asset.url)
            .await
            .map_err(|e| UwaError::Transport(format!("GET {}: {e}", asset.url)))?;
        let mime = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .or_else(|| asset.mime.clone())
            .unwrap_or_else(|| mime_for(asset.kind));
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| UwaError::Transport(format!("read {}: {e}", asset.url)))?;
        if bytes.len() as u64 > cfg.max_bytes {
            return Err(UwaError::BadRequest(format!(
                "remote asset is {} bytes, exceeds {}",
                bytes.len(),
                cfg.max_bytes
            )));
        }
        return write_to_disk(asset, &bytes, &mime, cfg);
    }

    // Served-by-UWA URL or something we don't recognise: pass through.
    Ok(())
}

async fn handle_data_uri(asset: &mut MediaAsset, rest: &str, cfg: &StoreCfg) -> Result<()> {
    if cfg.data_uri == DataUriMode::Inline {
        return Ok(());
    }
    let (meta, b64) = rest
        .split_once(',')
        .ok_or_else(|| UwaError::BadRequest("malformed data URI".into()))?;
    // `data:image/png;base64` → mime=image/png, encoding=base64
    let mut parts = meta.split(';');
    let mime = parts
        .next()
        .unwrap_or("application/octet-stream")
        .to_string();
    let encoding = parts.next().unwrap_or("");
    if encoding != "base64" {
        return Err(UwaError::BadRequest(format!(
            "data URI encoding `{encoding}` is not supported"
        )));
    }
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| UwaError::BadRequest(format!("bad data URI base64: {e}")))?;
    if bytes.len() as u64 > cfg.max_bytes {
        return Err(UwaError::BadRequest(format!(
            "data URI is {} bytes, exceeds {}",
            bytes.len(),
            cfg.max_bytes
        )));
    }
    asset.mime = Some(mime.clone());
    write_to_disk(asset, &bytes, &mime, cfg)
}

fn write_to_disk(asset: &mut MediaAsset, bytes: &[u8], mime: &str, cfg: &StoreCfg) -> Result<()> {
    // Audio: skip when disabled at the config level.
    if asset.kind == MediaKind::Audio && !cfg.save_audio {
        return Ok(());
    }
    // Video: never saved; size alone rules it out. Pass-through.
    if asset.kind == MediaKind::Video {
        return Ok(());
    }

    let (day, id) = (today(), uuid::Uuid::new_v4().simple().to_string());
    let dir = cfg.root.join(&day);
    std::fs::create_dir_all(&dir)
        .map_err(|e| UwaError::Internal(format!("mkdir {}: {e}", dir.display())))?;

    let ext = extension_for(mime, asset.kind);
    let file_name = format!("{id}.{ext}");
    let path = dir.join(&file_name);
    std::fs::write(&path, bytes)
        .map_err(|e| UwaError::Internal(format!("write {}: {e}", path.display())))?;

    asset.local_path = Some(path);
    asset.size_bytes = Some(bytes.len() as u64);
    asset.url = format!("{URL_PREFIX}/{day}/{file_name}");
    if asset.mime.is_none() {
        asset.mime = Some(mime.to_string());
    }
    Ok(())
}

fn extension_for(mime: &str, kind: MediaKind) -> &'static str {
    // Take the subtype after the slash, keep it short and safe.
    if let Some((_, sub)) = mime.split_once('/') {
        let clean: String = sub
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        if !clean.is_empty() {
            // We can't return a &'static str from a runtime String, so
            // map the common cases. Anything else falls back to the
            // kind's default.
            return match clean.as_str() {
                "png" => "png",
                "jpeg" => "jpg",
                "jpg" => "jpg",
                "gif" => "gif",
                "webp" => "webp",
                "svg" => "svg",
                "mp4" => "mp4",
                "webm" => "webm",
                "ogg" => "ogg",
                "mp3" => "mp3",
                "wav" => "wav",
                _ => kind.default_extension(),
            };
        }
    }
    kind.default_extension()
}

fn mime_for(kind: MediaKind) -> String {
    match kind {
        MediaKind::Image => "image/png".into(),
        MediaKind::Canvas => "image/png".into(),
        MediaKind::Audio => "audio/webm".into(),
        MediaKind::Video => "video/mp4".into(),
    }
}

fn today() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Howard Hinnant's algorithm. Small enough to inline; a chrono
/// dependency for one date format isn't worth it.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z.wrapping_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe.wrapping_sub(doe / 1460).wrapping_add(doe / 36_524).wrapping_sub(doe / 146_096)) / 365;
    let y = yoe.wrapping_add(era.wrapping_mul(400));
    let doy = doe.wrapping_sub(365_i64.wrapping_mul(yoe).wrapping_add(yoe / 4).wrapping_sub(yoe / 100));
    let mp = (5_i64.wrapping_mul(doy).wrapping_add(2)) / 153;
    let d = (doy.wrapping_sub(153_i64.wrapping_mul(mp).wrapping_add(2) / 5).wrapping_add(1)) as u32;
    let m = if mp < 10 { mp.wrapping_add(3) } else { mp.wrapping_sub(9) } as u32;
    let y = if m <= 2 { y.wrapping_add(1) } else { y };
    (y, m, d)
}

/// Public helper for the API crate: turn a `<day>/<name>` path into the
/// absolute file, refusing any path traversal. Returns `None` when the
/// path escapes the root or doesn't exist.
pub fn resolve_for_serving(root: &Path, rel: &str) -> Option<PathBuf> {
    if rel.contains("..") || rel.starts_with('/') {
        return None;
    }
    // Reject anything that isn't `<day>/<file>`.
    let mut parts = rel.split('/');
    let day = parts.next()?;
    let file = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if !day.chars().all(|c| c.is_ascii_digit() || c == '-') {
        return None;
    }
    if !file
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
    {
        return None;
    }
    let p = root.join(day).join(file);
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

/// Fetch a `blob:` URL from inside the page. Works for URLs that aren't
/// reachable from outside the page (blob:, some signed URLs with
/// session-bound tokens).
pub async fn fetch_blob_from_page(page: &dyn uwa_core::Page, url: &str) -> Result<Vec<u8>> {
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
        u = serde_json::to_string(url).unwrap_or_else(|_| unreachable!()),
    );
    let v = page.eval(&js).await?;
    if v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
        let err = v.get("error").and_then(|x| x.as_str()).unwrap_or("unknown");
        return Err(uwa_core::UwaError::Transport(format!("fetch blob: {err}")));
    }
    let b64 = v
        .get("b64")
        .and_then(|x| x.as_str())
        .ok_or_else(|| uwa_core::UwaError::Transport("fetch blob: no b64".into()))?;
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| uwa_core::UwaError::Transport(format!("decode b64: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn resolve_rejects_traversal() {
        let root = std::path::Path::new("/tmp/uwa-media-test");
        assert!(resolve_for_serving(root, "../etc/passwd").is_none());
        assert!(resolve_for_serving(root, "/etc/passwd").is_none());
        assert!(resolve_for_serving(root, "2026-01-01/../../etc/passwd").is_none());
        assert!(resolve_for_serving(root, "not-a-day/x").is_none());
        assert!(resolve_for_serving(root, "2026-01-01/x/y").is_none());
    }

    #[test]
    fn extension_mapping() {
        assert_eq!(extension_for("image/png", MediaKind::Image), "png");
        assert_eq!(extension_for("image/jpeg", MediaKind::Image), "jpg");
        assert_eq!(extension_for("audio/webm", MediaKind::Audio), "webm");
        assert_eq!(
            extension_for("application/octet-stream", MediaKind::Image),
            "png"
        );
    }
}
