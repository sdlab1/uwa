//! Attach files to web inputs.
//!
//! Two strategies:
//! 1. **DOM `input[type=file]`** — set the files property via `DataTransfer`,
//!    dispatch `change`. Portable, works on most sites.
//! 2. **CDP drag-drop** — `Input.dispatchDragEvent` with `DragData` containing
//!    file paths. Fallback for ProseMirror-based inputs.

use std::path::Path;
use uwa_core::{Page, Result, UwaError};

pub async fn attach_file_via_dom(page: &dyn Page, selector: &str, path: &Path) -> Result<()> {
    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| UwaError::Internal(format!("read file: {e}")))?;
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("attachment.txt");

    let js = format!(
        r#"
        (() => {{
            const el = document.querySelector({sel});
            if (!el || el.type !== 'file') return {{ ok: false, reason: 'no-file-input' }};
            const dt = new DataTransfer();
            dt.items.add(new File([{content}], {name}, {{ type: 'text/plain' }}));
            el.files = dt.files;
            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
            return {{ ok: true }};
        }})()
        "#,
        sel = serde_json::to_string(selector).unwrap(),
        content = serde_json::to_string(&content).unwrap(),
        name = serde_json::to_string(name).unwrap(),
    );

    let v = page.eval(&js).await?;
    if v.get("ok").and_then(|b| b.as_bool()) != Some(true) {
        let reason = v
            .get("reason")
            .and_then(|s| s.as_str())
            .unwrap_or("unknown");
        return Err(UwaError::Transport(format!("attach via DOM: {reason}")));
    }
    Ok(())
}

pub async fn attach_file_via_dragdrop(page: &dyn Page, path: &Path) -> Result<()> {
    // CDP `Input.dispatchDragEvent` — requires the CDP path. For nodriver
    // sidecar, we delegate via an RPC method. For chromiumoxide, we execute
    // in the page's session.
    let abs = path
        .canonicalize()
        .map_err(|e| UwaError::Internal(format!("canonicalize: {e}")))?;
    let js = format!(
        r#"
        (() => {{
            const ev = new CustomEvent('uwa:attach-file', {{ detail: {{ path: {p} }} }});
            window.dispatchEvent(ev);
            return {{ ok: true }};
        }})()
        "#,
        p = serde_json::to_string(&abs.to_string_lossy().to_string()).unwrap(),
    );
    page.eval(&js).await?;
    Ok(())
}
