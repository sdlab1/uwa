<script lang="ts">
  import { onMount } from 'svelte';
  import { getReady, getPool, getModels } from '../lib/api';

  let ready = $state<{ providers_loaded: boolean; models: number } | null>(null);
  let pool  = $state<{ total_tabs: number } | null>(null);
  let error = $state<string | null>(null);

  onMount(async () => {
    try {
      ready = await getReady();
      pool  = await getPool();
    } catch (e) {
      error = String(e);
    }
  });

  function finish() {
    localStorage.setItem('uwa:setup:done', '1');
    location.hash = '/';
  }
</script>

<h2>Setup</h2>
<p class="lead">Get uwa running end-to-end in two steps.</p>

<div class="card">
  <div class="card-head"><h3 class="m-0">1. Server status</h3></div>
  {#if error}
    <p class="err">Cannot reach the server: {error}</p>
  {:else if ready && pool}
    <dl class="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1 text-[13px]">
      <dt class="text-muted">providers</dt><dd>{ready.providers_loaded ? 'loaded' : 'none'}</dd>
      <dt class="text-muted">models</dt><dd>{ready.models}</dd>
      <dt class="text-muted">tabs</dt><dd>{pool.total_tabs}</dd>
    </dl>
    {#if !ready.providers_loaded}
      <p class="warn mt-3">No providers loaded. Add <code>[providers.*]</code> blocks to your uwa.toml.</p>
    {/if}
    {#if pool.total_tabs === 0}
      <p class="warn mt-3">Chromium has no tabs open. Open at least one tab.</p>
    {/if}
  {:else}
    <p class="muted">Checking…</p>
  {/if}
</div>

<div class="card">
  <div class="card-head"><h3 class="m-0">2. Confirmation</h3></div>
  <p class="muted text-[13px]">If everything looks good, jump to the Dashboard.</p>
  <button class="btn" onclick={finish}>Continue</button>
</div>
