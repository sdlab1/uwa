<script lang="ts">
  import { onMount } from 'svelte';
  import type { SessionInfo, SessionsResp } from '../lib/types';
  import { getSessions, dropSession, recoverSessions } from '../lib/api';
  import { toast } from '../lib/stores';
  import { fmtRel } from '../lib/format';

  let sessions = $state<SessionInfo[]>([]);
  let err = $state<string | null>(null);
  let loading = $state(true);
  let lastRefresh = $state<number | null>(null);
  let busy = $state<string | null>(null);

  async function load() {
    try {
      const r: SessionsResp = await getSessions();
      sessions = r.sessions;
      err = null;
      lastRefresh = Date.now();
    } catch (e) {
      err = String(e);
    } finally {
      loading = false;
    }
  }

  async function drop(id: string) {
    if (!confirm(`Drop session ${id}?`)) return;
    busy = id;
    try {
      await dropSession(id);
      toast('Session dropped', 'ok');
      await load();
    } catch (e) {
      toast(`Drop failed: ${String(e)}`, 'err');
    } finally {
      busy = null;
    }
  }

  async function recover() {
    try {
      const r = await recoverSessions();
      toast(
        r.dropped_tabs.length === 0
          ? 'All sessions healthy'
          : `Recovered: dropped ${r.dropped_tabs.length} unhealthy tab(s)`,
        'ok',
      );
      await load();
    } catch (e) {
      toast(`Recover failed: ${String(e)}`, 'err');
    }
  }

  onMount(() => {
    void load();
    const iv = setInterval(() => {
      if (document.visibilityState === 'visible') void load();
    }, 4000);
    return () => clearInterval(iv);
  });
</script>

<div class="flex items-center justify-between mb-1">
  <h2>Sessions</h2>
  <span class="text-[11px] muted">
    {sessions.length} active
    {#if lastRefresh}· updated {new Date(lastRefresh).toLocaleTimeString()}{/if}
  </span>
</div>
<p class="lead">Each conversation is pinned to a tab. Idle sessions are reaped automatically.</p>

<div class="flex items-center gap-2 mb-3">
  <button class="btn-secondary" onclick={load}>Refresh</button>
  <button class="btn-secondary" onclick={recover}>Recover unhealthy</button>
</div>

{#if err}
  <div class="card border-err">
    <p class="err text-[13px] m-0">{err}</p>
    <p class="muted text-[12px] mt-2 m-0">
      The session manager may be disabled in uwa.toml. Set the API key if required.
    </p>
  </div>
{:else if loading}
  <div class="card"><p class="muted m-0">Loading…</p></div>
{:else if sessions.length === 0}
  <div class="card">
    <p class="muted m-0">No sessions yet — they appear on the first request.</p>
  </div>
{:else}
  <div class="card overflow-x-auto">
    <table>
      <thead>
        <tr>
          <th>Conversation</th>
          <th>Tab</th>
          <th class="text-right">Age</th>
          <th class="text-right">Idle</th>
          <th class="text-right">Holders</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {#each sessions as s (s.conversation)}
          <tr>
            <td class="font-mono text-[12px]">{s.conversation}</td>
            <td class="font-mono text-[12px] text-muted">{s.tab.slice(0, 12)}…</td>
            <td class="text-right">{fmtRel(s.age_secs)}</td>
            <td class="text-right">{fmtRel(s.idle_secs)}</td>
            <td class="text-right">{s.generation}</td>
            <td class="text-right">
              <button
                class="btn-secondary"
                disabled={busy === s.conversation}
                onclick={() => drop(s.conversation)}>
                {busy === s.conversation ? '…' : 'Drop'}
              </button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}
