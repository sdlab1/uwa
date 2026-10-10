<script lang="ts">
  import type { Snippet } from 'svelte';

  let {
    title = '',
    open = $bindable(true),
    size = 'md',
    children,
    footer,
  }: {
    title?: string;
    open?: boolean;
    size?: 'sm' | 'md' | 'lg';
    children?: Snippet;
    footer?: Snippet;
  } = $props();

  const maxW = $derived(
    size === 'sm' ? 'max-w-[420px]'
    : size === 'lg' ? 'max-w-[860px]'
    : 'max-w-[640px]',
  );

  function onKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') open = false;
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if open}
  <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4"
       role="presentation" onclick={() => (open = false)}>
    <div class="bg-panel border border-border rounded w-full {maxW} max-h-[85vh] flex flex-col"
         role="dialog" onclick={(e) => e.stopPropagation()}>
      <div class="flex items-center justify-between px-4 py-3 border-b border-border shrink-0">
        <h3 class="m-0 text-base">{title}</h3>
        <button class="ghost-btn" onclick={() => (open = false)}>close</button>
      </div>
      <div class="overflow-auto p-4">
        {@render children?.()}
      </div>
      {#if footer}
        <div class="px-4 py-3 border-t border-border flex justify-end gap-2 shrink-0">
          {@render footer()}
        </div>
      {/if}
    </div>
  </div>
{/if}
