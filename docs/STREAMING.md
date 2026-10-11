# Streaming and TTFT

UWA's HTTP layer speaks SSE exactly like OpenAI and Anthropic — the shape
of every frame is standard. What's unusual is where the deltas come from.

## Where the tokens come from

When a provider has `extraction = "network_first"` (the default), UWA
subscribes to the tab's CDP network events and matches the request the
page fires against the provider's `[providers.X.net]` rules. Decoding:

- `kind = "site"` uses a built-in parser for that site (ChatGPT's
  cumulative `parts[0]`, Claude's `content_block_delta`, Gemini's
  `batchexecute`).
- `kind = "sse"` reads each SSE frame's `data` and extracts a JSON path.
- `kind = "json"` parses the whole body as one JSON document.

Each decoded piece is a **delta** — a fragment that appends to the
running text. The streaming pipeline forwards each one to the client
immediately, so `chat.completion.chunk` frames arrive as the browser
produces them.

When no matching network response arrives (encrypted payload, unusual
site), the pipeline falls back to DOM extraction. That fallback emits
the final text in a single chunk — streaming degrades gracefully to a
one-shot, but the wire format doesn't change.

## Time to First Token

UWA records, per request:

- **`send_ms`** — from the start of typing to the moment the send button
  click returns and the input has cleared.
- **`ttft_ms`** — from the end of `send_ms` to the first non-empty delta
  from the browser LLM.
- **`wait_ms`** — the rest of the streaming/extraction window.
- **`total_ms`** — the whole pipeline.

`ttft_ms` is only populated for requests that actually streamed (network-
first). The `/admin/stats` aggregator rolls it up as
`avg_ttft_ms` / `p50_ttft_ms` / `p95_ttft_ms` for the whole window and per
provider. Both the **Dashboard** and the **Debug** page display these
numbers live.

## Reading the numbers

- **TTFT ≪ wait** — the browser started answering fast; the tail is
  token generation. Nothing to tune.
- **TTFT ≈ wait** — either the site buffers its answer (common on some
  providers with `dom_only` extraction), or the network-first rules
  aren't matching. Check `/admin/history/:id` for
  `response.extraction_source: "dom"`.
- **`acquisition_ms` dominates** — the session manager is waiting on a
  tab. Look at `/admin/sessions`: too many leases on too few tabs, or a
  tab wedged behind a slow request.
- **`send_ms` dominates** — input injection or the send button are slow.
  On the **Config** page, use **test selector** on `input` and
  `send_button` to check they still match.

## Turning streaming off

Non-streaming clients (`stream: false` or omitted) get a buffered
response. The pipeline still uses the network-first extractor internally
— it just accumulates the deltas before answering, so extraction quality
is identical. Only the wire shape changes.

## Client-side cancel

The SSE response ends when either side closes the connection. If your
client aborts the request mid-flight, UWA stops reading deltas, touches
the session (so it doesn't get reaped), and records the request as an
error — that's the honest outcome when the client walks away mid-answer.
