<script lang="ts">
  import { onMount } from 'svelte';
  import { CLIENTS } from '../lib/clients';
  import { getModels } from '../lib/api';
  import { toast } from '../lib/stores';

  let base = $state('');
  let model = $state('');
  let key = $state('');
  let loading = $state(true);
  let err = $state<string | null>(null);

  async function load() {
    base = location.origin;
    key = localStorage.getItem('uwa:apiKey') ?? 'dummy-key';
    try {
      const list = await getModels();
      model = list.data[0]?.id ?? 'gpt-4o';
      err = null;
    } catch (e) {
      model = 'gpt-4o';
      err = String(e);
    } finally {
      loading = false;
    }
  }

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      toast('Copied', 'ok');
    } catch {
      toast('Copy failed — select and copy manually', 'err');
    }
  }

  onMount(load);
</script>

<h2>Clients</h2>
<p class="lead">
  Point any OpenAI-compatible client at this uwa instance. Values are filled
  in from your live config — copy and paste, no manual substitution.
</p>

{#if err}
  <div class="card border-warn">
    <p class="text-[13px] m-0">
      Could not load the model list ({err}); showing a placeholder. Set the
      API key in the top-right if the server requires one.
    </p>
  </div>
{:else if loading}
  <div class="card"><p class="muted m-0">Loading…</p></div>
{/if}

<div class="grid gap-3">
  {#each CLIENTS as c (c.id)}
    {@const snippet = c.render({ base, model, key })}
    <div class="card">
      <div class="card-head">
        <div>
          <h3 class="m-0 text-[15px]">{c.name}</h3>
          <div class="text-[11px] muted mt-0.5">{c.where}</div>
        </div>
        <button class="btn-secondary" onclick={() => copy(snippet)}>Copy</button>
      </div>
      <pre class="clients-snippet"><code>{snippet}</code></pre>
      {#if c.docs}
        <a class="text-[11px] muted no-underline" href={c.docs} target="_blank" rel="noopener noreferrer">
          docs ↗
        </a>
      {/if}
    </div>
  {/each}
</div>

<div class="card">
  <h3>Values used</h3>
  <dl class="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1 text-[13px] m-0">
    <dt class="muted">Base URL</dt><dd class="m-0 font-mono">{base}/v1</dd>
    <dt class="muted">API key</dt><dd class="m-0 font-mono">{key}</dd>
    <dt class="muted">Model</dt><dd class="m-0 font-mono">{model}</dd>
  </dl>
  <p class="text-[11px] muted mt-2 m-0">
    On loopback, uwa accepts any key — replace the value above with a
    placeholder if you'd rather not store the real one. Keys are only
    enforced when <code>[server].bind</code> is not <code>127.0.0.1</code>.
  </p>
</div>
