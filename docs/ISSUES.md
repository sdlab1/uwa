# Issue tracking

Live tracker for maintenance work on the bridge. Fixed items keep their
context (symptom → root cause → fix) so the history is searchable;
open items carry a repro, a root-cause hypothesis and a suggested fix.

Legend: ✅ closed · ⬜ open · 🔴 breaks the build/tests · 🟡 quality ·
⚪ polish.

---

## ✅ Closed

### UWA-001 🔴 SSE parser ate token deltas starting with a space
`uwa-extract/src/net.rs` — `trim_start()` after `data:` removed *all*
leading whitespace; the SSE spec says strip **one** space. `data:  world`
streamed as `world`. Fixed in `30b658b` (`strip_one_space`, feed+finish),
tests `sse_keeps_one_leading_space_after_colon`, `sse_finish_keeps_leading_space`.

### UWA-002 🔴 JSON-path keys mangled for non-ASCII
`uwa-extract/src/net.rs` — `bytes[i] as char` split multi-byte keys into
bogus scalars; `помощь.текст` / `檔案.內容` / `🚀.msg` never matched.
Fixed in `30b658b`, then fully char-driven in the lint pass (this
change); tests `json_path_non_ascii_keys`, `json_path_unclosed_bracket_is_none`.

### UWA-003 🔴 `logs --follow` panicked / went silent after rotation
`uwa-bin/src/commands/daemon.rs` — stale byte offset could slice
mid-UTF-8 (panic); truncation below the offset silenced the follower
forever. Fixed in `30b658b` with pure `follow_chunk()` (restart on
shrink, snap to char boundary, flush per chunk). See UWA-103 for the
in-place-rewrite limitation.

### UWA-004 🔴 `http_ok` reported a healthy daemon as down
`localhost:8080` (SocketAddr::parse rejects hostnames), port-less URLs,
`https://`, bracketed IPv6. Fixed in `30b658b` with `host_port()` +
`ToSocketAddrs`. Superseded in this change by UWA-010 (real HTTP GET).

### UWA-005 🔴 `/v1/messages`: `tool_use` with no `input` → arguments `"null"`
Fixed in `30b658b` (`filter(!is_null)` → `{}`), test
`tool_use_without_input_becomes_empty_object`.

### UWA-006 🔴 `/v1/messages` stream: untyped errors, open text block, silent drop
Fixed in `30b658b`: typed `error.type` (`anthropic_error_type`), the
open block is closed before the error, dropped producer → error instead
of `end_turn`. Tests: happy path, mid-stream error, dropped producer.

### UWA-007 🔴 `/v1/responses` stream: all function calls had `output_index: 0`
Fixed in `30b658b`: counted indices, starting after the text message.
Tests: `parallel_function_calls_get_distinct_output_indices`,
`function_call_indices_start_after_the_text_message`.

### UWA-008 🔴 Dropped producer reported as a clean completion
`/v1/responses` → `response.failed` (status "failed" + structured
error); `/v1/chat/completions` → `finish_reason: "error"` chunk +
`[DONE]`. Fixed in `30b658b`.

### UWA-009 🔴 HEAD did not compile: `uwa-config` missing deps + registry data
Introduced by `78027b8`: `registry.rs` used `tracing`/`serde_json`
without declaring them (E0433). Fixed here (added both deps).

### UWA-010 🔴 `http_ok` proved only a TCP handshake
A bare `TcpStream::connect_timeout` accepted any unrelated service on
the port as "healthy", and the URL path was ignored. Fixed here:
`GET /healthz HTTP/1.0` + `parse_status_line()` + **2xx required** —
foreign HTTP (404), wedged daemon (500), non-HTTP garbage and silent
sockets all report down. Scheme letter-case handled (`HTTP://`).
Read/write timeouts bound the probe (300 ms). Tests:
`http_ok_requires_a_real_2xx_http_answer`, `http_ok_rejects_foreign_services`,
`http_ok_silent_socket_times_out_as_down`, `parse_status_line_variants`,
`request_target_variants`, `host_port_is_scheme_case_insensitive`.

### UWA-011 🟡 `uwa-core` blocked all clippy runs (15 violations)
`error.rs` under forbid lints. Fixed in `567cc12`: `saturating_sub` in
`message()`, index-free iterator-based `levenshtein`. Textbook distances
locked by tests.

### UWA-012 🟡 Forbid-lint debt in `uwa-extract`, `uwa-tools`, `uwa-config`, `uwa-resilience`, `uwa-session` (46 violations)
All lib code now clean (13 + 17 + 10 + 5 + 8 = 53 fixed, incl. the
blocking `uwa-config` build script → `die()`/`exit(101)` with readable
messages). Highlights: char-driven `parse_path`; `let-else` DOM fallback
in the pipeline; mutex-poisoning recovery in the site parsers and the
circuit breaker; `checked_rem` round-robin in the session manager.

### UWA-013 🔴 `registry_fills_missing_selectors_for_known_provider` failed at HEAD
Two stacked causes from `78027b8`, both fixed here:
1. `url_patterns` sat below `[meta]` in **all four** `providers/*.toml`
   → it parsed into the `meta` table, the top-level field the overlay
   reads stayed empty, validation rejected the provider
   ("url_patterns is empty"). Moved above `[meta]`.
2. `ProviderCfg.url_patterns` / `.capabilities` were required serde
   fields, so strict parsing failed before the overlay could fill them.
   Both `#[serde(default)]` now; validation still rejects an empty
   pattern list when neither user nor registry supplied one.
Also: `panic!` on malformed built-ins → `tracing::error!` + skip (a
library constructor must not abort the daemon on schema drift).
`cargo test -p uwa-config`: 40/40.

---

## ⬜ Open

### UWA-101 🟡 Forbid-lint debt in the remaining lib code (88 violations)

**Update (this change): closed for 13 of 14 crates.** `uwa-history` (24),
`uwa-browser` (14), `uwa-providers` (12), `uwa-api` (19), `uwa-testkit`
(46) and `uwa-conformance` (18) are now clean — 133 violations removed
across the two passes, all behavior locked by the existing suites
(0 failures).

Recurring patterns that closed the debt (copy for the rest):
- mutex guards: a `lock()` helper recovering from poisoning
  (`unwrap_or_else(|p| p.into_inner())`) — testkit, circuit breaker,
  site parsers;
- counters/budgets: `saturating_add`/`saturating_sub`/`checked_rem`;
- container access: `get()`/`get_mut()`/iterators instead of `v[i]`;
- float division: UFCS `std::ops::Div::div(a, b)` (f32 has no
  `checked_div`; the lint flags the `/` operator, not the call);
- serialization of `&str`/enums: `unwrap_or_else(|_| format!("{x:?}"))`;
- statically-known-good data (test configs, URLs, the Hinnant civil-date
  math in conformance's `score.rs`): provably in-bounds arithmetic via
  `saturating_*`/`div_euclid`/`rem_euclid`, and a last-resort
  `unreachable!("…")` — permitted by the policy (the forbid list covers
  unwrap/expect/panic/todo/unimplemented, not unreachable) — used only
  where failure truly means a wiring bug.

Open remainder:

| Crate | Errors | Status |
|-------|-------:|--------|
| `uwa-bin` (bin target) | — | **Blocked by the concurrent agent's in-flight `commands/providers.rs`** (untracked, does not compile yet: `cannot find attribute serde`). Its other files are expected to be mostly clean — the daemon module already went through the same recipes. Revisit when the providers command lands; the recipes above cover everything. |

### UWA-102 🟡 `http_ok` cannot speak TLS
`https://` URLs fall back to the TCP-handshake check (std has no TLS).
The daemon URL is always `http://`, so this is theoretical today.
Options if it ever matters: use `ureq` (already a workspace dep with
`tls`) for the https path, or document "https = connect-only" in
`--help`. Test `http_ok_https_falls_back_to_tcp_handshake` pins the
current semantics.

### UWA-103 ⚪ `follow_chunk` cannot detect an in-place rewrite that grew
Detection is length-based (like `tail -f`): a log rewritten in place
with content *longer* than the stale offset is followed mid-file instead
of restarted. Fix: compare `st_dev`/`st_ino` (`std::os::unix::fs::MetadataExt`)
between polls and restart on inode change. Tests would need a
rename-and-recreate fixture.

### UWA-104 ⚪ `/v1/messages` stream `message_delta.usage` lacks `input_tokens`
Anthropic sends cumulative `input_tokens` + `output_tokens` there; we
send only `output_tokens`. No client observed to break. Fix: thread the
estimated input count into `stream_anthropic_from_channel`.

### UWA-105 ⚪ `/v1/responses` `response.completed` carries a non-standard `finish_reason`
The Responses API has no such field inside the response object. Harmless
(additive), but a spec-perfect writer would drop it and put the signal
into `status`/`incomplete_details`.

### UWA-106 ⚪ `SseParser` does not strip a UTF-8 BOM
A leading `\u{feff}` would land in the first field value. No known
vendor sends one. Fix: one `strip_prefix('\u{feff}')` on the first
`feed()`.

### UWA-107 ⚪ `daemon status` silently skips the health line when the config fails to parse
`if let Ok(cfg) = …` — a broken TOML prints no `health:` row and no
explanation. Fix: `match` and print `health: config unreadable ({e})`.

### UWA-108 🟡 Forbid lints vs test code (`--all-targets` debt)
The cycle cleaned **lib** targets. Test modules across the workspace
still use `unwrap()`/`expect()` heavily (e.g. `uwa-core/src/ids.rs`,
`uwa-core/src/net.rs`), so `cargo clippy --workspace --all-targets`
stays red even where the lib is clean. Either (a) port tests to
`unwrap_or`/`assert!`-style, or (b) declare `#![cfg_attr(test,
allow(clippy::unwrap_used, clippy::expect_used))]` per crate as an
explicit, documented policy decision.

### UWA-109 ⚪ Registry overlay does not fill `capabilities` from built-in entries
`Registry::overlay()` fills `url_patterns`, `selectors`, `net`,
`selectors_version` — but not `capabilities`, so a user overriding a
registered provider gets serde defaults (all features off) instead of
the entry's advertised capabilities. Fix: one more block in `overlay()`
+ a test mirroring `registry_fills_missing_selectors_for_known_provider`.
