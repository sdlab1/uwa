<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import type { LogEvent } from '../lib/types';
  import { streamLogs } from '../lib/api';
  import { toast } from '../lib/stores';

  type Line = LogEvent & { ts: number };

  let lines = $state<Line[]>([]);
  let connected = $state(false);
  let paused = $state(false);
  let follow = $state(true);
  let levelFilter = $state<'' | 'INFO' | 'WARN' | 'ERROR' | 'DEBUG' | 'TRACE'>('');
  let targetFilter = $state('');

  const MAX_LINES = 2000;
  let closeFn: (() => void) | null = null;
  let scrollRef: HTMLDivElement | undefined;

  const visible = $derived(
    lines.filter((l) => {
      if (levelFilter && l.level !== levelFilter) return false;
      if (targetFilter && !(l.target ?? '').toLowerCase().includes(targetFilter.toLowerCase())) return false;
      return true;
    }),
  );

  function onScroll() {
    if (!scrollRef) return;
    const gap = scrollRef.scrollHeight - scrollRef.scrollTop - scrollRef.clientHeight;
    follow = gap < 40;
  }

  // autoscroll only when following and not paused
  $effect(() => {
    void visible.length;
    if (!follow || paused || !scrollRef) return;
    requestAnimationFrame(() => {
      if (scrollRef) scrollRef.scrollTop = scrollRef.scrollHeight;
    });
  });

  function start() {
    if (closeFn) return;
    connected = true;
    closeFn = streamLogs(
      (ev) => {
        if (paused) return;
        const e = ev as LogEvent;
        if (typeof e !== 'object' || e == null) return;
        lines = [...lines, { ...e, ts: Date.now() }];
        if (lines.length > MAX_LINES) lines = lines.slice(lines.length - MAX_LINES);
      },
      () => {
        connected = false;
      },
    );
  }

  function stop() {
    if (!closeFn) return;
    closeFn();
    closeFn = null;
    connected = false;
  }

  function clear() {
    lines = [];
  }

  function togglePause() {
    paused = !paused;
    if (!paused && follow) {
      requestAnimationFrame(() => {
        if (scrollRef) scrollRef.scrollTop = scrollRef.scrollHeight;
      });
    }
  }

  onMount(start);
  onDestroy(stop);

  function levelClass(level: string): string {
    switch (level) {
      case 'ERROR': return 'log-ERROR';
      case 'WARN':  return 'log-WARN';
      case 'INFO':  return 'log-INFO';
      case 'DEBUG': return 'log-DEBUG';
      default:      return 'log-TRACE';
    }
  }
</script>

<div class="flex items-center justify-between mb-1">
  <h2>Logs</h2>
  <span class="text-[11px] muted inline-flex items-center gap-2">
    <span class="w-1.5 h-1.5 rounded-full {connected ? 'bg-ok' : 'bg-err'}"></span>
    {connected ? 'connected' : 'disconnected'}
    · {lines.length} line{lines.length === 1 ? '' : 's'}
    {#if paused}· <span class="warn">paused</span>{/if}
  </span>
</div>
<p class="lead">Live tracing output from uwa. Streamed over SSE from <code>/admin/logs/stream</code>.</p>

<div class="flex flex-wrap items-center gap-2 mb-3">
  <select class="input max-w-[130px]" bind:value={levelFilter}>
    <option value="">all levels</option>
    <option value="ERROR">ERROR</option>
    <option value="WARN">WARN</option>
    <option value="INFO">INFO</option>
    <option value="DEBUG">DEBUG</option>
    <option value="TRACE">TRACE</option>
  </select>
  <input class="input max-w-[220px]" placeholder="filter target (substring)" bind:value={targetFilter} />
  <button class="btn-secondary" onclick={togglePause}>{paused ? 'Resume' : 'Pause'}</button>
  <button class="btn-secondary" onclick={clear}>Clear</button>
  <label class="flex items-center gap-1.5 text-[12px] muted select-none cursor-pointer">
    <input type="checkbox" bind:checked={follow} /> follow
  </label>
  <span class="text-[11px] muted ml-auto">showing {visible.length} / {lines.length}</span>
</div>

<div class="log-stream" bind:this={scrollRef} onscroll={onScroll}>
  {#if visible.length === 0}
    <p class="muted text-[12px] m-0">
      {lines.length === 0
        ? 'Waiting for log events…'
        : 'No lines match the current filter.'}
    </p>
  {:else}
    {#each visible as l, i (i)}
      <div class="log-line {levelClass(l.level)}">
        <span class="log-time">{new Date(l.ts).toLocaleTimeString()}</span>
        <span class="log-level">[{l.level}]</span>
        <span class="log-target">{l.target}</span>
        <span class="log-msg">{l.message}{l.fields ? ' ' + l.fields : ''}</span>
      </div>
    {/each}
  {/if}
</div>
