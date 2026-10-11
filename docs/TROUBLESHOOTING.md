# Troubleshooting

## `./start.sh` fails at "Checking dependencies"

Missing `cargo` or `npm`. Install:
- Rust: <https://rustup.rs>
- Node.js 20+: <https://nodejs.org>

## `./start.sh` fails at "Chromium"

No Chrome/Chromium was found and none is listening on `UWA_CHROMIUM_WS`.
Either:

- install Chrome/Chromium (on Debian/Ubuntu:
  `sudo apt install chromium-browser`),
- or point at one explicitly:
  `UWA_CHROME_BIN=/path/to/chrome ./start.sh`,
- or reuse an external browser you started yourself:
  `UWA_CHROMIUM_WS=http://127.0.0.1:9222 ./start.sh --no-chromium`.

## "daemon ready" but the dashboard is empty

Set the API key. In the top-right of the dashboard, click **Set key** and
paste the value of `[server].api_key` from `uwa.toml` (the example ships
with `change-me`). The status pill turns green once it's set.

## `POST /v1/chat/completions` returns 503 `no_tab`

Chromium is up but there are no tabs to drive. Open at least one tab
(`about:blank` counts) — the tab pool only sees what Chromium exposes.

## Model returns `model_not_found`

The `model` you sent isn't in `[model_aliases]`. Check the **Config** page
or `GET /v1/models` for the list, or add the alias to `uwa.toml` and
restart.

## Responses are slow and `TTFT ≈ wait`

The site is either buffering or the network-first rules aren't matching.
Open **Debug** → **Slowest requests** and check `response.extraction_source`:

- `"dom"` → network rules didn't match. Verify `[providers.X.net]`
  (`url_contains`, `mime_contains`) against the site's actual traffic
  (see the **Logs** page, filter by the provider name).
- `"network"` with high TTFT → the site itself buffers. Nothing to tune.

## Requests through one provider fail with `circuit <name> open`

The breaker tripped after repeated failures. Look at the **History**
page filtered by that provider; fix the underlying issue (usually a
stale selector). The breaker re-probes automatically every 15 s after a
cooldown — no restart needed.

## A selector matched before but now matches nothing

Sites change their DOM. Use **Config** → **test selector** on the failing
provider. If `matched: 0`, click **scan** to auto-generate candidates
from the live page, then update `uwa.toml` and restart.

## `./start.sh --stop` says "nothing to stop"

The daemon was started outside `start.sh`, or its PID file was removed.
Try `pkill -f target/release/uwa` and, if needed, close the Chromium
that was launched with `--user-data-dir=…/uwa/chromium-profile`.

## Logs

- **Live tail** — `./start.sh` prints daemon logs to stderr in the
  foreground, and the **Logs** page streams them over SSE.
- **Persisted** — request records land in `data/logs/YYYY-MM-DD.jsonl`
  (one JSON object per line). Retention defaults to 14 days.

## Reset

Delete `uwa.toml` to regenerate from the example, and delete
`data/logs/` to drop history. `./start.sh` recreates the config on next
run. To also wipe logins, remove
`~/.cache/uwa/chromium-profile/` (Linux) or
`~/Library/Caches/uwa/chromium-profile/` (macOS).
