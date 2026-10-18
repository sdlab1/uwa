# Response Caching

UWA can cache successful pipeline results at the HTTP edge so that a
repeat deterministic request returns instantly without touching the
provider, the rate limiter, the circuit breaker, or Chromium.

## Behaviour

- **Opt-in, off by default.** Set `[cache] enabled = true` in `uwa.toml`.
- **Only `temperature == 0` (or absent) requests are eligible.** Any other
  temperature is a request for variety and bypasses the cache entirely.
- **Cache hit returns the stored `CachedOutcome` verbatim** — text, tool
  calls, finish reason, media, usage — and skips the whole pipeline.
  Per-request fields (`conversation`, `tab`, `retry_count`) are regenerated.
- **Hits skip the rate limiter** — cache is checked *before* rate limiting,
  so a hit never spends a rate-limit token. Order: cache → rate limit →
  breaker → semaphore → pipeline.
- **Cache key** = 64-bit `DefaultHasher` hash of
  `(provider, effective_model, profile, preset, messages JSON, tools JSON,
  response_format JSON)`. `temperature` is excluded because eligibility has
  already collapsed every eligible value to "deterministic".
- **Response header `X-UWA-Cache`** is `hit` or `miss` on every eligible
  request (ineligible requests omit it).

## Configuration

```toml
[cache]
enabled = true        # default false
ttl_secs = 300        # 1..86400, default 300
capacity = 256        # 1..10000, default 256
stream_from_cache = true  # default true
```

- `ttl_secs` — entry lifetime. Stale entries are dropped on access and by a
  periodic 60s sweep task.
- `capacity` — LRU capacity; oldest entries are evicted when full.
- `stream_from_cache` — when `false`, a streaming request that would otherwise
  hit is forced through the live pipeline instead. When `true` (default), a
  cached streaming response is replayed as SSE.

## Admin endpoints

### `GET /admin/cache`

Returns cache state and lifetime counters:

```json
{
  "enabled": true,
  "capacity": 256,
  "entries": 3,
  "ttl_secs": 300,
  "stream_from_cache": true,
  "hits": 42,
  "misses": 17,
  "stores": 20,
  "hit_rate": 0.71
}
```

When the cache is disabled, returns only `{ "enabled": false }`.

### `POST /admin/cache/clear`

Drops every entry. Lifetime counters survive so hit-rate panels keep meaning.

```json
{ "removed": 3 }
```

## Metrics

- `uwa_cache_hit_total{provider}` — counter incremented on every hit, labelled
  by provider.

## Streaming and cache

A non-stream request populates the cache. A subsequent stream of the same
request is a cache hit *only if* `stream_from_cache = true`; the cached text is
replayed as a single SSE `data` event followed by `[DONE]`. When
`stream_from_cache = false`, the stream always goes through the live pipeline
(and misses are not stored for streaming requests).

## Integration with rate limits (Session 18)

Because the cache is consulted before the rate limiter, a hot cached endpoint
can serve unlimited repeat requests without consuming any rate-limit tokens.
This is intentional: the cached answer required zero provider interaction, so
there is nothing to rate-limit.

## Caveats

- A hash collision would return the wrong answer (probability ~2^-64 per pair).
  Acceptable for a local bridge; a cryptographic hash would cost more than it
  earns.
- The cache is in-memory and per-process; restarts clear it.
- Only successful outcomes are cached. Errors are never stored.
- Streaming misses are not stored (the full streamed response isn't available
  until the pipeline finishes).
