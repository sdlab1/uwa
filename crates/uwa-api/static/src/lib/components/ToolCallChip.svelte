<script lang="ts">
  import { toast } from '../stores';

  let {
    id,
    name,
    args,
    done = true,
  }: { id?: string; name?: string; args?: string; done?: boolean } = $props();

  let open = $state(false);

  let pretty = $derived.by(() => {
    if (!args) return '';
    try {
      return JSON.stringify(JSON.parse(args), null, 2);
    } catch {
      return args;
    }
  });

  async function copy() {
    try {
      await navigator.clipboard.writeText(args ?? '');
      toast('Tool arguments copied', 'ok');
    } catch {
      toast('Copy failed', 'err');
    }
  }
</script>

<div class="tool-chip" class:streaming={!done}>
  <div class="tool-head">
    <span class="tool-name">{name ?? 'tool'}</span>
    {#if id}<code class="tool-id" title={id}>{id.slice(0, 12)}…</code>{/if}
    {#if !done}<span class="tool-live">streaming…</span>{/if}
    <span class="tool-actions">
      <button class="ghost-btn" onclick={() => (open = !open)}>{open ? 'hide' : 'args'}</button>
      <button class="ghost-btn" onclick={copy}>copy</button>
    </span>
  </div>
  {#if open}
    <pre class="tool-args"><code>{pretty || '(no arguments)'}</code></pre>
  {/if}
</div>
