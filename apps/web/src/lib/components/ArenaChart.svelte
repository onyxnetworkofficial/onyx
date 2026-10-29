<script lang="ts">
	// Price lines for the Markets arena (0 to 100%): one line per outcome, the first with an area
	// fill and a dot on its newest point. `small` draws a card sparkline without axes.
	let { series, colors, small = false, label }: { series: number[][]; colors: string[]; small?: boolean; label: string } = $props();
	const W = $derived(small ? 120 : 640);
	const H = $derived(small ? 36 : 220);
	const pad = $derived(small ? 2 : 28);
	const lo = $derived(small ? Math.max(0, Math.min(...series.flat()) - 0.04) : 0);
	const hi = $derived(small ? Math.min(1, Math.max(...series.flat()) + 0.04) : 1);
	const x = (k: number, n: number) => pad + (k / Math.max(1, n - 1)) * (W - pad * (small ? 2 : 2.4));
	const y = (v: number) => pad / 2 + (1 - (v - lo) / Math.max(0.0001, hi - lo)) * (H - pad);
	const path = (s: number[]) => s.map((v, k) => `${k ? 'L' : 'M'}${x(k, s.length).toFixed(1)},${y(v).toFixed(1)}`).join('');
	const area = (s: number[]) => `${path(s)}L${x(s.length - 1, s.length).toFixed(1)},${(H - pad / 2).toFixed(1)}L${x(0, s.length).toFixed(1)},${(H - pad / 2).toFixed(1)}Z`;
</script>

<svg viewBox="0 0 {W} {H}" class:small role="img" aria-label={label}>
	{#if !small}
		{#each [0.25, 0.5, 0.75] as g}
			<line x1={pad} x2={W - pad * 1.4} y1={y(g)} y2={y(g)} class="grid" />
			<text x={W - 2} y={y(g) + 4} text-anchor="end" class="axis">{Math.round(g * 100)}%</text>
		{/each}
	{/if}
	{#if series[0]?.length}
		<path d={area(series[0])} style:fill={colors[0]} class="fill" />
	{/if}
	{#each series as s, j}
		<path d={path(s)} style:stroke={colors[j]} fill="none" class="line" class:lead={j === 0} />
	{/each}
	{#each series as s, j}
		{#if s.length && (j === 0 || !small)}
			<circle cx={x(s.length - 1, s.length)} cy={y(s[s.length - 1])} r={small ? 2.5 : 4} style:fill={colors[j]} />
		{/if}
	{/each}
</svg>

<style>
	svg { display: block; width: 100%; height: auto; overflow: visible; }
	.grid { stroke: var(--line); stroke-width: 1; }
	.axis { fill: var(--muted); font: var(--text-xs) var(--mono); }
	.fill { opacity: 0.1; }
	.line { stroke-width: 1.5; vector-effect: non-scaling-stroke; stroke-linejoin: round; }
	.line.lead { stroke-width: 2.25; }
</style>
