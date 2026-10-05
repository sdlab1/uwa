# ⚡ project_uwa (Universal Web API — Rust Edition)

> **A lightweight, low-overhead, asynchronous Rust bridge designed to expose local browser-based LLM sessions as standard OpenAI and Anthropic-compatible HTTP APIs.**

---

> !WARNING
> **DISCLAIMER & LEGAL NOTICE**  
> This project is created strictly for **educational, academic, and research purposes** as a proof-of-concept in browser automation, CDP (Chrome DevTools Protocol) interaction, and low-level HTTP reverse-proxying.  
> 
> - **No Harm Intended:** The authors do not encourage, condone, or support any activity that violates the Terms of Service (ToS) of any web service or AI provider.
> - **User Responsibility:** Users are solely responsible for compliance with applicable terms, rate limits, and policies of third-party platforms.
> - **Non-Profit & Open Source:** This software is provided "as is" under the MIT License, without warranty of any kind. Use it responsibly and at your own risk.

---

## 💡 Motivation: Why Rust?

Modern developer tools and web-bridging scripts are heavily dominated by Python. While convenient, Python introduces significant runtime overhead, high memory usage, high abstraction layers, and threading constraints due to the GIL (Global Interpreter Lock).

**`project_uwa` was built with a simple philosophy: *Because we can, and Rust makes it better.***

This project serves as a clean-room, high-performance Rust refactoring inspired by open-source browser-bridge concepts like [`universal-web-api`](https://github.com/lumingya/universal-web-api). By leveraging Rust's zero-cost abstractions, asynchronous runtime (`tokio`), fast web framework (`axum`), and low-level browser automation (`cdp`), this project aims to demonstrate:

* **Minimal Memory Footprint:** Running a lean local bridge without heavy runtime interpreters.
* **Blazing Fast I/O:** Efficient event-driven async networking and direct CDP WebSocket communication.
* **Type-Safe Architecture:** Strong compile-time guarantees across session management, parsing, and request handling.
* **Rust Ecosystem Advocacy:** Promoting native, memory-safe, and resource-efficient tooling for developer infrastructure.

---

## 🛠️ Architecture Overview

The bridge operates as a multi-layered local proxy:

1. **HTTP API Layer (`axum` + `tokio`):** Accepts standard `/v1/chat/completions` requests from clients (Cursor, Continue, OpenAI SDKs) and streams back SSE responses.
2. **Session & Tab Management (`dashmap` + `governor`):** Manages local tab pools, concurrency, and session isolation.
3. **CDP Automation Layer:** Communicates with local Chromium browser instances via Chrome DevTools Protocol (CDP) for DOM interaction and network event monitoring.
4. **Response Parsing & Normalization (`scraper` + `serde_json`):** Parses DOM streams and network payloads into standardized JSON / SSE chunks.

---

## 🚀 Quick start

### Docker Compose (Chromium + daemon)

```bash
docker compose up --build
curl http://127.0.0.1:8080/healthz
curl http://127.0.0.1:8080/v1/models
```

The compose file starts a headless Chromium with `--remote-debugging-port=9222`
and the daemon pointed at it via `UWA_CHROMIUM_WS=http://chromium:9222`
(`CdpTransport` resolves that http endpoint through `/json/version`).
MCP over HTTP+SSE listens on `port + 1` (8081 here).

### Local

```bash
# 1. a browser with an open debugging port
chromium --headless=new --no-sandbox --disable-gpu \
  --password-store=basic --remote-debugging-port=9222 --user-data-dir=/tmp/uwa-profile &

# 2. the daemon
cargo run -p uwa-bin -- --config crates/uwa-bin/config.example.toml

# 3. a chat turn through the OpenAI-compatible surface
curl -s http://127.0.0.1:8080/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}'
```

---

## ⚙️ Configuration

`--config <path>` / `-c <path>` / env `UWA_CONFIG`, default `uwa.toml`.
A commented example lives in [`crates/uwa-bin/config.example.toml`](crates/uwa-bin/config.example.toml).

| Key / env | Meaning |
|---|---|
| `server.bind`, `server.port` | HTTP listener (default `127.0.0.1:8080`) |
| `server.api_key` | if set, `Authorization: Bearer <key>` is required |
| `server.pid_file` | single-instance guard (default `uwa.pid`) |
| `server.request_timeout_ms` | whole-request budget, feeds the HTTP timeout layer |
| `[model_aliases]` | public model name → provider name (`gpt-4o = "chatgpt"`) |
| `[providers.<name>]` | url patterns, capabilities, selectors, extraction rules |
| `[[mcp_clients]]` | external MCP servers spawned over stdio at start-up |
| `UWA_CHROMIUM_WS` | CDP endpoint, default `http://127.0.0.1:9222` |
| `RUST_LOG` | `tracing` filter, e.g. `info,uwa=debug` |

Logs go to **stderr** so stdout stays free for the MCP stdio transport.

---

## 🔌 API surface

| Endpoint | Purpose |
|---|---|
| `GET /healthz`, `GET /readyz` | liveness / readiness |
| `GET /v1/models` | configured model aliases + capabilities |
| `POST /v1/chat/completions` | OpenAI-shaped, streaming via SSE (`data: …`, `data: [DONE]`) |
| `POST /v1/messages` | Anthropic-shaped adapter (`/v1/messages/count_tokens` in `todo/02.md`) |
| `POST /mcp`, `GET /mcp/sse` | MCP over HTTP+SSE, needs `--features uwa-bin/mcp-http` |

Start-up order (and the graceful shutdown it reverses) lives in
[`crates/uwa-bin/src/wiring.rs`](crates/uwa-bin/src/wiring.rs).

---

## 🧪 Testing

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo test --workspace --all-features          # mcp-http + snapshot
cargo test --workspace --doc

# live Chromium tests (spawns its own browser; set UWA_CHROME_BIN if needed)
UWA_CHROMIUM=1 cargo test -p uwa-browser -- --include-ignored --test-threads=1
```

Shared test doubles — `MockPage`, `MockTransport`, `MockProvider`,
`MockToolProvider`, `test_server` — live in `crates/uwa-testkit`.
CI runs the same commands (see [`.github/workflows/rust.yml`](.github/workflows/rust.yml)).
