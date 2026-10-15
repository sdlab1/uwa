<script lang="ts">
  import { onMount } from 'svelte';
  import { CLIENTS } from '../lib/clients';
  import { getModels } from '../lib/api';
  import { toast } from '../lib/stores';

  interface ClientRow {
    id: string;
    name: string;
    format: string;
    path: string | null;
    state: string;
    note: string;
    last_backup: string | null;
  }

  let base = $state('');
  let model = $state('');
  let key = $state('');
  let loading = $state(true);
  let err = $state<string | null>(null);
  let installable = $state<ClientRow[]>([]);
  let busy = $state<string | null>(null);

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
    void refreshInstall();
  }

  async function refreshInstall() {
    try {
      const apiKey = localStorage.getItem('uwa:apiKey') ?? '';
      const r = await fetch('/admin/clients', {
        headers: { authorization: `Bearer ${apiKey}` },
      });
      if (!r.ok) throw new Error(`${r.status}`);
      const v = (await r.json()) as { clients: ClientRow[] } | ClientRow[];
      installable = Array.isArray(v) ? v : (v as any).clients ?? [];
    } catch {
      installable = [];
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

  async function doInstall(id: string) {
    busy = id;
    try {
      const apiKey = localStorage.getItem('uwa:apiKey') ?? '';
      const r = await fetch('/admin/clients/install', {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          authorization: `Bearer ${apiKey}`,
        },
        body: JSON.stringify({ id }),
      });
      const v = await r.json();
      toast(v.ok ? `Installed ${id}` : `Install failed: ${v.stderr || v.stdout}`, v.ok ? 'ok' : 'err');
      await refreshInstall();
    } finally {
      busy = null;
    }
  }

  async function doUninstall(id: string) {
    if (!confirm(`Undo uwa's patch for ${id}?`)) return;
    busy = id;
    try {
      const apiKey = localStorage.getItem('uwa:apiKey') ?? '';
      const r = await fetch('/admin/clients/uninstall', {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          authorization: `Bearer ${apiKey}`,
        },
        body: JSON.stringify({ id }),
      });
      const v = await r.json();
      toast(v.ok ? `Uninstalled ${id}` : `Uninstall failed: ${v.stderr || v.stdout}`, v.ok ? 'ok' : 'err');
      await refreshInstall();
    } finally {
      busy = null;
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

<div class="card">
  <div class="card-head">
    <h3 class="m-0">Auto-install</h3>
    <button class="btn-secondary" onclick={refreshInstall}>Refresh</button>
  </div>
  <p class="text-[12.5px] muted mt-0 mb-3">
    One-click setup writes to the client's own config file. Every write
    is backed up to <code>~/.uwa/client-backups/</code> first; uninstall
    strips the uwa block and leaves your edits in place.
  </p>
  {#if installable.length === 0}
    <p class="muted text-[12px] m-0">Not reachable. Ensure the daemon is running.</p>
  {:else}
    <table>
      <thead>
        <tr><th>Client</th><th>State</th><th>Path</th><th></th></tr>
      </thead>
      <tbody>
        {#each installable as c (c.id)}
          <tr>
            <td>{c.name}</td>
            <td class:ok={c.state === 'patched'} class:warn={c.state === 'not_patched' || c.state === 'missing_file'} class:muted={c.state === 'env_only'}>
              {c.state}
            </td>
            <td class="font-mono text-[11px] truncate" title={c.path ?? ''}>{c.path ?? '—'}</td>
            <td class="text-right whitespace-nowrap">
              {#if c.state === 'patched'}
                <button class="btn-secondary" disabled={busy === c.id} onclick={() => doUninstall(c.id)}>
                  {busy === c.id ? '…' : 'Uninstall'}
                </button>
              {:else if c.format !== 'shell' && c.path}
                <button class="btn-secondary" disabled={busy === c.id} onclick={() => doInstall(c.id)}>
                  {busy === c.id ? '…' : 'Install'}
                </button>
              {:else}
                <span class="text-[11px] muted">source <code>~/.uwa/clients/env.sh</code></span>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</div>

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