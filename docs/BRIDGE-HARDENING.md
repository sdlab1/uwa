# Bridge Hardening — maintenance log

A record of the maintenance sessions that hardened UWA's translation
bridge: wire-fidelity bugs in the streaming/adapters, daemon
observability, and bringing the workspace under the forbid-level clippy
policy (`unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`,
`indexing_slicing`, `arithmetic_side_effects` — see `~/.cargo/config.toml`
of the build sandbox; they apply to every crate).

Open follow-ups live in [ISSUES](ISSUES.md). Every claim below is
backed by a test in the named commit.

---

## Session 1 — wire & daemon bugfixes (`30b658b`)

Trigger: a review pass over `/v1/messages` (what Claude Code talks to),
`/v1/responses` (what Codex talks to), the SSE extraction parser and the
daemon CLI. Nine bugs, all fixed with regression tests:

| # | Area | Bug | Fix |
|---|------|-----|-----|
| 1 | `uwa-extract` SSE | `trim_start()` stripped **all** leading spaces after `data:` — a token delta `data:  world` streamed as `world`, corrupting every message that begins with a space | Strip exactly **one** space, per the SSE spec (`strip_one_space`), in `feed()` and `finish()` |
| 2 | `uwa-extract` JSON path | `bytes[i] as char` turned every byte of a multi-byte key into its own bogus `char` — `json_path_str(v, "помощь.текст")` never matched | Char-driven path parsing (`char_indices`) |
| 3 | `uwa-bin` `logs --follow` | Slicing the log at a stale byte offset could panic mid-UTF-8; after truncation/rotation it went silent forever | Pure `follow_chunk()`: restart from byte 0 when the file shrank, snap forward to the next char boundary; stdout flushed per chunk |
| 4 | `uwa-bin` `http_ok` | `localhost:8080` (SocketAddr::parse rejects hostnames), port-less URLs, `https://`, bracketed IPv6 — a **healthy** daemon reported as down | Pure `host_port()` parser + DNS resolution via `ToSocketAddrs` |
| 5 | `/v1/messages` | A `tool_use` block with missing/`null` `input` became the **string** `"null"` as tool arguments | `{}` (both missing and explicit null) |
| 6 | `/v1/messages` stream | The `error` event had no `error.type`; a partially streamed text block was left open (SDKs hang waiting for `content_block_stop`); a dropped producer masqueraded as a clean `end_turn` | Typed error event (`invalid_request_error`/`api_error` mapping), the open block is closed before the error, dropped producer fails the stream |
| 7 | `/v1/responses` stream | Every function call was emitted with `output_index: 0` — parallel calls collide for Codex | Indices count up, starting after the text message if there is one |
| 8 | `/v1/responses` stream | A dropped producer was reported as a clean `response.completed` | `response.failed` with `status: "failed"` and a structured `error` |
| 9 | `/v1/chat/completions` stream | A dropped producer was reported as a clean `stop` finish | `finish_reason: "error"` chunk + `[DONE]` |

Tests added: SSE leading-space (feed + finish), non-ASCII path keys
(Cyrillic/CJK/emoji), unclosed bracket, `follow_chunk` (append/no-change/
truncation/mid-UTF-8/multibyte), `host_port` variants, Anthropic stream
(happy path with two tool calls, mid-stream error, dropped producer,
null-input, error-type mapping), Responses stream (index sequences,
failed-on-drop), chat stream (normal completion, drop → error chunk) —
driven through channels and parsed from the SSE wire format via a new
`cfg(test)` helper (`routes::sse_testing`).

**Verification note:** a concurrent session was editing the same
checkout, so the exact commit tree (HEAD + these files only) was
certified in a detached `git worktree`: 26 test suites green.

## Session 2 — `uwa-core` under forbid lints (`567cc12`)

`error.rs` was the only lib-code violator in `uwa-core` (15 errors),
which made **any** clippy run in the workspace fail before even reaching
downstream crates. Refactor, not suppression — `#[allow]` cannot
re-enable a `forbid`:

- `ModelNotFound::message`: `sorted.len() - preview.len()` →
  `saturating_sub` (provably safe — `preview` is a prefix of `sorted` —
  the lint cannot see it).
- `levenshtein`: rewritten **index-free**. Iterator pairs replace all
  `a[i]/b[j]/cur[j]/prev[j]` indexing; `saturating_add` replaces `+ 1`;
  `unwrap_or()`/`last()` replace unwraps. Same two-row DP: `diag` trails
  the `prev` iterator by one cell, playing `prev[j-1]` against
  `above = prev[j]`.

Textbook distances locked by tests (`kitten/sitting = 3`, `flaw/lawn =
2`, `gumbo/gambol = 2`, empty edges, Cyrillic, symmetry).

## Session 3 — HTTP probe, lint chain, broken-HEAD repair (this change)

### 1. `http_ok` is now a real HTTP probe

The old probe (and session 1's fix) only did `TcpStream::connect_timeout`
— a bare TCP handshake. Any unrelated service on the port (a stale
Chromium, an SSH server, Postgres) reported "healthy", because the probe
never asked a question. Now:

1. Connect (300 ms timeout, read/write timeouts set on the socket).
2. `GET /healthz HTTP/1.0` + `Host:` + `Connection: close` is written,
   with the **path taken from the URL** (previously ignored).
3. The first bytes are parsed by `parse_status_line()`; the answer
   counts only if it is a valid HTTP status line **with a 2xx code** —
   a foreign HTTP server (404), a wedged daemon (500), a non-HTTP
   speaker (garbage) and a silent socket (read timeout) all report down.
4. Scheme letter-case is irrelevant (`HTTP://`, `Https://` —
   `scheme_parts()` lowercases for the check; fragments `#…` are cut).
5. `https://` URLs fall back to the TCP-handshake check: std has no TLS
   and pulling a TLS stack into a local-daemon probe is overkill (the
   daemon URL is always `http://`; tracked as UWA-102 in ISSUES).

Tests: a responder thread answering 200 (three probes: IP, `localhost`
resolution, uppercase scheme), foreign service rejections (garbage
responder, 404, 500), silent-socket timeout, https TCP fallback,
`parse_status_line` / `request_target` / case-insensitive `host_port`.

### 2. Forbid-lint chain, continued

After `uwa-core`, the same treatment for the next crates (lib code):

| Crate | Before | After | Notable rewrites |
|-------|-------:|------:|------------------|
| `uwa-core` | 15 | **0** | session 2 |
| `uwa-extract` | 13 | **0** | `parse_path` fully char-driven (no byte offsets at all — unclosed `[` handled by the iterator); `json_path_str` without `unwrap`; pipeline DOM fallback via `let-else`; Mutex poisoning recovered via `unwrap_or_else(into_inner)` in the site parsers |
| `uwa-tools` | 17 | **0** | the three tool-call scanners (`scan_markers`, `scan_fence`, `scan_bare`) now walk with `str::get(..)` + `saturating_add` instead of `text[i..]` + raw offsets; `safe_hold` prefix math via `saturating_sub` |
| `uwa-config` | 7 (build script) + 3 | **0** | build.rs fails loudly via `die()` → `process::exit(101)` (panic's code) with readable messages — the design intent ("hard error on missing provider files") survives without `expect` |
| `uwa-resilience` | 5 | **0** | circuit breaker recovers from mutex poisoning; retry loop without `expect` (loop provably runs ≥ 1 attempt) |
| `uwa-session` | 8 | **0** | round-robin rotation via `checked_rem` + `get()`; pinning prefix/suffix decisions via `get(..n)` with explicit Full-replay fallback |
| `uwa-history` | 24 | 24 | open — see UWA-101 |
| `uwa-browser` / `uwa-providers` / `uwa-api` | 13 / 13 / 13 | unchanged | open — see UWA-101 |
| `uwa-testkit` | 24 | unchanged | open — see UWA-101 |
| `uwa-bin` (lib-side) | 1 | 1 | open — see UWA-101 |

### 3. Broken-HEAD repair (introduced by the conformance commit `78027b8`)

The registry commit landed code that did not compile and a failing test:

- `uwa-config/registry.rs` used `tracing::warn!` and `serde_json::Value`
  without declaring the dependencies → **E0433, `uwa-config` (and every
  dependent) did not build**. Fixed by adding both to
  `crates/uwa-config/Cargo.toml`.
- `registry_fills_missing_selectors_for_known_provider` failed:
  *root cause 1* — in all four `providers/*.toml` files `url_patterns`
  sat **below** `[meta]`, i.e. inside the `meta` table, so the top-level
  field the overlay reads stayed empty; moved above `[meta]`.
  *root cause 2* — `ProviderCfg.url_patterns` / `.capabilities` were
  required serde fields, so the strict parse failed **before** the
  registry overlay ("fill what the user left unset") could run; both now
  `#[serde(default)]`, and validation still rejects an empty pattern
  list when neither user nor registry provided one.
- `panic!` on malformed built-in entries → `tracing::error!` + skip
  (a library constructor must not abort the daemon on schema drift; the
  drift stays loudly visible in logs).

`cargo test -p uwa-config`: 40/40 green.

## Method notes

- **Certify the tree you ship.** With a concurrent session editing the
  checkout, every batch was verified in a detached worktree containing
  exactly HEAD + the session's files.
- **Refactor over suppression.** `forbid` lints cannot be `allow`-ed;
  the fixes remove the flagged constructs (`get()`/iterators/
  `saturating_*`/`unwrap_or`) rather than argue with the linter.
- **Pure helpers first.** `follow_chunk`, `host_port`, `request_target`,
  `parse_status_line` are pure functions with unit tests; the IO shells
  around them stay thin.
- str-range-slicing (`text[i..]`) is *not* covered by the forbid list
  (`clippy::string_slice` is pedantic, not forbidden) — only Vec/slice
  indexing is. The chain work still removed most raw offsets where the
  rewrite made the code more obviously total.

## Session 4 — ChatGPT/Gemini rewrite corruption + dead code (this change)

### 1. ChatGPT / Gemini parser rewrite corrupts accumulated text (UWA-141)

Both `ChatGptParser` and `GeminiParser` consume *cumulative* text from
the server and emit deltas by prefix-diffing. When the model rewrites
(regenerate / edit / candidate switch), the new text is **not** a
prefix-extension of the old one — the diff function returned the whole
new text, and the pipeline accumulator appended it to the stale prefix,
producing concatenated garbage (e.g. `"HelloGoodbye world"` instead of
`"Goodbye world"`).

**Root cause:** the type `NetDelta(pub String)` could not express
"replace everything accumulated so far".

**Fix:** introduced `NetDelta::{Append,Replace}` enum:

- `Append(s)` — normal growth, suffix appended to accumulated text
- `Replace(s)` — site rewrote its message; accumulator clears and
  replaces with `s`

The diff functions (`diff` in chatgpt, inline in gemini) now emit
`Replace` when `!new_full.starts_with(last)`. The pipeline accumulator
(`pipeline.rs`) handles `Replace` by `text = s` instead of `push_str`.

**Files changed:**
- `crates/uwa-extract/src/net.rs` — `NetDelta` enum, `as_str()`, `is_empty()`, `collect_deltas()`, `DefaultNetExtractor`
- `crates/uwa-extract/src/parsers/mod.rs` — trait returns `Option<NetDelta>`
- `crates/uwa-extract/src/parsers/chatgpt.rs` — `diff()` returns `NetDelta`, `.get()` instead of slicing
- `crates/uwa-extract/src/parsers/gemini.rs` — `extract_delta()` returns `Replace` on candidate switch, `.get()`
- `crates/uwa-extract/src/parsers/claude.rs` — wraps in `NetDelta::Append`
- `crates/uwa-extract/src/pipeline.rs` — accumulator honours `Replace`
- `crates/uwa-providers/src/generic.rs` — uses `d.as_str().to_string()`

**Tests added (all pass):**
- `rewrite_signals_replace_not_append` (chatgpt unit)
- `rewrite_delta_is_replace_variant` (chatgpt unit)
- `diff_handles_multibyte_boundary` (chatgpt unit — Cyrillic)
- `candidate_switch_signals_replace` (gemini unit)
- `chatgpt_rewrite_discards_stale_prefix` (e2e: CDP → SSE → parser → accumulator)
- `replace_delta_clears_accumulated_text` (pipeline unit with fake extractor)

All existing tests pass; no behaviour change for normal prefix-extension
streams (Claude, standard SSE, JSON path).

### 2. Dead code in `fenced_regions` (UWA-142, trivial)

`crates/uwa-tools/src/parser.rs:283`:
```rust
let is_fence_boundary = trimmed.starts_with("```") || trimmed == "```";
```
The right side is always covered by the left — a line equal to ```````
also `starts_with("```")`. Removed the redundant `|| trimmed == "```"`.

No test changes; full suite green.
