<script lang="ts">
  import { onMount } from 'svelte';
  import type { ProviderInfo, ProviderMap, SelectorTestResp } from '../lib/types';
  import { getProviders, selectorTest, selectorGenerate } from '../lib/api';
  import { toast } from '../lib/stores';
  import KV from '../lib/components/KV.svelte';
  import CodeBlock from '../lib/components/CodeBlock.svelte';

  let providers = $state<ProviderMap>({});
  let err = $state<string | null>(null);
  let loading = $state(true);

  // Per-provider inline selector-test state.
  type TestState = {
    open: boolean;
    selector: string;
    result: SelectorTestResp | null;
    running: boolean;
  };
  let tests = $state<Record<string, TestState>>({});

  // Per-provider "generate" (autogen scan) state.
  type GenState = { open: boolean; analysis: unknown | null; running: boolean };
  let gens = $state<Record<string, GenState>>({});

  async function load() {
    try {
      providers = await getProviders();
      err = null;
    } catch (e) {
      err = String(e);
    } finally {
      loading = false;
    }
  }

  function testState(name: string, info: ProviderInfo): TestState {
    if (!tests[name]) {
      tests[name] = { open: false, selector: info.selectors.input ?? '', result: null, running: false };
    }
    return tests[name]!;
  }

  async function runTest(name: string) {
    const t = tests[name];
    if (!t) return;
    t.running = true;
    t.result = null;
    try {
      t.result = await selectorTest({ provider: name, selector: t.selector });
    } catch (e) {
      t.result = { matched: 0, first_text: null, duration_ms: 0, error: String(e) };
    } finally {
      t.running = false;
    }
  }

  function genState(name: string): GenState {
    if (!gens[name]) gens[name] = { open: false, analysis: null, running: false };
    return gens[name]!;
  }

  async function runGenerate(name: string) {
    const g = gens[name];
    if (!g) return;
    g.running = true;
    g.analysis = null;
    try {
      g.analysis = await selectorGenerate({ provider: name });
    } catch (e) {
      g.analysis = { error: String(e) };
    } finally {
      g.running = false;
    }
  }

  onMount(load);
</script>

<h2>Config</h2>
<p class="lead">Read-only view of the loaded provider config. Edit <code>uwa.toml</code> and restart to change.</p>

{#if err}
  <div class="card border-err">
    <p class="err text-[13px] m-0">{err}</p>
  </div>
{:else if loading}
  <div class="card"><p class="muted m-0">Loading…</p></div>
{:else if Object.keys(providers).length === 0}
  <div class="card">
    <p class="muted m-0">No providers configured. Add <code>[providers.*]</code> blocks to uwa.toml.</p>
  </div>
{:else}
  {#each Object.entries(providers) as [name, info] (name)}
    {@const t = testState(name, info)}
    {@const g = genState(name)}
    <div class="card">
      <div class="card-head">
        <div>
          <h3 class="m-0 text-[15px]">{name}</h3>
          <div class="text-[11px] muted mt-0.5">
            {info.extraction} · {info.backend ?? 'default backend'}
          </div>
        </div>
        <div class="flex gap-1">
          <button class="ghost-btn" onclick={() => (t.open = !t.open)}>test selector</button>
          <button class="ghost-btn" onclick={() => { g.open = !g.open; if (g.open && !g.analysis) void runGenerate(name); }}>
            scan
          </button>
        </div>
      </div>

      <KV rows={[
        ['url patterns', info.url_patterns.join(', ')],
        ['input', info.selectors.input],
        ['send_button', info.selectors.send_button],
        ['assistant_message', info.selectors.assistant_message],
        ['streams', info.capabilities.streams ? 'yes' : 'no'],
        ['tool_calls', info.capabilities.tool_calls ? 'yes' : 'no'],
        ['vision', info.capabilities.vision ? 'yes' : 'no'],
        ['max ctx', info.capabilities.max_context_tokens ?? '—'],
      ]} />

      {#if t.open}
        <div class="mt-3 p-3 border border-border rounded-sm bg-panel2">
          <div class="flex items-center gap-2">
            <input
              class="input font-mono text-[12px]"
              placeholder="CSS selector to test"
              bind:value={t.selector}
            />
            <button class="btn" disabled={t.running || !t.selector} onclick={() => runTest(name)}>
              {t.running ? 'Running…' : 'Run'}
            </button>
          </div>
          {#if t.result}
            <div class="mt-2 text-[12.5px]">
              {#if t.result.error}
                <span class="err">Error: {t.result.error}</span>
              {:else if t.result.matched === 0}
                <span class="warn">Matched 0 elements — not logged in, wrong page, or stale selector.</span>
              {:else}
                <span class="ok">✓ Matched {t.result.matched} element(s)</span>
                <span class="muted"> in {t.result.duration_ms} ms</span>
                {#if t.result.first_text}
                  <div class="mt-2 p-2 bg-panel3 rounded-sm font-mono text-[11.5px] whitespace-pre-wrap max-h-[120px] overflow-auto">
                    {t.result.first_text}
                  </div>
                {/if}
              {/if}
            </div>
          {/if}
        </div>
      {/if}

      {#if g.open}
        <div class="mt-3 p-3 border border-border rounded-sm bg-panel2">
          <div class="flex items-center justify-between mb-2">
            <span class="text-[12px] muted">Auto-generated candidates from the live page.</span>
            <button class="ghost-btn" disabled={g.running} onclick={() => runGenerate(name)}>
              {g.running ? 'Scanning…' : 'Re-scan'}
            </button>
          </div>
          {#if g.running}
            <p class="muted text-[12px] m-0">Analyzing DOM…</p>
          {:else if g.analysis}
            <CodeBlock code={JSON.stringify(g.analysis, null, 2)} lang="json" />
          {/if}
        </div>
      {/if}
    </div>
  {/each}
{/if}
