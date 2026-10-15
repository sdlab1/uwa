//! Auth + request-id middleware.

use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, HOST, ORIGIN};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use subtle::ConstantTimeEq;
use tracing::Instrument;
use uwa_core::{RequestId, UwaError};

use crate::error::ApiError;
use crate::state::AppState;

/// Longest client-supplied `x-request-id` we echo into logs and responses.
const MAX_REQUEST_ID_LEN: usize = 128;

/// A client-supplied request id is only trusted when it is short and made of
/// harmless characters; anything else (log-injection fodder, 8 KB blobs) is
/// replaced by a generated id.
fn sanitize_request_id(raw: &str) -> Option<&str> {
    let ok = !raw.is_empty()
        && raw.len() <= MAX_REQUEST_ID_LEN
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'));
    ok.then_some(raw)
}

pub async fn inject_request_id(mut req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .and_then(sanitize_request_id)
        .map(RequestId::from_raw)
        .unwrap_or_default();
    req.extensions_mut().insert(id.clone());
    let span = tracing::info_span!("http", request_id = %id);
    // `span.enter()` held across an `.await` attaches the span to whatever
    // task the worker thread polls next; `instrument` scopes it to this
    // future only.
    next.run(req).instrument(span).await
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
    // Accepted credentials, in order:
    //  1) `Authorization: Bearer <key>` (scheme is case-insensitive, RFC 7235)
    //  2) `x-api-key: <key>` - what the Anthropic SDK / Claude Code send
    //  3) `?api_key=<key>` - EventSource cannot set headers, so the UI
    //     passes the key in the query string.
    let provided = bearer_token(req.headers())
        .or_else(|| {
            req.headers()
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
        })
        .map(str::to_string)
        .or_else(|| {
            req.uri().query().and_then(|q| {
                q.split('&').find_map(|pair| {
                    let (k, v) = pair.split_once('=')?;
                    (k == "api_key").then(|| percent_decode(v)).flatten()
                })
            })
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

/// Extract the token of an `Authorization: Bearer <token>` header.
fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let v = headers.get(AUTHORIZATION)?.to_str().ok()?.trim();
    let (scheme, rest) = v.split_once(char::is_whitespace)?;
    scheme.eq_ignore_ascii_case("bearer").then(|| rest.trim())
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

pub(crate) fn is_loopback_bind(bind: &str) -> bool {
    let host = bind.trim().trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Is `authority` (`host[:port]`, as in a `Host` header) a loopback name?
fn is_loopback_authority(authority: &str) -> bool {
    url::Url::parse(&format!("http://{authority}"))
        .ok()
        .and_then(|u| u.host().map(|h| host_is_loopback(&h)))
        .unwrap_or(false)
}

/// Is `origin` (`scheme://host[:port]`, as in an `Origin` header) loopback?
pub(crate) fn is_loopback_origin(origin: &[u8]) -> bool {
    let Some(url) = std::str::from_utf8(origin)
        .ok()
        .and_then(|o| url::Url::parse(o).ok())
    else {
        return false; // includes the opaque `null` origin
    };
    matches!(url.scheme(), "http" | "https") && url.host().is_some_and(|h| host_is_loopback(&h))
}

fn host_is_loopback(h: &url::Host<&str>) -> bool {
    match h {
        url::Host::Domain(d) => d.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(ip) => ip.is_loopback(),
        url::Host::Ipv6(ip) => ip.is_loopback(),
    }
}

fn forbidden(code: &'static str, message: &'static str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({
            "error": { "message": message, "type": "invalid_request_error", "code": code }
        })),
    )
        .into_response()
}

/// Browser-origin guard for loopback binds.
///
/// A loopback daemon skips API-key checks (see `require_api_key`), but
/// "the socket is not reachable off-box" does not mean "only trusted code can
/// reach it": any web page open in the user's browser can `fetch()`
/// `http://127.0.0.1:<port>` (CSRF), and a DNS-rebinding page can even read the
/// answer same-origin. Both would let a random site drive the logged-in LLM
/// sessions, read request history and probe tabs via `/admin/selector-test`.
///
/// Non-browser clients (curl, SDKs, Cursor's backend) send no `Origin`
/// header and a loopback `Host`, so they are unaffected.
pub async fn guard_browser_origin(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    if !is_loopback_bind(&state.config.server.bind) {
        return next.run(req).await;
    }
    if let Some(host) = req.headers().get(HOST).and_then(|v| v.to_str().ok()) {
        if !is_loopback_authority(host) {
            return forbidden(
                "forbidden_host",
                "request Host is not a loopback address (DNS rebinding protection)",
            );
        }
    }
    if let Some(origin) = req.headers().get(ORIGIN) {
        if !is_loopback_origin(origin.as_bytes()) {
            return forbidden(
                "forbidden_origin",
                "cross-origin browser requests are not allowed on a loopback-bound uwa",
            );
        }
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn recognizes_loopback_binds() {
        for b in [
            "127.0.0.1",
            "127.0.0.2",
            "::1",
            "[::1]",
            "localhost",
            "LOCALHOST",
            "0:0:0:0:0:0:0:1",
        ] {
            assert!(is_loopback_bind(b), "{b}");
        }
        for b in [
            "0.0.0.0",
            "::",
            "192.168.1.5",
            "example.com",
            "",
            "127.0.0.1.evil.com",
        ] {
            assert!(!is_loopback_bind(b), "{b}");
        }
    }

    #[test]
    fn loopback_origins_are_recognized() {
        for o in [
            "http://127.0.0.1:8080",
            "http://localhost:3000",
            "https://localhost",
            "http://[::1]:8080",
        ] {
            assert!(is_loopback_origin(o.as_bytes()), "{o}");
        }
        for o in [
            "https://evil.example",
            "http://127.0.0.1.evil.example",
            "http://localhost.evil.example",
            "http://evil.example/127.0.0.1",
            "null",
            "file://",
            "ftp://127.0.0.1",
            "",
        ] {
            assert!(!is_loopback_origin(o.as_bytes()), "{o}");
        }
        assert!(
            !is_loopback_origin(&[0xff, 0xfe]),
            "non-UTF-8 must not pass"
        );
    }

    #[test]
    fn host_header_rebinding_is_rejected() {
        for h in ["127.0.0.1:8080", "localhost", "[::1]:8080", "LocalHost:1"] {
            assert!(is_loopback_authority(h), "{h}");
        }
        for h in [
            "evil.example",
            "evil.example:8080",
            "127.0.0.1@evil.example",
            "127.0.0.1.evil.example",
            "",
        ] {
            assert!(!is_loopback_authority(h), "{h}");
        }
    }

    fn headers(pairs: &[(&'static str, &str)]) -> Result<HeaderMap, Box<dyn std::error::Error>> {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.insert(*k, HeaderValue::from_str(v)?);
        }
        Ok(m)
    }

    #[test]
    fn bearer_scheme_is_case_insensitive() -> Result<(), Box<dyn std::error::Error>> {
        for v in ["Bearer k", "bearer k", "BEARER   k ", "Bearer\tk"] {
            let h = headers(&[("authorization", v)])?;
            assert_eq!(bearer_token(&h), Some("k"), "{v:?}");
        }
        for v in ["Basic k", "Bearerk", "k", ""] {
            let h = headers(&[("authorization", v)])?;
            assert_eq!(bearer_token(&h), None, "{v:?}");
        }
        assert_eq!(bearer_token(&HeaderMap::new()), None);
        Ok(())
    }

    #[test]
    fn percent_decode_handles_escapes_and_garbage() {
        assert_eq!(percent_decode("a%20b%2Fc").as_deref(), Some("a b/c"));
        assert_eq!(percent_decode("plain").as_deref(), Some("plain"));
        // A double-encoded value is decoded exactly once.
        assert_eq!(percent_decode("%252F").as_deref(), Some("%2F"));
        assert_eq!(percent_decode("%2"), None, "truncated escape");
        assert_eq!(percent_decode("%zz"), None, "non-hex escape");
        assert_eq!(percent_decode("%ff"), None, "invalid UTF-8");
    }

    #[test]
    fn percent_decode_does_not_treat_plus_as_space() {
        assert_eq!(percent_decode("a+b").as_deref(), Some("a+b"));
    }

    #[test]
    fn request_id_sanitizer_rejects_junk() {
        assert_eq!(sanitize_request_id("req_abc-1.2:3"), Some("req_abc-1.2:3"));
        assert_eq!(sanitize_request_id(""), None);
        assert_eq!(sanitize_request_id("a b"), None);
        assert_eq!(sanitize_request_id("a\"b"), None);
        assert_eq!(sanitize_request_id("é"), None);
        assert_eq!(
            sanitize_request_id(&"x".repeat(MAX_REQUEST_ID_LEN)).map(str::len),
            Some(MAX_REQUEST_ID_LEN)
        );
        assert_eq!(
            sanitize_request_id(&"x".repeat(MAX_REQUEST_ID_LEN + 1)),
            None
        );
    }
}
