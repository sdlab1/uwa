<script lang="ts">
  let {
    rows = [] as Array<[string, string | number | null | undefined]>,
  }: { rows?: Array<[string, string | number | null | undefined]> } = $props();
</script>

<dl class="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1 text-[13px] m-0">
  {#each rows as [k, v] (k)}
    <dt class="text-muted">{k}</dt>
    <dd class="m-0 font-mono text-[12.5px] break-all">{v ?? '—'}</dd>
  {/each}
</dl>
