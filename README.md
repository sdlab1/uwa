<h1 align="center">uwa</h1>

<p align="center">
  <b>Universal Web API</b> — one local endpoint for the web AIs you're already logged into.<br>
  <i>No token capture. No restart after login. Single binary. One layer.</i>
</p>

<p align="center">
  <a href="https://github.com/sdlab1/uwa/actions/workflows/ci.yml"><img alt="CI" src="https://img.shields.io/github/actions/workflow/status/sdlab1/uwa/ci.yml?branch=main&label=ci"></a>
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
  <a href="https://www.rust-lang.org"><img alt="Rust" src="https://img.shields.io/badge/rust-1.75%2B-orange.svg"></a>
</p>

<p align="center">
**Work In Progress** under active developement
</p>

---

## What this is

`uwa` turns the web UIs you're already logged into — ChatGPT, Claude,
Gemini, DeepSeek, Kimi, Qwen, Grok, Doubao, AI Studio, Arena — into a
single OpenAI- and Anthropic-compatible endpoint on `127.0.0.1`. Point
Cursor, Continue, Codex CLI, Claude Code or Aider at it and use the web
chats you already pay for.

It is a **translation bridge**, not an agent. It types your prompt into a
real Chromium tab, reads the answer back, and returns it in the standard
shape your client expects. Tool calls are returned to your client — uwa
never runs them, and never touches your credentials.

## Why another one of these

> **Why UWA is structurally different** — see [why_uwa.md](docs/why_uwa.md) for 72 detailed points. Below, 22 concrete reasons you will not find in a typical Python wrapper:
>
> **A. Architecturally (6)** — Single config-driven `GenericProvider` (#1) replaces a zoo of per-site classes; 8-action declarative `Workflow` DSL (#2); bridge semantics — tools parsed not executed (#3); 7 API surfaces not just `/v1/chat/completions` (#4); hint-based path routing with `/url/{domain}/` and `/tab/{tab_id}/` (#17); mini-router model groups with `round_robin`/`failover`/`hash_conversation` (#32). **Python has zero of these.**
>
> **B. Qualitatively (4)** — Real token-by-token SSE streaming with live TTFT (#5 vs `sleep(5)` post-hoc); finisher combining stop-button + network + DOM stability + timeout (#6); session-to-tab pinning with per-conversation mutex (#7); circuit breaker with self-healing half-open probe (#9). **Python projects fake or skip all four.**
>
> **C. Pedantically (5)** — 80-line JSON Schema validator replacing the 1 MB `jsonschema` crate (#26); `safe_emit_end` preventing partial marker leakage (#23); `ContentPart::Other` with `#[serde(other)]` for forward compat (#72); `Arc::strong_count` as live-handle busy marker so capacity eviction never drops a busy session (#29); `truncate_preview` with UTF-8 boundary safety (#67). **Python would not bother — it works "well enough" until it doesn't.**
>
> **D. Systems-level guarantees Rust provides that CPython cannot (7)** — `PR_SET_PDEATHSIG` at the kernel level so `kill -9` tests still clean up Chrome (#45); panic hook calling `killpg(-pgid, SIGKILL)` so a panic doesn't orphan Chromium (#46); `AtomicU8` compare-exchange to prevent parallel `npm install` (#50); `uwa update` with `.sha256` verification + atomic `rename()` swap (#49); `uwa doctor` one-command diagnostics (#48); `proptest` with 10k-case nightly runs covering chunk-invariance and panic-safety (#55); `uwa daemon` managing both PID **and PGID** for tree-wide SIGTERM→SIGKILL with drain (#47). **A Python venv cannot do kernel-level process signals, atomic binary swaps, or compile-time-checked atomic CAS.**

Every other "free web AI API" project has at least one of these problems:

- It sniffs tokens / cookies out of the network and replays them — brittle,
  breaks on every site update, and trivially blocked.
- It asks you to log in, then restart the daemon so the auth state is picked up.
- It installs a proxy layer between your client and the site, adding a
  hop where bugs live.
- It ships as a Python venv you have to keep alive.

uwa solves those four things by design:

| Problem | uwa |
|---|---|
| **Token capture** | Never. uwa reads the DOM and CDP events of a live tab. |
| **Restart after login** | Never. You log in *in the tab uwa is already driving*. |
| **Extra hop** | None. uwa is the client of the tab, nothing sits between the tab and your API call. |
| **Runtime** | One Rust binary. `uwa start` finds Chromium, launches it, and serves. |

## Quickstart

**1.** Install (macOS / Linux / WSL):

```bash
curl -fsSL https://github.com/sdlab1/uwa/releases/latest/download/install.sh | sh
```

See [docs/INSTALL.md](docs/INSTALL.md) for Homebrew, Scoop, Docker, and
build-from-source.

**2.** Run:

```bash
uwa start
```

The first run creates `uwa.toml` from an example, launches Chromium on a
dedicated profile (`~/.uwa/chromium-profile`), waits for it to be ready,
and starts the daemon on `http://127.0.0.1:8080`.

**3.** Log in and point a client.

Open `http://127.0.0.1:8080/#/clients` and copy the config for your
editor. Cursor, Continue, Codex CLI, Claude Code and Aider all have
one-click snippets with your real base URL, key and model pre-filled.

That's it. On loopback, any API key works — you don't need to match
`[server].api_key` from the config.

## Compatibility

| Client | Endpoint | Status | Config |
|---|---|---|---|
| Cursor | `/v1/chat/completions` | ✅ tested | Dashboard → Clients |
| Continue (VS Code / JetBrains) | `/v1/chat/completions` | ✅ tested | Dashboard → Clients |
| Codex CLI | `/v1/responses` | ✅ tested | Dashboard → Clients |
| Claude Code | `/v1/messages` | ✅ tested | Dashboard → Clients |
| Aider | `/v1/chat/completions` | ✅ tested | Dashboard → Clients |
| ZCode | `/v1/chat/completions` | ✅ tested | Dashboard → Clients |
| AutoClaw | `/v1/chat/completions` | ✅ tested | Dashboard → Clients |
| `openai` Python SDK | both | ✅ tested | `base_url="http://127.0.0.1:8080/v1"` |
| `anthropic` Python SDK | `/v1/messages` | ✅ tested | `base_url="http://127.0.0.1:8080"` |
| `curl` | both | ✅ tested | see below |

Compatibility means: the client's full request round-trips through uwa
and the answer lands in the editor without a manual step. Every row above
is covered by an end-to-end test in `verify.sh`.

## Endpoints

| Endpoint | Shape |
|---|---|
| `POST /v1/chat/completions` | OpenAI Chat Completions, streaming & non-streaming, tool calls |
| `POST /v1/messages`         | Anthropic Messages, streaming & non-streaming, tool use |
| `POST /v1/messages/count_tokens` | Anthropic token estimate |
| `POST /v1/responses`        | OpenAI Responses (for Codex CLI) |
| `GET  /v1/models`           | aliases from `uwa.toml`, live from the loaded config |
| `GET  /v1/provider/status`  | per-provider selectors, capabilities, backend |
| `GET  /healthz`, `/readyz`  | liveness & readiness, no auth |

Errors follow the OpenAI shape (`{"error": {"message", "type", "code"}}`)
on every endpoint. A miss on an unknown model returns the list of models
that *are* available plus a "did you mean" suggestion.

## Providers

Everything site-specific lives in `uwa.toml`: URL patterns, CSS
selectors, extraction strategy, finisher tuning. Adding a new provider is
a config edit and a fixture HTML file — no code changes.

```toml
[providers.chatgpt]
name         = "chatgpt"
url_patterns = ["https://chatgpt.com/*"]
capabilities = { streams = true, tool_calls = true, vision = false, max_context_tokens = 128000 }
extraction   = "network_first"    # real token-by-token SSE from the site

[providers.chatgpt.selectors]
input             = "#prompt-textarea"
send_button       = "[data-testid=send-button]"
stop_button       = "[data-testid=stop-button]"
assistant_message = "[data-message-author-role=assistant]"
```

The **Config** page in the dashboard shows every loaded provider and
includes a **Test selector** field that runs the configured CSS against
the live tab, so you can confirm a selector still matches without leaving
the browser.

## Dashboard

Everything is at `http://127.0.0.1:8080`:

- **Dashboard** — request totals, TTFT (avg / p50 / p95), per-provider
  breakdown, tab pool, finish reasons, tab utilization.
- **Playground** — real token-by-token streaming with a live TTFT badge
  and tool-call rendering. Cancel works mid-stream.
- **Clients** — copy-paste configs for the editors above, using your
  real base URL and models.
- **History** — every request with a per-stage waterfall (acquisition →
  send → wait). Click a row for the full record.
- **Sessions** — conversation-to-tab pins, idle reap, drop, recover.
- **Logs** — live SSE tail of tracing output, filterable by level and target.
- **Debug** — bottleneck summary, slowest requests, per-provider deep-dive,
  one-click debug bundle (JSON) for offline investigation.

## What we don't do

Stated plainly so there are no surprises:

- **No token or cookie capture.** uwa drives a tab. It never reads
  storage, `document.cookie`, `localStorage`, or the network bodies of
  auth endpoints. It has no way to hand those to a third party because it
  never acquires them.
- **No captcha solving, no Cloudflare bypass, no 403 evasion.** If a site
  wants a human, we don't pretend to be one.
- **No MCP server.** uwa is an OpenAI/Anthropic-compatible HTTP bridge. A
  client that speaks OpenAI can use it; a client that only speaks MCP
  should use one of the many MCP↔OpenAI shims. We don't add a layer.
- **No silent config edits.** The dashboard never rewrites your
  `uwa.toml` or your editor's config on disk. It shows you the diff, and
  you apply it.
- **No commercial hosting.** The MIT license is permissive, but uwa is
  built for a developer running it on their own laptop. If you deploy it
  to a shared host, you are the one answering to the sites.
- **No stealth arms race.** We add small, deterministic patches
  (`navigator.webdriver`, `window.chrome.runtime`) and stop there. Sites
  that fingerprint aggressively will notice; that's the honest outcome.

## Requirements

- **Chromium or Google Chrome.** uwa finds it, launches it, and manages
  its profile. Nothing else to install.
- **That's it.** The binary ships self-contained. No Rust, Node, Python
  or `npm install` needed to run.

## Development

```bash
git clone https://github.com/sdlab1/uwa.git
cd uwa
./start.sh          # build Rust + UI, launch Chromium, start the daemon
./verify.sh         # cargo test + typecheck + UI build + Playwright E2E
```

`./start.sh` and `uwa start` do the same thing, from different starting
points:

- `uwa start` — you have the binary; it launches Chromium and serves.
- `./start.sh` — you have the source; it builds first, then does the same.

## Docs

- [Install](docs/INSTALL.md) — Homebrew, Scoop, Docker, build from source.
- [Configuration](docs/CONFIG.md) — `uwa.toml`, providers, presets, workflows.
- [API](docs/API.md) — request/response shapes, error codes, examples.
- [Streaming](docs/STREAMING.md) — where the tokens come from, TTFT, cancel.
- [Compatibility](docs/COMPATIBILITY.md) — tested clients and versions.
- [Troubleshooting](docs/TROUBLESHOOTING.md) — the failures people actually hit.
- [Positioning](docs/POSITIONING.md) — what we're building and what we're not.
- [Why UWA](docs/why_uwa.md) — architectural deep-dive comparing UWA to typical Python wrappers.

## License

MIT.