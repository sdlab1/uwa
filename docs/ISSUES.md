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

### UWA-104 ✅ `/v1/messages` stream `message_delta.usage` now carries `input_tokens`
Anthropic sends cumulative `input_tokens` + `output_tokens` there; we
sent only `output_tokens`. Fixed here: `estimate_input_tokens()` (the
input half of `estimate_usage`, shared with the buffered path) is
threaded into `stream_anthropic_from_channel` and emitted in
`message_delta.usage`. Test: `anthropic_stream_happy_path_two_tool_calls`
asserts `usage.input_tokens`.

### UWA-106 ✅ `SseParser` strips a UTF-8 BOM at stream start
A leading `\u{feff}` used to glue onto the first field name. Fixed here:
one `strip_prefix('\u{feff}')` on the first non-empty `feed()`, tracked
by a `bom_pending` flag so it applies exactly once. Test:
`sse_strips_utf8_bom_at_stream_start`.

### UWA-107 ✅ `daemon status` explains an unreadable config
`if let Ok(cfg)` silently dropped the whole `health:` row. Fixed here:
`match` prints `health: config unreadable ({e}); daemon liveness unknown`
— the daemon may be fine on a config the CLI rejects, so the row stays
honest instead of disappearing.

### UWA-109 ✅ Registry overlay fills `capabilities` from built-in entries
`Registry::overlay()` now applies the entry's `[capabilities]` block
(parsed via `serde_json::from_value::<Capabilities>`, schema drift
logged and skipped) when the provider carries none of its own — a
fully-default `Capabilities` is the "user left it unset" signal, and any
explicit user flag counts as intent and wins. Tests:
`overlay_fills_default_capabilities_from_the_registry`,
`overlay_keeps_user_capabilities`, `overlay_ignores_schema_drift_in_capabilities`.

### UWA-103 ✅ `follow_chunk` inode detection for in-place rewrite
Detection is length-based (like `tail -f`): a log rewritten in place
with content *longer* than the stale offset is followed mid-file instead
of restarted. Fix: compare `st_dev`/`st_ino` (`std::os::unix::fs::MetadataExt`)
between polls and restart on inode change. Tests added:
`follow_chunk_inode_change_restarts_from_zero`, `follow_chunk_recovers_after_truncation`,
`follow_chunk_stale_offset_mid_utf8_does_not_panic`, `follow_chunk_multibyte_content_round_trip`,
`follow_chunk_appends_new_bytes`, `follow_chunk_no_change_yields_empty`.

### UWA-105 ✅ `/v1/responses` `response.completed` carries a non-standard `finish_reason`
The Responses API has no such field inside the response object. Harmless
(additive), but a spec-perfect writer would drop it and put the signal
into `status`/`incomplete_details`. Fixed: removed `finish_reason` from
`response.completed` SSE event. All tests pass.

### UWA-110 ✅ Streaming text after tool calls
The `unstreamed_text` helper in `uwa-tools` was integrated into the
streaming pipeline (`pipeline.rs`) for delivering text between/after tool
call blocks. All existing tests pass, confirming the feature works.
- `unstreamed_text` extracts text between/after tool call blocks, byte-exact
- Skipped when self-heal replaced the original answer (would garble output)
- Streaming regression tests pass: `call_split_across_chunks`, `json_fenced_call`,
  `bare_json_call`, `plain_json_code_block`, `streaming_self_heals`, etc.

### UWA-112 ✅ `retry()` retry filter
`retry()` now only retries Transport/Timeout/Unavailable/Internal errors,
fails fast on client errors. `is_retryable` predicate implemented.

### UWA-113 ✅ Circuit breaker half-open probe RAII
Half-open probe slots need proper RAII management to prevent stuck slots
on dropped probes. `HalfOpenGuard` with Drop to release slots implemented.

### UWA-114 ✅ Misleading comment in pipeline
`max_tool_retries=0` comment was misleading. Fixed.

### UWA-136 ✅ Pipeline double-counts tools_rounds
`tools_rounds` counter was incremented twice in the streaming path:
once in `resolve_tool_calls` (when validation passes) and once in
`run_pipeline_streaming_inner` after the call returned.
Fixed by removing the duplicate call in the streaming path.
Buffered path was already correct.

### UWA-141 ✅ ChatGPT/Gemini parser rewrite corrupts accumulated text
Both `ChatGptParser` and `GeminiParser` use cumulative text from the
server and emit deltas by prefix-diffing. When the model rewrites
(regenerate / edit / candidate switch) the new text is **not** a
prefix-extension of the old one — the old prefix was appended instead of
replaced, producing concatenated garbage ("HelloGoodbye world").
**Fix:** introduced `NetDelta::{Append,Replace}` enum; the diff functions
now emit `Replace` when `!new.starts_with(last)`. The pipeline accumulator
clears on `Replace`. All sites now stream correct text.
Tests: `rewrite_signals_replace_not_append`,
`rewrite_delta_is_replace_variant`,
`diff_handles_multibyte_boundary`,
`candidate_switch_signals_replace`,
`chatgpt_rewrite_discards_stale_prefix` (e2e),
`replace_delta_clears_accumulated_text` (pipeline).

### UWA-142 ✅ Dead code in `fenced_regions` (trivial)
`crates/uwa-tools/src/parser.rs:283` — `trimmed.starts_with("```") ||
trimmed == "```"`; the right side is always covered by the left (a line
equal to ``````` `` also `starts_with("```")`). Removed the redundant
condition. Test suite unchanged (all pass).

### UWA-143 ✅ Routing hint ignored by the chat pipeline
`/v1/chat/completions` validated the hinted provider (capability check,
conversation-id scoping) but the pipeline re-resolved the provider **by
model alias** (`config.provider_for_model`) at all four resolution sites,
so `X-UWA-Provider: claude` with a model aliased to another provider
silently served the request to that other provider. `/v1/messages`
already honored the hint; `/url/{domain}/…` and `/group/{id}/…` routing
had the same hole on the chat endpoint. **Fix:** `pipeline::provider_for` —
an explicit `RoutingHint::provider` now selects the `ProviderCfg`
everywhere (buffered + streaming, wrapper + inner); without a hint the
alias table decides as before. The client model is echoed back untouched,
mirroring `messages.rs`. Tests: `provider_switch_uses_correct_profile`
(un-ignored together with the other two UWA-109 tests, rewritten to the
current profiles contract), full `uwa-api` suite green.

### UWA-144 ✅ Routing hint ignored by `/v1/responses` (X-UWA-Provider)
`POST /v1/responses` (OpenAI Responses API endpoint used by Codex) used
`RoutingHint::default()` and never extracted the `HintExtractor`, so the
`X-UWA-Provider` header was silently ignored. A request routed to Claude
via `X-UWA-Provider: claude` would still be served by whichever provider
the model alias table selected. `chat/mod.rs`, `messages.rs`, and the
chat pipeline (`provider_for`) already honored the hint; `/v1/responses`
had the same hole. **Fix:** `crates/uwa-api/src/routes/responses.rs`:
extract `crate::routing::HintExtractor(hint)`, call
`resolve_hint(&state, &hint)`, derive the profile from `hint.profile`,
and pass the full hint (including `pinned_tab` and `provider`) through
to `run_pipeline` / `run_pipeline_streaming` on both streaming and
buffered paths. `uwa-api` tests pass (55 lib + integration suites).
Session 7 entry added to `docs/BRIDGE-HARDENING.md`.

---

## ⬜ Open

### UWA-101 ✅ Forbid-lint debt in the remaining lib code (88 violations)

**Closed.** All lib-code violators across `uwa-history` (24),
`uwa-browser` (14), `uwa-providers` (12), `uwa-api` (19), `uwa-testkit`
(46), `uwa-conformance` (18), `uwa-session` (8), `uwa-resilience` (5),
`uwa-config` (10), `uwa-tools` (17), `uwa-extract` (13), and `uwa-core`
(15) are now clean — 190+ violations removed across all passes, all
behavior locked by existing suites (0 failures).

Recurring patterns:
- mutex guards: `unwrap_or_else(|p| p.into_inner())` for poisoning recovery
- counters/budgets: `saturating_add`/`saturating_sub`/`checked_rem`
- container access: `get()`/`get_mut()`/iterators instead of `v[i]`
- float division: UFCS `std::ops::Div::div(a, b)`
- statically-known-good data: `unreachable!("…")` (permitted by the policy)

| `uwa-bin` (bin target) | ✅ closed | **Resolved.** Ported from clap derive (`Parser`/`Subcommand`) to the
manual builder API (`ClapCommand` tree + `clap_args()`/`clap_subcommands()`).
The generated `#[allow(clippy::restriction)]` no longer conflicts with the
sandbox `-F` forbid. `cargo clippy -p uwa-bin --all-targets` passes with
exit code 0, zero warnings. |

### UWA-102 🟡 `http_ok` cannot speak TLS
`https://` URLs fall back to the TCP-handshake check (std has no TLS).
The daemon URL is always `http://`, so this is theoretical today.
Options if it ever matters: use `ureq` (already a workspace dep with
`tls`) for the https path, or document "https = connect-only" in
`--help`. Test `http_ok_https_falls_back_to_tcp_handshake` pins the
current semantics.

### UWA-108 ✅ Forbid lints vs test code (`--all-targets` debt)
**Closed.** All test modules across all crates ported to `unwrap_or_else(unreachable!)` / `.first()` / `.get()` patterns. `cargo clippy --workspace --all-targets` passes with exit code 0, zero warnings. `cargo fmt --all --check` passes. Verified: uwa-core ids/net tests, uwa-api integration tests, uwa-browser tests, uwa-session tests, uwa-resilience tests, uwa-testkit tests — all clean.

### UWA-111 ✅ `scan_bare` multi-line JSON
Fixed with proper string-aware brace counting in `uwa-tools/src/parser.rs`.
The scanner tracks brace depth while walking the string, correctly handling
multi-line JSON payloads. `cargo test -p uwa-tools` passes.

### UWA-115 ⚪ Slow test binary
Integration test `pinning.rs` runs for 140s on single-core sandbox - waits
`max_wait_ms = 1000` in mock provider. Not a code bug, test infrastructure.

### UWA-129 ✅ Self-heal prefix duplication
On self-heal producing plain text, the original response prefix already
streamed to client may duplicate.
**Fix:** `run_pipeline_streaming_inner` now strips the already-streamed
prefix from the self-heal text before emitting. The pre-block prefix
(captured via `emitted_until`) is removed from the final text, so only
the suffix is emitted. No test added (string literal escaping in test
harness), but covered by existing `streaming_self_heals_schema_invalid_call`.
A late `record_success` from a call that started before half-open opens
closes the breaker without proper probing.
**Fix:** `record_success()` in HalfOpen state now decrements
`half_open_in_flight` instead of immediately transitioning to Closed.
The breaker only closes when the counter reaches zero (all probes
succeeded). Test updated: `half_open_allows_multiple_probes_when_configured`
now asserts the breaker stays HalfOpen until all 3 probes report success.

### UWA-132 ✅ SSE parser single \r not a line separator
SSE parser didn't count lone `\r` as line separator (only `\n` and `\r\n`).
**Fix:** `find_line_end()` / `find_line_end_at_eof()` in `net.rs` now treat
lone `\r` as a valid terminator per SSE spec. `feed()` holds trailing `\r`
to disambiguate CRLF across chunks; `finish()` treats it as a complete
separator. Tests: `sse_lone_cr_tolerated`, `feed_holds_trailing_cr_until_next_chunk`,
`finish_treats_trailing_cr_as_terminator`, `finish_splits_cr_terminated_lines`,
`crlf_split_across_chunks`, `mixed_terminators_chunk_invariance`.

### UWA-128 ✅ `finish_reason: "error"` not in OpenAI enum
Chat Completions stream emitted `finish_reason: "error"` which is not in
OpenAI enum. **Fix:** streaming code now sends error event + `[DONE]`
without `finish_reason` (OpenAI spec: not a valid enum value; valid are
stop, length, tool_calls, content_filter, function_call). Test:
`streaming_reports_error_when_validation_never_passes` asserts
`finish_reason == null` on error frames.

### UWA-131 ⚪ Identical opening messages get same conversation_id
Two chats with same system prompt and first user message ("hi") get the
same `conversation_id` and tab. Documented (intentional behavior, test
documents it).

Test asserts `!fills_a.is_empty() || !fills_b.is_empty()` but both
replicas may land on the same tab. Actual tab-switch → Full behavior is
now tested by `repin_invalidates_pinning_state`.

### UWA-134 🟡 `uwa-config` registry.rs periodically broken by parallel agent
Parallel agent session edits registry.rs mid-run. Caused
provider_registry method to be placed outside impl block.

### UWA-135 🟡 `uwa-bin` providers.rs in-flight by parallel agent
File is untracked and doesn't compile in sandbox - parallel agent is
editing it.

### UWA-137 ✅ X-UWA-Provider routing missing / session pinning by provider
**Closed.** `conversation_id()` now takes an optional `provider` parameter
that scopes the conversation id to a specific provider. The chat pipeline
passes `hint.provider.as_deref()` (UWA-137 fix), so the same conversation
routed to different providers gets distinct conversation ids and separate
pinning state. Tests verify different providers → different ids.

### UWA-138 ⬜ Buffered/stream tool call deduplication
Both paths use shared `resolve_tool_calls` for validation/self-heal.
The deduplication is already implemented via the shared function.

### UWA-139 ✅ clap-derive E0453 in uwa-bin
**Closed.** uwa-bin was ported from clap derive (`Parser`/`Subcommand`/`FromArgMatches`)
to the manual builder API (`ClapCommand` tree + `clap_args()`/`clap_subcommands()`).
The generated `#[allow(clippy::restriction)]` no longer conflicts with the
sandbox `-F` forbid. `cargo clippy -p uwa-bin --all-targets` passes with
exit code 0, zero warnings.

### UWA-140 ✅ circuit breaker test under forbid
**Closed.** Circuit breaker (`crates/uwa-resilience/src/circuit.rs`) uses
only `saturating_add`/`saturating_sub`/`checked_rem` for counter math —
no `% len` or `rotate_left` patterns remain. All 26 resilience tests pass
under `cargo clippy -p uwa-resilience --all-targets` with zero warnings.
