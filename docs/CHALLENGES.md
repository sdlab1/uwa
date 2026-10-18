# Challenge Detection & Human Verification

UWA detects when a browser provider page (Cloudflare, Akamai, etc.) replaces
the normal conversation interface with a **challenge interstitial** — a page
that asks you to "Checking your browser...", solve a Turnstile widget, or click
"Verify you are human" — and surfaces it as a structured API error with recovery
instructions.

UWA does **not** solve, bypass, or forge challenge state. Detection only.

---

## How detection works

Three signals, checked in order of reliability:

### 1. Response header (primary)

Cloudflare documents the `cf-mitigated: challenge` response header as the
definitive marker for an interstitial challenge. UWA transports this flag
through the `NetworkEvent::ResponseBody.cf_mitigated_challenge` field on
**all** transports:

- **Nodriver**: parsed from `network.response` event `cf_mitigated` field or
  the `cf-mitigated` response header.
- **CDP**: parsed from the `X-DevTools-Emulator` response headers in
  `pump_page` / `resolve_body`.

**Advantage**: works even when the top-level document URL and DOM haven't
changed (e.g. an XHR challenge on `/api/conversation`).

**Advantage**: no false positives from embedded Turnstile widgets or
JavaScript Detection scripts.

**Limitation**: only visible for responses that flow through the network
event pipeline. A direct page navigation may not be caught this way.

### 2. URL path

A top-level URL containing `/cdn-cgi/challenge-platform/` is a strong
indicator. However:

- `/scripts/jsd/` (JavaScript Detection) is **excluded** — it loads on
  healthy provider pages.
- `/turnstile/` paths are **excluded** — embedded Turnstile widgets use
  `challenges.cloudflare.com/cdn-cgi/challenge-platform/h/g/turnstile/…`.

### 3. DOM probe (fallback)

When the page loads normally (URL and headers are clean), UWA runs a
`DOM_PROBE` script that checks for:

| Marker | DOM selector |
|---|---|
| Challenge form | `#challenge-form` |
| CF challenge token | `[id^="cf-chl-"]` or `[name^="__cf_chl_"]` |
| Challenge stage | `#challenge-stage`, `#cf-challenge-running`, `.cf-browser-verification` |
| Challenge text | Title matching "just a moment\|checking your browser\|verify you are human\|performing security verification\|security check" **AND** body containing "cloudflare\|cf-ray" |

The DOM text check requires **both** the title keyword **and** the Cloudflare
body marker to avoid false positives from pages that merely mention these
phrases.

---

## Pipeline integration

The `ChallengeMonitor` is created per-request after the tab is acquired.
It wraps every browser interaction:

1. **Pre-send probe**: before `send_message`, the monitor checks the current
   tab for an existing challenge (left over from a previous turn).
2. **Send with check**: `send_with_challenge_check` wraps the send call. If
   the send times out or fails, the monitor probes the page to distinguish
   a challenge from a genuine transport/selector error.
3. **Wait with polling**: `await_with_challenge_check` polls the monitor
   every 1 second while waiting for the provider response stream. This
   surfaces challenges **before** the provider's longer timeout fires.
4. **Post-response probe**: after the stream ends (or errors), a final
   `ensure_clear` confirms the page isn't on a challenge interstitial.

A `HumanVerificationRequired` error is treated as **neutral for the circuit
breaker** — it's an interaction requirement, not provider unavailability.

---

## API response shape

When a challenge is detected, the API returns **HTTP 409 Conflict**:

```json
{
  "error": {
    "message": "human verification required for provider `deepseek` in tab `tab_1` at `https://challenges.cloudflare.com/...`",
    "type": "server_error",
    "code": "human_verification_required",
    "verification": {
      "provider": "deepseek",
      "tab_id": "tab_1",
      "url": "https://challenges.cloudflare.com/...",
      "action": "Complete the challenge manually in the logged-in Chromium tab, then retry the request once."
    }
  }
}
```

For streaming responses (SSE), the error is sent as a `data: {…}` frame
with the same structure, followed by `[DONE]`.

---

## Client behavior

The UWA dashboard (Vue/Svelte SPA) and `chatStream` client helper both
detect the `human_verification_required` code and raise an `ApiError` with
status 409. The client should:

1. Show a modal or notification pointing the user to the relevant browser tab.
2. Include the `url` from the `verification` object so the user can navigate
   directly.
3. **Retry the request manually once** after the user solves the challenge.

UWA does **not** automatically retry — the provider may have already
accepted the prompt, and retrying could duplicate the request.

---

## Manual recovery steps

1. Switch to the matching tab in the same Chromium profile UWA is connected to.
2. Complete the challenge yourself (checkbox, Turnstile, CAPTCHA, etc.).
3. Wait for the normal provider page to return.
4. Refresh `/verification` in the UI to confirm no active challenge.
5. Retry the API request manually **once**.

---

## Configuration

```toml
[challenge]
detect = true
auto_wait_ms = 15000  # 15s; 0 = never wait; 120000 max
poll_ms = 500         # 50-5000
```

- `detect = false` disables all challenge checking (pre-Session-20 behavior).
- `auto_wait_ms = 0` means probe once, fail immediately — useful for CI
  where no human can solve challenges.
- `poll_ms` controls how often the monitor polls while waiting.

Validation runs at config load time. Invalid values produce a
`UwaError::Config` at startup.

---

## Admin endpoints

| Method | Path | Description |
|---|---|---|
| GET | `/admin/verification` | Scan all open tabs for visible challenge pages; list detected challenges + scan errors |
| POST | `/admin/verification/:tab/resolved` | Mark a pending challenge as resolved for a tab |
| POST | `/admin/verification/clear` | Clear all pending challenge records from the store |

The store (`VerificationStore`) is an in-memory LRU with a 30-minute TTL,
swept every 120 seconds. It is a **convenience for the UI**, not a
control-flow gate — the pipeline probes the page directly.

---

## File map

| File | Role |
|---|---|
| `crates/uwa-api/src/verification.rs` | `ChallengeMonitor`, `inspect_page`, `info_from_signals`, `VerificationStore` |
| `crates/uwa-config/src/challenge.rs` | `ChallengeCfg` with validation + tests |
| `crates/uwa-core/src/traits.rs` | `NetworkEvent::ResponseBody.cf_mitigated_challenge` field |
| `crates/uwa-core/src/error.rs` | `HumanVerificationRequired` variant + `openai_shape` |
| `crates/uwa-api/src/routes/chat/pipeline.rs` | `send_with_challenge_check`, `await_with_challenge_check`, probe integration |
| `crates/uwa-api/src/routes/status.rs` | `verification_status` scan endpoint |
| `crates/uwa-api/src/error.rs` | Structured `verification` object in API error responses |
| `crates/uwa-api/src/routes/chat/stream.rs` | Structured error frame in SSE |
| `crates/uwa-browser/src/nodriver/mod.rs` | Parse `cf-mitigated` header from CDP events |
| `crates/uwa-browser/src/transport.rs` | Parse `cf-mitigated` header in CDP pump_page |
| `crates/uwa-extract/src/net.rs` | Pass-through for new `cf_mitigated_challenge` field |
| `crates/uwa-browser/src/tap.rs` | `spawn_tap_forwarder` for event broadcast survival |
| `crates/uwa-api/static/src/routes/Verification.svelte` | UI page for challenge detection |

---

## Sessions 20-23 summary

Session 20 (todo/49) implemented the core detection infrastructure. Sessions
21-23 (todo/50-52) refined the heuristics, added configuration, and hardened
edge cases. The full implementation is the union of all three sessions'
requirements.
