<script lang="ts">
	// One column of a bulk dataset over its newest rows (the All datasets page, ROADMAP S22): a line
	// on a scale from the lowest to the highest value (values may be negative), the zero line when
	// the range crosses it. The values are the rows' exact decimals (#lib/datasets.ts chartPoints);
	// they become numbers only for drawing. Style, keys, tooltip and table: ChartFrame (W6).
	import { edgeTicks, padFor, tickAnchor } from '#lib/chart.ts';
	import ChartFrame from './ChartFrame.svelte';

	type Point = { v: number; text: string; head: string };
	let { points, name, testid }: { points: Point[]; name: string; testid?: string } = $props();

	let w = $state(0);
	let hover = $state<number | null>(null);
	const COMPACT = new Intl.NumberFormat('en-US', { notation: 'compact', maximumFractionDigits: 2 });
	const tick = (v: number) => COMPACT.format(v);
	const n = $derived(points.length);
	const lo = $derived(Math.min(...points.map((p) => p.v)));
	const hi = $derived(Math.max(...points.map((p) => p.v)));
	// A flat line sits in the middle of a small band around its value.
	const span = $derived(hi > lo ? hi - lo : Math.max(1, Math.abs(hi)) * 0.1);
	const top = $derived(hi > lo ? hi : hi + span / 2);
	const bottom = $derived(hi > lo ? lo : lo - span / 2);
	const yTicks = $derived(bottom < 0 && top > 0 ? [top, 0, bottom] : [top, bottom]);
	const padL = $derived(padFor(yTicks.map(tick)));
	const padR = 4;
	const H = 112;
	const AX = 20;
	const band = $derived(Math.max(0, (w - padL - padR) / Math.max(1, n)));
	const cx = (i: number) => padL + band * i + band / 2;
	const y = (v: number) => 8 + ((top - v) / (top - bottom)) * (H - 18);
	const line = $derived(points.map((p, i) => `${i ? 'L' : 'M'}${cx(i)},${y(p.v)}`).join(''));
	const label = $derived(n === 0 ? `${name}: nothing to draw.` : `${name}, ${n} rows, from ${points[0].head} to ${points[n - 1].head}.`);
	const table = $derived({
		caption: label,
		testid: testid ? `${testid}-table` : undefined,
		columns: [{ label: 'Key' }, { label: name, right: true }],
		rows: points.map((p) => [p.head, p.text])
	});
	const index = (x: number) => Math.floor((x - padL) / Math.max(1, band));
	// Three dates need about 3 x 80 px of plot; a narrower chart labels its first and last row.
	const xTicks = $derived(w - padL - padR >= 260 ? edgeTicks(n) : [...new Set([0, n - 1])].filter((i) => i >= 0));
</script>

<ChartFrame {label} {n} step="rows" height={H + AX} bind:w bind:hover {index} at={hover === null ? null : cx(hover)} {band} {table} empty="{name}: nothing to draw.">
	{#snippet plot()}
		<svg width={w} height={H + AX} aria-hidden="true" data-testid={testid}>
			{#each yTicks as t (t)}
				<line class="grid" class:zero={t === 0} x1={padL} x2={w - padR} y1={y(t)} y2={y(t)} />
				<text class="axis" x={padL - 8} y={y(t) + 4} text-anchor="end">{tick(t)}</text>
			{/each}
			<path class="line" d={line} />
			{#if hover !== null}<circle class="dot" cx={cx(hover)} cy={y(points[hover].v)} r="3.5" />{/if}
			{#each xTicks as i (i)}
				<line class="tick" x1={cx(i)} x2={cx(i)} y1={H - 4} y2={H + 2} />
				<text class="axis" x={cx(i)} y={H + 14} text-anchor={tickAnchor(i, n)}>{points[i].head}</text>
			{/each}
		</svg>
	{/snippet}
	{#snippet tip(k: number)}
		{@const p = points[k]}
		<b>{p.head}</b>
		<span>{name} <em>{p.text}</em></span>
	{/snippet}
</ChartFrame>
