<script lang="ts">
	// One company's flow per block: received above the line, sent below, on one
	// BTC scale; then the running net in its own strip. Values come from the
	// verifier's checked sums (#lib/live.ts), never from the export's fields.
	// Style, keys, tooltip and table: ChartFrame and src/lib/chart.ts (W6).
	import { btc, int } from '#lib/format.ts';
	import { nice, type FlowPoint } from '#lib/live.ts';
	import { barWidth, COLOR, edgeTicks, gutterFor, heightLabel, padFor, tickAnchor, tickBtc } from '#lib/chart.ts';
	import ChartFrame from './ChartFrame.svelte';

	// `clip` (W12 round 3, Home's preview): when one block's flow is more than three times the next
	// busiest block's, the scale fits the next one and that block's bars are cut with a break mark
	// and their value, as with Data's `zoom` (one 450 BTC block left the rest of the preview empty).
	let { points, compact = false, zoom = null, clip = false }: { points: FlowPoint[]; compact?: boolean; zoom?: number | null; clip?: boolean } = $props();

	let w = $state(0);
	let hover = $state<number | null>(null);

	const toBtc = (s: bigint) => Number(s) / 1e8;
	const n = $derived(points.length);
	const peak = $derived(Math.max(...points.map((p) => Math.max(toBtc(p.received), toBtc(p.sent))), 0));
	const second = $derived(points.map((p) => Math.max(toBtc(p.received), toBtc(p.sent))).sort((a, b) => b - a)[1] ?? 0);
	const max = $derived(zoom ?? (clip && second > 0 && peak > 3 * second ? nice(second) : nice(peak)));
	const yTicks = $derived([max, max / 2, 0, -max / 2, -max]);

	const running = $derived.by(() => {
		let acc = 0n;
		return points.map((p) => (acc += p.net));
	});
	const rMax = $derived(nice(Math.max(1, ...running.map((r) => Math.abs(toBtc(r))))));
	const rLo = $derived(Math.min(0, ...running.map(toBtc)) < 0 ? -rMax : 0);
	const rHi = $derived(Math.max(0, ...running.map(toBtc)) > 0 ? rMax : 0);
	const rTicks = $derived([rHi, 0, rLo].filter((t, k, a) => a.indexOf(t) === k));

	const padL = $derived(compact ? 0 : padFor([...yTicks.map(tickBtc), ...rTicks.map(tickBtc), 'Out ↓']));
	// Compact (Home): no left axis; the scale (in above, out below, zero) sits in a right gutter.
	const padR = $derived(compact ? gutterFor([tickBtc(max), tickBtc(-max)]) : 4);
	const H = $derived(compact ? 96 : 220);
	const H2 = 84;
	const AX = 18;
	const band = $derived(Math.max(0, (w - padL - padR) / Math.max(1, n)));
	const bw = $derived(barWidth(band));
	const cx = (i: number) => padL + band * i + band / 2;
	const mid = $derived(H / 2);
	// Room above and below for the value labels of clipped bars.
	const room = $derived(compact && peak <= max ? 8 : compact ? 18 : 20);
	const y = (v: number) => mid - (Math.max(-max, Math.min(max, v)) / max) * (mid - room);
	const clipped = (v: number) => Math.abs(v) > max;
	const ry = (v: number) => 8 + (H2 - 16) * ((rHi - v) / (rHi - rLo || 1));
	const line = $derived(running.map((r, i) => `${i ? 'L' : 'M'}${cx(i)},${ry(toBtc(r))}`).join(''));

	/** Bar from the middle line to `v`, rounded at the data end; a 1.5 px stub for any non-zero value. */
	function bar(x: number, v: number, width: number): string {
		if (v === 0) return '';
		const y0 = mid;
		let end = y(v);
		const up = v > 0;
		if (Math.abs(end - y0) < 1.5) {
			if (compact) return '';
			end = y0 + (up ? -1.5 : 1.5);
		}
		const r = Math.min(3, Math.abs(end - y0), width / 2);
		const l = x - width / 2;
		const s = up ? 1 : -1;
		return `M${l},${y0}V${end + s * r}Q${l},${end} ${l + r},${end}H${l + width - r}Q${l + width},${end} ${l + width},${end + s * r}V${y0}Z`;
	}

	const active = $derived(points.filter((p) => p.txs > 0).length);
	const total = $derived(running[running.length - 1] ?? 0n);
	// Compact (no data table): the cut bars' values are in the text alternative too.
	const cutNote = $derived(
		compact
			? points
					.filter((p) => clipped(toBtc(p.received)) || clipped(toBtc(p.sent)))
					.map((p) => ` Block ${heightLabel(p.height)} runs past the scale (${tickBtc(max)} BTC each way): ${tickBtc(toBtc(p.received))} BTC in, ${tickBtc(toBtc(p.sent))} BTC out.`)
					.join('')
			: ''
	);
	const label = $derived(n === 0 ? 'No blocks to draw.' : `Flows in ${n} blocks: activity in ${active}; running net ends at ${btc(total)} BTC.${cutNote}`);
	// W3: the same numbers as the drawing and the tooltip.
	const table = $derived({
		caption: label,
		testid: 'flow-table',
		columns: [{ label: 'Block' }, { label: 'In (BTC)', right: true }, { label: 'Out (BTC)', right: true }, { label: 'Transactions', right: true }, { label: 'Running net (BTC)', right: true }],
		rows: points.map((p, i) => [int(p.height), btc(p.received), btc(p.sent), int(p.txs), btc(running[i])])
	});
	const index = (x: number) => Math.floor((x - padL) / Math.max(1, band));
</script>

<ChartFrame
	{label}
	{n}
	step="blocks"
	{compact}
	height={compact ? H : 22 + H + 22 + H2 + AX + 12}
	bind:w
	bind:hover
	{index}
	at={hover === null ? null : cx(hover)}
	{band}
	tipTop={compact ? 0 : 22}
	{table}
	empty="No flows in this range."
>
	{#snippet plot()}
		{#if !compact}<div class="chart-label" style:padding-left="{padL}px">BTC per block</div>{/if}
		<svg width={w} height={H} aria-hidden="true">
			{#if !compact}
				{#each yTicks as t (t)}
					<line class="grid" class:zero={t === 0} x1={padL} x2={w - padR} y1={y(t)} y2={y(t)} />
					{#if t !== 0}<text class="axis" x={padL - 8} y={y(t) + 4} text-anchor="end">{tickBtc(t)}</text>{/if}
				{/each}
				<text class="axis label" x={padL - 8} y={mid - 5} text-anchor="end">In ↑</text>
				<text class="axis label" x={padL - 8} y={mid + 13} text-anchor="end">Out ↓</text>
			{:else}
				{#each [max, 0, -max] as t (t)}
					<line class="grid" class:zero={t === 0} x1="0" x2={w - padR} y1={y(t)} y2={y(t)} />
					<text class="axis" x={w - padR + 6} y={y(t) + 4}>{tickBtc(t)}</text>
				{/each}
			{/if}
			{#each points as p, i (p.height)}
				{@const vi = toBtc(p.received)}
				{@const vo = -toBtc(p.sent)}
				<g class="mark" class:dim={hover !== null && hover !== i}>
					<path class="in" d={bar(cx(i), vi, bw)} />
					<path class="out" d={bar(cx(i), vo, bw)} />
					{#if clipped(vi)}<path class="cut" d="M{cx(i) - bw / 2 - 2},{y(max) + 7}l{bw / 2 + 2},-4l{bw / 2 + 2},4" /><text class="axis" x={cx(i)} y={y(max) - 4} text-anchor="middle">{tickBtc(vi)}</text>{/if}
					{#if clipped(vo)}<path class="cut" d="M{cx(i) - bw / 2 - 2},{y(-max) - 7}l{bw / 2 + 2},4l{bw / 2 + 2},-4" /><text class="axis" x={cx(i)} y={y(-max) + 13} text-anchor="middle">{tickBtc(-vo)}</text>{/if}
				</g>
			{/each}
		</svg>

		{#if !compact}
			<div class="chart-label" style:padding-left="{padL}px">Running net, BTC</div>
			<svg width={w} height={H2 + AX} aria-hidden="true">
				{#each rTicks as t (t)}
					<line class="grid" class:zero={t === 0} x1={padL} x2={w - padR} y1={ry(t)} y2={ry(t)} />
					<text class="axis" x={padL - 8} y={ry(t) + 4} text-anchor="end">{tickBtc(t)}</text>
				{/each}
				<path class="line" d={line} />
				{#each running as r, i (points[i].height)}
					{#if hover === i || i === n - 1}<circle class="dot" cx={cx(i)} cy={ry(toBtc(r))} r="3.5" />{/if}
				{/each}
				<text class="axis value" x={w - padR} y={ry(toBtc(total)) + (toBtc(total) < 0 ? 18 : -10)} text-anchor="end">{btc(total).replace(/(\.\d{2})\d+$/, '$1')} BTC</text>
				{#each edgeTicks(n) as i (i)}
					<text class="axis" x={cx(i)} y={H2 + AX - 2} text-anchor={tickAnchor(i, n)}>{heightLabel(points[i].height)}</text>
				{/each}
			</svg>
		{/if}
	{/snippet}
	{#snippet tip(k: number)}
		{@const p = points[k]}
		<b>{heightLabel(p.height)}</b>
		{#if p.txs === 0}
			<span>No transactions</span>
		{:else}
			<span><i class="key" style:background={COLOR.in}></i>In <em>{btc(p.received)}</em></span>
			<span><i class="key" style:background={COLOR.out}></i>Out <em>{btc(p.sent)}</em></span>
			<span>{p.txs} transaction{p.txs === 1 ? '' : 's'}</span>
		{/if}
		<span><i class="key line"></i>Running net <em>{btc(running[k])}</em></span>
	{/snippet}
</ChartFrame>

<style>
	/* The break mark on a bar cut by the scale. */
	.cut {
		fill: none;
		stroke: var(--panel-solid);
		stroke-width: 3;
	}
</style>
