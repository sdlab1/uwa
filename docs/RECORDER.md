# The recorder

The recorder is how UWA turns "I know what I clicked, I just don't want
to write a `Workflow` by hand" into a config fragment.

## What it is not

Not a drag-and-drop editor. Not a DSL. There is no node graph, no
pipeline builder, no per-step UI. The full surface is:

1. **Start recording.**
2. Do the thing once, by hand, in the tab.
3. **Stop.** You get a `[[providers.X.workflow]]` block to paste.

Everything else is the existing `Workflow` engine. This page is a thin
observation layer on top of it.

## Why

Every micro-workflow editor ends up as a project of its own — a
node-graph UI, a validation layer, a runtime, a visual debugger. The
audience that needs to configure a scraper against a chat site is
overwhelmed by that, and the audience that enjoys it doesn't need UWA.
The recorder inverts the problem: the user demonstrates, the machine
writes, the user deletes what it got wrong.

## What gets recorded

- **Clicks** on buttons, links, and any element with a `data-testid`.
  The recorder uses the same CSS-path heuristic as
  `uwa-providers::autogen` — id first, then well-known data
  attributes, then tag + up to two classes, capped at four levels.
- **Inputs** on `<input>`, `<textarea>`, and contenteditable elements.
  One `input` event per keystroke; the Rust side collapses a run on the
  same element to the final value.
- **Enter** keypresses. Chat UIs submit this way often enough that
  dropping them would miss the second half of the flow.
- **Native form submits.** Rare on the sites UWA drives, but free.

## What does not get recorded

- Navigation to a URL. Recording starts on whatever tab you point it
  at; the site is already open.
- Network responses. Those aren't user actions; extraction is handled
  by the provider's `net` rules, not by the workflow.
- Anything inside a cross-origin iframe. The observer is a single
  `document.addEventListener` chain; it doesn't reach into a different
  origin's realm.

## How the proposal is built

Three passes over the raw trace:

1. **Collapse inputs.** `input` fires per keystroke. A run on the same
   element becomes one `FillInput`.
2. **Dedupe clicks.** Same selector within 500 ms is one intent — chat
   UIs occasionally fire the button twice (icon + wrapper).
3. **Convert to actions.** Each event becomes an `Action`:
   - A click or input on a configured selector becomes a named target
     (`input_box`, `send_btn`, `result_container`, ...). The match is
     loose: `[data-testid=send-button]` and
     `button[data-testid="send-button"]` are the same element.
   - A click on anything else becomes a raw CSS selector, marked
     `optional = true`. The runner accepts raw selectors when the name
     contains CSS metacharacters.
   - Pauses longer than 1.5 s become `Wait`. The value is capped at 5 s
     so a coffee break doesn't become a `Wait { seconds: 3600 }`.
   - A `StreamWait` on the assistant message is appended at the end if
     the provider has one configured — it's the step users forget.

Numeric literals typed into the input (`"3"`, `"2.5"`) are recorded as
`value = "..."`; everything else is left as `value = None` so the real
user prompt is substituted at run time. Baking free text into the
workflow would replay the recording's prompt on every run — never what
a chat flow wants.

## Where the code lives

| Piece | File |
|---|---|
| JS observer | `crates/uwa-browser/src/js/recorder_install.js` |
| Rust poller / install | `crates/uwa-browser/src/recorder.rs` |
| HTTP state | `crates/uwa-api/src/recorder.rs` |
| Endpoints | `crates/uwa-api/src/routes/admin.rs` |
| Proposal | `crates/uwa-providers/src/workflow/recorder.rs` |
| TOML rendering | same file, `workflow_to_toml_snippet` |
| UI | `crates/uwa-api/static/src/routes/Recorder.svelte` |

## Endpoints

```
POST /admin/recorder/start   { "provider": "chatgpt" }  -> RecorderStatus
GET  /admin/recorder/status                             -> RecorderStatus
POST /admin/recorder/stop                               -> RecorderStopResponse
```

One recording at a time. A second `start` while one is active returns
`400` with a message naming the in-flight id.

## Limits

- **15-minute ceiling** on a single recording.
- **5000-event cap**; the oldest events are dropped beyond it.
- **Single tab.** No cross-tab or cross-origin iframe capture.
- **Origin-scoped state.** `sessionStorage` is per-origin, so navigating
  from `chatgpt.com` to `auth.openai.com` mid-recording loses the
  observer until the poller reinstalls it — which it does within 500 ms.
