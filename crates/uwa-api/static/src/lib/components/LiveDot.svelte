<script lang="ts">
  let { on = true, label = 'live' }: { on?: boolean; label?: string } = $props();
</script>

<span class="inline-flex items-center gap-1.5 text-[11px] text-muted">
  <span class="w-1.5 h-1.5 rounded-full {on ? 'bg-ok pulse' : 'bg-muted'}"></span>
  {label}
</span>

<style>
  .pulse { animation: pulse 1.6s ease-in-out infinite; }
  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50%      { opacity: 0.35; }
  }
</style>
