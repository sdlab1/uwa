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

Most "free web AI API" projects converge on the same four design choices — and each one is a bug you eventually get to meet:

| The typical playbook | Why it fails in practice | What uwa does instead |
|---|---|---|
| **Sniff tokens / cookies** from network traffic and replay them | Trivially blocked; breaks on every site update | **Never acquires credentials.** uwa drives a real tab and reads its DOM + CDP events — there is no token to leak because none is ever captured |
| **Log in, then restart the daemon** so auth state gets picked up | Session state lives in process memory, not the browser | **No restart.** You log in *in the tab uwa is already driving* — the login is live by construction |
| **Insert a proxy layer** between your client and the site | Every hop is a place for bugs, and one more thing to keep updated | **No extra hop.** uwa *is* the tab's client — nothing sits between it and your API call |
| **Ship a Python venv** you keep alive by hand | Dependency drift, manual supervision, silent crashes | **One static Rust binary.** `uwa start` finds Chromium, launches it on a dedicated profile, and serves — no venv, no runtime, no supervisor |

That covers the symptoms. The structural difference runs deeper — and it is not a matter of taste: [why_uwa.md](docs/why_uwa.md) walks through **72 points, each citing a file, line and function** from the actual source. **22 of them are things you will not find in a typical Python wrapper.**

<details open>
<summary><strong>A &middot; Architecturally (6)</strong> — the shape of the system</summary>

| # | Guarantee | Source |
|---|---|---|
| 1 | **One `GenericProvider`.** All site-specific behavior — selectors, capabilities, finisher tuning, workflows — is TOML *data*, not code. Adding a site is a config edit + a fixture HTML file | `crates/uwa-providers/src/generic.rs` |
| 2 | **Declarative workflow DSL** — `CLICK`, `FILL_INPUT`, `WAIT`, `STREAM_WAIT`, `KEY_PRESS`, `IF/ELSE`, `GROUP`, `CAPTURE`: a site scenario described in config, not hardcoded steps | `uwa-core/src/workflow.rs` |
| 3 | **Bridge semantics for tools** — client schemas injected into the prompt, replies parsed back into OpenAI `tool_calls`; uwa never executes tools, and unknown ones are passed through to your client | `crates/uwa-tools/src/parser.rs`, `schema.rs` |
| 4 | **7 API surfaces** — OpenAI Chat Completions *and* Responses, Anthropic Messages *and* `count_tokens`, plus models / provider-status / pool introspection | `crates/uwa-api/src/routes/` |
| 17 | **Path-based routing** — `/url/{domain}/v1/...` routes by site, `/tab/{tab_id}/v1/...` pins a specific tab, `X-UWA-Provider` overrides per-request | `crates/uwa-api/src/routing.rs` |
| 32 | **Model groups as a mini-router** — `round_robin`, `failover`, `hash_conversation`, with per-member `(provider, preset)` validation | `crates/uwa-config/src/groups.rs` |

*Python equivalent: none of the six — a new site there means a new class and new code paths.*

</details>

<details>
<summary><strong>B &middot; Qualitatively (4)</strong> — behavior under load, not in demos</summary>

| # | Guarantee | Source |
|---|---|---|
| 5 | **Real streaming** — per-site SSE decoders (ChatGPT cumulative&rarr;diff, Claude `content_block_delta`, Gemini `batchexecute` + XSSI-strip) produce actual token deltas; TTFT is measured, not simulated | `crates/uwa-extract/src/net.rs`, `parsers/` |
| 6 | **Finisher instead of `sleep(5)`** — completion is four independent signals: stop-button gone two polls in a row, network `Finished`, DOM stable, hard timeout — bounded by `min_wait`/`max_wait` | `crates/uwa-extract/src/finisher.rs` |
| 7 | **Sessions pinned to tabs** — `conversation_id` maps to one tab, per-conversation mutex, capacity eviction that never drops a live handle, idle TTL, dead-tab recovery | `crates/uwa-session/src/manager.rs` |
| 9 | **Self-healing circuit breaker** — rolling-window failure counting, half-open probes that *self-release* when the caller disconnects, configurable parallel probes | `crates/uwa-resilience/src/circuit.rs` |

*Python equivalent: post-hoc chunking of a finished answer, `wait_for_selector` + `sleep`, one global tab, crash on the first timeout.*

</details>

<details>
<summary><strong>C &middot; Pedantically (5)</strong> — edge cases nobody notices until production</summary>

| # | Guarantee | Source |
|---|---|---|
| 23 | **`safe_emit_end`** — the stream stops at a byte index that provably cannot leak half a tool-call marker, with UTF-8 boundaries respected | `crates/uwa-tools/src/parser.rs:229` |
| 26 | **80-line schema validator** — covers `type`/`properties`/`required`/`enum`/`items`, ignores unknown keywords; skips the ~1 MB `jsonschema` dependency | `crates/uwa-tools/src/schema.rs` |
| 29 | **`Arc::strong_count` as a busy marker** — capacity eviction checks refcounts, so a session with a live handle is never dropped mid-tool-loop | `crates/uwa-session/src/manager.rs` |
| 67 | **UTF-8-safe preview truncation** — multibyte characters are never split (tested by `does_not_split_multibyte_chars`) | `crates/uwa-history/src/store.rs` |
| 72 | **`ContentPart::Other` via `#[serde(other)]`** — unknown content types deserialize to a placeholder instead of failing the request: forward-compatible with types that do not exist yet | `uwa-core/src/types/openai.rs:90` |

*Python equivalent: "works well enough" — until the first multibyte character, clock skew, or schema evolution.*

</details>

<details>
<summary><strong>D &middot; Systems-level guarantees CPython cannot express (7)</strong></summary>

| # | Guarantee | Source |
|---|---|---|
| 45 | **`PR_SET_PDEATHSIG`** — the kernel itself kills Chromium if the parent test binary dies, so even `kill -9` (OOM, CI timeout) cleans up the browser | `crates/uwa-browser/tests/cdp_smoke.rs` |
| 46 | **Panic hook kills the process group** — `killpg(-pgid, SIGKILL)`: a panic never orphans a running Chrome | `crates/uwa-bin/src/main.rs` |
| 47 | **Daemon tracks PID *and* PGID** — stop is SIGTERM to the whole group, 15 s grace, then SIGKILL; `setsid` on spawn; drain before shutdown | `crates/uwa-bin/src/commands/daemon.rs` |
| 48 | **`uwa doctor`** — one command prints config, bind, key status, providers, models, detected Chromium and CDP readiness | `crates/uwa-bin/src/commands/doctor.rs` |
| 49 | **`uwa update`** — downloads from GitHub Releases, verifies the `.sha256` sidecar, atomically `rename()`s the running binary (kernel holds the inode) | `crates/uwa-bin/src/commands/update.rs` |
| 50 | **`AtomicU8` build guard** — `compare_exchange` prevents two concurrent `npm install`s; 600 s budget; 503 until dist exists | `crates/uwa-bin/src/main.rs` |
| 55 | **`proptest` property tests** — chunk-invariance of the SSE parser, tool-call round-trips, panic-freedom on arbitrary input; nightly runs at 10,000 cases via `PROPTEST_CASES` | `crates/uwa-extract/tests/proptest_net.rs` |

*A Python venv has no kernel-level child reaping, cannot atomically replace its own interpreter binary, and its "constants" are dictionary entries. These are runtime capabilities, not code style.*

</details>

**The numbers behind the claims** — 13 workspace crates &middot; 496 tests &middot; 10 providers in one `config.example.toml` &middot; one static binary &middot; every claim above cites a file you can open right now.

For the full argument, all 72 points: **[docs/why_uwa.md](docs/why_uwa.md)**.

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

| Client | Endpoint | Config |
|---|---|---|
| Cursor | `/v1/chat/completions` | Dashboard → Clients |
| Continue (VS Code / JetBrains) | `/v1/chat/completions` | Dashboard → Clients |
| Codex CLI | `/v1/responses` | Dashboard → Clients |
| Claude Code | `/v1/messages` | Dashboard → Clients |
| Aider | `/v1/chat/completions` | Dashboard → Clients |
| ZCode | `/v1/chat/completions` | Dashboard → Clients |
| AutoClaw | `/v1/chat/completions` | Dashboard → Clients |
| `openai` Python SDK | both | `base_url="http://127.0.0.1:8080/v1"` |
| `anthropic` Python SDK | `/v1/messages` | `base_url="http://127.0.0.1:8080"` |
| `curl` | both | see below |

Compatibility means: the client's full request round-trips through uwa
and the answer lands in the editor without a manual step. Every row above
is covered by an end-to-end test in `verify.sh`.

## Conformance

Tool-calling success rate across providers, measured end-to-end through
the real pipeline (browser, injection, parsing, retries). The table below
is regenerated nightly; a per-scenario breakdown lives on the
`Conformance` page in the dashboard.

<!-- CONFORMANCE-BEGIN -->
_See [docs/CONFORMANCE.md](docs/CONFORMANCE.md) for the current table._
<!-- CONFORMANCE-END -->

The corpus is 30 scenarios across 7 tags — see
[docs/CONFORMANCE.md](docs/CONFORMANCE.md) for the exact list, the
pass/fail rules, and how to add a scenario.

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

Selectors for the built-in providers also ship in `providers/*.toml` and
are refreshed independently of the binary. `uwa providers update` pulls
the latest verified copy into `~/.uwa/providers/`; the user's `uwa.toml`
always wins per-field.

```bash
uwa providers list                # what's in the registry
uwa providers show chatgpt        # merged view, per field
uwa providers update              # fetch + verify SHA256
```

A nightly canary runs each provider's selectors against a live browser
and files an issue when one stops matching —
[docs/CANARY.md](docs/CANARY.md) has the current status. The registry
schema, merge rules, and the fallback story are in
[docs/PROVIDERS.md](docs/PROVIDERS.md).

## Multi-account profiles

Run the same site as two different users at once. Each profile is a
separate browser context with its own cookies:

```toml
[profiles.work]
label = "Work accounts"
provider_limits = { chatgpt = 1 }
```

```bash
curl -H 'x-uwa-session: work' http://127.0.0.1:8080/v1/chat/completions \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}'
```

The **Profiles** page in the dashboard opens a login page inside the
profile's own context and lists the profile's tabs. See
[docs/PROFILES.md](docs/PROFILES.md) for the isolation model, the
per-profile limits, and what "separate context" actually means.

## Recording workflows

Some sites need a couple of clicks before the composer appears — "new
chat", a mode toggle, a model picker. Instead of hand-writing a
`Workflow`, drive the site once and let the recorder write it:

1. **Recorder** page -> **Start recording**.
2. Do the flow manually in the tab.
3. **Stop** — you get a `[[providers.X.workflow]]` block to paste into
   `uwa.toml`.

No drag-and-drop, no node graph, no YAML. See
[docs/RECORDER.md](docs/RECORDER.md) for what gets captured and how the
proposal is built.

## Structured output

`response_format: {"type": "json_object"}` returns a single JSON object —
prose and fences are stripped. `{"type": "json_schema", ...}` adds
validation with a bounded correction-retry, and `strict: false` halves
the budget. Works with `stream: true` (one clean chunk). The
`/v1/responses` spelling is `text.format`. See
[docs/JSON_MODE.md](docs/JSON_MODE.md).

## Dashboard

Everything is at `http://127.0.0.1:8080`:

- **Dashboard** — request totals, TTFT (avg / p50 / p95), per-provider
  breakdown, tab pool, finish reasons, tab utilization.
- **Tokens** — estimated `tokens_in` / `tokens_out` per window,
  per-minute sparklines, and a per-profile breakdown. See
  [docs/TOKENS.md](docs/TOKENS.md).
- **Playground** — real token-by-token streaming with a live TTFT badge
  and tool-call rendering. Cancel works mid-stream.
- **Clients** — copy-paste configs for the editors above, using your
  real base URL and models.
- **History** — cursor-paginated request log with provider, status,
  profile and model filters; per-stage waterfall per record. See
  [docs/PAGINATION.md](docs/PAGINATION.md).
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
- [Structured output](docs/JSON_MODE.md) — `response_format`, JSON-mode retries, streaming behaviour.
- [Token accounting](docs/TOKENS.md) — how usage is estimated and rolled up.
- [Compatibility](docs/COMPATIBILITY.md) — tested clients and versions.
- [Conformance](docs/CONFORMANCE.md) — tool-calling conformance across providers.
- [Providers](docs/PROVIDERS.md) — selector registry, merge rules, self-heal.
- [Profiles](docs/PROFILES.md) — multi-account browser contexts.
- [Recorder](docs/RECORDER.md) — record a flow by hand, get a Workflow.
- [Troubleshooting](docs/TROUBLESHOOTING.md) — the failures people actually hit.
- [Positioning](docs/POSITIONING.md) — what we're building and what we're not.
- [Why UWA](docs/why_uwa.md) — architectural deep-dive comparing UWA to typical Python wrappers.

## Maintenance & known issues

One document covers both the hardening history (wire-fidelity fixes in
the streaming adapters, the daemon probe and log follower, and the
forbid-level clippy campaign) and the live issue tracker — closed items
with their root causes, open ones with repro and suggested fixes:
[docs/ISSUES_AND_BRIDGE.md](docs/ISSUES_AND_BRIDGE.md). Nothing is
scattered across several docs anymore.

Known-open highlights: `https://` health probes are connect-only
(UWA-102), `uwa-config` registry.rs is occasionally edited mid-run by a
parallel agent session (UWA-134), and `uwa-bin` providers.rs is
in-flight / untracked in the sandbox (UWA-135). All forbid-lint debt is
closed (UWA-101, UWA-108).

## License

MIT.