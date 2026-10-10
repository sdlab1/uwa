<script lang="ts">
  import type { PoolStatus } from '../types';
  import { getPool } from '../api';

  let pool = $state<PoolStatus | null>(null);
  let error = $state<string | null>(null);
  let lastRefresh = $state<number | null>(null);

  /** Exposed via bind:this — parent polls it alongside stats. */
  export async function refresh() {
    try {
      pool = await getPool();
      error = null;
      lastRefresh = Date.now();
    } catch (e) {
      error = String(e);
    }
  }

  refresh();
</script>

<div class="card">
  <div class="card-head">
    <h3 class="m-0">Tab pool</h3>
    {#if pool}
      <span class="text-xs muted">
        {pool.total_tabs} tab{pool.total_tabs === 1 ? '' : 's'}
      </span>
    {/if}
  </div>
  {#if error}
    <p class="err text-[13px]">{error}</p>
    <p class="text-[11px] muted mt-1">
      Set the API key if configured, and confirm Chromium is running.
    </p>
  {:else if !pool}
    <p class="muted text-[13px]">Loading…</p>
  {:else if pool.tabs.length === 0}
    <p class="muted text-[13px]">
      No tabs — open Chromium and load a provider page
      (<code>chatgpt.com</code>, <code>claude.ai</code>, …).
    </p>
  {:else}
    <ul class="list-none m-0 p-0 space-y-1.5">
      {#each pool.tabs as t (t.id)}
        <li class="flex items-center gap-2 text-[12.5px] min-w-0">
          <span class="w-1.5 h-1.5 rounded-full bg-ok shrink-0"></span>
          <code class="text-muted shrink-0">{t.id.slice(0, 8)}…</code>
          <span class="truncate" title={t.url}>{t.url || '(no url)'}</span>
        </li>
      {/each}
    </ul>
  {/if}
</div>
