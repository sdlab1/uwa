//! Test-environment helpers: canonical Chromium endpoint resolution.

/// The canonical CDP endpoint for tests: an `http://` URL that
/// `CdpTransport::connect` resolves through `/json/version`.
pub const CANONICAL_CDP: &str = "http://127.0.0.1:9222";

/// Resolve the Chromium endpoint from `UWA_CHROMIUM_WS`, hardening against
/// the legacy bare `ws://host:port/devtools/browser` form (no browser GUID
/// → Chrome answers 404). Any such value is rewritten to the equivalent
/// `http://host:port`, which `connect` resolves properly.
///
/// This makes every e2e test immune to stale workflow env values.
pub fn chromium_ws_url() -> String {
    let raw = std::env::var("UWA_CHROMIUM_WS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| CANONICAL_CDP.to_string());
    normalize_cdp_url(&raw)
}

/// Rewrite a bare `ws://…/devtools/browser` (no GUID) to `http://host:port`.
/// Full debugger URLs (`ws://…/devtools/browser/<guid>`) pass through, as do
/// `http://` and anything else.
pub fn normalize_cdp_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if let Some(rest) = trimmed.strip_prefix("ws://") {
        if let Some(hostport) = rest.strip_suffix("/devtools/browser") {
            let hostport = hostport.trim_end_matches('/');
            if !hostport.contains("/devtools/browser/") {
                // Bare browser path without a GUID — not connectable.
                return format!("http://{hostport}");
            }
        }
    }
    raw.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_browser_path_is_rewritten() {
        assert_eq!(
            normalize_cdp_url("ws://127.0.0.1:9222/devtools/browser"),
            "http://127.0.0.1:9222"
        );
        assert_eq!(
            normalize_cdp_url("ws://localhost:9333/devtools/browser/"),
            "http://localhost:9333"
        );
    }

    #[test]
    fn full_debugger_url_passes_through() {
        let full = "ws://127.0.0.1:9222/devtools/browser/30c8508d-4e4f";
        assert_eq!(normalize_cdp_url(full), full);
    }

    #[test]
    fn http_passes_through() {
        assert_eq!(
            normalize_cdp_url("http://127.0.0.1:9222"),
            "http://127.0.0.1:9222"
        );
    }
}
