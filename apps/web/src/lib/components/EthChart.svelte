<script lang="ts">
	// One Ethereum metric over the window (ADR-0044): bars (one per run of blocks) or a line (one
	// point per block). Values and their exact labels come from the verifier's checked
	// `eth-chain-stats` rows (#lib/ethchain.ts); this file only draws them.
	// Style, keys, tooltip and table: ChartFrame and src/lib/chart.ts (W6).
	import { barPath, barWidth, edgeTicks, padFor, tickAnchor } from '#lib/chart.ts';
	import type { EthPoint } from '#lib/ethchain.ts';
	import { nice } from '#lib/live.ts';
	import ChartFrame from './ChartFrame.svelte';

	let {
		points,
		name,
		unit,
		mode = 'bars',
		tick,
		step = 'bars',
		testid
	}: {
		points: EthPoint[];
		name: string;
		unit: string;
		mode?: 'bars' | 'line';
		/** The y-axis label of a value in the chart's unit. */
		tick: (v: number) => string;
		step?: string;
		testid?: string;
	} = $props();

	let w = $state(0);
	let hover = $state<number | null>(null);
	const n = $derived(points.length);
	const max = $derived(nice(Math.max(0, ...points.map((p) => p.v))));
	const yTicks = $derived([max, 0]);
	const padL = $derived(padFor(yTicks.map(tick)));
	const padR = 4;
	const H = 112;
	const AX = 32;
	const band = $derived(Math.max(0, (w - padL - padR) / Math.max(1, n)));
	const bw = $derived(barWidth(band));
	const cx = (i: number) => padL + band * i + band / 2;
	const y = (v: number) => H - 8 - (max > 0 ? v / max : 0) * (H - 18);
	const line = $derived(points.map((p, i) => `${i ? 'L' : 'M'}${cx(i)},${y(p.v)}`).join(''));
	const label = $derived(
		n === 0 ? `${name}: nothing to draw.` : `${name} (${unit}), ${n} ${step}, from ${points[0].head} to ${points[n - 1].head}.`
	);
	const table = $derived({
		caption: label,
		testid: testid ? `${testid}-table` : undefined,
		columns: [{ label: mode === 'bars' ? 'Blocks · UTC' : 'Block · UTC' }, { label: 'First block' }, { label: `${name} (${unit})`, right: true }],
		rows: points.map((p) => [p.head, p.sub, p.text])
	});
	const index = (x: number) => Math.floor((x - padL) / Math.max(1, band));
	// Block numbers are long ("#26,225,261"): on a narrow plot three x labels would overlap, so it
	// keeps the first and last (W12: labels never collide; 7.2 px is one axis character).
	const labelW = $derived(Math.max(0, ...points.map((p) => Math.max(p.top.length, p.sub.length))) * 7.2);
	const xTicks = $derived(w - padL - padR < 3 * labelW + 24 ? edgeTicks(n).filter((i) => i === 0 || i === n - 1) : edgeTicks(n));
</script>

<ChartFrame {label} {n} {step} height={H + AX} bind:w bind:hover {index} at={hover === null ? null : cx(hover)} {band} {table} empty="{name}: nothing in this window.">
	{#snippet plot()}
		<svg width={w} height={H + AX} aria-hidden="true" data-testid={testid}>
			{#each yTicks as t (t)}
				<line class="grid" class:zero={t === 0} x1={padL} x2={w - padR} y1={y(t)} y2={y(t)} />
				<text class="axis" x={padL - 8} y={y(t) + 4} text-anchor="end">{tick(t)}</text>
			{/each}
			{#if mode === 'bars'}
				{#each points as p, i (i)}
					<path class="mark pos" class:dim={hover !== null && hover !== i} d={barPath(cx(i), y(0), y(p.v), bw)} />
				{/each}
			{:else}
				<path class="line" d={line} />
				{#if hover !== null}<circle class="dot" cx={cx(hover)} cy={y(points[hover].v)} r="3.5" />{/if}
			{/if}
			{#each xTicks as i (i)}
				<line class="tick" x1={cx(i)} x2={cx(i)} y1={H - 4} y2={H + 2} />
				<text class="axis" x={cx(i)} y={H + 14} text-anchor={tickAnchor(i, n)}>{points[i].top}</text>
				<text class="axis sub" x={cx(i)} y={H + AX - 4} text-anchor={tickAnchor(i, n)}>{points[i].sub}</text>
			{/each}
		</svg>
	{/snippet}
	{#snippet tip(k: number)}
		{@const p = points[k]}
		<b>{p.head}</b>
		<span>{name} <em>{p.text}</em></span>
	{/snippet}
</ChartFrame>
