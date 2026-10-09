//! Multimodal attachments: extract from `MessageContent::Parts`, resolve
//! to local temp files, hand off to the file-attach path.
//!
//! Supported sources:
//! * `data:<mime>;base64,<payload>` — decoded inline
//! * `http(s)://...` — downloaded
//!
//! Anything else is logged and skipped — a bad image must not fail the
//! whole chat turn.

use std::path::PathBuf;
use uwa_core::types::openai::{ChatCompletionRequest, ContentPart, ImageUrl, MessageContent};
use uwa_core::types::Attachment;
use uwa_core::{Result, UwaError};

/// Extract all image attachments from a chat request.
pub async fn extract_attachments(req: &ChatCompletionRequest) -> Result<Vec<Attachment>> {
    let mut out = Vec::new();
    for msg in &req.messages {
        let Some(MessageContent::Parts(parts)) = &msg.content else {
            continue;
        };
        for part in parts {
            if let ContentPart::ImageUrl { image_url } = part {
                match decode_image(image_url).await {
                    Ok(a) => out.push(a),
                    Err(e) => {
                        tracing::warn!(url = %image_url.url, "attachment decode failed: {e}");
                    }
                }
            }
        }
    }
    Ok(out)
}

async fn decode_image(img: &ImageUrl) -> Result<Attachment> {
    let url = &img.url;

    // data URI
    if let Some(rest) = url.strip_prefix("data:") {
        let (meta, b64) = rest
            .split_once(',')
            .ok_or_else(|| UwaError::BadRequest("bad data URI: missing comma".into()))?;
        let mime = meta
            .strip_suffix(";base64")
            .unwrap_or(meta)
            .split(';')
            .next()
            .unwrap_or("image/png")
            .to_string();
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .map_err(|e| UwaError::BadRequest(format!("bad base64: {e}")))?;
        return write_temp(&bytes, &mime, "inline");
    }

    // http(s)
    if url.starts_with("http://") || url.starts_with("https://") {
        let resp = reqwest::get(url)
            .await
            .map_err(|e| UwaError::Transport(format!("fetch image: {e}")))?;
        let mime = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("image/png")
            .to_string();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| UwaError::Transport(format!("read image: {e}")))?;
        return write_temp(&bytes, &mime, "http");
    }

    Err(UwaError::BadRequest(format!(
        "unsupported image url scheme: {url}"
    )))
}

fn write_temp(bytes: &[u8], mime: &str, label: &str) -> Result<Attachment> {
    let ext = mime.strip_prefix("image/").unwrap_or("bin");
    let name = format!("uwa-{label}-{}.{ext}", uuid::Uuid::new_v4().simple());
    let path: PathBuf = std::env::temp_dir().join(&name);
    std::fs::write(&path, bytes).map_err(|e| UwaError::Internal(format!("write temp: {e}")))?;
    Ok(Attachment {
        path,
        name,
        mime: mime.to_string(),
        size: bytes.len() as u64,
    })
}

/// Remove temp files. Best-effort.
pub fn cleanup(attachments: &[Attachment]) {
    for a in attachments {
        let _ = std::fs::remove_file(&a.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uwa_core::types::openai::ChatMessage;
    use uwa_core::types::Role;

    fn req_with_image(url: &str) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gpt-4o".into(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: Some(MessageContent::Parts(vec![
                    ContentPart::Text {
                        text: "describe this".into(),
                    },
                    ContentPart::ImageUrl {
                        image_url: ImageUrl {
                            url: url.into(),
                            detail: None,
                        },
                    },
                ])),
                name: None,
                tool_call_id: None,
                tool_calls: None,
            }],
            stream: None,
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            user: None,
        }
    }

    #[tokio::test]
    async fn extracts_data_uri() {
        // 1x1 red PNG.
        let png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        let url = format!("data:image/png;base64,{png_b64}");
        let req = req_with_image(&url);
        let atts = extract_attachments(&req).await.unwrap();
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].mime, "image/png");
        assert!(atts[0].size > 0);
        assert!(atts[0].path.exists());
        cleanup(&atts);
        assert!(!atts[0].path.exists());
    }

    #[tokio::test]
    async fn text_only_request_yields_no_attachments() {
        let req = ChatCompletionRequest {
            model: "gpt-4o".into(),
            messages: vec![ChatMessage::text(Role::User, "hi")],
            stream: None,
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            user: None,
        };
        let atts = extract_attachments(&req).await.unwrap();
        assert!(atts.is_empty());
    }

    #[tokio::test]
    async fn bad_data_uri_errors_quietly() {
        let req = req_with_image("data:image/png;base64,not-real-base64!!!");
        let atts = extract_attachments(&req).await.unwrap();
        // We log and skip; no panic, empty result.
        assert!(atts.is_empty());
    }

    #[tokio::test]
    async fn unsupported_scheme_skipped() {
        let req = req_with_image("ftp://example.com/img.png");
        let atts = extract_attachments(&req).await.unwrap();
        assert!(atts.is_empty());
    }
}
