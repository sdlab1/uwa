<script lang="ts">
  import { onMount } from 'svelte';
  import type { HistoryRecord, HistoryResp, ProviderMap } from '../lib/types';
  import { getHistory, getHistoryRecord, getProviders } from '../lib/api';
  import { toast } from '../lib/stores';
  import { fmtMs, fmtNum, fmtRel } from '../lib/format';
  import Modal from '../lib/components/Modal.svelte';
  import KV from '../lib/components/KV.svelte';
  import CodeBlock from '../lib/components/CodeBlock.svelte';

  let records = $state<HistoryRecord[]>([]);
  let providers = $state<ProviderMap>({});
  let bufferSize = $state(0);
  let loading = $state(true);
  let err = $state<string | null>(null);
  let lastRefresh = $state<number | null>(null);

  let providerFilter = $state('');
  let statusFilter = $state('');
  let limit = $state(100);

  let detailOpen = $state(false);
  let detail = $state<HistoryRecord | null>(null);
  let detailRaw = $state<string | null>(null);
  let detailLoading = $state(false);

  // -- helpers --
  function when(rec: HistoryRecord): string {
    if (!rec.started_at) return '—';
    const secs = rec.started_at.secs_since_epoch * 1000
      + Math.floor(rec.started_at.nanos_since_epoch / 1e6);
    return new Date(secs).toLocaleTimeString();
  }
  function ageStr(rec: HistoryRecord): string {
    if (!rec.started_at) return '';
    const secs = rec.started_at.secs_since_epoch
      + rec.started_at.nanos_since_epoch / 1e9;
    return fmtRel(Math.max(0, Date.now() / 1000 - secs));
  }

  async function load() {
    try {
      const r = await getHistory(limit, providerFilter || undefined, statusFilter || undefined);
      records = r.records;
      bufferSize = r.buffer_size;
      err = null;
      lastRefresh = Date.now();
    } catch (e) {
      err = String(e);
    } finally {
      loading = false;
    }
  }

  async function openDetail(rec: HistoryRecord) {
    detail = rec;
    detailOpen = true;
    detailRaw = null;
    detailLoading = true;
    try {
      const raw = await getHistoryRecord(rec.id);
      detailRaw = JSON.stringify(raw, null, 2);
    } catch (e) {
      detailRaw = `error: ${String(e)}`;
    } finally {
      detailLoading = false;
    }
  }

  onMount(() => {
    void (async () => {
      try { providers = await getProviders(); } catch { /* ignore */ }
      await load();
    })();
    const iv = setInterval(() => {
      if (document.visibilityState === 'visible') void load();
    }, 5000);
    const onVis = () => {
      if (document.visibilityState === 'visible') void load();
    };
    document.addEventListener('visibilitychange', onVis);
    return () => {
      clearInterval(iv);
      document.removeEventListener('visibilitychange', onVis);
    };
  });
</script>

<div class="flex items-center justify-between mb-1">
  <h2>History</h2>
  <span class="text-[11px] muted">
    {records.length} / {bufferSize} in buffer
    {#if lastRefresh}· updated {new Date(lastRefresh).toLocaleTimeString()}{/if}
  </span>
</div>
<p class="lead">Every request uwa has served. Click a row for full details.</p>

<div class="flex flex-wrap items-center gap-2 mb-3">
  <select class="input max-w-[180px]" bind:value={providerFilter} onchange={load}>
    <option value="">all providers</option>
    {#each Object.keys(providers) as p (p)}<option value={p}>{p}</option>{/each}
  </select>
  <select class="input max-w-[160px]" bind:value={statusFilter} onchange={load}>
    <option value="">any status</option>
    <option value="success">success</option>
    <option value="error">error</option>
    <option value="pending">pending</option>
  </select>
  <select class="input max-w-[120px]" bind:value={limit} onchange={load}>
    <option value={50}>50</option>
    <option value={100}>100</option>
    <option value={500}>500</option>
  </select>
  <button class="btn-secondary" onclick={load}>Refresh</button>
</div>

{#if err}
  <div class="card border-err">
    <p class="err text-[13px] m-0">{err}</p>
  </div>
{:else if loading}
  <div class="card"><p class="muted m-0">Loading…</p></div>
{:else if records.length === 0}
  <div class="card">
    <p class="muted m-0">No records yet. Fire a request from Playground or any client.</p>
  </div>
{:else}
  <div class="card overflow-x-auto">
    <table>
      <thead>
        <tr>
          <th>When</th>
          <th>Provider</th>
          <th>Model</th>
          <th>Status</th>
          <th class="text-right">Total</th>
          <th class="text-right">TTFT</th>
          <th>Finish</th>
          <th class="text-right">Tools</th>
        </tr>
      </thead>
      <tbody>
        {#each records as r (r.id)}
          <tr class="cursor-pointer" onclick={() => openDetail(r)}>
            <td>
              <div>{when(r)}</div>
              <div class="text-[10px] muted">{ageStr(r)} ago</div>
            </td>
            <td>{r.provider}</td>
            <td class="text-[12px]">{r.model}</td>
            <td
              class:ok={r.status === 'success'}
              class:err={r.status === 'error'}
              class:warn={r.status === 'pending'}>{r.status}</td>
            <td class="text-right">{fmtMs(r.timing.total_ms)}</td>
            <td class="text-right">{fmtMs(r.timing.ttft_ms)}</td>
            <td class="text-[12px]">{r.response?.finish_reason ?? '—'}</td>
            <td class="text-right">
              {#if r.response && r.response.tool_calls > 0}
                <span class="pill pill-warn">×{r.response.tool_calls}</span>
              {:else}—{/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}

<Modal bind:open={detailOpen} title={`Record ${detail?.id ?? ''}`} size="lg">
  {#if detail}
    <KV rows={[
      ['id', detail.id],
      ['provider', detail.provider],
      ['model', detail.model],
      ['status', detail.status],
      ['error', detail.error],
      ['finish', detail.response?.finish_reason],
      ['acquisition', fmtMs(detail.timing.acquisition_ms)],
      ['send', fmtMs(detail.timing.send_ms)],
      ['wait', fmtMs(detail.timing.wait_ms)],
      ['total', fmtMs(detail.timing.total_ms)],
      ['ttft', fmtMs(detail.timing.ttft_ms)],
      ['user preview', detail.request.user_preview],
      ['text preview', detail.response?.text_preview],
    ]} />
    <div class="mt-4">
      <div class="text-[11px] muted uppercase tracking-[0.5px] mb-1">Raw record</div>
      {#if detailLoading}
        <p class="muted text-[12px]">Loading…</p>
      {:else if detailRaw}
        <CodeBlock code={detailRaw} lang="json" />
      {/if}
    </div>
  {/if}
</Modal>
