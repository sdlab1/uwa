<script lang="ts">
  let {
    values = [] as number[],
    width = 160,
    height = 32,
    color = '#5a9eff',
  } = $props();

  const points = $derived.by(() => {
    if (values.length < 2) return '';
    const max = Math.max(...values, 1);
    const step = width / (values.length - 1);
    return values
      .map((v, i) => `${(i * step).toFixed(1)},${(height - (v / max) * (height - 2) - 1).toFixed(1)}`)
      .join(' ');
  });
</script>

<svg {width} {height} viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none" class="block">
  <polyline fill="none" stroke={color} stroke-width="1.5" points={points} />
</svg>
