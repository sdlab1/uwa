# Browser profiles

A profile is a separate browser context: its own cookies, its own
sessions, its own tabs. Use it to keep work and personal logins apart,
or to run the same site as two different users at the same time.

## Declaring one

```toml
[profiles.work]
label = "Work accounts"
notes = "Stricter limits - the IT department watches."
provider_limits = { chatgpt = 1, claude = 1 }

[profiles.personal]
label = "Personal"
```

Rules:

- `default` is reserved. It refers to the browser's own context - the
  one Chromium was launched with, and the one the user is already
  logged into.
- Names may contain only ASCII letters, digits, `-`, and `_`.
- Every key in `provider_limits` must name a provider that exists in
  the same `uwa.toml`.

## Picking one from a client

Send an `X-UWA-Session` header:

```bash
curl http://127.0.0.1:8080/v1/chat/completions \
  -H 'content-type: application/json' \
  -H 'x-uwa-session: work' \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}'
```

Omit the header to use the default profile. A header that names an
unconfigured profile is a `400` with a list of the configured ones -
we don't silently fall back, because silently falling back to the wrong
account is exactly what profiles exist to prevent.

The Playground has a profile dropdown so you can try it without a
terminal.

## What "separate profile" means

Each named profile is a CDP `BrowserContext`. Chromium gives it:

- its own cookie jar,
- its own `localStorage` and IndexedDB,
- its own set of tabs,
- its own permission grants.

They share the same process, the same network stack, and the same
rendering pipeline - but they don't share a session with any other
profile, including `default`.

When a profile is first used, uwa opens one blank tab in its context.
The user logs in through the dashboard (Profiles - *Open login page*),
and the login is stored inside that context. The next request that uses
the profile reuses the same tab.

## Managing profiles from the dashboard

The **Profiles** page lists every configured profile, its active tabs,
and two actions per profile:

- **Open login page** - opens a URL inside the profile's context. Paste
  a provider login page (`https://chatgpt.com/`), log in as the account
  that belongs to this profile, close the tab. The profile now has that
  login.
- **Reset** - closes the profile's browser context. Every tab it owns is
  dropped, the cookie jar is discarded, and the next use starts fresh.
  The default profile can't be reset this way; restart Chromium with a
  new `--user-data-dir` instead.

## Per-profile concurrency

`provider_limits` overrides the provider's own `max_concurrency` for
requests that use this profile. Useful when you want the same provider
to serve two profiles at different rates - e.g. `work` gets one
concurrent ChatGPT request so it doesn't trip the IT policy, while the
default profile keeps its normal limit.

## Diagnostics

```bash
uwa doctor                          # lists profiles and their active tab counts
curl http://127.0.0.1:8080/admin/profiles | jq
curl http://127.0.0.1:8080/admin/sessions | jq '.sessions[].profile'
```

The **Sessions** page has a `profile` column. A request with an unknown
profile returns `400` with the configured profile list.

## Not supported (yet)

- **Per-profile rate limits at the HTTP layer.** The session manager
  keys leases by `(conversation, profile)`; profile-level concurrency
  caps apply when the transport serializes per tab.
- **Exporting or importing a profile.** There is no "cookie jar" file
  in the API; use Chromium's own profile export if you need it.
- **Renaming a profile.** Delete the old block and add a new one. The
  old context persists on disk until Chromium is restarted with a fresh
  user-data-dir, but it becomes unreachable from uwa.
