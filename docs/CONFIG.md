# Configuration

`uwa.toml` is created from `crates/uwa-bin/config.example.toml` on the first
`./start.sh`. Anything in the example works out of the box; the sections
below explain the parts you'll actually want to touch.

## Server

```toml
[server]
bind  = "127.0.0.1"     # loopback only unless you set api_key
port  = 8080
api_key = "change-me"   # REQUIRED when bind is not loopback
request_timeout_ms = 120000
# pid_file = "uwa.pid"  # defaults to ./uwa.pid
```

`bind` outside loopback without `api_key` is rejected at load time.

## Model aliases

A flat map from the model name your client sends to the provider that
should handle it. Any string your client uses is fine — clients expect
`gpt-4o`, `claude-3-5-sonnet`, etc.

```toml
[model_aliases]
"gpt-4o"            = "chatgpt"
"claude-3-5-sonnet" = "claude"
"gemini-2.0-flash"  = "gemini"
```

## Providers

One `[providers.<name>]` block per site. The name must match `name`.

```toml
[providers.chatgpt]
name         = "chatgpt"
url_patterns = ["https://chatgpt.com/*"]
capabilities = { streams = true, tool_calls = true, vision = false, max_context_tokens = 128000 }
extraction   = "network_first"   # or "dom_only"

[providers.chatgpt.selectors]
input             = "#prompt-textarea"
send_button       = "[data-testid=send-button]"
stop_button       = "[data-testid=stop-button]"
assistant_message = "[data-message-author-role=assistant]"

[providers.chatgpt.finisher]
dom_stable_ms = 700
poll_ms       = 150
min_wait_ms   = 500
max_wait_ms   = 120000

[providers.chatgpt.net]
url_contains  = ["/backend-api/conversation"]
mime_contains = ["text/event-stream"]
idle_timeout_ms = 30000

[providers.chatgpt.net.decoder]
kind = "site"
name = "chatgpt"     # built-in parser; use `sse` / `json` for generic ones
```

- `extraction = "network_first"` prefers CDP-observed SSE bodies; falls back
  to DOM automatically when no matching response arrives.
- `extraction = "dom_only"` skips the network entirely.
- `backend = "cdp" | "nodriver"` overrides the global `[backend].kind`.

The **Config** page in the dashboard is a live read-out of every loaded
provider — selectors, capabilities, backend — plus a **test selector**
field that runs the configured CSS against the live page.

## Presets

A provider can carry several named variants (e.g. Gemini `flash` vs `pro`):

```toml
[providers.gemini]
default_preset = "flash"

[providers.gemini.presets.flash.selectors]
input       = "div.ql-editor"
send_button = "button.send-button"

[providers.gemini.presets.pro.selectors]
input       = "div.ql-editor[data-model='pro']"
```

Route to a preset explicitly with `X-UWA-Preset: pro` or via the group
config below.

## Groups

Fan-out under one logical name:

```toml
[groups.fast]
strategy = "round_robin"   # or "failover" / "hash_conversation"
members  = [
  { provider = "gemini", preset = "flash" },
  { provider = "chatgpt" },
]
```

Then `POST /group/fast/v1/chat/completions` load-balances between them.

## Stealth

```toml
[stealth]
pack = "default"        # or "full"
# user_scripts_dir = "/etc/uwa/stealth.d"
```

Built-in packs are deterministic — no random fingerprints. `default` fixes
`navigator.webdriver` and re-adds `window.chrome.runtime`.

## Proxy

```toml
[proxy]
enabled = true
address = "socks5://127.0.0.1:1080"
bypass  = "localhost,127.0.0.1"
```

Only applies to the nodriver backend (CDP attaches to an already-running
Chromium — restart Chrome with the same proxy flags).

## Backends

```toml
[backend]
kind = "nodriver"        # default; stealth-friendly

[backend.cdp]
ws_url        = "http://127.0.0.1:9222"
idle_ttl_secs = 1800

[backend.nodriver]
python         = "python3"
script         = "sidecar/uwa_nodriver_sidecar.py"
headless       = false
user_data_dir  = "/path/to/profile"
```

Use CDP when you're driving an external Chromium (that's what `start.sh`
sets up); use nodriver when you want the sidecar to launch Chrome with
stealth flags.

## Environment overrides

`UWA__SECTION__FIELD` (double underscore) beats the TOML:

```bash
UWA__SERVER__PORT=9090 UWA__SERVER__API_KEY=secret ./start.sh
```

## Restart schedule

```toml
[scheduled_restart]
enabled    = true
at         = "04:00"     # UTC
drain_secs = 30
```
