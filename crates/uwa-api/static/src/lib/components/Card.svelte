<script lang="ts">
  import type { Snippet } from 'svelte';

  let {
    title = '',
    subtitle = '',
    head,
    children,
  }: {
    title?: string;
    subtitle?: string;
    head?: Snippet;
    children?: Snippet;
  } = $props();
</script>

<div class="card">
  {#if head ?? title}
    <div class="card-head">
      {#if head}
        {@render head()}
      {:else}
        <h3 class="m-0">{title}</h3>
        {#if subtitle}<span class="muted text-xs">{subtitle}</span>{/if}
      {/if}
    </div>
  {/if}
  {@render children?.()}
</div>
