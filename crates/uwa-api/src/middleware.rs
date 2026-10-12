//! Auth + request-id middleware.

use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;
use subtle::ConstantTimeEq;
use uwa_core::{RequestId, UwaError};

use crate::error::ApiError;
use crate::state::AppState;

pub async fn inject_request_id(mut req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(RequestId::from_raw)
        .unwrap_or_default();
    req.extensions_mut().insert(id.clone());
    let span = tracing::info_span!("http", request_id = %id);
    let _g = span.enter();
    next.run(req).await
}

pub async fn require_api_key(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    // Loopback: the socket isn't reachable off-box, so we accept any key.
    // A local client (Cursor, Continue, curl) can use a placeholder string
    // and never has to match `[server].api_key`. This is the #1 cause of
    // "Authorization: Bearer dummy-key" failures in the wild.
    if is_loopback_bind(&state.config.server.bind) {
        return Ok(next.run(req).await);
    }

    let Some(expected) = state.config.server.api_key.as_deref() else {
        return Ok(next.run(req).await);
    };
    // 1) Authorization: Bearer <key>  2) ?api_key=<key> (EventSource can't
    // set headers, so the UI passes the key in the query string).
    let provided = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::trim)
        .map(str::to_string)
        .or_else(|| {
            let q = req.uri().query()?;
            for pair in q.split('&') {
                let mut it = pair.splitn(2, '=');
                let k = it.next().unwrap_or_default();
                if k == "api_key" {
                    return it.next().map(urldecode).and_then(|v| percent_decode(&v));
                }
            }
            None
        });
    let ok = match provided.as_deref() {
        Some(p) => p.as_bytes().ct_eq(expected.as_bytes()).into(),
        None => false,
    };
    if !ok {
        return Err(UwaError::Unauthorized.into());
    }
    Ok(next.run(req).await)
}

fn urldecode(s: &str) -> String {
    s.replace("%20", " ")
        .replace("%2F", "/")
        .replace("%3A", ":")
}

/// Minimal percent-decoder for the api_key query param. Returns None on
/// invalid input.
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut hex_iter = bytes.iter().copied();
    while let Some(b) = hex_iter.next() {
        if b == b'%' {
            // Two hex digits must follow, else the escape is malformed.
            let hex = [hex_iter.next()?, hex_iter.next()?];
            let hex_str = std::str::from_utf8(&hex).ok()?;
            let v = u8::from_str_radix(hex_str, 16).ok()?;
            out.push(v);
        } else {
            out.push(b);
        }
    }
    String::from_utf8(out).ok()
}

fn is_loopback_bind(bind: &str) -> bool {
    matches!(
        bind,
        "127.0.0.1" | "::1" | "localhost" | "0:0:0:0:0:0:0:1" | "127.0.0.0/8"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_loopback_binds() {
        for b in ["127.0.0.1", "::1", "localhost", "0:0:0:0:0:0:0:1"] {
            assert!(is_loopback_bind(b), "{b}");
        }
        for b in ["0.0.0.0", "192.168.1.5", "example.com", ""] {
            assert!(!is_loopback_bind(b), "{b}");
        }
    }
}
