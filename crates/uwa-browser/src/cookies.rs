//! CDP cookie plumbing for `Page::export_cookies` /
//! `Page::import_cookies`.
//!
//! Cookies live in the browser context the target belongs to (created
//! via `Target.createBrowserContext`). `Network.getAllCookies` and
//! `Network.setCookies` operate on the context of the target whose
//! session we're issuing the command in, which is exactly the profile
//! the caller asked for.

use chromiumoxide::cdp::browser_protocol::network::{
    Cookie, CookieParam, CookieSameSite, SetCookiesParams, TimeSinceEpoch,
};
use chromiumoxide::types::{Command, Method, MethodId};
use chromiumoxide::Page as CdpPage;
use serde::{Deserialize, Serialize};
use uwa_core::{CookieRecord, Result, UwaError};

/// `Network.getAllCookies` — present in the DevTools protocol but not in
/// this crate's generated PDL, so it is issued as a custom command.
#[derive(Debug, Default, Serialize)]
struct GetAllCookiesParams {}

impl Method for GetAllCookiesParams {
    fn identifier(&self) -> MethodId {
        "Network.getAllCookies".into()
    }
}

#[derive(Debug, Deserialize)]
struct GetAllCookiesReturns {
    cookies: Vec<Cookie>,
}

impl Command for GetAllCookiesParams {
    type Response = GetAllCookiesReturns;
}

pub async fn export(page: &CdpPage) -> Result<Vec<CookieRecord>> {
    let resp = page
        .execute(GetAllCookiesParams::default())
        .await
        .map_err(|e| UwaError::Transport(format!("getAllCookies: {e}")))?;
    Ok(resp.result.cookies.iter().map(to_record).collect())
}

pub async fn import(page: &CdpPage, records: &[CookieRecord]) -> Result<usize> {
    if records.is_empty() {
        return Ok(0);
    }
    let params: Vec<CookieParam> = records.iter().map(to_param).collect();
    page.execute(SetCookiesParams::new(params))
        .await
        .map_err(|e| UwaError::Transport(format!("setCookies: {e}")))?;
    Ok(records.len())
}

fn to_record(c: &Cookie) -> CookieRecord {
    CookieRecord {
        name: c.name.clone(),
        value: c.value.clone(),
        domain: c.domain.clone(),
        path: c.path.clone(),
        // CDP returns `-1` for a session cookie.
        expires: if c.expires < 0.0 {
            None
        } else {
            Some(c.expires)
        },
        http_only: c.http_only,
        secure: c.secure,
        session: c.session,
        same_site: c.same_site.as_ref().map(|s| s.as_ref().to_string()),
    }
}

fn to_param(r: &CookieRecord) -> CookieParam {
    let mut p = CookieParam::new(r.name.clone(), r.value.clone());
    p.domain = Some(r.domain.clone());
    p.path = Some(r.path.clone());
    p.http_only = Some(r.http_only);
    p.secure = Some(r.secure);
    // No expiry = a session cookie, exactly like the record says.
    p.expires = r.expires.map(TimeSinceEpoch::new);
    p.same_site = r.same_site.as_deref().and_then(parse_same_site);
    p
}

fn parse_same_site(s: &str) -> Option<CookieSameSite> {
    use std::str::FromStr;
    CookieSameSite::from_str(s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chromiumoxide::cdp::browser_protocol::network::{CookiePriority, CookieSourceScheme};

    #[test]
    fn expires_negative_becomes_none() {
        let c = Cookie {
            name: "a".into(),
            value: "b".into(),
            domain: ".x".into(),
            path: "/".into(),
            expires: -1.0,
            size: 0,
            http_only: false,
            secure: false,
            session: true,
            same_site: None,
            priority: CookiePriority::Medium,
            source_scheme: CookieSourceScheme::Unset,
            source_port: 0,
            partition_key: None,
            partition_key_opaque: None,
        };
        let r = to_record(&c);
        assert!(r.expires.is_none());
        assert!(r.session);
    }

    #[test]
    fn same_site_roundtrips() {
        for s in ["Strict", "Lax", "None"] {
            let parsed = parse_same_site(s).unwrap_or_else(|| unreachable!("parse {s}"));
            assert_eq!(parsed.as_ref(), s);
        }
    }

    #[test]
    fn same_site_garbage_is_none() {
        assert!(parse_same_site("weird").is_none());
    }

    #[test]
    fn param_keeps_session_cookies_session() {
        let r = CookieRecord {
            name: "s".into(),
            value: "v".into(),
            domain: ".x".into(),
            path: "/".into(),
            expires: None,
            http_only: false,
            secure: false,
            session: true,
            same_site: None,
        };
        let p = to_param(&r);
        assert!(p.expires.is_none());
    }
}
