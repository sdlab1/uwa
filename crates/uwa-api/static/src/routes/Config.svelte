<script lang="ts">
  import { onMount } from 'svelte';
  import type { ProviderInfo, ProviderMap, SelectorTestResp, RegistryInfo } from '../lib/types';
  import { getProviders, getProviderRegistry, selectorTest, selectorGenerate } from '../lib/api';
  import { toast } from '../lib/stores';
  import KV from '../lib/components/KV.svelte';
  import CodeBlock from '../lib/components/CodeBlock.svelte';

  let providers = $state<ProviderMap>({});
  let err = $state<string | null>(null);
  let loading = $state(true);

  // Registry info for the top-of-page card.
  let registry = $state<RegistryInfo | null>(null);

  // Per-provider inline selector-test state.
  type TestState = {
    open: boolean;
    field: string;
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
    try {
      registry = await getProviderRegistry();
    } catch {
      registry = null;
    }
  }

  /** Toggle the inline selector-test panel. Mutating $state here is legal:
   * event handlers run outside the render phase (unlike {@const} in the
   * template, which Svelte 5 rejects with `state_unsafe_mutation`). */
  function toggleTest(name: string, info: ProviderInfo): void {
    if (!tests[name]) {
      tests[name] = {
        open: false,
        field: 'input',
        selector: info.selectors.input ?? '',
        result: null,
        running: false,
      };
    }
    const t = tests[name]!;
    t.open = !t.open;
  }

  /** Switch which selector field the probe stands for; resets the result. */
  function setField(name: string, info: ProviderInfo, field: string): void {
    const t = tests[name];
    if (!t) return;
    t.field = field;
    t.selector =
      field === 'input' ? info.selectors.input ?? '' :
      field === 'send_button' ? info.selectors.send_button ?? '' :
      field === 'assistant_message' ? info.selectors.assistant_message ?? '' :
      '';
    t.result = null;
  }

  async function runTest(name: string) {
    const t = tests[name];
    if (!t) return;
    t.running = true;
    t.result = null;
    try {
      t.result = await selectorTest({
        provider: name,
        selector: t.selector,
        field: t.field,
      });
    } catch (e) {
      t.result = { matched: 0, first_text: null, duration_ms: 0, error: String(e) };
    } finally {
      t.running = false;
    }
  }

  /** We never touch the user's uwa.toml — put an override snippet on the
   * clipboard instead, so the registry suggestion stays explicit. */
  async function applyFallback(name: string, field: string, selector: string): Promise<void> {
    const snippet = `[providers.${name}.selectors]\n${field} = "${selector}"\n`;
    try {
      await navigator.clipboard.writeText(snippet);
      toast(`Copied override snippet for ${name}.${field}`, 'ok');
    } catch {
      toast('Copy failed — select and copy manually', 'err');
    }
  }

  /** Toggle the autogen scan panel; lazily kick off the first scan. */
  function toggleGen(name: string): void {
    if (!gens[name]) gens[name] = { open: false, analysis: null, running: false };
    const g = gens[name]!;
    g.open = !g.open;
    if (g.open && !g.analysis) void runGenerate(name);
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

{#if registry}
  <div class="card">
    <div class="card-head">
      <h3 class="m-0">Registry</h3>
      <span class="text-[11px] muted">{Object.keys(registry.entries).length} entries</span>
    </div>
    <dl class="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1 text-[13px] m-0">
      {#if registry.manifest}
        <dt class="muted">manifest</dt>
        <dd class="m-0 font-mono">{registry.manifest.registry_version}</dd>
        <dt class="muted">generated</dt>
        <dd class="m-0 font-mono">{registry.manifest.generated_at}</dd>
      {:else}
        <dt class="muted">manifest</dt>
        <dd class="m-0 muted">(none — using built-in entries)</dd>
      {/if}
      <dt class="muted">cache</dt>
      <dd class="m-0 font-mono">{registry.cache_dir ?? '(built-in only)'}</dd>
    </dl>
    <p class="text-[11px] muted mt-2 m-0">
      The registry supplies selectors the user config leaves unset. Update
      with <code>uwa providers update</code>; inspect with
      <code>uwa providers diff NAME</code>.
    </p>
  </div>
{/if}

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
    {@const t = tests[name]}
    {@const g = gens[name]}
    <div class="card">
      <div class="card-head">
        <div>
          <h3 class="m-0 text-[15px]">{name}</h3>
          <div class="text-[11px] muted mt-0.5">
            {info.extraction} · {info.backend ?? 'default backend'}
          </div>
        </div>
        <div class="flex gap-1">
          <button class="ghost-btn" onclick={() => toggleTest(name, info)}>test selector</button>
          <button class="ghost-btn" onclick={() => toggleGen(name)}>scan</button>
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

      {#if t && t.open}
        <div class="mt-3 p-3 border border-border rounded-sm bg-panel2">
          <div class="flex items-center gap-2 mb-2">
            <label class="text-[11px] muted uppercase tracking-[0.5px]" for={`sel-field-${name}`}>field</label>
            <select
              id={`sel-field-${name}`}
              class="input max-w-[180px]"
              value={t.field}
              onchange={(e) => setField(name, info, (e.currentTarget as HTMLSelectElement).value)}>
              <option value="input">input</option>
              <option value="send_button">send_button</option>
              <option value="assistant_message">assistant_message</option>
            </select>
          </div>
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
                <div class="warn">
                  <div>Matched 0 elements — not logged in, wrong page, or stale selector.</div>
                  {#if t.result.fallbacks && t.result.fallbacks.length > 0}
                    <div class="mt-2 muted text-[11.5px]">Registry fallbacks tried:</div>
                    <ul class="list-none m-0 p-0 mt-1 space-y-1">
                      {#each t.result.fallbacks as f (f.selector)}
                        <li class="flex items-center gap-2 text-[12px]">
                          <code class="flex-1 truncate" title={f.selector}>{f.selector}</code>
                          <span class="ok shrink-0">{f.matched} match</span>
                          <button
                            class="ghost-btn shrink-0"
                            onclick={() => applyFallback(name, t.field, f.selector)}>
                            copy override
                          </button>
                        </li>
                      {/each}
                    </ul>
                    <div class="text-[11px] muted mt-1">
                      Add the snippet to <code>uwa.toml</code>, then restart uwa.
                    </div>
                  {:else}
                    <div class="mt-1 text-[11.5px] muted">
                      No registry fallbacks matched. Try a full page load,
                      then re-run; or use the <b>scan</b> panel below.
                    </div>
                  {/if}
                </div>
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

      {#if g && g.open}
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
