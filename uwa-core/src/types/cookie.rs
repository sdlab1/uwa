//! Portable cookie record — the shape `uwa profiles export` writes and
//! `uwa profiles import` reads.
//!
//! Deliberately not Chromium's own `Cookie` struct: we want a stable
//! wire format that a future Chromium version can't break, and we want
//! to be able to diff two exports without wading through `size` or
//! `priority` fields nobody set on purpose.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CookieRecord {
    pub name: String,
    pub value: String,
    /// Leading dot means "any subdomain"; CDP reflects exactly what the
    /// store holds.
    pub domain: String,
    pub path: String,
    /// Unix seconds. Absent for a session cookie.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<f64>,
    #[serde(default)]
    pub http_only: bool,
    #[serde(default)]
    pub secure: bool,
    /// True when the cookie has no expiry — the browser drops it at
    /// shutdown. Recorded for round-trip fidelity.
    #[serde(default)]
    pub session: bool,
    /// `"Strict"` | `"Lax"` | `"None"`. Chromium omits this for cookies
    /// created before SameSite existed; kept as `None` in that case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_site: Option<String>,
}

/// A snapshot of a browser profile's cookies, as written to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CookieExport {
    /// Free-form, for humans.
    pub profile: String,
    pub exported_at: String,
    /// `uwa --version` at export time.
    pub uwa_version: String,
    pub cookies: Vec<CookieRecord>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_roundtrips() {
        let r = CookieRecord {
            name: "__Secure-next-auth".into(),
            value: "abc".into(),
            domain: ".chatgpt.com".into(),
            path: "/".into(),
            expires: Some(1_800_000_000.0),
            http_only: true,
            secure: true,
            session: false,
            same_site: Some("Lax".into()),
        };
        let s = serde_json::to_string(&r).unwrap_or_default();
        let back: CookieRecord =
            serde_json::from_str(&s).unwrap_or_else(|e| unreachable!("roundtrip: {e}"));
        assert_eq!(back.name, r.name);
        assert_eq!(back.expires, r.expires);
    }

    #[test]
    fn session_cookie_has_no_expires_field() {
        let r = CookieRecord {
            name: "x".into(),
            value: "y".into(),
            domain: "x.test".into(),
            path: "/".into(),
            expires: None,
            http_only: false,
            secure: false,
            session: true,
            same_site: None,
        };
        let s = serde_json::to_string(&r).unwrap_or_default();
        assert!(!s.contains("expires"));
        assert!(!s.contains("same_site"));
    }

    #[test]
    fn export_roundtrips() {
        let e = CookieExport {
            profile: "work".into(),
            exported_at: "2026-01-15T00:00:00Z".into(),
            uwa_version: "0.1.0".into(),
            cookies: vec![],
        };
        let s = serde_json::to_string(&e).unwrap_or_default();
        let back: CookieExport =
            serde_json::from_str(&s).unwrap_or_else(|er| unreachable!("roundtrip: {er}"));
        assert_eq!(back.profile, "work");
    }
}
