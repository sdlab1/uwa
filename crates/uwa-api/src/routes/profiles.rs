//! Profile-scoped endpoints: cookie export/import, login health.
//!
//! These are HTTP-layer wrappers around `Page::export_cookies` /
//! `Page::import_cookies`, plus a probe that answers "is this profile
//! still signed in to this provider?" by evaluating the provider's
//! `logged_in` / `logged_out` selectors on an already-open tab. Nothing
//! here touches `uwa.toml` or navigates a tab away from a live
//! conversation.

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

use uwa_core::{CookieExport, CookieRecord, TabId, UwaError};

use crate::error::ApiResult;
use crate::state::AppState;

// The `*`-glob the config layer already defines; reused so probe
// semantics can't drift from request routing.
use uwa_config::url_matches;

// ---------------------------------------------------------------------------
// cookies
// ---------------------------------------------------------------------------

/// `GET /admin/profiles/:name/cookies` - a JSON export of every cookie
/// in the profile's browser context.
///
/// `default` is allowed: it exports the browser's own context, which is
/// what a user who never created a named profile has.
pub async fn export_cookies(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<CookieExport>> {
    let tab = pick_tab_for_profile(&state, &name).await?;
    let page = state.transport.page(&tab).await?;
    let cookies = page.export_cookies().await?;
    Ok(Json(CookieExport {
        profile: name,
        exported_at: iso_now(),
        uwa_version: env!("CARGO_PKG_VERSION").into(),
        cookies,
    }))
}

/// `POST /admin/profiles/:name/cookies` - import cookies into a profile.
///
/// Accepts either a `CookieExport` (the shape the export writes) or a
/// bare array of `CookieRecord`. Replaces any cookie whose
/// `(name, domain, path)` triple matches; leaves others alone.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum CookieImportBody {
    Export(CookieExport),
    Array(Vec<CookieRecord>),
}

#[derive(Debug, Serialize)]
pub struct CookieImportResponse {
    pub profile: String,
    pub imported: usize,
}

pub async fn import_cookies(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<CookieImportBody>,
) -> ApiResult<Json<CookieImportResponse>> {
    let cookies: Vec<CookieRecord> = match body {
        CookieImportBody::Export(e) => e.cookies,
        CookieImportBody::Array(v) => v,
    };
    if cookies.is_empty() {
        return Err(UwaError::BadRequest("no cookies to import".into()).into());
    }

    let tab = pick_tab_for_profile(&state, &name).await?;
    let page = state.transport.page(&tab).await?;
    let imported = page.import_cookies(&cookies).await?;

    Ok(Json(CookieImportResponse {
        profile: name,
        imported,
    }))
}

// ---------------------------------------------------------------------------
// health
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ProviderHealth {
    pub provider: String,
    /// `"logged_in"` | `"logged_out"` | `"unknown"` | `"no_tab"` | `"error"`.
    pub status: &'static str,
    /// Tab the probe ran on, if one was found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab: Option<String>,
    /// Human-readable reason, when the status isn't obvious.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProfileHealth {
    pub profile: String,
    pub checked_at: String,
    pub providers: Vec<ProviderHealth>,
}

/// `GET /admin/profiles/:name/health` - probe every configured provider.
///
/// Non-destructive: only tabs that already match the provider's URL
/// patterns are evaluated. A provider with no open tab reports
/// `no_tab` - we don't navigate one to check, because doing so would
/// move a live conversation out from under the user.
pub async fn profile_health(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<ProfileHealth>> {
    let tabs = state.transport.list_tabs_in(&name).await?;

    // Snapshot tab URLs once, so the per-provider loop doesn't re-fetch.
    let mut tab_urls: Vec<(TabId, url::Url)> = Vec::with_capacity(tabs.len());
    for t in &tabs {
        if let Ok(page) = state.transport.page(t).await {
            if let Ok(u) = page.url().await {
                tab_urls.push((t.clone(), u));
            }
        }
    }

    let config = state.cfg();
    let mut names: Vec<&String> = config.providers.keys().collect();
    names.sort();

    let mut providers = Vec::new();
    for name_p in names {
        let Some(p) = config.providers.get(name_p) else {
            continue;
        };
        let matching = tab_urls
            .iter()
            .find(|(_, u)| p.url_patterns.iter().any(|pat| url_matches(pat, u)));

        let Some((tab, _)) = matching else {
            providers.push(ProviderHealth {
                provider: name_p.clone(),
                status: "no_tab",
                tab: None,
                note: Some("no open tab matches this provider's url_patterns".into()),
            });
            continue;
        };

        let logged_in_sel = p.selectors.logged_in.as_deref();
        let logged_out_sel = p.selectors.logged_out.as_deref();
        if logged_in_sel.is_none() && logged_out_sel.is_none() {
            providers.push(ProviderHealth {
                provider: name_p.clone(),
                status: "unknown",
                tab: Some(tab.to_string()),
                note: Some("no logged_in / logged_out selector configured".into()),
            });
            continue;
        }

        let page = match state.transport.page(tab).await {
            Ok(p) => p,
            Err(e) => {
                providers.push(ProviderHealth {
                    provider: name_p.clone(),
                    status: "error",
                    tab: Some(tab.to_string()),
                    note: Some(e.to_string()),
                });
                continue;
            }
        };

        let logged_out_present = match logged_out_sel {
            Some(s) => exists(page.as_ref(), s).await.unwrap_or(false),
            None => false,
        };
        let logged_in_present = match logged_in_sel {
            Some(s) => exists(page.as_ref(), s).await.unwrap_or(false),
            None => false,
        };

        // Priority: an explicit "logged_out" match beats everything - a
        // stale user menu can coexist with a "session expired" banner.
        // Then an explicit "logged_in" match. Then unknown.
        let (status, note) = if logged_out_present {
            ("logged_out", Some("logged_out selector matched".into()))
        } else if logged_in_present {
            ("logged_in", None)
        } else {
            ("unknown", Some("neither selector matched".into()))
        };

        providers.push(ProviderHealth {
            provider: name_p.clone(),
            status,
            tab: Some(tab.to_string()),
            note,
        });
    }

    Ok(Json(ProfileHealth {
        profile: name,
        checked_at: iso_now(),
        providers,
    }))
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Pick a tab in the profile to run a command on, opening a blank one if
/// the profile has no tabs yet. The `default` profile doesn't support
/// `open_tab`, so if it has no tabs we surface a clear error rather than
/// a silent no-op.
async fn pick_tab_for_profile(state: &AppState, profile: &str) -> Result<TabId, UwaError> {
    let tabs = state.transport.list_tabs_in(profile).await?;
    if let Some(t) = tabs.into_iter().next() {
        return Ok(t);
    }
    if profile == uwa_session::ProfileName::DEFAULT {
        return Err(UwaError::Unavailable(
            "the default profile has no tabs; open one in Chromium first".into(),
        ));
    }
    if !state.transport.supports_profiles() {
        return Err(UwaError::Config(
            "this transport does not support named profiles".into(),
        ));
    }
    state.transport.open_tab(profile, "about:blank").await
}

/// Selector-presence probe; the JS mirrors `uwa_providers::input::exists`
/// (kept local so the API layer doesn't grow a providers dependency).
async fn exists(page: &dyn uwa_core::Page, selector: &str) -> Result<bool, UwaError> {
    let js = format!(
        "!!document.querySelector({})",
        serde_json::to_string(selector).map_err(|e| UwaError::Internal(e.to_string()))?
    );
    Ok(page.eval(&js).await?.as_bool().unwrap_or(false))
}

/// RFC3339 UTC timestamp without a date crate: days-to-civil math from
/// Howard Hinnant's `chrono` notes.
fn iso_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (y, m, d) = civil(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn civil(days: u64) -> (i64, u32, u32) {
    // Howard Hinnant's civil_from_days; all arithmetic saturates — values are
    // small (days since Unix epoch) so saturating never triggers in practice.
    let z = i64::try_from(days)
        .unwrap_or(i64::MAX)
        .saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe
        .saturating_sub(doe / 1460)
        .saturating_add(doe / 36_524)
        .saturating_sub(doe / 146_096))
        / 365;
    let y = yoe.saturating_add(era.saturating_mul(400));
    let doy = doe.saturating_sub(
        365_i64
            .saturating_mul(yoe)
            .saturating_add(yoe / 4)
            .saturating_sub(yoe / 100),
    );
    let mp = 5_i64.saturating_mul(doy).saturating_add(2) / 153;
    let d = (doy
        .saturating_sub(153_i64.saturating_mul(mp).saturating_add(2) / 5)
        .saturating_add(1)) as u32;
    let m = (if mp < 10 {
        mp.saturating_add(3)
    } else {
        mp.saturating_sub(9)
    }) as u32;
    let y = if m <= 2 { y.saturating_add(1) } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_now_shape() {
        let s = iso_now();
        // 2026-10-17T09:43:00Z - fixed-width, RFC3339, UTC.
        assert_eq!(s.len(), 20, "got {s}");
        let bytes: Vec<u8> = s.as_bytes().to_vec();
        let is_dash = |i: usize| bytes.get(i) == Some(&b'-');
        let is_colon = |i: usize| bytes.get(i) == Some(&b':');
        assert!(is_dash(4) && is_dash(7) && bytes.get(10) == Some(&b'T'));
        assert!(is_colon(13) && is_colon(16) && s.ends_with('Z'));
    }

    #[test]
    fn civil_matches_known_date() {
        // 2026-10-17 is day 20743 since the Unix epoch (1970-01-01).
        let (y, m, d) = civil(20_743);
        assert_eq!((y, m, d), (2026, 10, 17));
    }
}
