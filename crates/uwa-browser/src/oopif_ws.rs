//! Direct WebSocket connections to OOPIF targets.
//!
//! ## Why this exists
//!
//! Chromiumoxide 0.7.0 does not expose session-scoped CDP commands: when a
//! cross-origin iframe becomes an OOPIF (separate renderer process), the
//! `Target.attachedToTarget` event carries the parent page's sessionId and
//! is routed to that page's Target handler. A browser-level listener never
//! sees it, and `Browser::execute` always sends to the default session.
//!
//! The production workaround: connect a **separate WebSocket** directly to
//! the OOPIF's `webSocketDebuggerUrl` (obtained from the debug HTTP
//! endpoint `/json/list`). Each target has its own dedicated WebSocket;
//! connecting to it creates a fresh CDP session without any session-id
//! routing. We can then send any command — `addScriptToEvaluateOnNewDocument`
//! for stealth, `Runtime.evaluate` for JS, etc.
//!
//! ## Lifecycle
//!
//! ```text
//! pump_page detects Target.attachedToTarget (iframe)
//!   → OopifRegistry tracks the session
//!   → oopif_ws::inject_stealth(target_id, stealth) is spawned
//!     → HTTP GET /json/list → find webSocketDebuggerUrl
//!     → WebSocket connect → send addScriptToEvaluateOnNewDocument
//!     → WebSocket close (one-shot; reconnect on next need)
//! ```
//!
//! The WebSocket is one-shot: we connect, inject, disconnect. This is
//! cheap (a single TCP handshake) and avoids connection leaks.

use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;
use uwa_core::{Result, UwaError};
use uwa_stealth::StealthPack;

/// Resolve a target's `webSocketDebuggerUrl` from the debug HTTP endpoint.
///
/// `debug_url` is the base URL of the CDP debug port (e.g. `http://127.0.0.1:9222`).
/// Makes a raw HTTP/1.1 GET to `/json/list` and parses the JSON response.
pub async fn resolve_ws_url(debug_url: &str, target_id: &str) -> Result<String> {
    // Strip scheme, extract host:port.
    let stripped = debug_url
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/');
    let (host, port) = stripped
        .rsplit_once(':')
        .ok_or_else(|| UwaError::Transport(format!("bad debug_url `{debug_url}`")))?;

    // Minimal HTTP/1.1 GET over TCP.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut sock = tokio::net::TcpStream::connect((host, port.parse().unwrap_or(9222)))
        .await
        .map_err(|e| UwaError::Transport(format!("connect {host}:{port}: {e}")))?;
    let req =
        format!("GET /json/list HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
    sock.write_all(req.as_bytes())
        .await
        .map_err(|e| UwaError::Transport(format!("send /json/list: {e}")))?;

    let mut buf = Vec::new();
    sock.read_to_end(&mut buf)
        .await
        .map_err(|e| UwaError::Transport(format!("read /json/list: {e}")))?;

    // Split headers from body.
    let raw = String::from_utf8_lossy(&buf);
    let body = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or(&raw);

    let targets: Vec<Value> = serde_json::from_str(body)
        .map_err(|e| UwaError::Transport(format!("parse /json/list: {e}")))?;

    for t in &targets {
        if t.get("id").and_then(|v| v.as_str()) == Some(target_id) {
            if let Some(ws) = t.get("webSocketDebuggerUrl").and_then(|v| v.as_str()) {
                return Ok(ws.to_string());
            }
        }
    }

    Err(UwaError::Transport(format!(
        "target `{target_id}` not found in /json/list ({} targets)",
        targets.len()
    )))
}

/// Inject stealth scripts into an OOPIF target via its dedicated WebSocket.
///
/// Connects to `ws_url`, sends `Page.addScriptToEvaluateOnNewDocument` for
/// each `apply_before_load` script in the pack, then disconnects. The
/// OOPIF's JS will see the stealth patches from the very first execution.
///
/// If the target is paused (`waitForDebuggerOnStart`), also sends
/// `Runtime.runIfWaitingForDebugger` to resume it.
pub async fn inject_stealth(ws_url: &str, pack: &StealthPack) -> Result<()> {
    let (ws, _) = tokio_tungstenite::connect_async(ws_url)
        .await
        .map_err(|e| UwaError::Transport(format!("ws connect to OOPIF `{ws_url}`: {e}")))?;
    let (mut write, mut read) = ws.split();

    let mut next_id: u64 = 1;

    // 1. Enable Page domain (required for addScriptToEvaluateOnNewDocument).
    let id = next_id;
    next_id += 1;
    let cmd = json!({"id": id, "method": "Page.enable", "params": {}});
    write
        .send(Message::Text(cmd.to_string()))
        .await
        .map_err(|e| UwaError::Transport(format!("send Page.enable: {e}")))?;
    let _ = read_next_response(&mut read, id).await?;

    // 2. Inject stealth scripts (before the target resumes).
    for script in pack.scripts() {
        if !script.apply_before_load {
            continue;
        }
        let id = next_id;
        next_id += 1;
        let wrapped = format!("(function(){{try{{{}}}catch(_e){{}}}})();", script.js);
        let cmd = json!({
            "id": id,
            "method": "Page.addScriptToEvaluateOnNewDocument",
            "params": {"source": wrapped}
        });
        write
            .send(Message::Text(cmd.to_string()))
            .await
            .map_err(|e| {
                UwaError::Transport(format!(
                    "send addScriptToEvaluateOnNewDocument `{}`: {e}",
                    script.name
                ))
            })?;
        let _ = read_next_response(&mut read, id).await?;
        tracing::debug!(script = %script.name, "stealth injected into OOPIF");
    }

    // 3. Resume the target if it was paused waiting for the debugger.
    let id = next_id;
    let cmd = json!({"id": id, "method": "Runtime.runIfWaitingForDebugger", "params": {}});
    write
        .send(Message::Text(cmd.to_string()))
        .await
        .map_err(|e| UwaError::Transport(format!("send runIfWaitingForDebugger: {e}")))?;
    let _ = read_next_response(&mut read, id).await?;

    // One-shot: close the WebSocket. The scripts are already registered
    // and will fire on every new document load in this target.
    let _ = write.close().await;
    Ok(())
}

/// Read the next WebSocket message and verify it's the response for `id`.
async fn read_next_response<S>(stream: &mut S, id: u64) -> Result<Value>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let msg = stream
            .next()
            .await
            .ok_or_else(|| UwaError::Transport("OOPIF WebSocket closed".into()))?
            .map_err(|e| UwaError::Transport(format!("OOPIF ws read: {e}")))?;

        match msg {
            Message::Text(txt) => {
                let v: Value = serde_json::from_str(&txt)
                    .map_err(|e| UwaError::Transport(format!("parse OOPIF response: {e}")))?;
                if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                    if let Some(err) = v.get("error") {
                        return Err(UwaError::Transport(format!(
                            "OOPIF command {id} failed: {err}"
                        )));
                    }
                    return Ok(v);
                }
                // Skip events and out-of-order responses.
            }
            Message::Close(_) => {
                return Err(UwaError::Transport("OOPIF WebSocket closed".into()));
            }
            _ => {}
        }
    }
}

/// Convenience: resolve the OOPIF target's WS URL and inject stealth in one call.
///
/// Called from `pump_page` when an OOPIF target attaches. Spawned in a
/// background task so the event loop stays responsive.
pub async fn stealth_oopif(debug_url: &str, target_id: &str, pack: Arc<StealthPack>) -> Result<()> {
    let ws_url = resolve_ws_url(debug_url, target_id).await?;
    inject_stealth(&ws_url, &pack).await
}

#[cfg(test)]
mod tests {

    #[test]
    fn stealth_pack_scripts_have_names() {
        let pack = uwa_stealth::default_pack();
        for s in pack.scripts() {
            assert!(!s.name.is_empty(), "script must have a name");
        }
    }
}
