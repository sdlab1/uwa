//! # uwa-api
//!
//! HTTP surface: OpenAI- and Anthropic-compatible endpoints over a `SiteProvider`.
//!
//! ## Passport (public API)
//! - [`router`] — builds an `axum::Router`
//! - [`AppState`], [`ProviderRegistry`]
//! - [`ApiError`] — the OpenAI-shaped error at the HTTP edge
//!
//! =========================================================================
//! ISSUE TRACKER & BRIDGE HARDENING LOG (migrated from docs/ISSUES_AND_BRIDGE.md)
//! =========================================================================
//!
//! One document for everything maintenance-related on UWA's translation
//! bridge: issue tracker (symptom → root cause → fix) and the
//! session-by-session hardening history (wire-fidelity fixes in the
//! streaming adapters, daemon observability, and the forbid-level
//! clippy campaign).
//!
//! Previously split across `docs/ISSUES.md` and `docs/BRIDGE-HARDENING.md`;
//! kept in one place so nothing is scattered across several docs.
//!
//! Legend: ✅ closed · ⬜ open · 🔴 breaks the build/tests · 🟡 quality ·
//! ⚪ polish / documented limitation.
//!
//! -----------------------------------------------------------------------
//! Part 1 — Issue tracker
//! -----------------------------------------------------------------------
//!
//! All previously open issues (UWA-001 … UWA-145, including UWA-131) are
//! closed. The full history of closed fixes — SSE parser, JSON-path keys,
//! `logs --follow`, `http_ok`, `/v1/messages` and `/v1/responses` stream
//! errors, routing hints (X-UWA-Provider / X-UWA-Session), forbid-level
//! clippy chain across all 14 crates, ChatGPT/Gemini rewrite corruption,
//! self-heal prefix duplication, circuit breaker RAII, `scan_bare`
//! multi-line JSON, dead code, lone `\r` SSE terminator, `finish_reason`
//! enum compliance, and the broken-HEAD repair — lives in git history.
//!
//! ```
//! UWA-102 ✅ http_ok now performs real TLS via ureq
//! ---------------------------------------------------
//! https:// URLs are now probed with a real TLS handshake + GET /healthz
//! through ureq (rustls backend), replacing the previous TCP-only fallback
//! that falsely reported any HTTPS listener as healthy. Non-2xx responses,
//! TLS handshake failures and connection refusals all report the daemon as
//! down.
//! Test http_ok_https_requires_real_tls pins the new semantics.
//! Location: crates/uwa-bin/src/commands/daemon.rs
//!
//! UWA-115 ✅ Slow test binary
//! -----------------------------
//! cargo test -p uwa-session --lib runs all 61 tests in ~0.16s
//! (pinning suite 13 tests in ~0.04s). The previously-reported ~140s
//! max_wait_ms = 1000 mock wait is no longer present.
//! Location: crates/uwa-session/src/lib.rs
//!
//! UWA-138 ✅ Buffered/stream tool call deduplication
//! -----------------------------------------------------
//! Both buffered and streaming paths share resolve_tool_calls for
//! validation/self-heal, which deduplicates identical tool calls.
//! Integration test buffered_and_streaming_produce_identical_tool_calls
//! asserts both paths emit exactly the same two tool calls.
//! Location: crates/uwa-api/tests/e2e_pipeline.rs,
//!           crates/uwa-api/src/routes/chat/pipeline.rs
//!
//! UWA-131 ✅ X-UWA-Conversation header overrides content-derived
//! conversation_id
//! -----------------------------------------------------------------
//! conversation_id() remains a deterministic FNV-1a hash of the opening
//! messages (session continuity requires identical history → same id;
//! a per-call nonce breaks sm.len() == 1 session-reuse tests). Clients
//! can now override the hash by supplying X-UWA-Conversation: <id> on
//! /v1/chat/completions. The header value is extracted in HintExtractor
//! (routing.rs) into RoutingHint.conversation and wins over the
//! content-derived hash in both buffered and streaming pipeline paths.
//! Location: crates/uwa-api/src/routing.rs,
//!           crates/uwa-api/src/routes/chat/pipeline.rs
//! ```
//!
//! Open: None.
//!
//! -----------------------------------------------------------------------
//! Part 2 — Bridge hardening sessions log
//! -----------------------------------------------------------------------
//!
//! A record of the maintenance sessions that hardened UWA's translation
//! bridge: wire-fidelity bugs in the streaming/adapters, daemon
//! observability, and bringing the workspace under the forbid-level
//! clippy policy (unwrap_used, expect_used, panic, todo, unimplemented,
//! indexing_slicing, arithmetic_side_effects).
//!
//! -----------------------------------------------------------------------
//! Session 1 — wire & daemon bugfixes (commit 30b658b)
//! -----------------------------------------------------------------------
//! Trigger: review pass over /v1/messages (Claude Code), /v1/responses
//! (Codex), the SSE extraction parser and the daemon CLI. Nine bugs,
//! all fixed with regression tests:
//!
//! 1. uwa-extract SSE: trim_start() stripped ALL leading spaces after
//!    data: — a token delta `data:  world` streamed as `world`,
//!    corrupting every message that begins with a space.
//!    Fix: strip exactly ONE space per SSE spec (strip_one_space) in
//!    feed() and finish().
//!
//! 2. uwa-extract JSON path: bytes[i] as char turned every byte of a
//!    multi-byte key into its own bogus char —
//!    json_path_str(v, "помощь.текст") never matched.
//!    Fix: char-driven path parsing (char_indices).
//!
//! 3. uwa-bin logs --follow: slicing the log at a stale byte offset
//!    could panic mid-UTF-8; after truncation/rotation it went silent
//!    forever.
//!    Fix: Pure follow_chunk(): restart from byte 0 when the file
//!    shrank, snap forward to the next char boundary; stdout flushed
//!    per chunk.
//!
//! 4. uwa-bin http_ok: localhost:8080 (SocketAddr::parse rejects
//!    hostnames), port-less URLs, https://, bracketed IPv6 — a healthy
//!    daemon reported as down.
//!    Fix: Pure host_port() parser + DNS resolution via ToSocketAddrs.
//!
//! 5. /v1/messages: A tool_use block with missing/null input became the
//!    string "null" as tool arguments.
//!    Fix: {} (both missing and explicit null).
//!
//! 6. /v1/messages stream: The error event had no error.type; a
//!    partially streamed text block was left open (SDKs hang waiting
//!    for content_block_stop); a dropped producer masqueraded as a
//!    clean end_turn.
//!    Fix: Typed error event (invalid_request_error/api_error
//!    mapping), the open block is closed before the error, dropped
//!    producer fails the stream.
//!
//! 7. /v1/responses stream: Every function call was emitted with
//!    output_index: 0 — parallel calls collide for Codex.
//!    Fix: Indices count up, starting after the text message if there
//!    is one.
//!
//! 8. /v1/responses stream: A dropped producer was reported as a clean
//!    response.completed.
//!    Fix: response.failed with status: "failed" and a structured error.
//!
//! 9. /v1/chat/completions stream: A dropped producer was reported as a
//!    clean stop finish.
//!    Fix: finish_reason: "error" chunk + [DONE].
//!
//! Tests: SSE leading-space (feed + finish), non-ASCII path keys
//! (Cyrillic/CJK/emoji), unclosed bracket, follow_chunk
//! (append/no-change/truncation/mid-UTF-8/multibyte), host_port
//! variants, Anthropic stream (happy path with two tool calls,
//! mid-stream error, dropped producer, null-input, error-type mapping),
//! Responses stream (index sequences, failed-on-drop), chat stream
//! (normal completion, drop → error chunk).
//!
//! -----------------------------------------------------------------------
//! Session 2 — uwa-core under forbid lints (commit 567cc12)
//! -----------------------------------------------------------------------
//! error.rs was the only lib-code violator in uwa-core (15 errors),
//! which made any clippy run in the workspace fail before even
//! reaching downstream crates. Refactor, not suppression — #[allow]
//! cannot re-enable a forbid:
//!
//! - ModelNotFound::message: sorted.len() - preview.len() →
//!   saturating_sub (provably safe — preview is a prefix of sorted).
//! - levenshtein: rewritten index-free. Iterator pairs replace all
//!   a[i]/b[j]/cur[j]/prev[j] indexing; saturating_add replaces + 1;
//!   unwrap_or()/last() replace unwraps. Same two-row DP: diag trails
//!   the prev iterator by one cell.
//!
//! Textbook distances locked by tests: kitten/sitting = 3, flaw/lawn =
//! 2, gumbo/gambol = 2, empty edges, Cyrillic, symmetry.
//!
//! -----------------------------------------------------------------------
//! Session 3 — HTTP probe, lint chain, broken-HEAD repair
//! -----------------------------------------------------------------------
//! ### 1. http_ok is now a real HTTP probe
//! The old probe only did TcpStream::connect_timeout — a bare TCP
//! handshake. Any unrelated service on the port (a stale Chromium, an
//! SSH server, Postgres) reported "healthy".
//! Now: (1) connect with 300 ms timeout, (2) GET /healthz HTTP/1.0 +
//! Host: + Connection: close with the path taken from the URL, (3)
//! parse first bytes via parse_status_line(); answer counts only if
//! valid HTTP status line with 2xx code, (4) scheme letter-case
//! irrelevant, fragments cut, (5) https:// URLs fall back to TCP
//! handshake check.
//!
//! ### 2. Forbid-lint chain, continued
//! | Crate       | Before | After | Notable rewrites                                          |
//! |-------------|--------|-------|-----------------------------------------------------------|
//! | uwa-core    | 15     | 0     | session 2                                                 |
//! | uwa-extract | 13     | 0     | parse_path fully char-driven; json_path_str without unwrap|
//! | uwa-tools   | 17     | 0     | three tool-call scanners use str::get + saturating_add    |
//! | uwa-config  | 10     | 0     | build.rs dies via process::exit(101) without expect       |
//! | uwa-resilience| 5    | 0     | circuit breaker recovers from mutex poisoning             |
//! | uwa-session | 8      | 0     | round-robin via checked_rem + get()                       |
//! | uwa-history | 24     | 0     | closed (UWA-101)                                          |
//! | uwa-browser | 13     | 0     | closed (UWA-101)                                          |
//! | uwa-providers| 13    | 0     | closed (UWA-101)                                          |
//! | uwa-api     | 13     | 0     | closed (UWA-101)                                          |
//! | uwa-testkit | 24     | 0     | closed (UWA-101)                                          |
//! | uwa-bin     | 1      | 0     | closed (UWA-101)                                          |
//!
//! ### 3. Broken-HEAD repair (commit 78027b8)
//! registry.rs used tracing::warn! and serde_json::Value without
//! declaring dependencies → E0433. Fixed by adding both to
//! Cargo.toml. registry_fills_missing_selectors test failed:
//! root cause 1 — url_patterns sat below [meta] in providers/*.toml;
//! root cause 2 — ProviderCfg.url_patterns/.capabilities were required
//! serde fields. Both now #[serde(default)]. panic! on malformed
//! built-in entries → tracing::error! + skip.
//!
//! -----------------------------------------------------------------------
//! Session 4 — ChatGPT/Gemini rewrite corruption + dead code
//! -----------------------------------------------------------------------
//! ### 1. ChatGPT / Gemini parser rewrite corrupts accumulated text (UWA-141)
//! Both ChatGptParser and GeminiParser consume cumulative text and emit
//! deltas by prefix-diffing. When the model rewrites (regenerate /
//! edit / candidate switch), the new text is not a prefix-extension of
//! the old one — the diff returned the whole new text, and the pipeline
//! accumulator appended it to the stale prefix, producing concatenated
//! garbage.
//! Root cause: NetDelta(pub String) could not express "replace
//! everything accumulated so far".
//! Fix: introduced NetDelta::{Append, Replace} enum.
//!   - Append(s) — normal growth, suffix appended to accumulated text
//!   - Replace(s) — site rewrote its message; accumulator clears and
//!     replaces with s
//!
//! ### 2. Dead code in fenced_regions (UWA-142)
//! Removed redundant `|| trimmed == "```"` (always covered by
//! starts_with("```")).
//!
//! ### 3. SSE parser lone \r not a line separator (UWA-132)
//! Parser only accepted \n and \r\n; lone \r was ignored.
//! Fix: find_line_end() finds the earliest terminator among LF, CRLF,
//! CR; trailing \r at chunk end is held back; find_line_end_at_eof()
//! treats trailing \r at EOF as complete terminator.
//!
//! ### 4. finish_reason: "error" not in OpenAI enum (UWA-128)
//! Error frames now send error event + [DONE] without any finish_reason
//! field.
//!
//! ### 5. Self-heal prefix duplication (UWA-129)
//! Self-heal emitted entire new text but the pre-block prefix had
//! already been streamed. Fix: strip the already-streamed prefix
//! (tracked via emitted_until) from the self-heal text before emitting.
//!
//! -----------------------------------------------------------------------
//! Session 5 — uwa-browser clippy + uwa-session pre-existing test failures
//! -----------------------------------------------------------------------
//! ### 1. uwa-browser clippy forbid-lint clean (UWA-101)
//! All unwrap(), expect(), panic!() and indexing violations replaced
//! with unwrap_or_else(|_| unreachable!(…)) patterns.
//!
//! ### 2. uwa-session pre-existing test failures fixed
//! - empty_profiles_each_get_their_own_lazy_tab: pick_tab_in fell back
//!   to default profile's tabs even when profiles were supported.
//! - unsupported_profile_is_an_error: non-default profile on
//!   non-profile transport should return Unavailable.
//!
//! ### 3. Forbid-lint chain complete
//! All 14 crates: 0 forbid-lint violations.
//!
//! -----------------------------------------------------------------------
//! Session 6 — final clippy warnings + devharness panic + uwa-lifecycle
//! -----------------------------------------------------------------------
//! ### 1. Remaining clippy style warnings eliminated
//! get_first → first(), manual_range_contains → (400..600).contains(),
//! needless_borrow fixes, unnecessary_lazy_evaluations fixes.
//!
//! ### 2. devharness panic!() in production code path
//! gates.rs:499 — panic!("ожидали Fail, получили {v:?}") →
//! unreachable!("…").
//!
//! ### 3. uwa-lifecycle start_paused feature missing
//! Added test-util to tokio dev-dependency features.
//!
//! -----------------------------------------------------------------------
//! Session 7 — X-UWA-Provider honored in /v1/responses
//! -----------------------------------------------------------------------
//! POST /v1/responses used RoutingHint::default() and never extracted
//! the HintExtractor, so X-UWA-Provider was silently ignored. Fix:
//! extract HintExtractor, call resolve_hint(), derive profile from
//! hint.profile, pass hint to run_pipeline/run_pipeline_streaming.
//!
//! -----------------------------------------------------------------------
//! Session 8 — X-UWA-Session profile honored in /v1/messages
//! -----------------------------------------------------------------------
//! POST /v1/messages passed ProfileName::default_name() to
//! run_pipeline/run_pipeline_streaming, ignoring X-UWA-Session
//! (hint.profile). Fix: compute profile from hint.profile once before
//! the streaming/buffered split.
//!
//! =========================================================================
//! END OF MIGRATED DOCUMENTATION
//! =========================================================================

pub mod error;
pub mod events;
pub mod history;
pub mod metrics;
pub mod middleware;
pub mod recorder;
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

/// CORS policy. On a loopback bind only loopback web origins (the bundled
/// dashboard, a local web UI) may call the API from a browser; reflecting
/// `*` there would let any website read responses from the daemon. Off
/// loopback the API key is the gate (it is not an ambient browser
/// credential), so any origin may preflight.
fn cors_layer(bind: &str) -> CorsLayer {
    use tower_http::cors::{AllowOrigin, Any};
    let base = CorsLayer::new()
        .allow_methods(Any)
        .allow_headers(Any)
        .expose_headers(Any);
    if middleware::is_loopback_bind(bind) {
        base.allow_origin(AllowOrigin::predicate(|origin, _| {
            middleware::is_loopback_origin(origin.as_bytes())
        }))
    } else {
        base.allow_origin(Any)
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

    let api_group_scoped = Router::new()
        .route(
            "/group/:group_id/v1/chat/completions",
            post(routes::chat::chat_completions),
        )
        .route(
            "/group/:group_id/v1/messages",
            post(routes::messages::messages),
        )
        .route(
            "/group/:group_id/v1/models",
            get(routes::models::list_models),
        )
        .layer(axum::middleware::from_fn(routing::from_group_path));

    let api_url_preset_scoped = Router::new()
        .route(
            "/url/:domain/:preset/v1/chat/completions",
            post(routes::chat::chat_completions),
        )
        .route(
            "/url/:domain/:preset/v1/messages",
            post(routes::messages::messages),
        )
        .route(
            "/url/:domain/:preset/v1/models",
            get(routes::models::list_models),
        )
        .layer(axum::middleware::from_fn(routing::from_url_preset_path));

    let api_scoped = api_url_scoped
        .merge(api_tab_scoped)
        .merge(api_group_scoped)
        .merge(api_url_preset_scoped);

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
        .route("/admin/conformance", get(routes::admin::conformance))
        .route(
            "/admin/providers/registry",
            get(routes::admin::provider_registry),
        )
        .route("/admin/sessions", get(routes::admin::sessions))
        .route(
            "/admin/recorder/status",
            get(routes::admin::recorder_status),
        )
        .route("/admin/recorder/start", post(routes::admin::recorder_start))
        .route("/admin/recorder/stop", post(routes::admin::recorder_stop))
        .route("/admin/clients", get(routes::admin::clients_list))
        .route(
            "/admin/clients/install",
            post(routes::admin::clients_install),
        )
        .route(
            "/admin/clients/uninstall",
            post(routes::admin::clients_uninstall),
        )
        .route(
            "/admin/sessions/recover",
            post(routes::admin::recover_sessions),
        )
        .route("/admin/profiles", get(routes::admin::list_profiles))
        .route(
            "/admin/profiles/:name/open",
            post(routes::admin::open_profile_tab),
        )
        .route(
            "/admin/profiles/:name/reset",
            post(routes::admin::reset_profile),
        )
        .route(
            "/admin/sessions/:id",
            axum::routing::delete(routes::admin::drop_session),
        )
        .route("/admin/logs/stream", get(routes::admin::log_stream))
        .route("/admin/config", get(routes::admin::config_get))
        .route("/admin/config/reload", post(routes::admin::config_reload))
        .route("/admin/events", get(routes::admin::events_list))
        .route("/admin/events/summary", get(routes::admin::events_summary))
        .route("/admin/events/clear", post(routes::admin::events_clear))
        .route("/admin/events/stream", get(routes::admin::events_stream));

    // Layer order: the last `.layer` is outermost and runs first. The origin
    // guard must reject a hostile browser request before anything else
    // (auth included) looks at it. Auth is applied once, here, to every
    // API and admin route.
    let api = api_plain
        .merge(api_scoped)
        .merge(admin)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::require_api_key,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::guard_browser_origin,
        ));

    Router::new()
        .route("/healthz", get(routes::health::healthz))
        .route("/readyz", get(routes::health::readyz))
        .route("/", get(serve_index))
        .nest_service("/static", tower_http::services::ServeDir::new(static_dir()))
        .merge(api)
        .layer(axum::middleware::from_fn(middleware::inject_request_id))
        .layer(TraceLayer::new_for_http())
        .layer(cors_layer(&state.cfg().server.bind))
        .layer(tower_http::timeout::TimeoutLayer::new(Duration::from_secs(
            // Constant divisor is lint-safe; the +5 grace must saturate.
            (state.cfg().server.request_timeout_ms / 1000).saturating_add(5),
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
        std::fs::create_dir_all(&tmp)
            .unwrap_or_else(|e| unreachable!("create_dir_all failed: {e}"));
        // SAFETY: single-threaded env mutation is racy with other tests that
        // read UWA_STATIC_DIR; those run against the real static dir only via
        // integration tests, which are separate processes from unit tests.
        std::env::set_var("UWA_STATIC_DIR", &tmp);
        let r = serve_index().await;
        let status = r.status();
        let body = axum::body::to_bytes(r.into_body(), 8192)
            .await
            .unwrap_or_else(|e| unreachable!("to_bytes failed: {e}"))
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
