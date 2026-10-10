<script lang="ts">
  import { onMount } from 'svelte';
  import { apiKey, serverStatus, toast } from '../stores';
  import { getApiKey, setApiKey, onKeyChange, getHealth } from '../api';

  let keyOpen = $state(false);
  let keyInput = $state('');

  function openDialog() { keyInput = getApiKey(); keyOpen = true; }
  function saveKey() { setApiKey(keyInput.trim()); keyOpen = false; toast('API key saved', 'ok'); }
  function clearKey() { setApiKey(''); keyOpen = false; toast('API key cleared', 'warn'); }

  async function ping() {
    try {
      const r = await getHealth();
      serverStatus.set(r.status === 'ok' ? 'ok' : 'degraded');
    } catch {
      serverStatus.set('down');
    }
  }

  onMount(() => {
    const unsub = onKeyChange((v) => apiKey.set(v));
    void ping();
    const iv = window.setInterval(ping, 5000);
    return () => { unsub(); window.clearInterval(iv); };
  });
</script>

<header class="sticky top-0 z-10 flex items-center justify-between px-5 py-3 border-b border-border bg-panel">
  <div class="flex items-baseline gap-2.5">
    <span class="text-lg font-bold bg-gradient-to-br from-accent to-accent2 bg-clip-text text-transparent tracking-wide">uwa</span>
    <span class="text-xs text-muted">Universal Web API</span>
  </div>
  <div class="flex items-center gap-2">
    <span class="pill" class:pill-ok={$apiKey} class:pill-warn={!$apiKey}>key: {$apiKey ? 'set' : 'none'}</span>
    <span class="pill"
          class:pill-ok={$serverStatus === 'ok'}
          class:pill-warn={$serverStatus === 'unknown' || $serverStatus === 'degraded'}
          class:pill-err={$serverStatus === 'down'}>server: {$serverStatus}</span>
    <button class="ghost-btn" onclick={openDialog}>Set key</button>
  </div>
</header>

{#if keyOpen}
  <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/60"
       role="presentation" onclick={() => (keyOpen = false)}>
    <div class="bg-panel border border-border rounded p-5 max-w-[440px] w-[90vw]"
         role="dialog" onclick={(e) => e.stopPropagation()}>
      <h3 class="text-base font-semibold mb-3">Set API key</h3>
      <p class="text-muted text-xs mb-3">
        Stored in this browser. Same value as <code>[server].api_key</code> in uwa.toml.
      </p>
      <input type="password" bind:value={keyInput} placeholder="API key" class="input mb-4" />
      <div class="flex justify-end gap-2">
        <button class="btn-secondary" onclick={clearKey}>Clear</button>
        <button class="btn" onclick={saveKey}>Save</button>
      </div>
    </div>
  </div>
{/if}
