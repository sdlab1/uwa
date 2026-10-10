<script lang="ts">
  import { onMount } from 'svelte';
  import type { ProviderMap, ReadyResp, StatsResp } from '../lib/types';
  import { getProviders, getReady, getStats } from '../lib/api';
  import Card from '../lib/components/Card.svelte';
  import StatCard from '../lib/components/StatCard.svelte';
  import MiniBars from '../lib/components/MiniBars.svelte';
  import LiveDot from '../lib/components/LiveDot.svelte';
  import PoolPanel from '../lib/components/PoolPanel.svelte';
  import { fmtMs, fmtNum, fmtPercent, pct } from '../lib/format';

  type WindowKey = 'last50' | 'last200' | 'all';

  let statsWindow: WindowKey = $state('last200');
  let stats    = $state<StatsResp | null>(null);
  let ready    = $state<ReadyResp | null>(null);
  let providers= $state<ProviderMap | null>(null);
  let err      = $state<string | null>(null);
  let updatedAt= $state<number | null>(null);
  let refreshing = false;

  // Client-side history of `requests_per_minute[last]` — the backend
  // already returns the full 60-slot histogram, but a sparkline over time
  // makes the *current* rate legible at a glance.
  const rpmHistory: number[] = [];

  let poolRef: PoolPanel | undefined;

  async function refresh(silent = false) {
    if (refreshing) return;
    refreshing = true;
    try {
      const [s, r, p] = await Promise.all([
        getStats(statsWindow),
        getReady().catch(() => null),
        getProviders().catch(() => null),
      ]);
      stats = s;
      ready = r;
      providers = p;
      err = null;
      updatedAt = Date.now();

      const tail = s.requests_per_minute ?? [];
      const last = tail.length ? tail[tail.length - 1]! : 0;
      rpmHistory.push(last);
      if (rpmHistory.length > 60) rpmHistory.shift();
    } catch (e) {
      if (!silent) err = String(e);
    } finally {
      refreshing = false;
    }
  }

  onMount(() => {
    void refresh();
    const iv = setInterval(() => {
      void refresh(true);
      void poolRef?.refresh();
    }, 3000);

    // A hidden tab keeps a network + battery cost we don't want; skip
    // ticks until the user comes back.
    const onVis = () => {
      if (document.visibilityState === 'visible') {
        void refresh(true);
        void poolRef?.refresh();
      }
    };
    document.addEventListener('visibilitychange', onVis);

    return () => {
      clearInterval(iv);
      document.removeEventListener('visibilitychange', onVis);
    };
  });

  // Derived values
  const errRate = $derived(stats && stats.total > 0 ? stats.error / stats.total : 0);
  const rpmArr  = $derived(stats?.requests_per_minute?.length
    ? stats.requests_per_minute
    : Array(60).fill(0) as number[]);
  const currentRpm = $derived(rpmArr[rpmArr.length - 1] ?? 0);
  const maxTabUse  = $derived(
    stats ? Math.max(1, ...Object.values(stats.tab_utilization)) : 1
  );

  function switchWindow(w: WindowKey) {
    if (statsWindow === w) return;
    statsWindow = w;
    rpmHistory.length = 0;
    void refresh();
  }
</script>

<div class="flex items-center justify-between mb-1">
  <h2>Dashboard</h2>
  <div class="flex items-center gap-3">
    <LiveDot on={!err} label={err ? 'offline' : 'live'} />
    {#if updatedAt}
      <span class="text-[11px] muted">
        updated {new Date(updatedAt).toLocaleTimeString()}
      </span>
    {/if}
  </div>
</div>
<p class="lead">Live view of traffic, latency, time-to-first-token and provider health.</p>

{#if err}
  <div class="card border-err">
    <div class="card-head">
      <h3 class="m-0 text-err">Cannot reach stats</h3>
      <button class="btn-secondary" onclick={() => void refresh()}>Retry</button>
    </div>
    <p class="text-[13px] muted">{err}</p>
    <p class="text-[12px] muted mt-2">
      If an API key is configured, set it in the top-right. The history store
      may also be disabled in <code>uwa.toml</code>.
    </p>
  </div>
{:else if !stats}
  <div class="card"><p class="muted">Loading…</p></div>
{:else}
  <!-- Overview KPIs -->
  <div class="card">
    <div class="card-head">
      <h3 class="m-0">Overview</h3>
      <div class="flex items-center gap-1">
        {#each ['last50', 'last200', 'all'] as w (w)}
          <button
            class="ghost-btn"
            class:border-accent={statsWindow === w}
            class:text-text={statsWindow === w}
            onclick={() => switchWindow(w as WindowKey)}>{w}</button>
        {/each}
      </div>
    </div>
    <div class="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-6 gap-2.5">
      <StatCard label="Total"   value={fmtNum(stats.total)} />
      <StatCard label="Success" value={fmtNum(stats.success)} tone="ok" />
      <StatCard
        label="Errors"
        value={fmtNum(stats.error)}
        tone={stats.error > 0 ? 'err' : 'neutral'}
        sub={fmtPercent(errRate)} />
      <StatCard label="Avg" value={fmtMs(stats.avg_total_ms)} />
      <StatCard label="p50" value={fmtMs(stats.p50_total_ms)} />
      <StatCard label="p95" value={fmtMs(stats.p95_total_ms)} />
    </div>
  </div>

  <!-- TTFT + RPM -->
  <div class="grid grid-cols-1 lg:grid-cols-2 gap-4">
    <Card title="Time to first token">
      <div class="grid grid-cols-3 gap-2.5 mb-3">
        <StatCard label="avg TTFT"
                  value={fmtMs(stats.avg_ttft_ms)}
                  highlight={stats.avg_ttft_ms != null} />
        <StatCard label="p50 TTFT" value={fmtMs(stats.p50_ttft_ms)} />
        <StatCard label="p95 TTFT" value={fmtMs(stats.p95_ttft_ms)} />
      </div>
      {#if stats.avg_ttft_ms == null}
        <p class="text-[12px] muted">
          TTFT will populate once the backend streams real token deltas
          (Session 6). Until then this card shows <code>—</code>.
        </p>
      {:else}
        <p class="text-[12px] muted">
          Lower is better. p95 above ~2s usually points to a slow tab
          or a cold session acquisition.
        </p>
      {/if}
    </Card>

    <Card title="Requests per minute" subtitle="last 60 min">
      <div class="flex items-baseline gap-2 mb-2">
        <span class="text-2xl font-semibold">{currentRpm}</span>
        <span class="text-xs muted">rpm now</span>
        {#if rpmHistory.length >= 2}
          {@const prev = rpmHistory[rpmHistory.length - 2] ?? 0}
          {@const delta = currentRpm - prev}
          {#if delta !== 0}
            <span class="text-[11px]" class:ok={delta > 0} class:err={delta < 0}>
              {delta > 0 ? '+' : ''}{delta}
            </span>
          {/if}
        {/if}
      </div>
      <MiniBars values={rpmArr} height={44} />
      <div class="flex justify-between text-[10px] muted mt-1">
        <span>-60m</span><span>-30m</span><span>now</span>
      </div>
    </Card>
  </div>

  <!-- Per provider -->
  <Card title="Per provider">
    {#if stats.providers.length === 0}
      <p class="muted text-[13px]">No traffic yet.</p>
    {:else}
      <div class="overflow-x-auto">
        <table>
          <thead>
            <tr>
              <th>Provider</th>
              <th class="text-right">Total</th>
              <th class="text-right">Err %</th>
              <th class="text-right">Avg</th>
              <th class="text-right">p95</th>
              <th class="text-right">Avg TTFT</th>
              <th class="text-right">Source</th>
            </tr>
          </thead>
          <tbody>
            {#each stats.providers as p (p.provider)}
              <tr>
                <td class="font-medium">{p.provider}</td>
                <td class="text-right">{fmtNum(p.total)}</td>
                <td
                  class="text-right"
                  class:err={p.error > 0 && p.error_rate >= 0.1}
                  class:warn={p.error > 0 && p.error_rate < 0.1}
                  class:ok={p.error === 0}>
                  {fmtPercent(p.error_rate)}
                </td>
                <td class="text-right">{fmtMs(p.avg_total_ms)}</td>
                <td class="text-right">{fmtMs(p.p95_total_ms)}</td>
                <td class="text-right">{fmtMs(p.avg_ttft_ms)}</td>
                <td class="text-right text-[11px] muted">
                  {#if p.network_count != null || p.dom_count != null}
                    {p.network_count ?? 0} net / {p.dom_count ?? 0} dom
                  {:else}—{/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </Card>

  <!-- Readiness + Pool -->
  <div class="grid grid-cols-1 lg:grid-cols-2 gap-4">
    <Card title="Readiness">
      {#if ready}
        <div class="flex items-center gap-2 mb-3">
          <span class="pill"
                class:pill-ok={ready.providers_loaded}
                class:pill-warn={!ready.providers_loaded}>
            {ready.status}
          </span>
          <span class="text-[12px] muted">
            {ready.models} model alias{ready.models === 1 ? '' : 'es'}
          </span>
        </div>
        {#if providers && Object.keys(providers).length > 0}
          <div class="space-y-1.5">
            {#each Object.entries(providers) as [name, info] (name)}
              <div class="flex items-center justify-between text-[12.5px]">
                <span>{name}</span>
                <span class="muted text-[11px]">
                  {info.extraction} · {info.backend ?? 'default'}
                </span>
              </div>
            {/each}
          </div>
        {:else}
          <p class="muted text-[12px]">
            No providers loaded — add <code>[providers.*]</code> blocks to uwa.toml.
          </p>
        {/if}
      {:else}
        <p class="muted text-[13px]">Not available.</p>
      {/if}
    </Card>

    <PoolPanel bind:this={poolRef} />
  </div>

  <!-- Finish reasons + Tab utilization -->
  <div class="grid grid-cols-1 lg:grid-cols-2 gap-4">
    <Card title="Finish reasons">
      {#if Object.keys(stats.finish_reasons).length === 0}
        <p class="muted text-[13px]">No data.</p>
      {:else}
        <ul class="list-none m-0 p-0 space-y-2">
          {#each Object.entries(stats.finish_reasons)
                   .sort((a, b) => b[1] - a[1]) as [reason, n] (reason)}
            {@const p = stats.total > 0 ? (n / stats.total) * 100 : 0}
            <li>
              <div class="flex justify-between text-[12.5px] mb-1">
                <span>{reason}</span>
                <span class="muted">{n} ({p.toFixed(1)}%)</span>
              </div>
              <div class="h-1.5 rounded-sm bg-panel3 overflow-hidden">
                <div class="h-full bg-accent" style="width: {p}%"></div>
              </div>
            </li>
          {/each}
        </ul>
      {/if}
    </Card>

    <Card title="Tab utilization">
      {#if Object.keys(stats.tab_utilization).length === 0}
        <p class="muted text-[13px]">No tab traffic yet.</p>
      {:else}
        <ul class="list-none m-0 p-0 space-y-2">
          {#each Object.entries(stats.tab_utilization)
                   .sort((a, b) => b[1] - a[1]) as [tab, n] (tab)}
            <li class="flex items-center gap-2 text-[12.5px]">
              <code class="text-muted shrink-0">{tab.slice(0, 10)}…</code>
              <div class="flex-1 h-1.5 rounded-sm bg-panel3 overflow-hidden">
                <div class="h-full bg-accent2" style="width: {pct(n, maxTabUse)}%"></div>
              </div>
              <span class="w-8 text-right muted">{n}</span>
            </li>
          {/each}
        </ul>
      {/if}
    </Card>
  </div>

  {#if stats.total === 0}
    <div class="card">
      <h3>Get started</h3>
      <p class="muted text-[13px] mb-3">
        No requests yet. Try the Playground, or point any OpenAI-compatible
        client at <code>http://127.0.0.1:8080/v1</code>.
      </p>
      <a href="#/playground" class="btn inline-block no-underline">Open Playground</a>
    </div>
  {/if}
{/if}
