<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import type { ProviderMap, RecorderStatus, RecorderStopResponse } from '../lib/types';
  import { getProviders, recorderStatus, recorderStart, recorderStop } from '../lib/api';
  import { toast } from '../lib/stores';
  import { fmtRel } from '../lib/format';
  import CodeBlock from '../lib/components/CodeBlock.svelte';

  let providers = $state<ProviderMap>({});
  let provider = $state('');
  let status = $state<RecorderStatus>({ active: false });
  let result = $state<RecorderStopResponse | null>(null);
  let busy = $state(false);
  let err = $state<string | null>(null);

  let timer: number | undefined;

  async function refreshStatus() {
    try {
      status = await recorderStatus();
      err = null;
    } catch (e) {
      err = String(e);
    }
  }

  async function start() {
    if (!provider) return;
    busy = true;
    result = null;
    try {
      const st = await recorderStart({ provider });
      status = st;
      toast(`Recording on ${provider}`, 'ok');
    } catch (e) {
      toast(`Start failed: ${String(e)}`, 'err');
    } finally {
      busy = false;
    }
  }

  async function stop() {
    busy = true;
    try {
      const r = await recorderStop();
      result = r;
      status = { active: false };
      toast(`Recorded ${r.event_count} event(s)`, 'ok');
    } catch (e) {
      toast(`Stop failed: ${String(e)}`, 'err');
    } finally {
      busy = false;
    }
  }

  async function copyToml() {
    if (!result) return;
    try {
      await navigator.clipboard.writeText(result.toml_snippet);
      toast('Workflow TOML copied', 'ok');
    } catch {
      toast('Copy failed', 'err');
    }
  }

  onMount(async () => {
    try {
      providers = await getProviders();
      const names = Object.keys(providers);
      provider = names[0] ?? '';
    } catch (e) {
      err = String(e);
    }
    await refreshStatus();
    timer = window.setInterval(() => void refreshStatus(), 1000);
  });

  onDestroy(() => {
    if (timer !== undefined) window.clearInterval(timer);
  });

  const stepCount = $derived(result?.workflow.steps.length ?? 0);
</script>

<div class="flex items-center justify-between mb-1">
  <h2 class="m-0">Recorder</h2>
  {#if status.active}
    <span class="text-[11px] ok inline-flex items-center gap-1.5">
      <span class="w-1.5 h-1.5 rounded-full bg-ok"></span>
      recording
    </span>
  {/if}
</div>
<p class="lead">
  Drive the site once by hand. UWA observes which selectors you touched,
  in what order, and proposes a workflow you can paste into
  <code>uwa.toml</code>. No drag-and-drop, no YAML authoring.
</p>

{#if err}
  <div class="card border-err">
    <p class="err m-0 text-[13px]">{err}</p>
  </div>
{/if}

<div class="card">
  <div class="card-head">
    <h3 class="m-0">{status.active ? 'Recording in progress' : 'Start a recording'}</h3>
    {#if status.active}
      <button class="btn" disabled={busy} onclick={stop}>
        {busy ? '…' : 'Stop'}
      </button>
    {:else}
      <button class="btn" disabled={busy || !provider} onclick={start}>
        {busy ? 'Starting…' : 'Start recording'}
      </button>
    {/if}
  </div>

  {#if !status.active}
    <div class="flex flex-wrap items-center gap-2">
      <label class="text-[11px] muted uppercase tracking-[0.5px]" for="rec-provider">provider</label>
      <select id="rec-provider" class="input max-w-[220px]" bind:value={provider}>
        {#each Object.keys(providers) as p (p)}
          <option value={p}>{p}</option>
        {/each}
      </select>
    </div>
    <ol class="mt-3 mb-0 pl-5 text-[13px] text-muted space-y-1">
      <li>Start recording.</li>
      <li>
        Switch to the tab showing this provider and do what you'd normally
        do: click "new chat", type a message, hit send, wait for the answer.
      </li>
      <li>Come back here and stop recording. UWA writes the workflow.</li>
    </ol>
  {:else}
    <dl class="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1 text-[13px] m-0">
      <dt class="muted">provider</dt>
      <dd class="m-0 font-mono">{status.provider}</dd>
      <dt class="muted">tab</dt>
      <dd class="m-0 font-mono">{status.tab}</dd>
      <dt class="muted">elapsed</dt>
      <dd class="m-0 font-mono">{fmtRel((status.elapsed_ms ?? 0) / 1000)}</dd>
      <dt class="muted">events</dt>
      <dd class="m-0 font-mono">{status.events ?? 0}</dd>
      <dt class="muted">observer</dt>
      <dd class="m-0">
        {#if status.installed}
          <span class="ok">attached</span>
        {:else}
          <span class="warn">detached — check that the tab is still open</span>
        {/if}
      </dd>
    </dl>
    {#if (status.poll_errors ?? 0) > 0}
      <p class="text-[11.5px] warn mt-2 mb-0">
        {status.poll_errors} poll error(s). The tab may have been closed or
        navigated to a page the debugger can't reach.
      </p>
    {/if}
  {/if}
</div>

{#if result}
  <div class="card">
    <div class="card-head">
      <h3 class="m-0">
        Proposal — {stepCount} step{stepCount === 1 ? '' : 's'} from
        {result.event_count} raw event{result.event_count === 1 ? '' : 's'}
      </h3>
      <button class="btn-secondary" onclick={copyToml}>Copy TOML</button>
    </div>

    {#if result.warnings.length > 0}
      <div class="mt-3 p-3 border border-border rounded-sm bg-panel2">
        <div class="text-[12px] font-medium mb-1">Notes from the recorder</div>
        <ul class="list-none m-0 p-0 text-[12px] space-y-0.5">
          {#each result.warnings as w (w)}
            <li>· {w}</li>
          {/each}
        </ul>
      </div>
    {/if}

    <p class="text-[12.5px] muted mt-0 mb-2">
      Paste this under <code>[providers.{result.provider}]</code> in
      <code>uwa.toml</code>, then restart uwa. Delete any step you didn't
      intend — a recorded workflow is a suggestion, not a policy.
    </p>
    <CodeBlock code={result.toml_snippet} lang="toml" />
  </div>
{/if}
