# History pagination

`GET /admin/history` is cursor-paginated. Every page carries the cursor
you need to ask for the next one.

## Shape

```
GET /admin/history?limit=100&before_id=req_abc
```

| Query | Meaning |
|---|---|
| `limit` | Page size. Clamped to 1..500; default 50. |
| `before_id` | Cursor. The id of the **newest** record you already have. The response starts strictly older than it. |
| `provider` | Exact match on the provider name. |
| `status` | `success` \| `error` \| `pending`. Anything else is 400. |
| `profile` | Exact match on profile name; `default` matches records without one. |
| `model` | Exact match on the model alias the client sent. |

Filters AND together. Cursors and filters compose — a "page 4 of the
`work` profile, `chatgpt` provider, errors only" request is one line.

## Response

```json
{
  "count": 100,
  "buffer_size": 1000,
  "total_matching": 342,
  "next_before_id": "req_xyz",
  "records": [ /* newest first */ ]
}
```

- `count` — how many records are in this page.
- `buffer_size` — how many the in-memory ring holds right now.
- `total_matching` — how many records match the current filters across
  the whole scanned range. Use it to render "100 of 342".
- `next_before_id` — the cursor for the next page, or `null` when there
  is nothing left to fetch.

## Cursor semantics

The in-memory ring is bounded (`[history].buffer_size`, default 1000).
The cursor is the id of a record **in the ring**. Three cases:

- **Found.** Start after it, newest-first within the filters.
- **Evicted.** The record the cursor points at has aged out of the ring
  since the caller got it. The response is empty with
  `next_before_id: null`. We don't guess where to resume; the data the
  caller asked to continue from isn't there anymore.
- **Missing entirely** (typo, wrong endpoint). Same as evicted.

A caller that wants a snapshot of "everything up to now" should fetch
with `limit` high enough for the ring, or handle the empty page
gracefully — an empty page is a legitimate end-of-stream marker.

## Why not offset/limit

Offsets and a bounded ring don't compose: a request for `offset=800` when
the ring holds 1000 records and has evicted 5000 would return data that
shifted meaning between pages. Cursor is monotonic relative to what the
caller has already seen, so a page you fetched is still "the page after
the previous one" even if the ring shifted underneath.

## UI

The History page in the dashboard uses infinite scroll with a **Load
more** button as a fallback. It stops its periodic auto-refresh the
moment the user starts paginating (a background refresh would blow away
the extra pages). **Reset** clears every filter and re-fetches page 1.

## What we don't do

- **Persisted cursor.** The ring is in memory. After a daemon restart,
  every cursor becomes stale; clients handle this the same way they
  handle evicted cursors — start over.
- **Reverse order.** Newest-first only. If you need chronological
  order, reverse the array client-side.
- **Streaming pages.** Each page is a single JSON response. A caller
  that wants every record should walk the cursor to `null`.
