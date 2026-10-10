<script lang="ts">
  import { toasts } from '../stores';
  const border: Record<string, string> = {
    info: 'border-border', ok: 'border-ok', warn: 'border-warn', err: 'border-err',
  };
</script>

<div class="fixed bottom-5 right-5 flex flex-col gap-2 z-[100]">
  {#each $toasts as t (t.id)}
    <div class="px-3.5 py-2.5 rounded-sm bg-panel3 shadow-lg text-[13px] max-w-[440px] border {border[t.kind] ?? 'border-border'}">
      {t.msg}
    </div>
  {/each}
</div>
