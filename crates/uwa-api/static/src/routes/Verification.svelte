<script lang="ts">
  import { onMount } from 'svelte';
  import type { VerificationResp } from '../lib/types';
  import { getVerification } from '../lib/api';

  let report = $state<VerificationResp | null>(null);
  let err = $state<string | null>(null);
  let loading = $state(true);
  let lastRefresh = $state<number | null>(null);

  async function load() {
    try {
      report = await getVerification();
      err = null;
      lastRefresh = Date.now();
    } catch (e) {
      err = String(e);
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    void load();
    const iv = setInterval(() => {
      if (document.visibilityState === 'visible') void load();
    }, 2500);
    return () => clearInterval(iv);
  });
</script>

<div class="flex items-center justify-between mb-1">
  <h2>Browser verification</h2>
  <span class="text-[11px] muted">
    {#if report}{report.challenges.length} challenge(s){/if}
    {#if lastRefresh} · updated {new Date(lastRefresh).toLocaleTimeString()}{/if}
  </span>
</div>
<p class="lead">
  Detects visible Cloudflare challenge pages in connected Chromium tabs. Response-header-only challenges are reported in the failed API request's structured error (`human_verification_required`), because a page scan cannot reconstruct every past network response. UWA does not solve or bypass challenges.
</p>

<div class="card mb-3">
  <h3 class="m-0 mb-2">Manual recovery</h3>
  <ol class="list-decimal pl-5 text-[13px] leading-6">
    <li>Switch to the matching tab in the same Chromium profile UWA is connected to.</li>
    <li>Complete the challenge yourself, if Cloudflare presents one, and wait for the normal provider page to return.</li>
    <li>Confirm this page reports no active challenge, then retry the API request manually once.</li>
  </ol>
  <p class="muted text-[12px] mt-2 mb-0">
    UWA does not automatically replay a failed prompt. If the provider may already have accepted the prompt, check the browser conversation before retrying to avoid duplicates.
  </p>
</div>

<div class="flex items-center gap-2 mb-3">
  <button class="btn-secondary" onclick={load}>Refresh now</button>
  {#if report}
    <span class="text-[12px] muted">Scanned {report.scanned_tabs}/{report.total_tabs} tabs</span>
  {/if}
</div>

{#if err}
  <div class="card border-err"><p class="err text-[13px] m-0">{err}</p></div>
{:else if loading}
  <div class="card"><p class="muted m-0">Scanning browser tabs…</p></div>
{:else if !report || report.challenges.length === 0}
  <div class="card">
    <h3 class="m-0 mb-1">No challenge detected</h3>
    <p class="muted text-[13px] m-0">No Cloudflare-specific URL or DOM markers were found in the tabs that could be scanned.</p>
    {#if report && report.scan_errors.length > 0}
      <p class="text-[12px] mt-2 mb-0">Some tabs could not be inspected; this result is not conclusive.</p>
    {/if}
  </div>
{:else}
  <div class="space-y-3">
    {#each report.challenges as c (c.tab_id)}
      <div class="card border-warn">
        <div class="flex items-start justify-between gap-3">
          <div>
            <h3 class="m-0">Cloudflare challenge detected</h3>
            <p class="muted text-[12px] mt-1 mb-0">Provider: {c.provider ?? 'unknown'}</p>
          </div>
          <span class="text-[11px] rounded border border-warn px-2 py-1">Manual action required</span>
        </div>
        <dl class="grid grid-cols-[90px_minmax(0,1fr)] gap-x-3 gap-y-2 text-[12px] mt-3">
          <dt class="muted">Tab ID</dt><dd class="font-mono break-all m-0">{c.tab_id}</dd>
          <dt class="muted">Current URL</dt><dd class="font-mono break-all m-0">{c.url}</dd>
          <dt class="muted">Signals</dt><dd class="m-0">{c.signals.join(', ')}</dd>
        </dl>
      </div>
    {/each}
  </div>
{/if}

{#if report && report.scan_errors.length > 0}
  <div class="card mt-3">
    <h3 class="m-0 mb-2">Tabs not inspected</h3>
    {#each report.scan_errors as item (item.tab_id)}
      <p class="text-[12px] m-0 mb-1"><span class="font-mono">{item.tab_id}</span>: {item.error}</p>
    {/each}
  </div>
{/if}
