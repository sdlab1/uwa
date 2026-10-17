# Token accounting

UWA records an estimate of token usage for every request and rolls it up
for the dashboard.

## How tokens are estimated

The estimator lives in `uwa-tools::tokenizer` and weighs characters:

- ASCII word chars, spaces, newlines: 1 unit each
- ASCII punctuation: 2 units
- Non-ASCII (CJK, Cyrillic, etc.): 5 units

Tokens ≈ units / 4. That lands within ±15% of `cl100k_base` on mixed
English/CJK/code input — good enough for context budgeting and cost
hints, which is all a client uses `usage` for.

Prompt tokens count:
- every message's content (text parts verbatim; image parts at ~150
  tokens each — no detail-level inspection),
- the 4-token role overhead per message,
- every tool-call's name and serialized arguments,
- the JSON of the `tools` array when the client sent one.

Completion tokens count:
- the assistant's text,
- every tool-call the model produced (name + serialized arguments).

The estimate is computed **once** per request, in the pipeline, and
travels through the response unchanged. Clients see the same numbers the
dashboard shows.

## Where it lands

- **Response** — `usage: { prompt_tokens, completion_tokens,
  total_tokens }` in OpenAI shape. Anthropic-shaped responses keep their
  own `usage` mapping, unchanged.
- **History** — every `RequestRecord` carries
  `response.usage.{prompt_tokens, completion_tokens, total_tokens}` and
  the `profile` it ran under.
- **Stats** — `/admin/stats` returns totals (`tokens_in`,
  `tokens_out`), per-minute buckets (`tokens_*_per_minute`, 60 slots),
  and breakdowns per provider (`providers[].tokens_in/out`) and per
  profile (`profiles[].tokens_in/out`).

## Reading the numbers

- **tokens_out / tokens_in** above ~1 with a small prompt usually
  means the model is elaborating. Below ~0.1 means the request is
  heavy on context (tools or history) and light on output — expected
  for tool-call turns.
- **Per-profile** shows which account is generating the cost. Useful
  when you have `[profiles.work]` and `[profiles.personal]` on the
  same site.
- **Per-minute buckets** are 60 slots, most recent last. Same shape as
  `requests_per_minute`, so a client that already renders one sparkline
  can render the other.

## Accuracy caveats

- **Not exact.** If you need exact counts, call
  `/v1/messages/count_tokens` for the prompt and add your own
  completion estimator. The estimator here is deliberately not a BPE
  table — those drift every time a vendor ships a new tokenizer, and
  a 3 MB table in the binary is not a trade we want to make.
- **Images are approximated.** A `detail: low` image is ~85 tokens in
  OpenAI's own accounting; `detail: high` scales with size. The
  estimator uses 150 for every image, which is a middle ground.
- **`response_format` schemas are counted.** A big JSON schema can add
  a few hundred tokens to the prompt; it's counted exactly like any
  other system text.
