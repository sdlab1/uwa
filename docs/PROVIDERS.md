# Provider selector registry

## Why a registry

Selectors break. ChatGPT renames `#prompt-textarea` every few months;
Claude swaps `div[contenteditable]` for `div.ProseMirror`; Gemini moves
the send button. When a selector goes stale, the whole provider stops
working — the browser has nothing to type into.

The registry is how we ship a fix without a release and without asking
the user to hand-edit their config.

## Layers

```
user's uwa.toml           ← always wins
        ▼
~/.uwa/providers/*.toml   ← written by `uwa providers update`
        ▼
providers/*.toml (repo)   ← embedded in the binary at build time
```

Merge is **per-field**. If the user overrides only `input`, the registry
still supplies `send_button`, `stop_button`, `assistant_message`, and the
net rules. Nothing the user wrote is overwritten.

The registry can only fill fields for providers the user's config already
declares. It never adds a provider — a `[providers.chatgpt]` block has to
exist in `uwa.toml` (with at least `name` and a URL pattern) for the
entry to apply.

## Alternates

Each entry carries a list of fallback candidates per field:

```toml
[alternates.input]
candidates = [
    "#prompt-textarea",
    "div[contenteditable=true][data-lexical-editor]",
    "textarea[placeholder]",
]
```

These are **not** used automatically. They're surfaced in two places:

1. `uwa providers show <name>` prints them.
2. The **Config** page's *test selector* panel, when the primary matches
   0 elements, probes each alternate and offers a **copy override**
   button that puts a snippet on the clipboard.

The point is: you get a suggested fix without the registry mutating your
config behind your back.

## Commands

```bash
uwa providers list           # every entry, its version, whether you override it
uwa providers show chatgpt   # merged view: registry + user, per field
uwa providers diff chatgpt   # only what the user config changes
uwa providers update         # fetch the latest, verify SHA256, write the cache
uwa providers update --dry-run
uwa providers canary --markdown   # check live selectors against open tabs
```

`uwa providers update` never writes to `uwa.toml`. It writes to
`~/.uwa/providers/`; the next `uwa start` (or `uwa daemon restart`) picks
the changes up. To roll back, delete the file from `~/.uwa/providers/` and
restart — the built-in copy takes over.

## Verification

Every entry carries a SHA256 in `providers/manifest.toml`. `uwa providers
update` verifies it before writing. A checksum mismatch aborts the update
and prints both hashes; nothing lands in the cache dir.

The nightly `.github/workflows/canary.yml` opens an issue when a live
selector stops matching. The report lands in
[`docs/CANARY.md`](CANARY.md) with a per-field breakdown: `ok`,
`fallback`, or `miss`.

## Adding or updating an entry

1. Edit `providers/<name>.toml`.
2. Bump `meta.version` to today's date (calendar versioning:
   `YYYY.MM.DD`).
3. Bump the matching entry in `providers/manifest.toml`, then run
   `scripts/regen-registry-sha.sh` to recompute `sha256`.
4. Open a PR. CI runs `cargo test -p uwa-config registry` — every entry
   is parsed and schema-checked before merge.

Never rename an existing field or a `meta.name`. Clients correlate the
registry by name; a rename looks like the provider disappearing.

## Self-heal path

When the pipeline runs and the primary selector matches 0 elements, no
runtime fix happens. What happens is:

1. The Config page's *test selector* probes each alternate.
2. The Debug page's per-request record notes `extraction_source: "dom"`
   and `pin_mode` — an indication the pipeline fell back.
3. `uwa providers canary` reports the provider as `broken` in the
   nightly issue.

We deliberately don't auto-apply fallbacks at runtime: a silently-changed
selector would make "same prompt, different answer" hard to debug. The
alternates are a **suggestion**, and the user or the next registry
release applies them.
