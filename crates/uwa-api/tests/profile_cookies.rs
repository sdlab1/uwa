//! Cookie export/import and login health over HTTP.

use serde_json::{json, Value};
use std::sync::Arc;
use uwa_core::{CookieRecord, TabId};
use uwa_testkit::{config::TEST_AUTH_HEADER, AppBuilder, MockPage, MockProvider, MockTransport};

fn cookie(name: &str, value: &str) -> CookieRecord {
    CookieRecord {
        name: name.into(),
        value: value.into(),
        domain: ".chatgpt.com".into(),
        path: "/".into(),
        expires: Some(1_900_000_000.0),
        http_only: true,
        secure: true,
        session: false,
        same_site: Some("Lax".into()),
    }
}

/// A page that answers `export_cookies` with two records and records
/// every `import_cookies` call.
fn cookie_page() -> MockPage {
    MockPage::new()
        .with_url("https://chatgpt.com/")
        .expect_default(json!(true))
        .with_cookies(vec![cookie("session", "abc"), cookie("csrf", "xyz")])
}

fn app_with_profile() -> (axum_test::TestServer, Arc<MockPage>) {
    let page = Arc::new(cookie_page());
    let t = MockTransport::new().with_profiles().with_page_in(
        "work",
        TabId::from_raw("t_work"),
        page.clone(),
    );

    let cfg = uwa_config::Config::load_from_str(
        r##"
            [server]
            bind = "127.0.0.1"
            port = 8080
            api_key = "k"

            [model_aliases]
            "gpt-4o" = "chatgpt"

            [providers.chatgpt]
            name = "chatgpt"
            url_patterns = ["https://chatgpt.com/*"]
            capabilities = { streams = true, tool_calls = false, vision = false }
            [providers.chatgpt.selectors]
            input = "#i"
            send_button = "#s"
            assistant_message = "#a"
            logged_in = "[data-testid='user-menu']"
            logged_out = "a[href='/auth/login']"

            [profiles.work]
            label = "Work"
        "##,
    )
    .unwrap_or_else(|e| {
        eprintln!("config parse error: {e}");
        std::process::exit(1);
    });
    let cfg = Arc::new(cfg);

    let app = AppBuilder::new()
        .with_config(cfg)
        .with_transport(Arc::new(t))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("ok")))
        .build();
    (app.server, page)
}

#[tokio::test]
async fn export_returns_the_pages_cookies() {
    let (s, _page) = app_with_profile();
    let r = s
        .get("/admin/profiles/work/cookies")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(
        v.get("profile")
            .and_then(|v| v.as_str())
            .unwrap_or_default(),
        "work"
    );
    let cookies = v
        .get("cookies")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(cookies.len(), 2);
    assert_eq!(
        cookies
            .first()
            .and_then(|c| c.get("name").and_then(|n| n.as_str()))
            .unwrap_or_default(),
        "session"
    );
    assert_eq!(
        cookies
            .first()
            .and_then(|c| c.get("domain").and_then(|d| d.as_str()))
            .unwrap_or_default(),
        ".chatgpt.com"
    );
}

#[tokio::test]
async fn import_accepts_a_full_export_or_a_bare_array() {
    let (s, page) = app_with_profile();

    // Bare array.
    let r = s
        .post("/admin/profiles/work/cookies")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!([cookie("one", "1"), cookie("two", "2")]))
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(v.get("imported").and_then(|v| v.as_u64()).unwrap_or(0), 2);

    // Full export shape.
    let r = s
        .post("/admin/profiles/work/cookies")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "profile": "work",
            "exported_at": "2026-01-01T00:00:00Z",
            "uwa_version": "0.1.0",
            "cookies": [cookie("three", "3")]
        }))
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(v.get("imported").and_then(|v| v.as_u64()).unwrap_or(0), 1);

    // Both calls reached the page.
    let log = page.log();
    assert!(log.iter().filter(|l| l.contains("import_cookies")).count() >= 2);
}

#[tokio::test]
async fn import_rejects_empty_body() {
    let (s, _page) = app_with_profile();
    let r = s
        .post("/admin/profiles/work/cookies")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!([]))
        .await;
    r.assert_status_bad_request();
    let v: Value = r.json();
    assert!(v
        .get("error")
        .and_then(|v| v.get("message").and_then(|m| m.as_str()))
        .unwrap_or_default()
        .contains("no cookies"));
}

#[tokio::test]
async fn export_on_unknown_profile_opens_a_blank_tab() {
    // `personal` is not in the config. The cookie endpoint doesn't
    // validate profile membership: opening an empty tab is the right
    // behavior, so an admin can seed a fresh profile before it exists
    // in the running config.
    let (s, _page) = app_with_profile();
    let r = s
        .get("/admin/profiles/personal/cookies")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    // Without a tab in that profile, the picker opens a blank one and
    // export runs against it.
    r.assert_status_ok();
}

#[tokio::test]
async fn health_probes_every_provider() {
    let (s, _page) = app_with_profile();
    // The MockPage answers `querySelector` with `true` by default, so
    // both `logged_in` and `logged_out` would match. The probe
    // prioritizes `logged_out`, so we expect `logged_out`.
    let r = s
        .get("/admin/profiles/work/health")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(
        v.get("profile")
            .and_then(|v| v.as_str())
            .unwrap_or_default(),
        "work"
    );
    let providers = v
        .get("providers")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(providers.len(), 1);
    assert_eq!(
        providers
            .first()
            .and_then(|p| p.get("provider").and_then(|v| v.as_str()))
            .unwrap_or_default(),
        "chatgpt"
    );
    assert_eq!(
        providers
            .first()
            .and_then(|p| p.get("status").and_then(|v| v.as_str()))
            .unwrap_or_default(),
        "logged_out"
    );
    assert!(providers
        .first()
        .and_then(|p| p.get("tab").and_then(|v| v.as_str()))
        .unwrap_or_default()
        .contains("t_work"));
}

#[tokio::test]
async fn health_reports_no_tab_when_nothing_matches() {
    // Build a config with a provider whose URL doesn't match the tab.
    let page = Arc::new(cookie_page()); // URL = https://chatgpt.com/
    let t =
        MockTransport::new()
            .with_profiles()
            .with_page_in("work", TabId::from_raw("t_work"), page);

    let cfg = uwa_config::Config::load_from_str(
        r##"
            [server]
            bind = "127.0.0.1"
            port = 8080
            api_key = "k"

            [model_aliases]
            "m" = "other"

            [providers.other]
            name = "other"
            url_patterns = ["https://claude.ai/*"]
            capabilities = { streams = true, tool_calls = false, vision = false }
            [providers.other.selectors]
            input = "#i"
            send_button = "#s"
            assistant_message = "#a"
            logged_in = "[data-testid='user-menu']"

            [profiles.work]
            label = "Work"
        "##,
    )
    .unwrap_or_else(|e| {
        eprintln!("config parse error: {e}");
        std::process::exit(1);
    });
    let cfg = Arc::new(cfg);

    let app = AppBuilder::new()
        .with_config(cfg)
        .with_transport(Arc::new(t))
        .with_provider(Arc::new(MockProvider::new("other").with_answer("ok")))
        .build();

    let r = app
        .server
        .get("/admin/profiles/work/health")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    let p = v
        .get("providers")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        p.get("status").and_then(|v| v.as_str()).unwrap_or_default(),
        "no_tab"
    );
    assert!(p
        .get("note")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .contains("url_patterns"));
}

#[tokio::test]
async fn health_reports_unknown_without_selectors() {
    // Provider has no `logged_in` / `logged_out`. Status is `unknown`,
    // not an error — the operator simply hasn't configured the probe.
    let page = Arc::new(cookie_page());
    let t =
        MockTransport::new()
            .with_profiles()
            .with_page_in("work", TabId::from_raw("t_work"), page);

    let cfg = uwa_config::Config::load_from_str(
        r##"
            [server]
            bind = "127.0.0.1"
            port = 8080
            api_key = "k"

            [model_aliases]
            "gpt-4o" = "chatgpt"

            [providers.chatgpt]
            name = "chatgpt"
            url_patterns = ["https://chatgpt.com/*"]
            capabilities = { streams = true, tool_calls = false, vision = false }
            [providers.chatgpt.selectors]
            input = "#i"
            send_button = "#s"
            assistant_message = "#a"

            [profiles.work]
            label = "Work"
        "##,
    )
    .unwrap_or_else(|e| {
        eprintln!("config parse error: {e}");
        std::process::exit(1);
    });
    let cfg = Arc::new(cfg);

    let app = AppBuilder::new()
        .with_config(cfg)
        .with_transport(Arc::new(t))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("ok")))
        .build();

    let r = app
        .server
        .get("/admin/profiles/work/health")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(
        v.get("providers")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|p| p.get("status").and_then(|v| v.as_str()))
            .unwrap_or_default(),
        "unknown"
    );
}
