<div align="center">

# uwa — Universal Web API

> **Turn your logged-in browser LLM sessions into OpenAI- and Anthropic-compatible HTTP APIs.**

Local-first. No API keys. Rust binary, no runtime dependencies.

[![ci](https://github.com/sdlab1/uwa/actions/workflows/ci.yml/badge.svg)](https://github.com/sdlab1/uwa/actions/workflows/ci.yml)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

</div>

`uwa` sits between your tools (Cursor, Continue, Codex CLI, Claude Code, any
OpenAI/Anthropic SDK) and a Chromium instance where you're already logged in
to web LLMs — ChatGPT, Claude, Gemini, DeepSeek, and more. Your clients see a
standard HTTP API. The browser does the actual work. Nothing leaves your
machine.

---

## Quickstart

**TL;DR:** clone, launch Chromium, run the daemon, open `http://127.0.0.1:8080/`. The setup wizard does the rest.

### 1. Clone and build

```bash
git clone https://github.com/sdlab1/uwa
cd uwa
cargo build --release -p uwa-bin
```

<sub>Requires [Rust 1.75+](https://rustup.rs). ~2 minutes on first build.</sub>

### 2. Launch Chromium with the debug port

Leave it running. You'll log in here.

```bash
# Linux
chromium --remote-debugging-port=9222 --user-data-dir="$HOME/.uwa-chrome" &

# macOS
/Applications/Chromium.app/Contents/MacOS/Chromium \
  --remote-debugging-port=9222 --user-data-dir="$HOME/.uwa-chrome" &
```

Open `chatgpt.com` (or any supported site) in that window and **log in normally**. Leave the tab open.

### 3. Run uwa

```bash
cp crates/uwa-bin/config.example.toml uwa.toml
UWA_CHROMIUM_WS=http://127.0.0.1:9222 \
  ./target/release/uwa --config uwa.toml
```

### 4. Open the dashboard

**<http://127.0.0.1:8080/>**

The dashboard opens a **7-step setup wizard** that:
- checks the server is up,
- detects your Chromium tabs,
- confirms you're logged in to each site,
- sends a live test message,
- prints copy-paste-ready `curl` / Python / Cursor snippets.

Follow it once. When it turns green, you're done.

---

## Point your tools at it

Once the wizard is done, any OpenAI- or Anthropic-compatible client works.

```bash
export OPENAI_BASE_URL=http://127.0.0.1:8080/v1
export OPENAI_API_KEY=change-me          # same value as server.api_key in uwa.toml

curl $OPENAI_BASE_URL/models -H "Authorization: Bearer $OPENAI_API_KEY"

curl $OPENAI_BASE_URL/chat/completions \
  -H "Authorization: Bearer $OPENAI_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"say hi"}]}'
```

**Cursor / Continue** — set the OpenAI base URL to `http://127.0.0.1:8080/v1` and the API key to your `server.api_key`. Restart the editor.

**Claude Code / Claude SDK** — set `ANTHROPIC_BASE_URL=http://127.0.0.1:8080` and `ANTHROPIC_API_KEY=change-me`. Messages go to `/v1/messages`.

**Codex CLI** — set `OPENAI_BASE_URL` as above; the Responses endpoint (`/v1/responses`) is supported.

---

## Supported providers

All configured in `uwa.toml`. Adding another site is a TOML edit — no code changes.

| Provider | Model aliases | Streaming | Tools |
|---|---|---|---|
| **ChatGPT** | `gpt-4o`, `gpt-4o-mini` | ✅ network | ✅ |
| **Claude** | `claude-3-5-sonnet` | ✅ network | ✅ |
| **Gemini** | `gemini-1.5-pro`, `gemini-2.0-flash` | ✅ network | — |
| **DeepSeek** | `deepseek-chat`, `deepseek-r1` | ✅ DOM | ✅ |
| **Kimi** | `kimi-k2` | ✅ DOM | ✅ |
| **Qwen** | `qwen-max`, `qwen-plus` | ✅ DOM | — |
| **Grok** | `grok-2` | ✅ DOM | — |
| **Doubao** | `doubao-pro` | ✅ DOM | — |
| **AI Studio** | `aistudio-gemini` | ✅ DOM | — |
| **LMArena** | `arena-model` | ✅ DOM | — |

Adding a site means adding a `[providers.<name>]` block with URL patterns and
CSS selectors. The dashboard has a **Selector Test** panel and a
**Generate** button that analyzes the live DOM and proposes selectors for you.

---

## Configuration

All configuration lives in a single TOML file. The shipped
[`config.example.toml`](crates/uwa-bin/config.example.toml) has every section
documented.

### Minimal config

```toml
[server]
bind = "127.0.0.1"
port = 8080
api_key = "change-me"           # required unless bind is loopback-only

[model_aliases]
"gpt-4o" = "chatgpt"

[providers.chatgpt]
name = "chatgpt"
url_patterns = ["https://chatgpt.com/*"]
capabilities = { streams = true, tool_calls = true, vision = false }

[providers.chatgpt.selectors]
input = "#prompt-textarea"
send_button = "[data-testid='send-button']"
stop_button = "[data-testid='stop-button']"
assistant_message = "[data-message-author-role='assistant']"
```

### Hot reload

Change the TOML, then push it to a running daemon — no restart:

```bash
curl -X POST http://127.0.0.1:8080/admin/config/reload \
  -H "Authorization: Bearer $OPENAI_API_KEY" \
  -H "Content-Type: application/json" \
  -d "$(jq -Rs '{toml: ., dry_run: false}' < uwa.toml)"
```

The dashboard's **Config** tab does the same thing with a dry-run preview.

---

## How it works

`uwa` is a **translation bridge**, not an agent. Tools come from your client
request (`tools: [...]`). uwa injects their schemas into the browser prompt,
parses the browser LLM's tool-call markers, and returns them as standard
OpenAI `tool_calls` / Anthropic `tool_use` blocks. **The client executes the
tools** — via MCP, shell, or anything else — and sends the results back.

This means:

- Configure MCP servers in your **agent** (Claude Code, Cursor, …), not in uwa.
- uwa stays a transparent pipe — no tool execution, no hidden magic.

---

## Troubleshooting

**`connect Chromium at http://127.0.0.1:9222: ...` fails**
Chromium isn't running with `--remote-debugging-port=9222`. Relaunch it (step 2).

**`0 tabs visible`**
Chromium is up but has no tabs. Open one — `about:blank` is fine.

**Wizard step 4 shows `selector matched 0 elements`**
You're not logged in to that site in the Chromium window, or the site's
selectors have drifted. Log in, then click **Re-check**. If it still fails,
open the **Selector Test** tab, inspect the page with Chrome DevTools, and
update the selector in `uwa.toml`.

**Client returns `401 Unauthorized`**
Set `api_key` in `uwa.toml`, then paste the same value into the dashboard's
**Set key** dialog (top-right). The dashboard stores it in `localStorage`.

**Requests are slow (>30 s)**
Normal for the first message after a page reload — the browser has to warm
up. Subsequent messages in the same conversation are fast.

---

## Updating

```bash
git pull
cargo build --release -p uwa-bin
# restart the daemon; the wizard will re-verify everything
```

Selectors drift as sites redesign. When something stops working, the fastest
fix is to open the **Selector Test** tab, fix the selector against the live
page, and update `uwa.toml`. The daemon hot-reloads without dropping the
browser session.

---

## Development

```bash
./scripts/verify.sh
```

The canonical 10-step gate: fmt, build (default + all-features), clippy
`-D warnings`, unit + integration tests, property-based tests
(`PROPTEST_CASES=5000`), the feature-matrix builds, dead-code scan, and
snapshot tests. **Zero ignored tests.** If it isn't green, it doesn't merge.

For architecture and per-crate documentation, see the crate-level
`lib.rs` docs and the tests — they're the spec.

---

## Docker

```bash
docker compose up --build
curl http://127.0.0.1:8080/healthz
```

Two containers: `chromium` (headless, `--remote-debugging-port=9222`) and
`uwa` (the daemon, wired to it). Login state is stored in a Docker volume so
it survives restarts.

---

## License & disclaimer

MIT — see [LICENSE](LICENSE).

**For educational, academic, and research use.** The authors do not encourage
or support violating the Terms of Service of any web service or AI provider.
You must log into your own valid accounts. You are responsible for complying
with each site's terms of service, rate limits, and policies. Do not use this
tool for high-frequency automated requests or commercial purposes.

`uwa` runs entirely on your local system. It does not bypass authentication,
solve CAPTCHAs, or reverse-engineer encrypted APIs.

---

<div align="center">

**[Issues](https://github.com/sdlab1/uwa/issues) · [Discussions](https://github.com/sdlab1/uwa/discussions)**

</div>
