//! # uwa-api
//!
//! HTTP surface: OpenAI- and Anthropic-compatible endpoints over a `SiteProvider`.
//!
//! ## Passport (public API)
//! - [`router`] — builds an `axum::Router`
//! - [`AppState`], [`ProviderRegistry`]
//! - [`ApiError`] — the OpenAI-shaped error at the HTTP edge

pub mod error;
pub mod history;
pub mod metrics;
pub mod middleware;
pub mod routes;
pub mod routing;
pub mod state;

pub use error::ApiError;
pub use state::{AppState, ProviderRegistry};

use axum::routing::{get, post};
use axum::Router;
use std::time::Duration;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

/// Resolve the UI directory: `UWA_STATIC_DIR` wins, else the crate's own
/// `static/` at build time (works for `cargo run` and the built binary in
/// the repo).
fn static_dir() -> std::path::PathBuf {
    std::env::var("UWA_STATIC_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static"))
}

/// UI auto-build state: 0=not attempted, 1=building, 2=ok, 3=failed.
static UI_BUILD_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Run the UI bundle in the background so a fresh checkout serves a working
/// dashboard without a manual `build.sh` step. One-shot: guarded by
/// `UI_BUILD_STATE`, so concurrent requests don't spawn parallel npm runs.
/// The script path is resolved by the caller — the spawned task must not
/// re-read `UWA_STATIC_DIR` (the env can change under tests).
async fn run_ui_build(script: std::path::PathBuf) -> Result<(), String> {
    if !script.exists() {
        return Err(format!("build script not found at {}", script.display()));
    }
    let cwd = script
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    // First run does an `npm install`; be generous, then kill a hung build.
    let out = tokio::time::timeout(
        Duration::from_secs(600),
        tokio::process::Command::new("bash")
            .arg(&script)
            .current_dir(cwd)
            .output(),
    )
    .await
    .map_err(|_| "timed out after 600s".to_string())?
    .map_err(|e| format!("spawn failed: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "build.sh failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Kick off exactly one background `build.sh` when the UI bundle is missing
/// but the sources (build.sh) are present — the fresh-clone case.
/// `UI_BUILD_STATE` (0=none, 1=building, 2=ok, 3=failed) prevents parallel
/// npm runs; a failed build is reported to the log and stays failed until
/// restart.
async fn maybe_start_ui_build(root: &std::path::Path) {
    use std::sync::atomic::Ordering;

    if root.join("dist").join("main.js").exists() {
        return;
    }
    let script = root.join("build.sh");
    if !script.exists() {
        return;
    }
    if UI_BUILD_STATE
        .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return; // already building / already done once this process
    }
    tokio::spawn(async move {
        match run_ui_build(script).await {
            Ok(()) => {
                tracing::info!("uwa-ui: auto-build finished, dashboard bundle ready");
                UI_BUILD_STATE.store(2, Ordering::SeqCst);
            }
            Err(err) => {
                tracing::warn!("uwa-ui: auto-build failed: {err}");
                UI_BUILD_STATE.store(3, Ordering::SeqCst);
            }
        }
    });
}

async fn serve_index() -> axum::response::Response {
    use axum::http::header::CONTENT_TYPE;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    let root = static_dir();
    let path = root.join("index.html");
    match tokio::fs::read_to_string(&path).await {
        Ok(html) => {
            // Fresh clone: page committed, bundle missing → self-heal in the
            // background and serve the page now (its boot banner covers the
            // gap until the bundle appears; a reload picks it up).
            maybe_start_ui_build(&root).await;
            ([(CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Auto-build only makes sense when the build script ships with
            // the static dir; otherwise answer immediately with the hint.
            let script = static_dir().join("build.sh");
            if !script.exists() {
                return (
                    StatusCode::NOT_FOUND,
                    format!(
                        "uwa-ui: index.html not found at {}: {e}\n\
                         and no build.sh next to it — cannot auto-build.\n\
                         Set UWA_STATIC_DIR to a directory containing the built UI.",
                        path.display()
                    ),
                )
                    .into_response();
            }
            // Auto-build can still produce dist/, but never index.html —
            // start it and tell the client to reload.
            maybe_start_ui_build(&root).await;
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "uwa-ui: dashboard is not built yet — auto-build started in the \
                 background (npm install + esbuild). Reload this page in a minute.\n\
                 Manual: crates/uwa-api/static/build.sh\n",
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            format!(
                "uwa-ui: index.html not found at {}: {e}\n\
                 (build the UI with `crates/uwa-api/static/build.sh`)",
                path.display()
            ),
        )
            .into_response(),
    }
}

pub fn router(state: AppState) -> Router {
    let api_plain = Router::new()
        .route("/v1/models", get(routes::models::list_models))
        .route("/v1/chat/completions", post(routes::chat::chat_completions))
        .route("/v1/messages", post(routes::messages::messages))
        .route(
            "/v1/messages/count_tokens",
            post(routes::messages::count_tokens),
        )
        .route("/v1/responses", post(routes::responses::create))
        .route("/v1/provider/status", get(routes::status::provider_status))
        .route("/api/pool/status", get(routes::status::pool_status));

    let api_url_scoped = Router::new()
        .route(
            "/url/:domain/v1/chat/completions",
            post(routes::chat::chat_completions),
        )
        .route("/url/:domain/v1/messages", post(routes::messages::messages))
        .route("/url/:domain/v1/models", get(routes::models::list_models))
        .layer(axum::middleware::from_fn(routing::from_url_path));

    let api_tab_scoped = Router::new()
        .route(
            "/tab/:tab_id/v1/chat/completions",
            post(routes::chat::chat_completions),
        )
        .route("/tab/:tab_id/v1/messages", post(routes::messages::messages))
        .route("/tab/:tab_id/v1/models", get(routes::models::list_models))
        .layer(axum::middleware::from_fn(routing::from_tab_path));

    let api_scoped = api_url_scoped.merge(api_tab_scoped);

    let admin = Router::new()
        .route("/admin/history", get(routes::admin::history))
        .route("/admin/history/:id", get(routes::admin::history_record))
        .route("/admin/stats", get(routes::admin::stats))
        .route("/admin/selector-test", post(routes::admin::selector_test))
        .route(
            "/admin/selector-generate",
            post(routes::admin::selector_generate),
        )
        .route("/admin/selector-apply", post(routes::admin::selector_apply))
        .route("/admin/sessions", get(routes::admin::sessions))
        .route(
            "/admin/sessions/recover",
            post(routes::admin::recover_sessions),
        )
        .route(
            "/admin/sessions/:id",
            axum::routing::delete(routes::admin::drop_session),
        )
        .route("/admin/logs/stream", get(routes::admin::log_stream))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::require_api_key,
        ));

    let api = api_plain
        .merge(api_scoped)
        .merge(admin)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::require_api_key,
        ));

    Router::new()
        .route("/healthz", get(routes::health::healthz))
        .route("/readyz", get(routes::health::readyz))
        .route("/", get(serve_index))
        .nest_service("/static", tower_http::services::ServeDir::new(static_dir()))
        .merge(api)
        .layer(axum::middleware::from_fn(middleware::inject_request_id))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .layer(tower_http::timeout::TimeoutLayer::new(Duration::from_secs(
            state.config.server.request_timeout_ms / 1000 + 5,
        )))
        .with_state(state)
}

/// Test seam: integration tests exercise the auto-build flow without
/// booting a whole router.
#[doc(hidden)]
pub async fn serve_index_for_test() -> axum::response::Response {
    serve_index().await
}

#[cfg(test)]
mod ui_auto_build_tests {
    use super::*;

    #[tokio::test]
    async fn missing_ui_without_build_script_answers_fast() {
        // Point static_dir at an empty dir: no index.html, no build.sh →
        // the handler must NOT spawn anything and must answer immediately
        // with a hint, not hang or panic.
        let tmp = std::env::temp_dir().join(format!("uwa-ui-empty-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        // SAFETY: single-threaded env mutation is racy with other tests that
        // read UWA_STATIC_DIR; those run against the real static dir only via
        // integration tests, which are separate processes from unit tests.
        std::env::set_var("UWA_STATIC_DIR", &tmp);
        let r = serve_index().await;
        let status = r.status();
        let body = axum::body::to_bytes(r.into_body(), 8192)
            .await
            .unwrap()
            .to_vec();
        let body = String::from_utf8_lossy(&body).into_owned();
        assert_ne!(status, 200, "no UI → not 200");
        assert!(
            body.contains("uwa-ui"),
            "body must carry the uwa-ui hint, got: {body}"
        );
        std::env::remove_var("UWA_STATIC_DIR");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
