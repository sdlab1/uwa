<script module lang="ts">
  export type Phase =
    | 'idle' | 'connecting' | 'waiting' | 'first-token'
    | 'streaming' | 'done' | 'error';
</script>

<script lang="ts">
  let {
    phase,
    ttftMs,
    totalMs,
    chars,
  }: {
    phase: Phase;
    ttftMs?: number;
    totalMs?: number;
    chars: number;
  } = $props();

  const label = $derived(
    phase === 'connecting' ? 'connecting'
    : phase === 'waiting'  ? 'waiting for first token'
    : phase === 'first-token' ? 'first token'
    : phase === 'streaming'   ? 'streaming'
    : phase === 'done'        ? 'done'
    : phase === 'error'       ? 'error'
    : 'idle',
  );

  const tone = $derived(
    phase === 'done' ? 'ok'
    : phase === 'error' ? 'err'
    : phase === 'idle' ? 'muted'
    : 'accent',
  );

  const tps = $derived.by(() => {
    if (phase !== 'streaming' || !ttftMs || !totalMs || totalMs <= ttftMs) return null;
    const secs = (totalMs - ttftMs) / 1000;
    if (secs <= 0) return null;
    return chars / 4 / secs; // rough tokens-per-second
  });
</script>

<span class="stream-badge badge-{tone}">
  {#if phase === 'connecting' || phase === 'waiting' || phase === 'streaming'}
    <span class="dot"></span>
  {/if}
  <span class="badge-label">{label}</span>
  {#if ttftMs != null && phase !== 'idle' && phase !== 'connecting'}
    <span class="badge-metric">TTFT {Math.round(ttftMs)} ms</span>
  {/if}
  {#if tps != null}
    <span class="badge-metric">~{tps.toFixed(1)} tok/s</span>
  {/if}
  {#if totalMs != null && phase === 'done'}
    <span class="badge-metric">total {Math.round(totalMs)} ms</span>
  {/if}
</span>
