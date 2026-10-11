# Why UWA — Architectural Deep Dive

UWA differs from typical "free web AI API" Python projects **much more deeply** than a simple rewrite. This is not "Python rewritten in Rust" — it's a fundamentally different architecture.

---

## 1. One `GenericProvider`, Not a Zoo of Site Adapters

In typical Python projects, every site gets its own class: `ChatGPTProvider`, `ClaudeProvider`, `GeminiProvider`, each with custom logic, selectors, waits. UWA has exactly **one** `GenericProvider` (`crates/uwa-providers/src/generic.rs`), and all site-specific data lives in TOML:

- `url_patterns`
- `selectors` (input, send_button, stop_button, assistant_message, conversation_root)
- `capabilities` (streams, tool_calls, vision, max_context_tokens)
- `extraction` strategy
- `net` / `finisher` tuning
- `presets` (flash/pro variants)
- `workflow` (declarative action list)
- `media` (audio capture, video detection)
- `fallback_model`
- `max_concurrency`
- `max_tool_retries`

Adding a new site = editing TOML + fixture HTML, **no code changes**. Python projects almost never do this.

> **Code:** `crates/uwa-providers/src/generic.rs:45-75` — `GenericProvider` reads everything from `ProviderCfg`; `send_message`, `send_multimodal`, `wait_response`, `wait_response_stream` are all driven by config fields.

---

## 2. Declarative Workflows Instead of Hardcoded Steps

In `uwa-core/src/workflow.rs` there's a full declarative language:

- `CLICK`
- `FILL_INPUT`
- `WAIT`
- `STREAM_WAIT`
- `KEY_PRESS`
- `IF / ELSE`
- `GROUP`
- `CAPTURE`

This lets you describe a site's scenario in config, not in Python code. Typical Python projects either hardcode steps or lack this abstraction entirely.

> **Code:** `uwa-core/src/workflow.rs:50-160` — `Action` enum with tagged serialization; `Workflow` is a transparent list of actions. Execution in `crates/uwa-providers/src/workflow/mod.rs` (not shown here, but `GenericProvider::send_message` at `generic.rs:230-245` delegates to `WorkflowRunner` when `cfg.workflow` is non-empty).

---

## 3. Bridge Semantics for Tool Calls

This is arguably the main ideological difference. UWA **does not execute tools**. It:

- Accepts `tools` from the client
- Injects their schemas into the browser LLM's prompt
- Parses `<tool_call>` / `</tool_call>- returns standard OpenAI `tool_calls` to the client
- supports schema validation and correction retry (`max_tool_retries`)

Typical Python projects either don't support tools, try to execute them themselves, or drag in an MCP server. UWA remains a transparent bridge: the client decides what to do with tool calls.

> **Code:** `crates/uwa-tools/src/parser.rs:1-100` — `parse()` extracts `tool_call` markers, ```tool_call```/```json``` fences, bare JSON objects, validates against an allow-list; `crates/uwa-tools/src/schema.rs:1-80` — minimal JSON Schema validator (type, properties, required, enum, items, additionalProperties); `crates/uwa-providers/src/generic.rs:200-230` — schemas injected into prompt, results parsed back, unknown tools passed through (bridge semantics).

---

## 4. Multiple API Surfaces, Not Just OpenAI

UWA exposes several endpoints:

- `POST /v1/chat/completions` — OpenAI Chat Completions
- `POST /v1/messages` — Anthropic Messages
- `POST /v1/messages/count_tokens`
- `POST /v1/responses` — OpenAI Responses API
- `GET /v1/models`
- `GET /v1/provider/status`
- `GET /api/pool/status`

> **Code:** `crates/uwa-api/src/routes/` — directory listing shows `chat_completions.rs`, `messages.rs`, `count_tokens.rs`, `responses.rs`, `models.rs`, `provider_status.rs`, `pool_status.rs`. Each is a separate axum handler.

---

## 5. Real Streaming and True TTFT

In `uwa-extract` there's a network-first pipeline with an SSE parser and per-site decoders:

- ChatGPT cumulative → diff
- Claude `content_block_delta`
- Gemini `batchexecute`

This gives real token deltas, not "pseudo-streaming" from chunked pre-rendered text. TTFT is tracked in history/stats.

> **Code:** `crates/uwa-extract/src/net.rs:1-200` — `SseParser` incremental SSE frame parser; `DefaultNetExtractor::deltas()` spawns a tokio task that forwards `NetDelta` items; `crates/uwa-extract/src/parsers/mod.rs:30-55` — `parser.rs` has the registry and three per-site decoders (`ChatGptParser` does cumulative-diff, `ClaudeParser` handles `content_block_delta`, `GeminiParser` cuts XSSI prefix `)]}'`).

---

## 6. Finisher Instead of sleep(5)

uwa-extract/src/finisher.rs has Finisher which combines multiple independent signals:

- stop button disappeared
- network Finished event
- DOM stable for dom_stable_ms
- hard timeout (max_wait_ms)

Plus min_wait_ms and max_wait_ms.

> **Code:** crates/uwa-extract/src/finisher.rs:40-130 — Finisher struct with wait() method; FinisherCfg struct; FinishSignal enum with Stop, Timeout, Stable, NetworkDone.

---

## 7. Session Manager with Tab Pinning

uwa-session/src/manager.rs:
conversation_id derived from first messages; conversations pinned to a tab; per-conversation mutex; idle eviction; capacity eviction that never evicts a busy session; unhealthy tab recovery.

> **Code:** crates/uwa-session/src/manager.rs:95-140 — acquire() picks least-leased tab; evict_idle() evicts only entries with strong_count <= 2; evict_if_over_capacity() skips busy entries; recover() drops sessions whose tab fails health check.

---

## 8. TabPool with Health, Round-Robin, and LRU/TTL

crates/uwa-browser/src/tabpool.rs — round-robin, per-tab mutex, health-check, idle TTL eviction, mark_unhealthy/mark_healthy/acquire_specific.

> **Code:** crates/uwa-browser/src/tabpool.rs:50-200 — TabPool::acquire() does round-robin (self.rr.fetch_add(1) % ids.len()); evict_idle() removes tabs older than idle_ttl; acquire_specific() for named tab; TabGuard restores health on drop.

---

## 9. Resilience: Circuit Breaker, Semaphores, Retry

uwa-resilience: circuit breaker with rolling window, half-open probe with self-release; ProviderSemaphores with max_concurrency from config; retry with exponential backoff and jitter.

> **Code:** crates/uwa-resilience/src/circuit.rs:1-200 — CircuitBreaker with allow()/record_success()/record_failure(); VecDeque rolling window; half_open_probe_at reclaims cancelled probes. crates/uwa-resilience/src/retry.rs — exponential backoff with jitter. crates/uwa-resilience/src/semaphore.rs — ProviderSemaphores.

---

## 10. Observability: history, stats, dashboard, logs

uwa-history writes append-only JSONL, keeps a ring buffer, supports retention and computes:
- p50/p95
- avg TTFT
- error_rate per provider
- tab utilization
- requests per minute
- finish reasons

Plus a Svelte dashboard: Dashboard, History, Sessions, Config, Logs, Debug, Playground. Live SSE log streaming.

> **Code:** crates/uwa-history/src/stats.rs:1-200 — compute() over a window of records, computing p50/p95 avg_total_ms, avg_ttft_ms, p50_ttft_ms, p95_ttft_ms per provider; crate/uwa-history/src/store.rs — append-only JSONL writer with retention; crate/uwa-history/src/record.rs — RequestRecord with RoundTiming and finish_reason. Dashboard lives in crates/uwa-api/static/src/app/ (Svelte).

---

## 11. Operational Maturity

uwa-bin is not just python main.py:
- uwa daemon start|stop|status|logs
- uwa service install|uninstall|status for systemd / launchd / sc.exe
- uwa update with GitHub Releases and SHA256
- uwa doctor
- pid-file, graceful shutdown, drain

> **Code:** crates/uwa-bin/src/commands/daemon.rs:1-180 — start() writes pid + pgid files, uses setsid, stop() sends SIGTERM to process group then SIGKILL after 15s, logs --follow polls daemon.log; crates/uwa-bin/src/commands/doctor.rs — run() prints config, bind, api_key, providers, models, detected chromium, CDP status; crates/uwa-bin/src/commands/update.rs — update downloads .tar.xz, verifies .sha256, atomic rename.

---

## 12. UI and Ready Client Snippets

uwa-api/static contains a full Svelte UI. clients.ts provides ready configs for:
- Cursor, Continue, Codex CLI, Claude Code, Aider, ZCode, AutoClaw

> **Code:** crates/uwa-api/static/src/app/ — Svelte dashboard app; crates/uwa-api/static/src/lib/clients.ts — client snippet generator that builds configs with real base_url, key and model. Python projects usually offer only README with curl.

---

## 13. Stealth and OOPIF

UWA:
- injects stealth scripts before JS execution on the page
- works with OOPIF via Target.setAutoAttach({ flatten: true })
- eval_in_frame for cross-origin iframes
- connects to OOPIF via separate WebSocket and injects stealth there

Python projects often use undetected-chromedriver or cannot handle cross-origin iframes.

> **Code:** crates/uwa-stealth/src/apply.rs:40-90 — apply_pack() wraps each script in IIFE with try/catch via wrap(): (function(){try{...}catch(_e){}})(); before eval_early (before page JS). crates/uwa-browser/src/oopif.rs:60-250 — OopifRegistry keyed by frameId (not targetId) for cross-origin nav stability; install_auto_attach() sets flatten: true. crates/uwa-browser/src/oopif_ws.rs — separate WebSocket connection to OOPIF webSocketDebugUrl.

---

## 14. Multimodality and Media

- attachments.rs: data URI / HTTP image -> temp file -> attach via file input or drag-drop -> cleanup
- file_attach.rs: file paste for long prompts
- media_capture.rs: audio capture via Web Audio hook, video detection

Python projects are usually text-only.

> **Code:** crates/uwa-providers/src/attachments.rs:30-120 — extract_attachments() from data URI or HTTP to temp file, cleanup(); crates/uwa-providers/src/file_attach.rs — attach_file_via_dom() / attach_file_via_dragdrop(); crates/uwa-browser/src/media_capture.rs — audio capture patches HTMLMediaElement.prototype.play, creates AudioContext + MediaRecorder, routes to both ctx.destination and recorder.

---

## 15. Config Validation and Error Normalization

uwa-config validates:
- loopback/API-key invariant
- model aliases point to existing providers
- provider key == name
- required selectors present
- default_preset exists
- CDP ws_url scheme
- HH:MM for scheduled restart

Errors are shaped as OpenAI or Anthropic forms, with Levenshtein-based Did-you-mean hints.

> **Code:** crates/uwa-config/src/lib.rs (Config::validate, 250-310 and 330-420) — validates loopback/API-key invariant, model aliases existence, provider name == key, required selectors (input, send_button, assistant_message), default_preset existence, CDP ws_url scheme, HH:MM format, proxy address parsable. All checks run on every load_from_str/load_from_path.

---

## 16. Routing and Models

- model_aliases
- resolve_model with vendor prefix stripping (openai/gpt-4o)
- fallback_model
- groups: round_robin, failover, hash_conversation
- provider presets (e.g. Gemini flash/pro)

This is a mini-router, not one-site-one-model.

> **Code:** crates/uwa-config/src/lib.rs:430-470 — resolve_model() strips vendor prefix (split_once('/')), checks model_aliases, then stem fallback to a provider with matching name prefix that declares fallback_model. crates/uwa-config/src/groups.rs — GroupCfg with round_robin/failover/hash_conversation strategies, validate() checks members reference real provider+preset. crates/uwa-config/src/preset.rs — PresetCfg overlay with effective() merging selectors, capabilities, finisher.

---

## 17. Hint-based Routing per Path

In routing.rs there is HintExtractor — implements FromRequestParts and extracts RoutingHint from extensions (set by middleware) or from X-UWA-Provider header. Enables:
- POST /url/{domain}/v1/chat/completions — routing by domain
- POST /tab/{tab_id}/v1/chat/completions — pinning to a specific tab
- X-UWA-Provider: chatgpt — provider override

> **Code:** crates/uwa-api/src/routing.rs:30-120 — HintExtractor struct implements FromRequestParts, extracts RoutingHint from extensions or X-UWA-Provider header; from_url_path() middleware sets hint.provider = url:{domain}; from_tab_path() sets hint.tab; resolve_hint() maps url:<domain> to provider name via config url_patterns.

---

## 18. Different Error Shapes for Different APIs

/v1/messages responds in Anthropic format ({type: error, error: {type, message}}), because Claude Code and SDKs parse it. All other API uses OpenAI format. Solved in messages.rs via anthropic_error_response.

> **Code:** crates/uwa-api/src/routes/messages.rs — anthropic_error_response() maps UwaError to Anthropic error shape; crates/uwa-core/src/error.rs — UwaError::openai_shape() formats as {error: {message, type, code}}. Python projects give either OpenAI shape everywhere or break Anthropic clients.

---

## 19. ModelNotFound with Levenshtein Hint

In uwa-core/src/error.rs there is ModelNotFound:
- carries the full list of available models
- offers the closest by prefix or by Levenshtein distance
- produces Did-you-mean `deepseek-chat`?

> **Code:** uwa-core/src/error.rs:1-60 — ModelNotFound::new(requested, available) computes Levenshtein distance and prefix match to find closest model. UwaError::openai_shape() formats the error with "Did you mean" suggestion. Python response: 404 Model not found.

---

## 20. resolve_model Cleans Vendor Prefixes

In uwa-config/src/lib.rs:
- openai/gpt-4o -> alias gpt-4o
- deepseek-flash -> stem-fallback to deepseek-chat if provider declares fallback_model

Covers Aider and OpenAI-compatible SDKs that send vendor prefixes.

> **Code:** crates/uwa-config/src/lib.rs:430-470 (resolve_model) — strips vendor prefix via split_once('/'); then checks exact alias; then stem fallback: splits on first non-alphanumeric, lowercases, finds provider whose name starts with the stem, checks fallback_model field.

---

## 21. Loopback Bypass + Constant-Time Comparison

In middleware.rs:
- if bind is loopback, any Authorization accepted
- api_key compared via subtle::ConstantTimeEq
- supports ?api_key= in query string (EventSource cannot set headers)
- custom percent-decoder

This is the #1 cause of failures in free web AI projects.

> **Code:** crate/uwa-api/src/middleware.rs:40-120 — require_api_key() returns Ok next.run() immediately if is_loopback_bind (line 50-52); compares via p.as_bytes().ct_eq(expected.as_bytes()) (line 95); handles ?api_key= query param (line 70-82) with custom percent_decode() (line 100-116).

---

## 22. RequestRecorder Step-by-Step Metrics

In uwa-api/src/history.rs lives a full recorder:
- mark_send_start / mark_send_end
- mark_first_delta (TTFT)
- mark_wait_end
- mark_acquisition_ms
- increment_round / record_tool_calls
- RoundTiming per round
- finish_success / finish_error

> **Code:** crate/uwa-api/src/history.rs:1-200 — RequestRecorder with mark_send_start(), mark_first_delta(), finish_success(), finish_error(); RoundTiming struct tracks acquisition_ms, send_ms, wait_ms, ttft_ms.

---

## 23. safe_emit_end — Stream Hygiene

In parser.rs there is safe_emit_end() which returns the byte index up to which it is safe to stream to the client without leaking half a tool-call marker:
- on a full marker, returns the marker start
- on a partial marker (e.g. '<tool_') — holds back
- correct UTF-8 boundary handling

> **Code:** crates/uwa-tools/src/parser.rs:190-220 — safe_emit_end() finds the earliest tool marker index; if none complete, computes longest proper prefix of each marker that buf ends with, and returns buf.len() - hold. This prevents clients from seeing partial markers in deltas.

---

## 24. already_injected for System Prompt

uwa-tools/src/prompt.rs detects that a system message already contains tool markers and does not duplicate the preamble when resending history.

> **Code:** crates/uwa-tools/src/prompt.rs — prompt module builds the system prompt with tool schemas; checks if message text already contains tool markers before re-injecting.

---

## 25. compose_browser_turn Takes Only the Tail

In feedback.rs, the system does not resend the entire history to the browser — only messages after the last assistant. This protects against re-sending old turns and saves context. Also explicitly enables role:system in the turn that would otherwise be lost.

> **Code:** crates/uwa-tools/src/feedback.rs — compose_browser_turn() iterates only over messages after the last assistant role; inserts a system message for tool schemas that would otherwise be dropped.

---

## 26. Own JSON-Schema Validator Without jsonschema

In schema.rs — a minimal validator in ~80 lines: type, properties, required, enum, items, additionalProperties. Ignores unknown keywords instead of pulling the jsonschema crate (~1MB of compiled tables).

Combined with validate_calls, yields three outcomes: Ok, Failed {corrections}, UnknownTools {names}. Unknown tools are skipped (bridge semantics).

> **Code:** crates/uwa-tools/src/schema.rs:1-80 — validate_arguments() checks type, properties, required, enum, items against a Value; validate_inner() recurses; unknown keywords ignored (returns Ok). Unknown tool names are passed through.

---

## 27. RetryCfg::new() Due to #[non_exhaustive]

RetryCfg is marked #[non_exhaustive], so it cannot be constructed literally from another crate. There is an explicit constructor RetryCfg::new(attempts) with a doc-test.

> **Code:** crate/uwa-resilience/src/retry.rs — RetryCfg struct has #[non_exhaustive]; RetryCfg::new(attempts) provides a public constructor; doc-test shows how to tune fields. This level of API ergonomics is not found in Python projects.

---

## 28. Self-Healing Circuit Breaker

In circuit.rs:
- rolling window via VecDeque<Instant>
- half-open probe with deadline (half_open_probe_at)
- if probe is cancelled (client disconnected / task aborted), the slot self-releases via cooldown
- allows multiple half-open probes via half_open_max_calls

> **Code:** crate/uwa-resilience/src/circuit.rs:140-200 — Inner struct tracks half_open_probe_at deadline; allow() in HalfOpen state checks if probe is stale (elapsed >= cooldown) and reaps it, enabling new probes. Without this, a cancelled probe would hold the half-open slot forever.

---

## 29. Arc::strong_count as Busy-session Marker

In uwa-session/src/manager.rs:
- holders = Arc::strong_count(&entry).saturating_sub(2) — accounting for map + snapshot
- capacity eviction never drops an entry with a live handle
- idle eviction checks strong_count <= 2

> **Code:** crate/uwa-session/src/manager.rs:175-185 — evict_idle() filters on Arc::strong_count(entry) <= 2; evict_if_over_capacity() skips entries where Arc::strong_count(&entry) > 2 (busy). SessionHandle holds an Arc clone, incrementing strong_count.

---

## 30. Least-Leased Tab Selection

When creating a new session, UWA selects the least-leased tab, not just the first one:

    min_by_key(|t| lease_count.get(*t).copied().unwrap_or(0))

Plus round-robin cursor for tie-breaking. Python projects always take tabs[0].

> **Code:** crate/uwa-session/src/manager.rs:115-125 — acquire() builds lease_count HashMap by scanning all entries, then uses min_by_key to pick the tab with the fewest active leases. Round-robin offset via self.rr.fetch_add(1) for tie-breaking.

---

## 31. Provider Presets

preset.rs implements an overlay: top-level config + named preset. Routing:
- /url/{domain}/v1/... uses default_preset
- /url/{domain}/{preset}/v1/... uses specific preset
- X-UWA-Preset: pro header override

effective() merges selectors, capabilities, finisher.

> **Code:** crate/uwa-config/src/preset.rs — PresetCfg struct with optional overrides for url_patterns, selectors, capabilities, extraction, net, finisher; effective() function applies PresetOverlay on top of ProviderCfg. ProviderCfg::effective(preset) calls preset::effective_provider().

---

## 32. Routing Groups

Groups.rs: round_robin, failover, hash_conversation. A group is a list of (provider, preset) pairs with validation that the preset exists.

> **Code:** crate/uwa-config/src/groups.rs:1-88 — GroupCfg with strategy field (round_robin/failover/hash_conversation) and members list; GroupMember validates preset exists on provider; Config::validate() checks all groups.

---

## 33. Scheduled Restart

ScheduledRestartCfg with at = "04:00", drain_secs, validated HH:MM.

> **Code:** crate/uwa-config/src/lib.rs:75-80 — ScheduledRestartCfg struct; crate/uwa-config/src/lib.rs:410-415 in validate() calls validate_hhmm(at)? when scheduled_restart.enabled. crate/uwa-bin/src/commands/scheduled_restart.rs — runtime scheduling logic.

---

## 34. Chrome Proxy via chrome_args()

ProxyCfg::chrome_args() generates --proxy-server= and --proxy-bypass-list=. On CDP connection to already-running Chrome, warns that proxy does not apply.

> **Code:** crate/uwa-config/src/features.rs — ProxyCfg struct with enabled, address, bypass fields; chrome_args() method returns Vec<String> of Chrome CLI flags. crate/uwa-config/src/lib.rs:460-463 validates proxy.address with Url::parse.

---

## 35. Prompt Padding

PromptPaddingCfg inserts wrapper markers around the prompt (segments_per_side, randomize, random_chars). Default off — degrades quality.

> **Code:** crate/uwa-config/src/features.rs:30-50 — PromptPaddingCfg with enabled, segments_per_side, randomize, random_chars fields; apply(&self, text: &str) -> String inserts markers. crate/uwa-providers/src/generic.rs:95 — build_send_text() applies prompt padding.

---

## 36. FilePasteCfg with reacquire

If prompt exceeds threshold_bytes (default 130KB), it goes to a temp file, attached, then input selector is re-queried — some sites rebuild DOM after file load.

> **Code:** crate/uwa-config/src/features.rs:55-70 — FilePasteCfg with enabled, threshold_bytes, hint_text, reacquire_input_after_upload; crate/uwa-providers/src/generic.rs:160-205 — send_via_file() writes temp file, attaches via file input or drag-drop, optionally reacquires input selector after upload.

---

## 37. MediaCfg with Autoplay Policy

When audio_capture_enabled, --autoplay-policy=no-user-gesture-required is added to Chrome args, otherwise Chrome refuses to play without a user gesture.

> **Code:** crate/uwa-config/src/features.rs:75-90 — MediaCfg with audio_capture_enabled, video_detection_enabled fields; crate/uwa-config/src/features.rs chrome_args() adds --autoplay-policy=no-user-gesture-required when audio_capture_enabled. crate/uwa-bin/src/main.rs — merges config chrome_args into Chrome launch.

---

## 38. Audio Capture via HTMLMediaElement.prototype.play Patch

audio_capture_start.js:
- patches play()
- creates AudioContext + MediaStreamAudioDestinationNode
- MediaRecorder writes chunks as base64
- routes both to ctx.destination (user hears) and recorder

Video is never auto-downloaded — only detection.

> **Code:** crate/uwa-browser/src/media_capture.rs:1-180 — MediaCapture impl patches HTMLMediaElement.prototype.play, creates AudioContext and MediaRecorder, captures chunks; stop_capture() collects base64 chunks into a blob.

---

## 39. Blob URL Fetch via eval

fetch_blob() takes a blob: URL (unreachable from outside the page) via page.eval with fetch() inside the page and base64-encodes in 8192-byte chunks.

> **Code:** crate/uwa-browser/src/oopif.rs:300-340 or crate/uwa-browser/src/frame.rs — fetch_blob() evaluates fetch(blobUrl).arrayBuffer() inside page context, base64-encodes result in 8KB chunks via page.eval.

---

## 40. eval_in_frame via Isolated World

For same-process iframe: Page.createIsolatedWorld + Runtime.evaluate in that world. For cross-origin OOPIF: separate WebSocket directly to webSocketDebugUrl (chromiumoxide 0.7 cannot do session-scoped commands).

> **Code:** crate/uwa-browser/src/frame.rs:30-80 — eval_in_frame() for same-process iframes uses Page.createIsolatedWorld + Runtime.evaluate; crate/uwa-browser/src/oopif_ws.rs — for cross-origin OOPIF, opens a direct WebSocket to the iframe target webSocketDebugUrl.

---

## 41. resolve_ws_url with Content-Length Parsing

In oopif_ws.rs there is a minimal HTTP client for /json/list with two phases: headers to Content-Length, then body. One of the first implementations read once and lost the webSocketDebugUrl (it is in the second array element).

> **Code:** crate/uwa-browser/src/oopif_ws.rs:1-120 — custom HTTP client reads headers first to find Content-Length, then reads exactly that many body bytes; parse_json_list() extracts webSocketDebugUrl from the array of targets.

---

## 42. One-shot OOPIF WebSocket

Connecting to OOPIF is one-shot: connect, Page.enable, addScriptToEvaluateOnNewDocument for each script, Runtime.runIfWaitingForDebugger, then close. Cheaper than keeping connections alive.

> **Code:** crate/uwa-browser/src/oopif_ws.rs:200-280 — setup_oopif() connects via WebSocket, sends Page.enable, injects scripts via addScriptToEvaluateOnNewDocument, calls Runtime.runIfWaitingForDebugger, then closes the connection (no keep-alive).

---

## 43. wrap Stealth in IIFE with try/catch

apply.rs wraps each stealth script in (function(){try{...}catch(_e){}})(); — guarantees a broken patch does not break navigation.

> **Code:** crate/uwa-stealth/src/apply.rs:60-75 — wrap(body: &str) -> String returns format!("((function(){{try{{{body}}}catch(_e){{}}}})();"); apply_one() uses it before calling eval_early or eval.

---

## 44. reap_stale_chrome with Whitelist argv[0]

In cdp_smoke.rs, before spawning, only processes are killed where:
- cmdline contains the profile marker (uwa-cdp-smoke-*)
- argv[0] is one of BROWSER_BASENAMES (chrome/chromium/...)
- pid is not self

A naive pkill -f would kill editors that mention the marker in args.

> **Code:** crate/uwa-browser/src/js/nodriver/cdp_smoke.rs — reap_stale_chrome() checks process cmdline for profile marker AND argv[0] in BROWSER_BASENAMES; skips std::process::id(); only kills matching processes.

---

## 45. PR_SET_PDEATHSIG in Tests

If a test binary is killed (kill -9, OOM, CI timeout), the Drop guard will not fire. pre_exec(prctl(PR_SET_PDEATHSIG, SIGKILL)) makes the kernel kill Chrome for us. Plus killpg via process_group(0) — Chrome becomes a process group leader, and Drop kills the whole tree.

> **Code:** crate/uwa-browser/src/js/nodriver/cdp_smoke.rs:50-110 — pre_exec closure calls prctl(PR_SET_PDEATHSIG, SIGKILL, 0, 0, 0); process_group(0) makes the child a group leader; Drop impl sends SIGKILL to the process group.

---

## 46. Panic -> Kill Process Group

In uwa-bin/src/main.rs there is a panic hook that sends killpg(-pgid, SIGKILL) if child processes were spawned. Otherwise a panic would leave Chromium alive.

> **Code:** crate/uwa-bin/src/main.rs — install_panic_hook() sets a hook that, on panic, calls killpg(-pgid, SIGKILL) to kill the entire process group (daemon + Chrome + sidecar).

---

## 47. Daemon with pgid and --force

daemon.rs writes both pid AND pgid, uses setsid via pre_exec, supports --force, stop sends SIGTERM to group then waits 15s then SIGKILL, logs --follow uses polling.

> **Code:** crate/uwa-bin/src/commands/daemon.rs:80-200 — start() writes pid_path and pgid_path; pre_exec calls setsid(); stop() reads pgid, sends kill(-pgid, SIGTERM), waits 15s, then kill(-pgid, SIGKILL); logs() with follow polls daemon.log every 500ms.

---

## 48. doctor as First Line of Diagnostics

uwa doctor prints: config path, bind address, api_key set/not set, provider list, model count, detected Chromium path, CDP readiness.

> **Code:** crate/uwa-bin/src/commands/doctor.rs:1-60 — run() loads config, prints providers and models, calls browser::detect_chrome() and browser::cdp_ready() to report Chromium status.

---

## 49. uwa update with SHA256

Downloads .tar.xz from GitHub Releases, downloads .sha256 sidecar, verifies hash, extracts, atomically replaces binary via rename. Unix allows rename of running binary (kernel keeps inode). Windows: rename old, move new.

> **Code:** crate/uwa-bin/src/commands/update.rs — update() downloads release archive + sha256, verifies hash via sha2 crate, extracts to temp dir, renames into place atomically.

---

## 50. UI Auto-build with Atomic Guard

maybe_start_ui_build uses AtomicU8 (0=none, 1=building, 2=ok, 3=failed) + compare_exchange, so parallel npm install does not start. Timeout 600s. serve_index returns 503 if dist not built yet.

> **Code:** crate/uwa-bin/src/main.rs:60-120 — maybe_start_ui_build() uses AtomicU8 state with compare_exchange to prevent concurrent builds; spawn_join() runs npm install + vite build with 600s timeout. serve_index() returns 503 while build state is 1 or 3.

---

## 51. serve_index_for_test as Test Seam

Public function under #[doc(hidden)], so tests can call serve_index() without standing up the full router.

> **Code:** crate/uwa-api/src/lib.rs — serve_index_for_test() returns the index handler; test missing_ui_without_build_script_answers_fast checks it 503s before the UI is built.

---

## 52. playwright.config.ts with Mock Server

E2E tests run against mock-server.mjs on 127.0.0.1:9876 with /__test__/reset endpoint. reuseExistingServer: !CI.

> **Code:** crate/uwa-api/static/playwright.config.ts — configures localhost:9876 mock server; crate/uwa-api/static/tests/mock-server.mjs — serves fixture HTML, provides /__test__/reset. Full pretest:e2e runs UI build first.

---

## 53. tests/clients/ as Separate Directory

aider.rs, claude_code.rs, codex_cli.rs, continue_vscode.rs, cursor.rs, openai_sdk.rs — each replicates the wire shape of a specific client. Contract tests that break before users notice.

> **Code:** crate/uwa-api/tests/clients/ — directory with one test file per client, each constructing a real ChatCompletionRequest / MessagesRequest matching exact wire shape for Aider, Claude Code, Codex CLI, Continue, Cursor, OpenAI SDK.

---

## 54. insta Snapshots for Extraction

dom_snapshots.rs and snapshots.rs capture extracted text from HTML fixtures. Changes show up in cargo insta review.

> **Code:** crate/uwa-providers/tests/dom_snapshots.rs — insta::assert_snapshot! on extracted text from HTML fixtures; cargo insta review for diff inspection.

---

## 55. proptest with PROPTEST_CASES Override

proptest_net.rs and proptest_parser.rs use env var PROPTEST_CASES for nightly runs with 10000 cases. Property tests cover:
- chunk-invariance of SSE parser
- round-trip of tool call
- allow-list enforcement
- no panics on arbitrary input

> **Code:** crate/uwa-extract/tests/proptest_net.rs and crate/uwa-tools/tests/proptest_parser.rs — #[test] functions using proptest! macro; nightly runs set PROPTEST_CASES=10000. Property tests cover chunk-invariance, round-trip, allow-list, panic safety.

---

## 56. fixture_server_smoke.rs + e2e_fixture.rs

One test checks fixture server returns correct HTML. The other runs full pipeline through real Chromium. This split lets you diagnose: is the fixture broken or the pipeline?

> **Code:** crate/uwa-providers/tests/fixture_server_smoke.rs — starts mock fixture server, asserts it returns expected HTML (tests the fixture itself); crate/uwa-providers/tests/e2e_fixture.rs — launches real Chromium, drives the provider through the fixture HTML, checks extraction output (tests the pipeline).

---

## 57. config.example.toml as Source of Truth

config_example.rs loads ../../uwa-bin/config.example.toml and checks it parses and all providers build. registry.rs checks all 10+ providers are present.

> **Code:** crate/uwa-providers/tests/config_example.rs — loads config.example.toml, asserts Config::load_from_str succeeds; crate/uwa-providers/tests/registry.rs — checks that all expected providers (chatgpt, claude, gemini, deepseek, etc.) are registered and have selectors.

---

## 58. rust_build.md with Real Numbers

Separate document about target/debug shrinking from 58GB to 1.1GB:
- incremental = false
- debug = 1 for own code, debug = 0 for package "*""
- opt-level = "s"
- codegen-units = 4

Plus recipes for sccache, cargo-sweep, cargo-bloat.

> **Code:** docs/rust_build.md — detailed build-size optimization guide; Cargo.toml [profile.dev] section with incremental=false, opt-level=s, debug-level=1; crate/uwa-bin/Cargo.toml build configuration.

---

## 59. Config::validate on Every Load

load_from_str and load_from_path always call validate(). Checks:
- loopback/API-key invariant
- model aliases exist
- provider key == name
- required selectors
- default_preset exists
- max_concurrency >= 1
- groups reference real provider+preset
- CDP ws_url scheme
- HH:MM for scheduled restart
- proxy address parsable

> **Code:** crate/uwa-config/src/lib.rs:255-310 — validate() runs on every load_from_str/load_from_path; checks all invariants listed above with specific error messages; fails before browser is started.

---

## 60. default_for_test via test-helpers Feature

ProviderCfg::default_for_test(name) is available in-crate (test) and to external crates via feature test-helpers. Eliminates boilerplate of filling 11 fields.

> **Code:** crate/uwa-config/src/lib.rs:180-200 — ProviderCfg::default_for_test() behind #[cfg(any(test, feature = "test-helpers"))]; crate/uwa-config/Cargo.toml — exposes test-helpers feature.

---

## 61. uwa-testkit as Separate Crate

Shared test doubles: MockPage, MockTransport, MockProvider, AppBuilder, TestApp, config_with_key, config_with_key_lan, CANONICAL_CDP, normalize_cdp_url. A full order of magnitude more serious than conftest.py.

> **Code:** crates/uwa-testkit/src/ — lib.rs re-exports: page.rs (MockPage impl), transport.rs (MockTransport), provider.rs (MockProvider), server.rs (AppBuilder, TestApp), config.rs (config_with_key, config_with_key_lan, CANONICAL_CDP, normalize_cdp_url).

---

## 62. normalize_cdp_url for Legacy Env

chromium_ws_url() rewrites ws://host:port/devtools/browser (no GUID, Chrome answers 404) to http://host:port. Protects CI from stale env vars.

> **Code:** crates/uwa-testkit/src/chromium.rs — normalize_cdp_url() replaces /devtools/browser with /json/version path; crates/uwa-testkit/src/config.rs — config_with_key() uses normalize_cdp_url() to sanitize the URL before constructing test config.

---

## 63. NetDecoder::Site with Fresh State per Response

ParserRegistry::new() is created inside deltas(), so ChatGptParser with last_full does not leak between two responses. Critical for cumulative diff.

> **Code:** crate/uwa-extract/src/net.rs:110-120 — inside DefaultNetExtractor::deltas(), ParserRegistry::new() creates a fresh registry with new ChatGptParser/ClaudeParser/GeminiParser instances; the tokio::spawn block captures this fresh registry per response.

---

## 64. XSSI Guard for Gemini

GeminiParser strips )]}' prefix. ChatGptParser does prefix-diff for cumulative. ClaudeParser ignores input_json_delta (that is tool_use, not text). Three different approaches in one registry.

> **Code:** crate/uwa-extract/src/parsers/chatgpt.rs — ChatGptParser tracks last_full text, strips prefix to emit only delta; crate/uwa-extract/src/parsers/claude.rs — ClaudeParser handles content_block_delta, ignores input_json_delta; crate/uwa-extract/src/parsers/gemini.rs — GeminiParser strips )]}' XSSI prefix before parsing JSON.

---

## 65. idle_timeout_ms for Network Stream

Extractor exits on idle timeout, not forever. Plus Finisher gives a wall-clock bound.

> **Code:** crate/uwa-extract/src/net.rs:80 — DefaultNetExtractor::deltas() wraps rx.recv() in tokio::time::timeout(idle, ...) using rules.idle_timeout_ms; crate/uwa-extract/src/finisher.rs — FinisherCfg.max_wait provides independent wall-clock bound.

---

## 66. Does Not Delete Today's JSONL

sweep_old checks that filename != today's date — otherwise writer could write to a deleted file on clock skew.

> **Code:** crate/uwa-history/src/store.rs — sweep_old() checks file name against today's date before deletion; uses SystemTime::now() with explicit date comparison to avoid deleting the active log file under clock skew.

---

## 67. truncate_preview with UTF-8 Boundary

store.rs truncates preview at UTF-8 boundary, not breaking multibyte chars. Test: does_not_split_multibyte_chars.

> **Code:** crate/uwa-history/src/store.rs:140-165 — truncate_preview() uses char_indices to find safe byte boundary; crate/uwa-history/tests/store_persistence.rs — does_not_split_multibyte_chars test verifies correct truncation.

---

## 68. ServerCfg.pid_file Optional

Key pid_file may be absent — field Option<PathBuf>. Validation allows it.

> **Code:** crate/uwa-config/src/lib.rs:95 — ServerCfg.pid_file: Option<PathBuf>; crate/uwa-config/src/lib.rs:310-315 — validate() does not require pid_file; crate/uwa-config/tests/validation.rs — existing_config_without_pid_file_parses test confirms None is accepted.

---

## 69. AppState via Builders, Not Mutation

with_sessions, with_semaphores, with_history return a NEW AppState (RuntimeServices rebuilt). Test runtime_services_defaults_and_overrides confirms original is not mutated.

> **Code:** crate/uwa-api/src/state.rs:50-120 — AppState with with_sessions(), with_semaphores(), with_history() builder methods returning new AppState; crate/uwa-bin/tests/state_assembly.rs — runtime_services_defaults_and_overrides verifies original is not mutated.

---

## 70. ChatMessage::content via text_content! Macro

Allows writing content: text_content!("hi") instead of Some(MessageContent::Text("hi".into())). Shows API ergonomics level.

> **Code:** uwa-core/src/types/openai.rs — text_content! macro wraps string into MessageContent::Text; ChatMessage::text() constructor uses it; crate/uwa-providers/tests/snapshots.rs uses it in test fixtures.

---

## 71. stream_chunks in uwa-core/src/types

General helper that splits text into chunks of 24 characters — used by both OpenAI and Anthropic streams. Single source of truth.

> **Code:** uwa-core/src/types/mod.rs:100-120 — stream_chunks() splits text into 24-char segments; used by both ChatCompletionsStreamer (OpenAI) and MessagesStreamer (Anthropic) in crate/uwa-api/src/routes/.

---

## 72. ContentPart::Other with #[serde(other)]

Unknown content part types do not break deserialization, become Other -> [unsupported] in text. Forward-compatible with new types.

> **Code:** uwa-core/src/types/openai.rs:30-45 — ContentPart enum with #[serde(other)] variant Other; crate/uwa-providers/src/attachments.rs:45 — handles known types, maps unknown to "[unsupported]"; crate/uwa-providers/tests/snapshots.rs — forward_compatibility_snapshot test verifies unknown parts do not error.

---

UWA is not a "Python project ported to Rust" — it is a project with a different engineering culture: config-driven, property-tested, observable, operationally mature, and with attention to edge cases.
