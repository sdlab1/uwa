<script lang="ts">
  import { onMount } from 'svelte';
  import type { ConformanceReport } from '../lib/api';
  import { getConformance } from '../lib/api';
  import { toast } from '../lib/stores';
  import { fmtPercent } from '../lib/format';

  let report = $state<ConformanceReport | null>(null);
  let err = $state<string | null>(null);
  let loading = $state(true);
  let expanded: string | null = $state(null);

  const TAGS = ['basic', 'schema', 'parallel', 'arguments', 'selection', 'errors', 'edge'] as const;

  async function load() {
    try {
      report = await getConformance();
      err = null;
    } catch (e) {
      err = String(e);
    } finally {
      loading = false;
    }
  }

  function rateColor(rate: number): string {
    if (rate >= 0.95) return 'ok';
    if (rate >= 0.8) return 'warn';
    return 'err';
  }

  function tagCell(run: ConformanceReport['runs'][number], tag: string): string {
    const t = run.by_tag[tag];
    if (!t) return '—';
    return `${t.passed}/${t.total}`;
  }

  async function copyMarkdown() {
    if (!report || report.runs.length === 0) return;
    let md = `| Provider | Pass | Total | Success |\n|---|---:|---:|---:|\n`;
    for (const r of report.runs) {
      md += `| \`${r.provider}\` | ${r.passed} | ${r.scenarios_total} | ${(r.success_rate * 100).toFixed(1)}% |\n`;
    }
    try {
      await navigator.clipboard.writeText(md);
      toast('Markdown table copied', 'ok');
    } catch {
      toast('Copy failed', 'err');
    }
  }

  onMount(load);
</script>

<div class="flex items-center justify-between mb-1">
  <h2>Conformance</h2>
  <div class="flex items-center gap-2">
    {#if report?.generated_at}
      <span class="text-[11px] muted">
        last run {report.generated_at} · uwa {report.uwa_version}
      </span>
    {/if}
    <button class="btn-secondary" onclick={load}>Refresh</button>
    {#if report?.runs.length}
      <button class="btn-secondary" onclick={copyMarkdown}>Copy table</button>
    {/if}
  </div>
</div>
<p class="lead">
  Tool-calling conformance across providers. Every scenario is a prompt,
  a tool set, and a pass/fail predicate; the table below is what real
  browsers produced the last time the suite ran.
</p>

{#if err}
  <div class="card border-err">
    <p class="err text-[13px] m-0">{err}</p>
    <button class="btn-secondary mt-3" onclick={load}>Retry</button>
  </div>
{:else if loading}
  <div class="card"><p class="muted m-0">Loading…</p></div>
{:else if !report || report.runs.length === 0}
  <div class="card">
    <h3>No conformance data yet</h3>
    <p class="text-[13px] muted">
      Run the suite against a provider to populate this page.
      The report is written to <code>~/.uwa/conformance/latest.json</code>
      and read back on every page load.
    </p>
    <pre class="clients-snippet mt-2"><code>uwa conformance run --provider chatgpt --markdown</code></pre>
    {#if report?.hint}
      <p class="text-[12px] muted mt-2">{report.hint}</p>
    {/if}
  </div>
{:else}
  <div class="card overflow-x-auto">
    <table>
      <thead>
        <tr>
          <th>Provider</th>
          <th class="text-right">Pass</th>
          <th class="text-right">Total</th>
          <th class="text-right">Success</th>
          {#each TAGS as t (t)}
            <th class="text-right">{t}</th>
          {/each}
        </tr>
      </thead>
      <tbody>
        {#each report.runs as r (r.provider)}
          <tr>
            <td class="font-medium">{r.provider}</td>
            <td class="text-right">{r.passed}</td>
            <td class="text-right">{r.scenarios_total}</td>
            <td class="text-right {rateColor(r.success_rate)}">
              {fmtPercent(r.success_rate)}
            </td>
            {#each TAGS as t (t)}
              <td class="text-right font-mono text-[11px]">{tagCell(r, t)}</td>
            {/each}
          </tr>
        {/each}
      </tbody>
    </table>
  </div>

  {#if report.runs.some((r) => r.failures.length > 0)}
    {#each report.runs as r (r.provider)}
      {#if r.failures.length > 0}
        <div class="card">
          <div class="card-head">
            <h3 class="m-0">{r.provider} — {r.failed} failure{r.failed === 1 ? '' : 's'}</h3>
            <button class="ghost-btn" onclick={() => (expanded = expanded === r.provider ? null : r.provider)}>
              {expanded === r.provider ? 'hide' : 'show'}
            </button>
          </div>
          {#if expanded === r.provider}
            <ul class="list-none m-0 p-0 space-y-2">
              {#each r.failures as f (f.id)}
                <li class="text-[12.5px]">
                  <code class="text-muted">{f.id}</code>
                  <span class="mx-2">·</span>
                  <span>{f.name}</span>
                  <div class="text-[11.5px] err mt-0.5">{f.reason ?? 'no reason'}</div>
                </li>
              {/each}
            </ul>
          {/if}
        </div>
      {/if}
    {/each}
  {/if}

  <div class="card">
    <h3>What this covers</h3>
    <p class="text-[13px] muted">
      The corpus has {report.scenarios_total} scenarios across 7 tags: basic
      tool calls, JSON-Schema conformance, parallel calls, argument shape,
      tool selection, error recovery, and edge inputs. The full scenario
      list, with descriptions, lives in
      <code>crates/uwa-conformance/src/scenarios.rs</code>. The Markdown
      table in the project README is generated from the same JSON this
      page reads.
    </p>
  </div>
{/if}