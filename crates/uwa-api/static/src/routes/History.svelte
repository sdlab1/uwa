<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import type { HistoryRecord, ProviderMap, ProfileSummary } from '../lib/types';
  import { getHistory, getHistoryRecord, getProviders, getProfiles } from '../lib/api';
  import { toast } from '../lib/stores';
  import { fmtMs, fmtNum, fmtRel, fmtTokens } from '../lib/format';
  import Modal from '../lib/components/Modal.svelte';
  import KV from '../lib/components/KV.svelte';
  import CodeBlock from '../lib/components/CodeBlock.svelte';

  // ---- filter state ----
  let providerFilter = $state('');
  let statusFilter   = $state('');
  let profileFilter  = $state('');
  let modelFilter    = $state('');
  let limit          = $state(100);

  // ---- data ----
  let records     = $state<HistoryRecord[]>([]);
  let bufferSize  = $state(0);
  let totalMatching = $state(0);
  let nextBeforeId  = $state<string | null>(null);
  let loading     = $state(false);
  let loadingMore = $state(false);
  let err         = $state<string | null>(null);
  let lastRefresh = $state<number | null>(null);

  // ---- detail ----
  let detailOpen  = $state(false);
  let detail      = $state<HistoryRecord | null>(null);
  let detailRaw   = $state<string | null>(null);
  let detailLoading = $state(false);

  // ---- referential data ----
  let providers = $state<ProviderMap>({});
  let profiles  = $state<ProfileSummary[]>([]);

  // Auto-refresh only while the list is a single page. Once the user
  // paginates, we stop — refreshing would drop the extra pages.
  let paginated = $derived(nextBeforeId !== null);

  let sentinelRef: HTMLDivElement | undefined;

  function when(rec: HistoryRecord): string {
    if (!rec.started_at) return '—';
    const ms = rec.started_at.secs_since_epoch * 1000
      + Math.floor(rec.started_at.nanos_since_epoch / 1e6);
    return new Date(ms).toLocaleTimeString();
  }
  function ageStr(rec: HistoryRecord): string {
    if (!rec.started_at) return '';
    const secs = rec.started_at.secs_since_epoch + rec.started_at.nanos_since_epoch / 1e9;
    return fmtRel(Math.max(0, Date.now() / 1000 - secs));
  }

  function currentQuery(before?: string) {
    return {
      limit,
      provider: providerFilter || undefined,
      status: statusFilter || undefined,
      profile: profileFilter || undefined,
      model: modelFilter || undefined,
      before_id: before,
    };
  }

  async function loadFirst() {
    loading = true;
    err = null;
    try {
      const r = await getHistory(currentQuery());
      records = r.records;
      bufferSize = r.buffer_size;
      totalMatching = r.total_matching;
      nextBeforeId = r.next_before_id;
      lastRefresh = Date.now();
    } catch (e) {
      err = String(e);
      records = [];
      nextBeforeId = null;
    } finally {
      loading = false;
    }
  }

  async function loadMore() {
    if (!nextBeforeId || loadingMore) return;
    loadingMore = true;
    try {
      const r = await getHistory(currentQuery(nextBeforeId));
      records = [...records, ...r.records];
      bufferSize = r.buffer_size;
      totalMatching = r.total_matching;
      nextBeforeId = r.next_before_id;
    } catch (e) {
      toast(`Load more: ${String(e)}`, 'err');
    } finally {
      loadingMore = false;
    }
  }

  // Infinite scroll: the window scroller, the sentinel sits at the end
  // of the list — when it approaches the viewport, load the next page.
  function onScroll() {
    if (!sentinelRef || loadingMore || !nextBeforeId) return;
    const rect = sentinelRef.getBoundingClientRect();
    if (rect.top - window.innerHeight < 300) void loadMore();
  }

  // Filters change → reset the list (debounced while typing).
  let resetTimer: number | undefined;
  $effect(() => {
    void providerFilter; void statusFilter; void profileFilter;
    void modelFilter; void limit;
    if (resetTimer) clearTimeout(resetTimer);
    resetTimer = window.setTimeout(() => void loadFirst(), 200);
    return () => { if (resetTimer) clearTimeout(resetTimer); };
  });

  // Periodic refresh, only when pagination is not active.
  let refreshIv: number | undefined;
  onMount(async () => {
    try { providers = await getProviders(); } catch { /* ignore */ }
    try {
      const p = await getProfiles();
      profiles = p.profiles;
    } catch { /* ignore */ }
    await loadFirst();
    refreshIv = window.setInterval(() => {
      if (document.visibilityState !== 'visible') return;
      if (paginated) return;
      void loadFirst();
    }, 5000);
  });
  onDestroy(() => { if (refreshIv) clearInterval(refreshIv); });

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

  function resetFilters() {
    providerFilter = '';
    statusFilter = '';
    profileFilter = '';
    modelFilter = '';
  }
</script>

<svelte:window onscroll={onScroll} />

<div class="flex items-center justify-between mb-1">
  <h2>History</h2>
  <span class="text-[11px] muted">
    {records.length} / {totalMatching} matching · buffer {bufferSize}
    {#if lastRefresh}· updated {new Date(lastRefresh).toLocaleTimeString()}{/if}
  </span>
</div>
<p class="lead">Every request uwa has served. Scroll to load older records.</p>

<div class="flex flex-wrap items-center gap-2 mb-3">
  <select class="input max-w-[170px]" bind:value={providerFilter}>
    <option value="">all providers</option>
    {#each Object.keys(providers) as p (p)}<option value={p}>{p}</option>{/each}
  </select>
  <select class="input max-w-[150px]" bind:value={statusFilter}>
    <option value="">any status</option>
    <option value="success">success</option>
    <option value="error">error</option>
    <option value="pending">pending</option>
  </select>
  <select class="input max-w-[150px]" bind:value={profileFilter}>
    <option value="">any profile</option>
    {#each profiles as p (p.name)}<option value={p.name}>{p.label || p.name}</option>{/each}
  </select>
  <input class="input max-w-[200px]" placeholder="model (exact)" bind:value={modelFilter} />
  <select class="input max-w-[110px]" bind:value={limit}>
    <option value={50}>50 / page</option>
    <option value={100}>100 / page</option>
    <option value={250}>250 / page</option>
  </select>
  <button class="btn-secondary" onclick={() => void loadFirst()}>Refresh</button>
  <button class="btn-secondary" onclick={resetFilters}>Reset</button>
</div>

{#if err}
  <div class="card border-err">
    <p class="err text-[13px] m-0">{err}</p>
  </div>
{:else if loading && records.length === 0}
  <div class="card"><p class="muted m-0">Loading…</p></div>
{:else if records.length === 0}
  <div class="card">
    <p class="muted m-0">No records match the filters.</p>
  </div>
{:else}
  <div class="card overflow-x-auto">
    <table>
      <thead>
        <tr>
          <th>When</th>
          <th>Provider</th>
          <th>Profile</th>
          <th>Model</th>
          <th>Status</th>
          <th class="text-right">Total</th>
          <th class="text-right">TTFT</th>
          <th class="text-right">Tokens</th>
          <th>Finish</th>
          <th class="text-right">Tools</th>
        </tr>
      </thead>
      <tbody>
        {#each records as r (r.id)}
          <tr class="cursor-pointer" onclick={() => void openDetail(r)}>
            <td>
              <div>{when(r)}</div>
              <div class="text-[10px] muted">{ageStr(r)} ago</div>
            </td>
            <td>{r.provider}</td>
            <td class="font-mono text-[11px] muted">{r.profile ?? 'default'}</td>
            <td class="text-[12px]">{r.model}</td>
            <td class:ok={r.status === 'success'} class:err={r.status === 'error'} class:warn={r.status === 'pending'}>
              {r.status}
            </td>
            <td class="text-right">{fmtMs(r.timing.total_ms)}</td>
            <td class="text-right">{fmtMs(r.timing.ttft_ms)}</td>
            <td class="text-right font-mono text-[11px]">
              {#if r.response?.usage}
                {fmtTokens(r.response.usage.prompt_tokens)} / {fmtTokens(r.response.usage.completion_tokens)}
              {:else}—{/if}
            </td>
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

  <div class="flex items-center justify-center py-4" bind:this={sentinelRef}>
    {#if loadingMore}
      <span class="muted text-[12px]">Loading more…</span>
    {:else if nextBeforeId}
      <button class="btn-secondary" onclick={() => void loadMore()}>Load more</button>
    {:else}
      <span class="muted text-[12px]">End of history ({records.length} record{records.length === 1 ? '' : 's'}).</span>
    {/if}
  </div>
{/if}

<Modal bind:open={detailOpen} title={`Record ${detail?.id ?? ''}`} size="lg">
  {#if detail}
    <KV rows={[
      ['id', detail.id],
      ['provider', detail.provider],
      ['profile', detail.profile ?? 'default'],
      ['model', detail.model],
      ['status', detail.status],
      ['error', detail.error],
      ['finish', detail.response?.finish_reason],
      ['acquisition', fmtMs(detail.timing.acquisition_ms)],
      ['send', fmtMs(detail.timing.send_ms)],
      ['wait', fmtMs(detail.timing.wait_ms)],
      ['total', fmtMs(detail.timing.total_ms)],
      ['ttft', fmtMs(detail.timing.ttft_ms)],
      ['tokens in', fmtTokens(detail.response?.usage?.prompt_tokens)],
      ['tokens out', fmtTokens(detail.response?.usage?.completion_tokens)],
      ['tokens total', fmtTokens(detail.response?.usage?.total_tokens)],
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
