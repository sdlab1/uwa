# uwa — Universal Web API

Local OpenAI/Anthropic-compatible HTTP bridge over **logged-in web UIs**:
ChatGPT, Claude, Gemini, DeepSeek — and any MCP server you throw at it.

No API keys. No scraping service. Just a bridge between your browser session
and any OpenAI-compatible client (Cursor, Continue, Codex CLI, Claude Desktop,
the official SDKs).

```
┌───────────────┐   OpenAI/Anthropic    ┌───────────────┐   CDP    ┌───────────────┐
│ Cursor / Codex│ ────────────────────► │  uwa daemon   │ ───────► │ Chromium with │
│   Claude SDK  │ ◄──────────────────── │  (this repo)  │          │ logged-in UIs │
└───────────────┘    tools / SSE        └───────────────┘          └───────────────┘
                                               │
                                               │  MCP (stdio + HTTP+SSE)
                                               ▼
                                        ┌───────────────┐
                                        │ Claude Desktop│
                                        │ / Cursor MCP  │
                                        └───────────────┘
```

## Quickstart

### 1. Launch Chromium with remote debugging

```bash
# Linux
chromium --remote-debugging-port=9222 \
         --user-data-dir=$HOME/.uwa-chrome &

# macOS
/Applications/Chromium.app/Contents/MacOS/Chromium \
  --remote-debugging-port=9222 --user-data-dir=$HOME/.uwa-chrome &
```

In that browser: log in to `chatgpt.com`, `claude.ai`, `gemini.google.com`,
`chat.deepseek.com`. Leave the tabs open.

### 2. Run the daemon

```bash
cp crates/uwa-bin/config.example.toml uwa.toml
# edit `api_key` and selectors if needed

UWA_CHROMIUM_WS=ws://127.0.0.1:9222/devtools/browser \
  cargo run -p uwa-bin --release --features metrics -- --config uwa.toml
```

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

That's it — Cursor / Continue / Codex / Claude SDK now see `gpt-4o`,
`claude-3-5-sonnet`, etc. as normal models.

## Endpoints

| Method | Path | Purpose |
|---|---|---|
| GET | `/healthz` | liveness |
| GET | `/readyz` | readiness |
| GET | `/metrics` | Prometheus (feature `metrics`) |
| GET | `/v1/models` | OpenAI model list |
| POST | `/v1/chat/completions` | OpenAI chat (JSON + SSE + tools) |
| POST | `/v1/messages` | Anthropic Messages (JSON + SSE) |
| POST | `/v1/messages/count_tokens` | Anthropic token count |
| POST | `/v1/responses` | OpenAI Responses (Codex CLI) |
| GET | `/v1/provider/status` | provider diagnostics |
| GET | `/api/pool/status` | tab pool diagnostics |
| GET | `/admin/sessions` | live sessions |
| POST | `/admin/sessions/recover` | re-probe tabs
- DELETE | `/admin/sessions/:id` | drop a session
- GET | `/admin/providers` | provider configs
- GET | `/admin/breakers` | circuit breaker states
- POST | `/admin/breakers/:name/reset` | force-close a breaker

**Scoped routing** — override provider without changing the body:

```
POST /url/chatgpt.com/v1/chat/completions     # pick provider by domain
POST /tab/tab_abc/v1/chat/completions         # pin a specific tab
POST /v1/chat/completions                     # default (from `model` alias)
   + Header: X-UWA-Provider: chatgpt          # provider override
```

## MCP

**As a server** (`mcp_server.enabled = true` in config): `uwa` exposes its
own tools over stdio and (optionally) HTTP+SSE. Add to Claude Desktop:

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

Tools exposed:
- `web__chat(provider, message)` — one-shot ask
- `web__list_tabs()` — list connected tabs
- prompt `ask(provider, question)` — templated ask
- resource `uwa://web/tabs` — JSON list of tabs

**As a client** (`[[mcp_clients]]` in config): `uwa` consumes external MCP
servers and injects their tools into the web-UI LLM via the `<tool_call>`
protocol. That means your Cursor session can call `fs__read_file` even
though ChatGPT's web UI has no idea what MCP is.

## Configuration

All TOML + `UWA__*` env vars. See `crates/uwa-bin/config.example.toml`.

Key sections:

- `[server]` — bind, port, api_key, timeouts, pid_file
- `[stealth]` — pack name, optional user scripts dir
- `[mcp_server]` — enable MCP server (stdio + optional HTTP)
- `[[mcp_clients]]` — external MCP servers to consume
- `[model_aliases]` — `"gpt-4o" = "chatgpt"` mapping
- `[providers.*]` — per-site config: URL patterns, capabilities, selectors,
  extraction strategy, finisher tuning, network rules

## Development

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test --workspace --all-features

# Real Chromium integration
UWA_CHROMIUM=1 cargo test -p uwa-browser -- --ignored --test-threads=1
```

## Docker

```bash
docker compose up --build
curl http://127.0.0.1:8080/healthz
```

## Architecture

- `uwa-core` — DTO, traits, errors. No I/O.
- `uwa-config` — TOML loader + validation.
- `uwa-tools` — `<tool_call>` parsing, prompt injection, tool history.
- `uwa-mcp` — MCP server + client (stdio + HTTP+SSE).
- `uwa-api` — axum HTTP surface (OpenAI + Anthropic + admin).
- `uwa-browser` — CDP transport + `TabPool` + `NetBus`.
- `uwa-extract` — network-first + DOM-fallback extraction + finisher.
- `uwa-providers` — one generic `SiteProvider` driven by TOML.
- `uwa-session` → conversation → tab mapping with LRU + TTL.
- `uwa-resilience` → circuit breaker, semaphore, retry.
- `uwa-lifecycle` → PID file + unified shutdown.
- `uwa-stealth` → deterministic JS patches.
- `uwa-testkit` → shared test doubles.

## Known limitations

- **Anthropic streaming** is pseudo-chunked; real token streaming requires
  network-first SSE capture, currently implemented and tested only against
  ChatGPT.
- **`FrameId → TargetId` mapping** handles main frames and same-target
  iframes; cross-origin isolated frames (OOPIF) are not mapped.
- **Selectors drift** — mitigate with `selectors_version` in config and the
  `uwa-snapshot` binary.

## License

MIT.