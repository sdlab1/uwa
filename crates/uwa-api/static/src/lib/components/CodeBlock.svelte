<script lang="ts">
  import { toast } from '../stores';

  let { code = '', lang = '' }: { code?: string; lang?: string } = $props();

  let copied = $state(false);

  async function copy() {
    try {
      await navigator.clipboard.writeText(code);
      copied = true;
      toast('Copied', 'ok');
      setTimeout(() => (copied = false), 1200);
    } catch {
      toast('Copy failed', 'err');
    }
  }
</script>

<div class="relative bg-[#0b0e13] border border-border rounded-sm p-3 my-2 overflow-auto">
  {#if lang}<span class="absolute top-1.5 right-14 text-[10px] muted uppercase tracking-[0.5px]">{lang}</span>{/if}
  <button class="absolute top-1.5 right-1.5 ghost-btn" onclick={copy}>
    {copied ? 'copied' : 'copy'}
  </button>
  <pre class="whitespace-pre text-[#c9d4e6]"><code>{code}</code></pre>
</div>
