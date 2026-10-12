<script lang="ts">
  import { onMount } from 'svelte';
  import type { HistoryRecord, ProviderMap, StatsResp } from '../lib/types';
  import { getHistory, getPool, getProviders, getStats } from '../lib/api';
  import { stream, toast } from '../lib/stores';
  import { fmtMs, fmtNum, fmtPercent } from '../lib/format';
  import { downloadJson } from '../lib/download';

  import Card from '../lib/components/Card.svelte';
  import StatCard from '../lib/components/StatCard.svelte';
  import StageWaterfall from '../lib/components/StageWaterfall.svelte';
  import LiveDot from '../lib/components/LiveDot.svelte';

  let stats = $state<StatsResp | null>(null);
  let history = $state<HistoryRecord[]>([]);
  let providers = $state<ProviderMap>({});
  let err = $state<string | null>(null);
  let loading = $state(true);
  let updatedAt = $state<number | null>(null);
  let bundleBusy = $state(false);

  async function load(silent = false) {
    try {
      const [s, h, p] = await Promise.all([
        getStats('last200'),
        getHistory(200),
        getProviders().catch(() => ({})),
      ]);
      stats = s;
      history = h.records;
      providers = p;
      err = null;
      updatedAt = Date.now();
    } catch (e) {
      if (!silent) err = String(e);
    } finally {
      loading = false;
    }
  }

  // ---- Derived: average per stage (client-side, from history) ----
  const avgStages = $derived.by(() => {
    let acq = 0, send = 0, wait = 0, total = 0, n = 0;
    for (const r of history) {
      const t = r.timing;
      if (!t || (t.total_ms ?? 0) <= 0) continue;
      acq += t.acquisition_ms ?? 0;
      send += t.send_ms ?? 0;
      wait += t.wait_ms ?? 0;
      total += t.total_ms;
      n++;
    }
    if (!n) return { acq: 0, send: 0, wait: 0, total: 0, n: 0 };
    return { acq: acq / n, send: send / n, wait: wait / n, total: total / n, n };
  });

  const bottleneck = $derived.by(() => {
    const { acq, send, wait, n } = avgStages;
    if (n === 0) return { label: 'No data', hint: 'Fire a few requests to see where time goes.' };
    const max = Math.max(acq, send, wait);
    if (max === acq)  return { label: 'Session acquisition', hint: 'Tab pool is a bottleneck — check /sessions for stuck leases.' };
    if (max === send) return { label: 'Typing & sending',    hint: 'Injection or send-button wait dominates — inspect selector health.' };
    return                  { label: 'Waiting for response', hint: 'Extraction is the long pole — check provider finisher tuning.' };
  });

  const slow = $derived(
    [...history]
      .filter((r) => (r.timing?.total_ms ?? 0) > 0)
      .sort((a, b) => (b.timing.total_ms ?? 0) - (a.timing.total_ms ?? 0))
      .slice(0, 20),
  );

  // ---- Live stream ----
  const sPhase = $derived($stream.phase);
  const sTtft  = $derived($stream.ttftMs);
  const sTotal = $derived($stream.totalMs);
  const sChars = $derived($stream.chars);

  const sPhaseTone = $derived(
    sPhase === 'error' ? 'err'
    : sPhase === 'done' ? 'ok'
    : sPhase === 'idle' ? 'muted'
    : 'accent',
  );

  // ---- Debug bundle ----
  async function downloadBundle() {
    bundleBusy = true;
    try {
      const [s, h, p, pool] = await Promise.all([
        getStats('last200').catch(() => null),
        getHistory(500).catch(() => null),
        getProviders().catch(() => null),
        getPool().catch(() => null),
      ]);
      const bundle = {
        generated_at: new Date().toISOString(),
        user_agent: navigator.userAgent,
        stats: s,
        history: h,
        providers: p,
        pool,
      };
      const stamp = new Date().toISOString().replace(/[:.]/g, '-');
      downloadJson(`uwa-debug-${stamp}.json`, bundle);
      toast('Debug bundle downloaded', 'ok');
    } catch (e) {
      toast(`Bundle failed: ${String(e)}`, 'err');
    } finally {
      bundleBusy = false;
    }
  }

  onMount(() => {
    void load();
    const iv = setInterval(() => {
      if (document.visibilityState === 'visible') void load(true);
    }, 5000);
    const onVis = () => { if (document.visibilityState === 'visible') void load(true); };
    document.addEventListener('visibilitychange', onVis);
    return () => {
      clearInterval(iv);
      document.removeEventListener('visibilitychange', onVis);
    };
  });
</script>

<div class="flex items-center justify-between mb-1">
  <h2>Debug</h2>
  <div class="flex items-center gap-3">
    <LiveDot on={!err} label={err ? 'offline' : 'live'} />
    {#if updatedAt}
      <span class="text-[11px] muted">updated {new Date(updatedAt).toLocaleTimeString()}</span>
    {/if}
    <button class="btn-secondary" disabled={bundleBusy} onclick={downloadBundle}>
      {bundleBusy ? 'Preparing…' : 'Download bundle'}
    </button>
  </div>
</div>
<p class="lead">
  Detailed timings, bottleneck analysis and per-provider deep-dive.
  Use the bundle for offline investigation.
</p>

{#if err}
  <div class="card border-err">
    <div class="card-head">
      <h3 class="m-0 text-err">Cannot load debug data</h3>
      <button class="btn-secondary" onclick={() => void load()}>Retry</button>
    </div>
    <p class="text-[13px] muted m-0">{err}</p>
  </div>
{:else if loading}
  <div class="card"><p class="muted m-0">Loading…</p></div>
{:else if !stats}
  <div class="card"><p class="muted m-0">No data.</p></div>
{:else}
  <!-- ============ Bottleneck summary ============ -->
  <Card title="Bottleneck summary" subtitle={`avg across last ${avgStages.n} requests`}>
    {#if avgStages.n === 0}
      <p class="muted text-[13px] m-0">
        No timed requests yet — fire a few from Playground.
      </p>
    {:else}
      <div class="grid grid-cols-2 sm:grid-cols-4 gap-2.5 mb-4">
        <StatCard label="Acquisition" value={fmtMs(avgStages.acq)} />
        <StatCard label="Send"        value={fmtMs(avgStages.send)} />
        <StatCard label="Wait"        value={fmtMs(avgStages.wait)} />
        <StatCard label="Total (avg)" value={fmtMs(avgStages.total)} highlight />
      </div>

      <div class="mb-3">
        <div class="text-[11px] muted uppercase tracking-[0.5px] mb-2">Stage breakdown</div>
        <StageWaterfall
          acquisition={avgStages.acq}
          send={avgStages.send}
          wait={avgStages.wait}
          height={22}
          showLegend
        />
      </div>

      <div class="rounded-sm border border-border bg-panel2 px-3 py-2.5">
        <div class="text-[11px] muted uppercase tracking-[0.5px]">Biggest contributor</div>
        <div class="text-[14px] font-semibold mt-0.5">{bottleneck.label}</div>
        <div class="text-[12px] muted mt-1">{bottleneck.hint}</div>
      </div>
    {/if}
  </Card>

  <!-- ============ Live stream ============ -->
  <Card title="Live stream"
        subtitle={sPhase === 'idle' ? 'no active stream' : `phase: ${sPhase}`}>
    <div class="flex items-center gap-3 flex-wrap">
      <span class="stream-badge badge-{sPhaseTone}">
        {#if sPhase === 'connecting' || sPhase === 'waiting' || sPhase === 'streaming' || sPhase === 'first-token'}
          <span class="dot"></span>
        {/if}
        <span class="badge-label">{sPhase}</span>
        {#if sTtft != null}<span class="badge-metric">TTFT {Math.round(sTtft)} ms</span>{/if}
        {#if sTotal != null && sPhase === 'done'}
          <span class="badge-metric">total {Math.round(sTotal)} ms</span>
        {/if}
        {#if sChars > 0}<span class="badge-metric">{sChars} chars</span>{/if}
      </span>
      {#if sPhase === 'idle'}
        <span class="text-[12px] muted">
          Trigger a stream from Playground — metrics will appear here in real time.
        </span>
      {/if}
    </div>
  </Card>

  <!-- ============ Slow requests ============ -->
  <Card title="Slowest requests" subtitle={`top ${slow.length} of ${history.length}`}>
    {#if slow.length === 0}
      <p class="muted text-[13px] m-0">No timed requests.</p>
    {:else}
      <div class="overflow-x-auto">
        <table>
          <thead>
            <tr>
              <th>ID</th>
              <th>Provider</th>
              <th>Status</th>
              <th class="text-right">Total</th>
              <th class="w-[240px]">Waterfall</th>
              <th class="text-right">TTFT</th>
              <th>Pin</th>
            </tr>
          </thead>
          <tbody>
            {#each slow as r (r.id)}
              <tr>
                <td class="font-mono text-[11px]">
                  <a href="#/history" class="no-underline" title={r.id}>
                    {r.id.slice(0, 12)}…
                  </a>
                </td>
                <td>{r.provider}</td>
                <td
                  class:ok={r.status === 'success'}
                  class:err={r.status === 'error'}
                  class:warn={r.status === 'pending'}>{r.status}</td>
                <td class="text-right font-mono">{fmtMs(r.timing.total_ms)}</td>
                <td>
                  <StageWaterfall
                    acquisition={r.timing.acquisition_ms}
                    send={r.timing.send_ms}
                    wait={r.timing.wait_ms}
                    height={10}
                  />
                </td>
                <td class="text-right font-mono">{fmtMs(r.timing.ttft_ms)}</td>
                <td>
                  {#if r.response?.pin_mode === 'delta'}
                    <span class="pill" style="border-color: var(--ok); color: var(--ok);">delta</span>
                  {:else if r.response?.pin_mode === 'full'}
                    <span class="pill muted">full</span>
                  {:else}
                    <span class="muted">—</span>
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
      <p class="text-[11px] muted mt-3 mb-0">
        Waterfall colors: <span class="wf-dot wf-acq"></span> acquisition ·
        <span class="wf-dot wf-send"></span> send ·
        <span class="wf-dot wf-wait"></span> wait.
        <span class="muted">Full record is on the History page.</span>
      </p>
    {/if}
  </Card>

  <!-- ============ Provider deep-dive ============ -->
  <Card title="Per-provider deep-dive">
    {#if stats.providers.length === 0}
      <p class="muted text-[13px] m-0">No provider traffic.</p>
    {:else}
      <div class="overflow-x-auto">
        <table>
          <thead>
            <tr>
              <th>Provider</th>
              <th class="text-right">Total</th>
              <th class="text-right">Err%</th>
              <th class="text-right">Avg</th>
              <th class="text-right">p50</th>
              <th class="text-right">p95</th>
              <th class="text-right">Avg TTFT</th>
              <th class="text-right">Send / Wait</th>
              <th>Source</th>
              <th class="text-right">Tabs</th>
            </tr>
          </thead>
          <tbody>
            {#each stats.providers as p (p.provider)}
              <tr>
                <td class="font-medium">{p.provider}</td>
                <td class="text-right">{fmtNum(p.total)}</td>
                <td class="text-right"
                    class:err={p.error > 0 && p.error_rate >= 0.1}
                    class:warn={p.error > 0 && p.error_rate < 0.1}
                    class:ok={p.error === 0}>{fmtPercent(p.error_rate)}</td>
                <td class="text-right font-mono">{fmtMs(p.avg_total_ms)}</td>
                <td class="text-right font-mono">{fmtMs(p.p50_total_ms)}</td>
                <td class="text-right font-mono">{fmtMs(p.p95_total_ms)}</td>
                <td class="text-right font-mono">{fmtMs(p.avg_ttft_ms)}</td>
                <td class="text-right font-mono text-[12px]">
                  {fmtMs(p.avg_send_ms)} / {fmtMs(p.avg_wait_ms)}
                </td>
                <td class="text-[11px] muted">
                  {#if p.network_count != null || p.dom_count != null}
                    {p.network_count ?? 0} net / {p.dom_count ?? 0} dom
                  {:else}—{/if}
                </td>
                <td class="text-right">{p.tabs_used ?? '—'}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </Card>

  <!-- ============ Provider config snapshot ============ -->
  <Card title="Provider config snapshot">
    {#if Object.keys(providers).length === 0}
      <p class="muted text-[13px] m-0">No providers loaded.</p>
    {:else}
      <div class="grid grid-cols-1 md:grid-cols-2 gap-3">
        {#each Object.entries(providers) as [name, info] (name)}
          <div class="p-3 rounded-sm border border-border bg-panel2">
            <div class="flex items-center justify-between mb-2">
              <span class="font-medium">{name}</span>
              <span class="text-[11px] muted">{info.extraction}</span>
            </div>
            <dl class="grid grid-cols-[max-content_1fr] gap-x-3 gap-y-1 text-[12px] m-0">
              <dt class="muted">streams</dt>     <dd class="m-0 font-mono">{info.capabilities.streams ? 'yes' : 'no'}</dd>
              <dt class="muted">tool_calls</dt>  <dd class="m-0 font-mono">{info.capabilities.tool_calls ? 'yes' : 'no'}</dd>
              <dt class="muted">vision</dt>      <dd class="m-0 font-mono">{info.capabilities.vision ? 'yes' : 'no'}</dd>
              <dt class="muted">max ctx</dt>     <dd class="m-0 font-mono">{info.capabilities.max_context_tokens ?? '—'}</dd>
              <dt class="muted">backend</dt>     <dd class="m-0 font-mono">{info.backend ?? 'default'}</dd>
            </dl>
          </div>
        {/each}
      </div>
    {/if}
  </Card>

  <!-- ============ Raw events ============ -->
  <Card title="Raw events" subtitle="live tail">
    <p class="text-[13px] muted m-0">
      For raw tracing output, network frames and provider events, use the
      <a href="#/logs" class="no-underline">Logs</a> page — filter by level or target
      to isolate a specific subsystem. The full traffic record is on
      <a href="#/history" class="no-underline">History</a>.
    </p>
  </Card>
{/if}
