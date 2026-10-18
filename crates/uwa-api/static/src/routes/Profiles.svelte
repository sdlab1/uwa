<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import type { ProfileListResp, ProfileSummary } from '../lib/types';
  import type { ProfileHealth, CookieRecord } from '../lib/api';
  import {
    getProfiles,
    openProfileTab,
    resetProfile,
    getProfileHealth,
    exportProfileCookies,
    importProfileCookies,
  } from '../lib/api';
  import { toast } from '../lib/stores';
  import CodeBlock from '../lib/components/CodeBlock.svelte';

  let data = $state<ProfileListResp | null>(null);
  let err = $state<string | null>(null);
  let loading = $state(true);
  let busy: string | null = $state(null);
  let healthByProfile: Record<string, ProfileHealth> = $state({});
  let healthTimer: number | undefined;

  async function load() {
    try {
      data = await getProfiles();
      err = null;
    } catch (e) {
      err = String(e);
    } finally {
      loading = false;
    }
  }

  async function checkHealth(p: ProfileSummary) {
    if (!p.named) return;
    try {
      healthByProfile = {
        ...healthByProfile,
        [p.name]: await getProfileHealth(p.name),
      };
    } catch (e) {
      toast(`Health ${p.name}: ${String(e)}`, 'err');
    }
  }

  async function checkAll() {
    if (!data) return;
    for (const p of data.profiles) {
      if (p.named) await checkHealth(p);
    }
  }

  async function exportCookies(p: ProfileSummary) {
    busy = p.name;
    try {
      const e = await exportProfileCookies(p.name);
      const blob = new Blob([JSON.stringify(e, null, 2)], { type: 'application/json' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      const stamp = new Date().toISOString().replace(/[:.]/g, '-');
      a.download = `uwa-cookies-${p.name}-${stamp}.json`;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(url);
      toast(`Exported ${e.cookies.length} cookie(s)`, 'ok');
    } catch (e) {
      toast(`Export failed: ${String(e)}`, 'err');
    } finally {
      busy = null;
    }
  }

  async function importCookies(p: ProfileSummary) {
    const input = document.createElement('input');
    input.type = 'file';
    input.accept = 'application/json,.json';
    input.onchange = async () => {
      const file = input.files?.[0];
      if (!file) return;
      busy = p.name;
      try {
        const raw = await file.text();
        const parsed = JSON.parse(raw);
        const cookies: CookieRecord[] = Array.isArray(parsed)
          ? parsed
          : (parsed.cookies ?? []);
        if (cookies.length === 0) {
          toast('File has no cookies', 'warn');
          return;
        }
        const r = await importProfileCookies(p.name, cookies);
        toast(`Imported ${r.imported} cookie(s) into ${p.name}`, 'ok');
        await checkHealth(p);
      } catch (e) {
        toast(`Import failed: ${String(e)}`, 'err');
      } finally {
        busy = null;
      }
    };
    input.click();
  }

  function promptUrl(): string | null {
    const raw = window.prompt(
      'Provider URL to open in this profile for login:',
      'https://chatgpt.com/',
    );
    if (!raw) return null;
    try {
      return new URL(raw.trim()).toString();
    } catch {
      toast(`Not a valid URL: ${raw}`, 'warn');
      return null;
    }
  }

  async function openSite(p: ProfileSummary, url: string) {
    busy = p.name;
    try {
      const reuse = new URL(url).origin;
      const r = await openProfileTab(p.name, {
        url,
        reuse_if_url_starts_with: reuse,
      });
      toast(
        `${r.reused ? 'Reused tab' : 'Opened tab'} ${r.tab} for ${p.name}`,
        'ok',
      );
    } catch (e) {
      toast(`Open failed: ${String(e)}`, 'err');
    } finally {
      busy = null;
    }
  }

  async function reset(p: ProfileSummary) {
    const sure = window.confirm(
      `Reset profile "${p.name}"?\n\nEvery tab in it is closed and its cookies/storage are discarded.`,
    );
    if (!sure) return;
    busy = p.name;
    try {
      const r = await resetProfile(p.name);
      toast(`Dropped ${r.dropped_tabs.length} tab(s) from ${p.name}`, 'ok');
      delete healthByProfile[p.name];
      healthByProfile = { ...healthByProfile };
      await load();
    } catch (e) {
      toast(`Reset failed: ${String(e)}`, 'err');
    } finally {
      busy = null;
    }
  }

  function statusTone(s: ProfileHealth['providers'][number]['status']): string {
    switch (s) {
      case 'logged_in': return 'ok';
      case 'logged_out': return 'err';
      case 'no_tab': return 'muted';
      default: return 'warn';
    }
  }

  onMount(async () => {
    await load();
    void checkAll();
    healthTimer = window.setInterval(() => {
      if (document.visibilityState === 'visible') void checkAll();
    }, 60_000);
  });

  onDestroy(() => {
    if (healthTimer) window.clearInterval(healthTimer);
  });
</script>

<div class="flex items-center justify-between mb-1">
  <h2 class="m-0">Profiles</h2>
  <button class="btn-secondary" onclick={load}>Refresh</button>
</div>
<p class="lead">
  Each profile is a separate browser context with its own cookies and
  logins. Clients pick one with the <code>X-UWA-Session</code> header.
</p>

{#if err}
  <div class="card border-err">
    <p class="err m-0 text-[13px]">{err}</p>
    <button class="btn-secondary mt-3" onclick={load}>Retry</button>
  </div>
{:else if loading}
  <div class="card"><p class="muted m-0">Loading…</p></div>
{:else if data}
  {#if !data.supports_profiles}
    <div class="card border-warn">
      <h3 class="m-0">Multi-profile is not available on this transport</h3>
      <p class="text-[12.5px] muted mt-2 mb-0">
        The current browser backend doesn't expose browser contexts. Only
        the default profile is usable. Named profiles require the CDP
        backend (which is what <code>uwa start</code> launches).
      </p>
    </div>
  {/if}

  {#if data.profiles.filter((p) => p.named).length === 0}
    <div class="card">
      <h3 class="m-0">No named profiles configured</h3>
      <p class="text-[13px] muted mt-2">Add one to <code>uwa.toml</code>:</p>
      <pre class="clients-snippet"><code>[profiles.work]
label = "Work accounts"</code></pre>
      <p class="text-[12px] muted m-0">
        Restart uwa to pick the change up. Then come back here to open a
        login page inside the profile.
      </p>
    </div>
  {/if}

  <div class="grid gap-3">
    {#each data.profiles as p (p.name)}
      <div class="card">
        <div class="card-head">
          <div>
            <h3 class="m-0 text-[15px]">
              {p.label}
              {#if p.name === 'default'}
                <span class="text-[11px] muted ml-2">default</span>
              {/if}
            </h3>
            <div class="text-[11px] muted mt-0.5">
              {p.name}{#if p.notes} · {p.notes}{/if}
            </div>
          </div>
          <div class="flex gap-1">
            {#if p.named}
              <button
                class="btn-secondary"
                disabled={busy === p.name}
                onclick={() => {
                  const u = promptUrl();
                  if (u) void openSite(p, u);
                }}>
                Open login page
              </button>
              <button
                class="btn-secondary"
                disabled={busy === p.name || p.tabs.length === 0}
                onclick={() => reset(p)}>
                {busy === p.name ? '…' : 'Reset'}
              </button>
            {:else}
              <span class="text-[11px] muted">managed by Chromium</span>
            {/if}
          </div>
        </div>

        <div class="text-[12.5px] mt-3">
          <div class="flex items-center justify-between mb-1">
            <span class="muted uppercase tracking-[0.5px] text-[10px]">login health</span>
            {#if p.named}
              <button
                class="ghost-btn"
                disabled={busy === p.name}
                onclick={() => checkHealth(p)}>
                Check now
              </button>
            {/if}
          </div>
          {#if !p.named}
            <span class="muted">The default context's login state can't be probed
              from uwa — check it directly in Chromium.</span>
          {:else if !healthByProfile[p.name]}
            <span class="muted">Not checked yet.</span>
          {:else if healthByProfile[p.name]?.providers.length === 0}
            <span class="muted">No providers configured.</span>
          {:else}
            <ul class="list-none m-0 p-0 space-y-0.5">
              {#each (healthByProfile[p.name]?.providers ?? []) as h (h.provider)}
                <li class="flex items-center gap-2">
                  <span
                    class="pill pill-{statusTone(h.status)} text-[10px]"
                    style="min-width: 88px; text-align: center;">
                    {h.status}
                  </span>
                  <span class="flex-1">{h.provider}</span>
                  {#if h.note}<span class="muted text-[11px] truncate" title={h.note}>{h.note}</span>{/if}
                </li>
              {/each}
            </ul>
          {/if}
        </div>

        {#if p.named}
          <div class="flex gap-2 mt-3 pt-3 border-t border-border">
            <button
              class="btn-secondary"
              disabled={busy === p.name}
              onclick={() => exportCookies(p)}>
              {busy === p.name ? '…' : 'Export cookies'}
            </button>
            <button
              class="btn-secondary"
              disabled={busy === p.name}
              onclick={() => importCookies(p)}>
              {busy === p.name ? '…' : 'Import cookies'}
            </button>
            <span class="text-[11px] muted ml-auto self-center">
              portable JSON · includes login cookies
            </span>
          </div>
        {/if}
        <div class="text-[12.5px]">
          {#if p.tabs.length === 0}
            <span class="muted">No tabs — the profile has never been used.</span>
          {:else}
            <span class="muted">{p.tabs.length} tab{p.tabs.length === 1 ? '' : 's'}:</span>
            <ul class="list-none m-0 mt-1 p-0 space-y-0.5">
              {#each p.tabs as t (t)}
                <li><code class="text-muted text-[11.5px]">{t}</code></li>
              {/each}
            </ul>
          {/if}
        </div>
      </div>
    {/each}
  </div>

  <div class="card">
    <h3 class="m-0">Using a profile from a client</h3>
    <p class="text-[13px] muted mt-2">
      Send the name in an <code>X-UWA-Session</code> header on any chat
      request. The Playground has a profile selector too, so you can try
      it without a terminal.
    </p>
    <CodeBlock lang="bash" code={`curl http://127.0.0.1:8080/v1/chat/completions \\\n  -H "content-type: application/json" \\\n  -H "x-uwa-session: work" \\\n  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}'`} />
  </div>
{/if}
