<script lang="ts">
  import { onMount } from 'svelte';
  import type { ProfileListResp, ProfileSummary } from '../lib/types';
  import { getProfiles, openProfileTab, resetProfile } from '../lib/api';
  import { toast } from '../lib/stores';
  import CodeBlock from '../lib/components/CodeBlock.svelte';

  let data = $state<ProfileListResp | null>(null);
  let err = $state<string | null>(null);
  let loading = $state(true);
  let busy: string | null = $state(null);

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

  async function openSite(p: ProfileSummary, url: string) {
    busy = p.name;
    try {
      const r = await openProfileTab(p.name, { url, reuse_if_url_starts_with: url });
      toast(r.reused ? `Reused tab ${r.tab}` : `Opened tab ${r.tab} in ${p.name}`, 'ok');
      await load();
    } catch (e) {
      toast(`Open failed: ${String(e)}`, 'err');
    } finally {
      busy = null;
    }
  }

  async function reset(p: ProfileSummary) {
    const confirmed = confirm(
      `Reset profile "${p.label}"?\n\n` +
        'Every tab in this profile will be closed and its cookies cleared. ' +
        'You will have to log in again the next time you use it.',
    );
    if (!confirmed) return;
    busy = p.name;
    try {
      const r = await resetProfile(p.name);
      const n = r.dropped_tabs?.length ?? 0;
      toast(`Reset ${p.name} (dropped ${n} tab(s))`, 'ok');
      await load();
    } catch (e) {
      toast(`Reset failed: ${String(e)}`, 'err');
    } finally {
      busy = null;
    }
  }

  /** Prompt the user for a URL — a full URL picker is overkill, and
   * most usage is a provider login page. */
  function promptUrl(): string | null {
    const v = prompt('URL to open in this profile', 'https://chatgpt.com/');
    return v && v.trim() ? v.trim() : null;
  }

  onMount(load);
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
