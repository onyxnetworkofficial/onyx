<script lang="ts">
	// One company's net flow per block (bars) and its running total (line), on
	// one BTC scale so the two are directly comparable. Values come from the
	// verifier's checked series (#lib/live.ts), never from the export's fields.
	// Style, keys, tooltip and table: ChartFrame and src/lib/chart.ts (W6).
	import { btc, int } from '#lib/format.ts';
	import { nice, utc, type FlowPoint } from '#lib/live.ts';
	import { barPath, barWidth, COLOR, edgeTicks, heightLabel, padFor, tickAnchor, tickBtc } from '#lib/chart.ts';
	import ChartFrame from './ChartFrame.svelte';

	// `times` (block height -> header time, unix seconds) adds a time line to the x axis;
	// the history chart passes the times its verifier read from the headers.
	// `ends` (first height -> last height) marks points that sum several
	// blocks (the 7-day view's six-block buckets). `showTxs` is off for sums of
	// several companies: one transaction can touch two of them (IDX-R1-F4).
	let {
		points,
		name,
		times,
		ends,
		showTxs = true
	}: { points: FlowPoint[]; name: string; times?: Record<number, number>; ends?: Record<number, number>; showTxs?: boolean } = $props();

	let w = $state(0);
	let hover = $state<number | null>(null);

	const toBtc = (s: bigint) => Number(s) / 1e8;
	const n = $derived(points.length);
	const running = $derived.by(() => {
		let acc = 0n;
		return points.map((p) => (acc += p.net));
	});
	const max = $derived(nice(Math.max(1e-8, ...points.map((p) => Math.abs(toBtc(p.net))), ...running.map((r) => Math.abs(toBtc(r))))));
	const yTicks = $derived([max, 0, -max]);
	const padL = $derived(padFor(yTicks.map(tickBtc)));
	const padR = 4;
	const H = 128;
	// x axis: the time (when known) over the height, at the first, middle and last point.
	const AX = $derived(times ? 32 : 18);
	const band = $derived(Math.max(0, (w - padL - padR) / Math.max(1, n)));
	const bw = $derived(barWidth(band));
	const cx = (i: number) => padL + band * i + band / 2;
	const y = (v: number) => H / 2 - (v / max) * (H / 2 - 10);
	const line = $derived(running.map((r, i) => `${i ? 'L' : 'M'}${cx(i)},${y(toBtc(r))}`).join(''));
	const day = (t: number) => new Date(t * 1000).toLocaleDateString('en-GB', { day: 'numeric', month: 'short', timeZone: 'UTC' });
	const total = $derived(running[running.length - 1] ?? 0n);
	const steps = $derived(ends ? 'six-block steps' : 'blocks');
	const label = $derived(n === 0 ? `${name}: no blocks to draw.` : `${name}: net flow in ${n} ${steps}, ending at ${btc(total)} BTC.`);
	const span = (h: number) => (ends?.[h] !== undefined && ends[h] !== h ? `${int(h)}–${int(ends[h])}` : int(h));
	// W3: the same numbers as the drawing and the tooltip.
	const table = $derived({
		caption: label,
		testid: 'series-table',
		columns: [
			{ label: ends ? 'Blocks' : 'Block' },
			...(times ? [{ label: 'Time (UTC)' }] : []),
			{ label: 'Net (BTC)', right: true },
			...(showTxs ? [{ label: 'Transactions', right: true }] : []),
			{ label: 'Running total (BTC)', right: true }
		],
		rows: points.map((p, i) => [
			span(p.height),
			...(times ? [times[p.height] ? utc(times[p.height]) : '–'] : []),
			btc(p.net),
			...(showTxs ? [int(p.txs)] : []),
			btc(running[i])
		])
	});
	/** A bar from the zero line, with a 1.5 px stub for any non-zero value. */
	function bar(x: number, v: number): string {
		if (v === 0) return '';
		let end = y(v);
		if (Math.abs(end - H / 2) < 1.5) end = H / 2 + (v > 0 ? -1.5 : 1.5);
		return barPath(x, H / 2, end, bw);
	}
	const index = (x: number) => Math.floor((x - padL) / Math.max(1, band));
</script>

<ChartFrame {label} {n} step={steps} height={H + AX} bind:w bind:hover {index} at={hover === null ? null : cx(hover)} {band} {table} empty="{name}: no blocks in this range.">
	{#snippet plot()}
		<svg width={w} height={H + AX} aria-hidden="true">
			{#each yTicks as t (t)}
				<line class="grid" class:zero={t === 0} x1={padL} x2={w - padR} y1={y(t)} y2={y(t)} />
				<text class="axis" x={padL - 8} y={y(t) + 4} text-anchor="end">{tickBtc(t)}</text>
			{/each}
			{#each points as p, i (p.height)}
				{@const v = toBtc(p.net)}
				<path class="mark" class:pos={v > 0} class:neg={v < 0} class:dim={hover !== null && hover !== i} d={bar(cx(i), v)} />
			{/each}
			<path class="line" d={line} />
			{#if hover !== null}<circle class="dot" cx={cx(hover)} cy={y(toBtc(running[hover]))} r="3.5" />{/if}
			{#each edgeTicks(n) as i (i)}
				{@const t = times?.[points[i].height]}
				<line class="tick" x1={cx(i)} x2={cx(i)} y1={H - 4} y2={H + 2} />
				{#if times}<text class="axis" x={cx(i)} y={H + 14} text-anchor={tickAnchor(i, n)}>{t ? (ends ? day(t) : utc(t)) : ''}</text>{/if}
				<text class="axis sub" x={cx(i)} y={H + AX - 4} text-anchor={tickAnchor(i, n)}>{heightLabel(points[i].height)}</text>
			{/each}
		</svg>
	{/snippet}
	{#snippet tip(k: number)}
		{@const p = points[k]}
		<b>#{span(p.height)}{#if times?.[p.height]}{" "}· {utc(times[p.height])} UTC{/if}</b>
		<span><i class="key" style:background={toBtc(p.net) < 0 ? COLOR.out : COLOR.in}></i>Net <em>{btc(p.net)}</em></span>
		{#if showTxs}<span>{p.txs} transaction{p.txs === 1 ? '' : 's'}</span>{/if}
		<span><i class="key line"></i>Running total <em>{btc(running[k])}</em></span>
	{/snippet}
</ChartFrame>
