<script lang="ts">
  import { parseMarkdown } from '../markdown';

  let { source }: { source: string } = $props();

  const blocks = $derived(parseMarkdown(source));
</script>

<div class="markdown">
  {#each blocks as b, i (i)}
    {#if b.type === 'code'}
      <div class="md-code">
        {#if b.lang}<span class="md-lang">{b.lang}</span>{/if}
        <pre><code>{b.code}</code></pre>
      </div>
    {:else}
      <p class="md-p">{@html b.html}</p>
    {/if}
  {/each}
</div>
