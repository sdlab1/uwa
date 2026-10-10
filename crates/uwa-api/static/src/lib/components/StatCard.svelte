<script lang="ts">
  let {
    label,
    value,
    sub = '',
    tone = 'neutral',
    highlight = false,
  }: {
    label: string;
    value: string;
    sub?: string;
    tone?: 'neutral' | 'ok' | 'warn' | 'err';
    highlight?: boolean;
  } = $props();

  const toneClass: Record<string, string> = {
    neutral: 'text-text',
    ok: 'text-ok',
    warn: 'text-warn',
    err: 'text-err',
  };
</script>

<div class="rounded-sm border border-border bg-panel2 px-3 py-2.5"
     class:ring-1={highlight}
     class:ring-accent={highlight}>
  <div class="text-[11px] uppercase tracking-[0.5px] text-muted">{label}</div>
  <div class="text-lg font-semibold mt-0.5 {toneClass[tone]}">{value}</div>
  {#if sub}<div class="text-[11px] text-muted mt-0.5">{sub}</div>{/if}
</div>
