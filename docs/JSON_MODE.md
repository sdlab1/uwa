# Structured output (`response_format`)

Ask for JSON and UWA makes the browser LLM produce it.

## `json_object`

```bash
curl http://127.0.0.1:8080/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{
    "model":"gpt-4o",
    "messages":[{"role":"user","content":"weather in NYC as JSON"}],
    "response_format":{"type":"json_object"}
  }'
```

The response's `content` is a JSON string with a single object. No
prose, no fences. Leading/trailing text the model emits anyway is
stripped.

## `json_schema`

```bash
curl http://127.0.0.1:8080/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{
    "model":"gpt-4o",
    "messages":[{"role":"user","content":"weather in NYC"}],
    "response_format":{
      "type":"json_schema",
      "json_schema":{
        "name":"weather",
        "schema":{
          "type":"object",
          "properties":{
            "city":{"type":"string"},
            "temp_c":{"type":"integer"}
          },
          "required":["city","temp_c"]
        },
        "strict":true
      }
    }
  }'
```

The response is validated against `schema`. On failure the pipeline sends
a correction prompt to the browser and retries; if retries are
exhausted, the request fails with **400** and a message naming the
schema violation.

`strict: false` halves the retry budget — the caller opted into
leniency, so we fight the model less before giving up.

## What the schema supports

The same validator the tool-call path uses (`uwa-tools::schema`):

- `type` — `object`, `array`, `string`, `number`, `integer`, `boolean`, `null`
- `properties` (recursive)
- `required`
- `enum`
- `items` (recursive)

Unknown keywords are ignored. Draft-07 `oneOf`/`anyOf`/`$ref` are not
implemented — a schema that relies on them will validate more loosely
than the reference implementation.

## Streaming

`stream: true` works. In JSON mode the pipeline buffers the whole
response internally, validates it, then emits **one** content chunk
followed by the finish chunk. This matches OpenAI's own behaviour and
means a client that expects one clean JSON blob gets exactly that, not
a token-split one.

Non-JSON streaming is unchanged: deltas arrive as the browser produces
them.

## Interaction with tools

Both can be requested together. The pipeline decides:

1. If the response contains tool markers, it returns `tool_calls` with
   `finish_reason: "tool_calls"`. The client runs them, sends the
   results back as `role:"tool"`.
2. Otherwise the text is validated as JSON against `response_format` and
   returned as `content`.

This mirrors OpenAI's own behaviour for the same combination.

## Errors

A failing `json_schema` request returns:

```json
{
  "error": {
    "message": "response_format `json_schema` not satisfied after 3 attempt(s): schema violation: $.: missing required property `city`",
    "type": "invalid_request_error",
    "code": "bad_request"
  }
}
```

The `attempt(s)` count is `max_tool_retries + 1` for strict schemas,
`max_tool_retries / 2 + 1` for lenient ones.

## Configuration

No new keys. Retries reuse the provider's existing
`max_tool_retries`; the strict/lenient split is the schema's own
`strict` bit. If you want more attempts, raise `max_tool_retries` in
`uwa.toml`.

## Anthropic Messages API

`/v1/messages` has no `response_format` equivalent. A client that wants
structured output uses `/v1/chat/completions`. Anthropic's own API does
not offer this, and inventing an extension here would be a second
surface for no gain.

## Responses API

`/v1/responses` accepts `text.format`:

```json
{
  "text": {
    "format": {
      "type": "json_schema",
      "name": "weather",
      "schema": {"type":"object"},
      "strict": true
    }
  }
}
```

It is mapped to `response_format` before the pipeline runs, so the
retry and validation behaviour is identical to Chat Completions.
