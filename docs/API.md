# API

UWA speaks two wire protocols on the same port:

- **OpenAI Chat Completions** — `/v1/chat/completions`, `/v1/models`, `/v1/responses`.
- **Anthropic Messages** — `/v1/messages`, `/v1/messages/count_tokens`.

Both are authenticated with `Authorization: Bearer <api_key>` when
`[server].api_key` is set (query param `?api_key=…` also works, for
`EventSource`).

## OpenAI Chat Completions

```bash
curl -N http://127.0.0.1:8080/v1/chat/completions \
  -H "authorization: Bearer $UWA_KEY" \
  -H "content-type: application/json" \
  -d '{
    "model": "gpt-4o",
    "stream": true,
    "messages": [{"role": "user", "content": "count to three"}]
  }'
```

Non-streaming response is a standard `chat.completion` object.

Streaming response is SSE: `data: {...chat.completion.chunk...}` frames,
terminated by `data: [DONE]`. Deltas carry **real token chunks** from the
underlying web UI (see [STREAMING.md](STREAMING.md)), not pre-cut strings.

### Tools

UWA is a bridge — it never executes tools. Send `tools: [...]` in the
request; UWA injects their schemas into the browser prompt, parses the
model's tool-call markers, and returns them as standard
`tool_calls`. You run them, then send the results back as `role: "tool"`.

```json
{
  "model": "gpt-4o",
  "messages": [{"role": "user", "content": "weather in NYC?"}],
  "tools": [{
    "type": "function",
    "function": {
      "name": "get_weather",
      "description": "lookup by city",
      "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}
    }
  }]
}
```

The response has `finish_reason: "tool_calls"` and a normal
`message.tool_calls` array. To continue the turn:

```json
{
  "messages": [
    {"role": "user", "content": "weather in NYC?"},
    {"role": "assistant", "content": null,
     "tool_calls": [{"id": "call_1", "type": "function",
                     "function": {"name": "get_weather", "arguments": "{\"city\":\"NYC\"}"}}]},
    {"role": "tool", "tool_call_id": "call_1", "content": "22C"}
  ]
}
```

## Anthropic Messages

```bash
curl -N http://127.0.0.1:8080/v1/messages \
  -H "authorization: Bearer $UWA_KEY" \
  -H "content-type: application/json" \
  -d '{
    "model": "gpt-4o",
    "max_tokens": 128,
    "stream": true,
    "system": "be terse",
    "messages": [{"role": "user", "content": "hi"}]
  }'
```

System may be a string or an array of `{"type":"text","text":…}` blocks.
Content blocks (`tool_use` / `tool_result`) are converted to OpenAI
`tool_calls` / `role:"tool"` and back — history round-trips losslessly.

Streaming emits the standard event sequence: `message_start`,
`content_block_start`, `content_block_delta` (`text_delta` /
`input_json_delta`), `content_block_stop`, `message_delta`, `message_stop`.

## OpenAI Responses

`POST /v1/responses` accepts the Responses API shape (`input` as a string
or array of message objects, `instructions`, `tools`) and returns the
Responses vocabulary (`object: "response"`, `output[].type`,
`output_text`). Streaming is not implemented for this endpoint —
`stream: true` returns a complete body.

## Models

`GET /v1/models` returns the aliases from `[model_aliases]`.

## Diagnostics

| Endpoint | Purpose |
|---|---|
| `GET /healthz` | liveness, no auth |
| `GET /readyz` | provider + model counts, no auth |
| `GET /v1/provider/status` | per-provider selectors / caps / backend |
| `GET /api/pool/status` | tab pool with URLs |
| `GET /admin/history` | recent request records (`?limit=`, `?provider=`, `?status=`) |
| `GET /admin/history/:id` | one record, full JSON |
| `GET /admin/stats` | aggregated stats (`?window=last50|last200|all`) |
| `GET /admin/sessions` | live sessions |
| `DELETE /admin/sessions/:id` | drop a session |
| `POST /admin/sessions/recover` | health-check every session's tab |
| `POST /admin/selector-test` | run a CSS selector against a live tab |
| `POST /admin/selector-generate` | heuristic candidates from a live page |
| `POST /admin/selector-apply` | dry-run: diff proposed vs current selectors |
| `GET /admin/logs/stream` | SSE tail of tracing output |

## Error shape

Every non-2xx is `{"error": {"message", "type", "code"}}` — the OpenAI
shape — regardless of which endpoint you called. Codes:
`bad_request`, `unauthorized`, `model_not_found`, `no_provider`,
`transport_error`, `no_tab`, `extraction_error`, `timeout`,
`unavailable`, `internal_error`, `config_error`.
