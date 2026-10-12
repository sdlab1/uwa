# Selector health

_This file is regenerated nightly by
[`.github/workflows/canary.yml`](../.github/workflows/canary.yml)._

`uwa providers canary` opens each provider's site in a real browser,
runs the primary selector for `input`, `send_button`, and
`assistant_message`, and records `ok` / `fallback` / `miss` per field.

- **ok** — the primary selector matched at least one element.
- **fallback** — the primary matched 0, but a registry alternate matched.
  The provider works only if the user has applied the fallback.
- **miss** — nothing matched. The provider is broken for every user.
- **unavailable** — no tab was open for that provider's URL. This is not
  a failure of the selector; it means the site wasn't part of this run.

A single red `miss` opens a `canary` issue with the affected providers
listed.

_No run yet. The nightly workflow populates this section._
