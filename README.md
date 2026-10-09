<div align="center">

# uwa — Universal Web API

**A high-performance, asynchronous Rust bridge that turns logged-in browser LLM sessions into standard OpenAI / Anthropic APIs.**

[![ci](https://github.com/sdlab1/uwa/actions/workflows/ci.yml/badge.svg)](https://github.com/sdlab1/uwa/actions/workflows/ci.yml)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![rust: 1.75+](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://rustup.rs)

</div>

---

## What it is

`uwa` sits between your tools (Cursor, Continue, Codex CLI, Claude Desktop, any OpenAI/Anthropic SDK) and a Chromium instance where you're already logged in to web LLMs (ChatGPT, Claude, Gemini, DeepSeek, Kimi, Qwen, Grok, Doubao, AI Studio, LMArena).

It exposes a **local, standard HTTP API** — same wire format your clients already speak — and drives the browser behind the scenes over CDP (Chrome DevTools Protocol). No API keys, no scraping service, no MITM.

```
┌───────────────┐   OpenAI/Anthropic    ┌───────────────┐   CDP / pipe   ┌─────────────────┐
│ Cursor / Codex│ ────────────────────► │  uwa daemon   │ ─────────────► │ Chromium with   │
│  Claude SDK   │ ◄──────────────────── │  (this repo)  │                │ logged-in UIs   │
└───────────────┘   tools / SSE         └───────┬───────┘                └─────────────────┘
                                                │
                                                │  MCP (stdio + HTTP+SSE)
                                                ▼
                                        ┌───────────────┐
                                        │ Claude Desktop│
                                        │ / Cursor MCP  │
                                        └───────────────┘
```

---

## Table of contents

- [Features](#features)
- [Quickstart](#quickstart)
- [Endpoints](#endpoints)
- [Providers](#providers)
- [Configuration](#configuration)
- [MCP integration](#mcp-integration)
- [Web dashboard](#web-dashboard)
- [Dual backend](#dual-backend)
- [Architecture](#architecture)
- [Development](#development)
- [Docker](#docker)
- [Known limitations](#known-limitations)
- [License](#license)

---

## Features

### Core protocol support

- **OpenAI Chat Completions** — `/v1/chat/completions`, JSON + SSE streaming, tool calls.
- **OpenAI Responses API** — `/v1/responses`, adapter for Codex CLI.
- **Anthropic Messages** — `/v1/messages`, JSON + SSE streaming, `tool_use` / `tool_result` blocks.
- **Anthropic token count** — `/v1/messages/count_tokens` for Claude SDK pre-checks.
- **MCP server** (stdio + HTTP+SSE) — exposes `web__chat`, `web__list_tabs`, prompt `ask`, resource `uwa://web/tabs`.
- **MCP client** — consumes external MCP servers and injects their tools into the web-UI LLM via the `<tool_call>` protocol.

### Transport & extraction

- **Dual backend** — pick per provider:
  - `chromiumoxide` — attach to a running Chrome via CDP (debug / CI / oracle).
  - `nodriver` — Python sidecar that spawns Chrome with stealth flags (aggressive sites).
- **Network-first extraction** — real token deltas from intercepted SSE.
- **DOM fallback** — selector-driven extraction when network is encrypted or absent.
- **Deterministic finisher** — stop-button-gone + DOM-stable + network-finished.
- **OOPIF support** — `Target.setAutoAttach(flatten)` maps cross-origin iframes; `eval_in_frame` + `scan_media` reach inside them.
- **Per-provider SSE parsers** — ChatGPT (cumulative → incremental), Claude (`content_block_delta`), Gemini (`batchexecute`).
- **Stealth** — deterministic JS patches, applied *before* any page script, also injected into OOPIF sessions.

### Configuration & operations

- **TOML config** with hot reload — `POST /admin/config/reload`, no restart.
- **Preset system** — multiple configurations per provider, routed via `/url/{domain}/{preset}/v1/…`.
- **Routing groups** — round-robin / failover / hash-conversation across providers.
- **Scoped routing** — `/url/{domain}/…`, `/tab/{id}/…`, `/group/{id}/…`, header `X-UWA-Provider`.
- **Proxy support** — HTTP/SOCKS5 via Chrome launch flags.
- **Scheduled restart** — daily drain-and-exit at `HH:MM` UTC.

### Tooling

- **Declarative workflow** — CLICK, FILL_INPUT, WAIT, STREAM_WAIT, KEY_PRESS, IF/ELSE, GROUP, CAPTURE.
- **File paste** — auto-switch to file upload above a byte threshold.
- **Prompt padding** — mask real prompt length with marker segments.
- **Selector auto-generation** — analyze live DOM, propose CSS selectors with scores.
- **Request history** — append-only JSONL, ring buffer in memory, `/admin/history`.
- **Statistics** — per-provider error rates, timing percentiles, requests-per-minute.
- **Web dashboard** — static HTML/CSS/JS at `/`, tabs for stats / history / sessions / selector test / config / logs.
- **Live logs** — `tracing` events streamed over SSE at `/admin/logs/stream`.

### Resilience

- **Circuit breaker** per provider — rolling window, half-open probe.
- **Semaphore** per provider — bounded concurrency.
- **Retry** with jittered exponential backoff — idempotent ops only.
- **Session pinning** — conversation → tab, LRU + TTL, `touch()` keeps long tool loops alive.
- **Graceful shutdown** — drains HTTP, waits for sessions, removes PID.

### Multimodal

- **Image input** — `content: [{type:"image_url", ...}]` decoded from data URI or HTTP, attached via file input.
- **Audio capture** — Web Audio API hook, records any `<audio>` that plays, returns webm/opus.
- **Video detection** — scans `<video>` / `<audio>` sources, blob URLs fetchable.

---

## Quickstart

### 1. Launch Chromium

```bash
# Linux
chromium --remote-debugging-port=9222 \
         --user-data-dir=$HOME/.uwa-chrome &

# macOS
/Applications/Chromium.app/Contents/MacOS/Chromium \
  --remote-debugging-port=9222 \
  --user-data-dir=$HOME/.uwa-chrome &
```

In that browser, log in to the sites you'll use — `chatgpt.com`, `claude.ai`, `gemini.google.com`, `chat.deepseek.com`, etc. Leave the tabs open.

### 2. Run the daemon

```bash
cp crates/uwa-bin/config.example.toml uwa.toml
# edit `api_key` if you want auth, adjust selectors if needed

UWA_CHROMIUM_WS=ws://127.0.0.1:9222/devtools/browser \
  cargo run -p uwa-bin --release --features metrics -- --config uwa.toml
```

The daemon prints its listen address on stderr. Logs go to stderr so they don't mix with MCP stdio.

### 3. Point any OpenAI client at it

```bash
export OPENAI_BASE_URL=http://127.0.0.1:8080/v1
export OPENAI_API_KEY=change-me

curl $OPENAI_BASE_URL/models -H "Authorization: Bearer $OPENAI_API_KEY"

curl $OPENAI_BASE_URL/chat/completions \
  -H "Authorization: Bearer $OPENAI_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"say hi"}]}'
```

Cursor, Continue, Codex CLI, Claude SDK, and the OpenAI Python/Node SDKs now see `gpt-4o`, `claude-3-5-sonnet`, `gemini-1.5-pro`, `deepseek-chat` as normal models.

### 4. Open the dashboard

Browse to <http://127.0.0.1:8080/>. Enter your API key once (stored in `localStorage`); the dashboard provides live stats, request history, session control, selector testing, config editing, and log streaming.

---

## Endpoints

### OpenAI

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/v1/models` | Model list (from `model_aliases`) |
| `POST` | `/v1/chat/completions` | OpenAI chat — JSON + SSE + tools |
| `POST` | `/v1/responses` | OpenAI Responses (Codex CLI) |

### Anthropic

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/v1/messages` | Anthropic Messages — JSON + SSE + `tool_use` |
| `POST` | `/v1/messages/count_tokens` | Approximate token count |

### Scoped routing

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/url/{domain}/v1/chat/completions` | Route by domain |
| `POST` | `/url/{domain}/{preset}/v1/chat/completions` | Route by domain + preset |
| `POST` | `/tab/{tab_id}/v1/chat/completions` | Pin a specific tab |
| `POST` | `/group/{group_id}/v1/chat/completions` | Route via a group |
| Header | `X-UWA-Provider: chatgpt` | Provider override |

### Diagnostics & admin

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/healthz` | Liveness |
| `GET` | `/readyz` | Readiness |
| `GET` | `/metrics` | Prometheus (feature `metrics`) |
| `GET` | `/v1/provider/status` | Provider diagnostics |
| `GET` | `/api/pool/status` | Tab pool state |
| `GET` | `/admin/sessions` | Live sessions |
| `POST` | `/admin/sessions/recover` | Re-probe tabs |
| `DELETE` | `/admin/sessions/:id` | Drop a session |
| `GET` | `/admin/providers` | Provider configs |
| `GET` | `/admin/breakers` | Circuit breaker states |
| `POST` | `/admin/breakers/:name/reset` | Force-close a breaker |
| `GET` | `/admin/history` | Request history (JSONL-backed) |
| `GET` | `/admin/history/:id` | Full record |
| `GET` | `/admin/stats` | Aggregated statistics |
| `POST` | `/admin/selector-test` | Test CSS selector against live tab |
| `POST` | `/admin/selector-generate` | Auto-generate candidates |
| `POST` | `/admin/selector-apply` | Dry-run / apply selector changes |
| `GET` | `/admin/config` | Current config summary |
| `POST` | `/admin/config/reload` | Hot reload from TOML |
| `GET` | `/admin/logs/stream` | Live logs (SSE) |
| `GET` | `/` | Web dashboard |
| `GET` | `/static/*` | Dashboard assets |

---

## Providers

The default config ships with 10+ providers. Adding another is a TOML edit — no code changes.

| Provider | URL pattern | Extraction | Notes |
|---|---|---|---|
| **ChatGPT** | `chatgpt.com/*` | `network_first` | Site parser `chatgpt` |
| **Claude** | `claude.ai/*` | `dom_only` | Site parser `claude` |
| **Gemini** | `gemini.google.com/*` | `dom_only` | Presets: `flash`, `pro` |
| **DeepSeek** | `chat.deepseek.com/*` | `dom_only` | |
| **Kimi** | `kimi.moonshot.cn/*` | `dom_only` | |
| **Qwen** | `chat.qwen.ai/*` | `dom_only` | |
| **Grok** | `grok.com/*` | `dom_only` | `stealth = true` |
| **Doubao** | `www.doubao.com/*` | `dom_only` | Vision |
| **AI Studio** | `aistudio.google.com/*` | `dom_only` | 1M context |
| **LMArena** | `lmarena.ai/*` | `dom_only` | |

Each provider has:

- `capabilities` — `streams`, `tool_calls`, `vision`, `max_context_tokens`.
- `selectors` — `input`, `send_button`, `stop_button`, `assistant_message`, `conversation_root`.
- `extraction` — `network_first` or `dom_only`.
- `net` — URL patterns + MIME + decoder (SSE / JSON / site parser).
- `finisher` — DOM-stable / poll / min-wait / max-wait timings.
- `file_paste`, `prompt_padding`, `media`, `stealth` — feature toggles.
- `presets` — named alternative configs.

### Selector drift

Selectors will drift as sites redesign. Two mitigations:

1. **`selectors_version`** — free-form tag in config; shown in `/readyz`.
2. **`uwa-snapshot`** — pulls real HTML from logged-in tabs and writes fixtures for snapshot tests.

```bash
cargo run -p uwa-providers --features snapshot --bin uwa-snapshot -- \
  --config uwa.toml \
  --out crates/uwa-providers/tests/fixtures \
  --include-oopifs
```

---

## Configuration

Full reference: [`crates/uwa-bin/config.example.toml`](crates/uwa-bin/config.example.toml).

### Top-level sections

| Section | Purpose |
|---|---|
| `[server]` | bind, port, api_key, timeouts, pid_file |
| `[backend]` | `kind = "cdp" \| "nodriver"`, plus per-backend options |
| `[proxy]` | HTTP/SOCKS5 proxy settings |
| `[stealth]` | pack name (`default`, `full`, `none`), user scripts dir |
| `[mcp_server]` | enable MCP server over stdio (+ optional HTTP) |
| `[[mcp_clients]]` | external MCP servers to consume |
| `[model_aliases]` | `"gpt-4o" = "chatgpt"` mapping |
| `[providers.*]` | per-site config |
| `[groups.*]` | routing groups (round_robin / failover / hash_conversation) |
| `[scheduled_restart]` | daily drain-and-exit at `HH:MM` UTC |

### Hot reload

```bash
curl -X POST http://127.0.0.1:8080/admin/config/reload \
  -H "Authorization: Bearer $OPENAI_API_KEY" \
  -H "Content-Type: application/json" \
  -d "$(jq -Rs '{toml: ., dry_run: false}' < uwa.toml)"
```

The daemon parses, validates, builds providers, and atomically swaps them via `ArcSwap`. In-flight requests finish against the old config; new requests see the new one.

### Preset system

```toml
[providers.gemini]
default_preset = "flash"

[providers.gemini.selectors]
input = "div.ql-editor"
send_button = "button.send-button"
assistant_message = "model-response"

[providers.gemini.presets.flash]
display_name = "Gemini 2.0 Flash"

[providers.gemini.presets.pro]
display_name = "Gemini 1.5 Pro"
[providers.gemini.presets.pro.selectors]
input = "div.ql-editor[data-model='pro']"
send_button = "button.send-button.pro"
```

Route with `/url/gemini.google.com/pro/v1/chat/completions`.

### Routing groups

```toml
[groups.fast]
strategy = "round_robin"
members = [
    { provider = "gemini", preset = "flash" },
    { provider = "chatgpt" },
]

[groups.fallback]
strategy = "failover"
members = [
    { provider = "chatgpt" },
    { provider = "claude" },
    { provider = "gemini" },
]
```

Route with `/group/fast/v1/chat/completions`.

---

## MCP integration

### As a server

```toml
[mcp_server]
enabled = true
```

Exposed tools:

- `web__chat(provider, message)` — one-shot ask.
- `web__list_tabs()` — list connected tabs.
- Prompt `ask(provider, question)` — templated ask.
- Resource `uwa://web/tabs` — JSON list of tabs.

Add to Claude Desktop:

```json
{
  "mcpServers": {
    "uwa": {
      "command": "/path/to/uwa",
      "args": ["--config", "/path/to/uwa.toml"]
    }
  }
}
```

The MCP server writes to **stdout**; all `tracing` logs go to **stderr** — no interleaving.

### As a client

```toml
[[mcp_clients]]
name = "fs"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]

[[mcp_clients]]
name = "git"
command = "uvx"
args = ["mcp-server-git", "--repository", "."]
```

`uwa` consumes the external server's tool list and **injects it into the web-UI LLM via the `<tool_call>` protocol**. ChatGPT's web UI doesn't know what MCP is; the model just sees tool descriptions and emits `<tool_call>{"name":"fs__read_file",...}</tool_call>` blocks — which `uwa` parses, dispatches, and feeds back as `<tool_response>`.

---

## Web dashboard

Open <http://127.0.0.1:8080/>. Tabs:

| Tab | What it shows |
|---|---|
| **Overview** | Live counters, requests-per-minute chart, finish-reason histogram |
| **Providers** | Per-provider totals, error rates, timing percentiles |
| **History** | Filterable request list; click any row for the full record |
| **Sessions** | Live session → tab mapping; drop or re-probe |
| **Selector Test** | Pick a provider + tab, enter a CSS selector, see matched count and first text |
| **Config** | View current config; edit TOML and apply via hot reload (dry-run first) |
| **Logs** | Live SSE stream of `tracing` events with level filtering |

The dashboard is plain HTML/CSS/JS — no bundler, no framework. Assets live in [`crates/uwa-api/static/`](crates/uwa-api/static/).

---

## Dual backend

Two transports behind the same `uwa_core::Transport` trait:

### `chromiumoxide` (CDP)

Attach to an already-running Chrome (`--remote-debugging-port=9222`). Fast (~0.1 ms per CDP command). Used for:

- Local development — no restart to debug selectors.
- CI — `--headless=new` with real Chromium.
- Sites without aggressive bot detection.

### `nodriver` (Python sidecar)

`uwa` spawns a Python subprocess (`sidecar/uwa_nodriver_sidecar.py`), which spawns Chrome directly with stealth flags:

```
--disable-blink-features=AutomationControlled
--no-remote-debugging-port
```

No `enable-automation` flag, no open debug port, no CDP surface visible from outside. This is the **default** for aggressive sites (ChatGPT, Claude, Gemini, DeepSeek, Grok).

Choose per provider:

```toml
[backend]
kind = "nodriver"          # global default

[providers.chatgpt]
backend = "nodriver"       # aggressive site

[providers.simple-site]
backend = "cdp"            # oracle path for debug
```

Transport overhead through the sidecar is ~30–150 ms per request — invisible against 2–30 s LLM response times.

---

## Architecture

Fourteen Rust crates, one Python sidecar, one static dashboard.

```
uwa-core          DTO, traits, ids, errors, workflow schema, net rules. No I/O.
uwa-config        TOML loader + validator + presets + groups + features.
uwa-tools         <tool_call> parser (4 strategies), prompt injection, history.
uwa-mcp           MCP server + client (stdio + HTTP+SSE), tool router.
uwa-api           axum HTTP surface (OpenAI + Anthropic + admin + dashboard).
uwa-browser       CDP + nodriver transports, TabPool, NetBus, OOPIF registry.
uwa-extract       Network-first + DOM-fallback extraction + finisher + parsers.
uwa-providers     One generic SiteProvider driven by TOML + workflow runner.
uwa-session       Conversation → tab mapping, LRU + TTL, DashMap-safe eviction.
uwa-resilience    Circuit breaker, semaphore, retry with jitter.
uwa-lifecycle     PID file + unified shutdown bus.
uwa-stealth       Deterministic JS patches, applied per session (incl. OOPIF).
uwa-history       Request records (JSONL + ring buffer) + statistics.
uwa-testkit       Shared test doubles (MockPage / MockTransport / …).

sidecar/          Python nodriver sidecar (JSON-RPC over pipes).
crates/uwa-api/static/  Dashboard (HTML / CSS / JS).
```

### Key design decisions

- **`uwa-core` is pure** — no axum, no chromiumoxide, no HTTP clients. Just `serde`, `thiserror`, `async-trait`.
- **Everything external is a trait** — `Page`, `Transport`, `SiteProvider`, `ToolProvider`, `McpClient`, `McpHandler`. Tests mock 90%.
- **`RuntimeServices` uses `Arc<…>` fields** — `AppState::with_*` is a cheap copy, no data loss.
- **`DashMap` guards never held across `.await`** — always snapshot first, then lock inner mutexes.
- **Config is reloadable via `ArcSwap`** — in-flight requests keep their snapshot; new requests see the new config.
- **OOPIF via `Target.setAutoAttach(flatten)`** — one WebSocket, many sessions, `waitForDebuggerOnStart` for stealth injection before iframe JS runs.

---

## Development

### Prerequisites

- Rust 1.75+
- Chromium (for integration tests)
- Python 3.10+ with `nodriver` (for the nodriver backend)

```bash
pip install -r sidecar/requirements.txt
```

### Build & test

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test --workspace --all-features

# Feature combos
cargo test --workspace --features uwa-api/metrics
cargo test --workspace --features uwa-mcp/mcp-http
cargo build -p uwa-providers --features snapshot
cargo build -p uwa-providers --features fixture-server
```

### Integration tests (real Chromium)

```bash
# Terminal 1
chromium --headless=new --no-sandbox --disable-gpu \
  --remote-debugging-address=127.0.0.1 --remote-debugging-port=9222 \
  --user-data-dir=/tmp/cdp &

# Terminal 2
UWA_CHROMIUM=1 cargo test -p uwa-browser -- --ignored --test-threads=1
UWA_CHROMIUM=1 cargo test -p uwa-providers --features fixture-server \
  --test e2e_fixture -- --ignored --test-threads=1
```

### Sidecar tests

```bash
cd sidecar
pytest tests/ -m "not integration"   # unit
pytest tests/ -m integration         # requires Chrome
```

### Property-based tests

```bash
cargo test -p uwa-tools --test proptest_parser
cargo test -p uwa-extract --test proptest_net
PROPTEST_CASES=10000 cargo test -p uwa-tools --test proptest_parser
```

---

## Docker

The compose setup runs Chromium in a sidecar container and `uwa` in another.

```bash
docker compose up --build
curl http://127.0.0.1:8080/healthz
```

`docker-compose.yml` provisions:

- `chromium` — headless Chrome with `--remote-debugging-port=9222`, healthcheck on `/json/version`.
- `uwa` — the daemon, waiting on Chromium's healthcheck, with `UWA_CHROMIUM_WS` pointed at the `chromium` service.

For the nodriver backend inside Docker, add the sidecar files to the image and set `[backend].kind = "nodriver"`.

---

## Known limitations

- **Anthropic streaming** is pseudo-chunked — real token streaming works only when the site uses network-first extraction (currently ChatGPT).
- **`FrameId → TargetId` mapping** handles main frames and same-target iframes. Cross-origin isolated frames are mapped via `Target.setAutoAttach(flatten)` but `eval_in_frame` inside a **cross-process** OOPIF requires `Page.createIsolatedWorld` scoped to the OOPIF's session, which chromiumoxide 0.7.0 doesn't expose through its public API. A small fork or a bump to 0.8+ unlocks it.
- **Selectors drift** — mitigate with `selectors_version` and the `uwa-snapshot` binary.
- **Audio capture** requires Chrome launched with `--autoplay-policy=no-user-gesture-required` (added automatically when `media.audio_capture_enabled = true`).
- **TTS fallback** (Session 4 scope) — audio capture works; routing captured audio back through a TTS-capable provider is not implemented.

---

## License

MIT — see [LICENSE](LICENSE).

Built for educational and research purposes. You are responsible for complying with each site's terms of service.

---

<div align="center">

**[Report a bug](https://github.com/sdlab1/uwa/issues) · [Request a feature](https://github.com/sdlab1/uwa/issues) · [Discussions](https://github.com/sdlab1/uwa/discussions)**

</div>