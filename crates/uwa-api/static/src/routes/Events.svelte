<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import type { EventListResp, LoggedEvent, EventSummary } from '../lib/api';
  import { getEvents, clearEvents, streamEvents } from '../lib/api';
  import { toast } from '../lib/stores';
  import { fmtRel } from '../lib/format';

  const MAX_LINES = 2000;

  let target = $state<string>('');
  let kind = $state<string>('');
  let url = $state<string>('');
  let paused = $state(false);
  let follow = $state(true);
  let available = $state(true);
  let hint = $state<string | null>(null);
  let summary = $state<EventSummary | null>(null);
  let events: LoggedEvent[] = $state([]);
  let selected: LoggedEvent | null = $state(null);
  let closeFn: (() => void) | null = null;
  let scrollRef: HTMLDivElement | undefined;

  function when(ms: number): string {
    const d = new Date(ms);
    return d.toLocaleTimeString() + '.' + String(d.getMilliseconds()).padStart(3, '0');
  }
  function shortTarget(t: string): string {
    return t.length > 12 ? t.slice(0, 8) + '…' : t;
  }
  function sizeStr(n?: number): string {
    if (n == null) return '';
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
    return `${(n / 1024 / 1024).toFixed(2)} MiB`;
  }

  async function refresh() {
    try {
      const r: EventListResp = await getEvents({
        limit: 200,
        target: target || undefined,
        kind: kind || undefined,
        url: url || undefined,
      });
      available = r.available;
      hint = r.hint ?? null;
      summary = r.summary ?? null;
      events = r.events.slice().reverse();
    } catch (e) {
      toast(`Load failed: ${String(e)}`, 'err');
    }
  }

  function startStream() {
    if (closeFn) return;
    closeFn = streamEvents(
      {
        target: target || undefined,
        kind: kind || undefined,
        url: url || undefined,
      },
      (ev) => {
        if (paused) return;
        if (events.length > 0 && events[events.length - 1]!.id === ev.id) return;
        events = [...events, ev];
        if (events.length > MAX_LINES) events = events.slice(events.length - MAX_LINES);
      },
    );
  }

  function stopStream() {
    closeFn?.();
    closeFn = null;
  }

  function reapply() {
    stopStream();
    events = [];
    selected = null;
    void refresh().then(() => startStream());
  }

  function onScroll() {
    if (!scrollRef) return;
    const gap = scrollRef.scrollHeight - scrollRef.scrollTop - scrollRef.clientHeight;
    follow = gap < 40;
  }

  $effect(() => {
    if (!follow || paused || !scrollRef) return;
    requestAnimationFrame(() => {
      if (scrollRef) scrollRef.scrollTop = scrollRef.scrollHeight;
    });
  });

  $effect(() => {
    void target;
    void kind;
    void url;
    const h = window.setTimeout(reapply, 250);
    return () => window.clearTimeout(h);
  });

  async function clearAll() {
    if (!confirm('Clear the retained events? The lifetime counter stays.')) return;
    try {
      await clearEvents();
      events = [];
      selected = null;
      await refresh();
    } catch (e) {
      toast(`Clear failed: ${String(e)}`, 'err');
    }
  }

  onMount(() => {
    const hash = location.hash;
    const q = hash.indexOf('?');
    if (q >= 0) {
      const p = new URLSearchParams(hash.slice(q + 1));
      const t = p.get('target');
      if (t) target = t;
    }
    void refresh().then(() => startStream());
  });

  onDestroy(() => stopStream());

  const detailBody = $derived.by(() => {
    if (!selected) return '';
    if (selected.kind === 'response_body') return selected.body ?? '';
    return '';
  });
</script>

<div class="flex items-center justify-between mb-1">
  <h2>Events</h2>
  <div class="flex items-center gap-2">
    {#if summary}
      <span class="text-[11px] muted">
        {summary.retained} / {summary.capacity} retained · {summary.total} total
      </span>
    {/if}
    <button class="btn-secondary" onclick={() => (paused = !paused)}>
      {paused ? 'Resume' : 'Pause'}
    </button>
    <button class="btn-secondary" onclick={clearAll}>Clear</button>
  </div>
</div>
<p class="lead">
  Every network response the browser produced, as uwa sees it. This is
  the raw material the extraction pipeline reads — useful when a
  selector goes stale and you want to know whether the response arrived
  at all.
</p>

{#if !available}
  <div class="card border-warn">
    <h3 class="m-0">No event tap available</h3>
    <p class="text-[12.5px] muted mt-2 mb-0">
      {hint ?? 'The active transport does not expose a network event tap.'}
      This is a known limitation of the nodriver sidecar backend; the CDP
      backend (what <code>uwa start</code> uses) always has one.
    </p>
  </div>
{:else}
  <div class="flex flex-wrap items-center gap-2 mb-3">
    <select class="input max-w-[200px]" bind:value={target}>
      <option value="">all tabs</option>
      {#each summary?.targets ?? [] as t (t.target)}
        <option value={t.target}>{shortTarget(t.target)} ({t.count})</option>
      {/each}
    </select>
    <select class="input max-w-[170px]" bind:value={kind}>
      <option value="">any kind</option>
      <option value="response_body">response_body</option>
      <option value="finished">finished</option>
    </select>
    <input class="input max-w-[260px]" placeholder="url contains…" bind:value={url} />
    <label class="flex items-center gap-1.5 text-[12px] muted select-none cursor-pointer">
      <input type="checkbox" bind:checked={follow} /> follow
    </label>
    <span class="text-[11px] muted ml-auto">{events.length} shown</span>
  </div>

  <div class="events-layout">
    <div class="events-list" bind:this={scrollRef} onscroll={onScroll}>
      {#if events.length === 0}
        <p class="muted text-[12px] m-0">Waiting for events…</p>
      {:else}
        {#each events as ev (ev.id)}
          <div
            class="event-row"
            class:selected={selected?.id === ev.id}
            onclick={() => (selected = ev)}
            role="button"
            tabindex="0"
            onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') selected = ev; }}>
            <span class="event-time">{when(ev.at_ms)}</span>
            <span class="event-target" title={ev.target}>{shortTarget(ev.target)}</span>
            <span class="event-kind {ev.kind}">{ev.kind === 'response_body' ? 'resp' : 'done'}</span>
            {#if ev.kind === 'response_body'}
              <span class="event-url" title={ev.url ?? ''}>{ev.url}</span>
              <span class="event-mime">{ev.mime}</span>
              <span class="event-size">{sizeStr(ev.size_bytes)}{#if ev.truncated}✱{/if}</span>
            {:else}
              <span class="event-url" title={ev.request_id ?? ''}>{ev.request_id}</span>
            {/if}
          </div>
        {/each}
      {/if}
    </div>

    <div class="events-detail">
      {#if selected}
        <div class="card" style="margin: 0;">
          <div class="card-head">
            <h3 class="m-0 text-[14px]">
              {selected.kind === 'response_body' ? 'Response body' : 'Request finished'}
            </h3>
            <button class="ghost-btn" onclick={() => (selected = null)}>close</button>
          </div>
          <dl class="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1 text-[12.5px] m-0 mb-3">
            <dt class="muted">id</dt><dd class="m-0 font-mono">{selected.id}</dd>
            <dt class="muted">at</dt><dd class="m-0 font-mono">{when(selected.at_ms)}</dd>
            <dt class="muted">target</dt><dd class="m-0 font-mono">{selected.target}</dd>
            {#if selected.kind === 'response_body'}
              <dt class="muted">url</dt><dd class="m-0 font-mono break-all">{selected.url}</dd>
              <dt class="muted">mime</dt><dd class="m-0 font-mono">{selected.mime}</dd>
              <dt class="muted">size</dt>
              <dd class="m-0 font-mono">
                {sizeStr(selected.size_bytes)}{#if selected.truncated} (truncated){/if}
              </dd>
            {:else}
              <dt class="muted">request_id</dt>
              <dd class="m-0 font-mono break-all">{selected.request_id}</dd>
            {/if}
          </dl>
          {#if selected.kind === 'response_body'}
            <pre class="event-body">{detailBody}</pre>
          {/if}
        </div>
      {:else}
        <div class="card" style="margin: 0;">
          <p class="muted text-[12.5px] m-0">
            Select an event to see its full body.
          </p>
        </div>
      {/if}
    </div>
  </div>
{/if}
