<script lang="ts">
  let {
    values = [] as number[],
    height = 32,
    color = 'var(--accent)',
  }: { values?: number[]; height?: number; color?: string } = $props();

  const max = $derived(Math.max(1, ...values));
</script>

<div class="flex items-end gap-[2px]" style="height: {height}px">
  {#each values as v, i (i)}
    <div class="flex-1 rounded-t-[2px]"
         style="height: {Math.max(2, (v / max) * height)}px; background: {color}; opacity: 0.75"
         title={`${v} rpm`}></div>
  {/each}
</div>
