<script lang="ts">
	// One company's on-chain balance at the end of each reserve shard (a line),
	// from the verifier's checked chain (verify/src/lib.rs `check_reserves`),
	// never from the export's fields. The axis spans the week's low to high, so
	// small moves on a large balance stay visible; the legend says so.
	// Style, keys, tooltip and table: ChartFrame and src/lib/chart.ts (W6).
	// W11: a week that ended lower draws its line in --btc (the site's "down / out" colour) and
	// ends in a down triangle; a week that ended higher ends in an up triangle. The shape says
	// it without colour; the name, the table caption and the card's "Week" line say it in words.
	import { btc, int } from '#lib/format.ts';
	import { COLOR, edgeTicks, heightLabel, padFor, tickAnchor, tickBtc } from '#lib/chart.ts';
	import ChartFrame from './ChartFrame.svelte';

	let { points, name }: { points: { height: number; balance: bigint }[]; name: string } = $props();

	let w = $state(0);
	let hover = $state<number | null>(null);
	const H = 120;
	const AX = 18;
	const padR = 4;
	const toBtc = (s: bigint) => Number(s) / 1e8;
	const n = $derived(points.length);
	const vals = $derived(points.map((p) => toBtc(p.balance)));
	const lo = $derived(Math.min(...vals));
	const hi = $derived(Math.max(...vals));
	const span = $derived(hi - lo || Math.max(1, hi * 0.01));
	const yTicks = $derived([hi, (hi + lo) / 2, lo]);
	const padL = $derived(padFor(yTicks.map(tickBtc)));
	const stepW = $derived((w - padL - padR) / Math.max(1, n - 1));
	const x = (i: number) => padL + stepW * i;
	const y = (v: number) => 8 + (H - 16) * (1 - (v - lo) / span);
	const line = $derived(vals.map((v, i) => `${i ? 'L' : 'M'}${x(i)},${y(v)}`).join(''));
	const trend = $derived(n < 2 ? 0 : Math.sign(Number(points[n - 1].balance - points[0].balance)));
	const label = $derived(
		n === 0
			? `${name}: no balances to draw.`
			: `${name}: balance over ${n} points, from ${btc(points[0].balance)} to ${btc(points[n - 1].balance)} BTC${trend < 0 ? ', lower at the end of the week' : trend > 0 ? ', higher at the end of the week' : ''}.`
	);
	/** A triangle pointing at the last point: from above for a lower week (▼), from below for a
	 *  higher one (▲), on the side the line leaves free; kept inside the plot's right edge. */
	const arrow = (cx: number, cy: number, down: boolean) => {
		const c = Math.min(cx, w - padR - 5);
		return down ? `M${c - 5},${cy - 14}h10l-5,8z` : `M${c - 5},${cy + 14}h10l-5,-8z`;
	};
	// W3: the same numbers as the drawing and the tooltip.
	const table = $derived({
		caption: label,
		testid: 'reserve-table',
		columns: [{ label: 'After block' }, { label: 'Balance (BTC)', right: true }],
		rows: points.map((p) => [int(p.height), btc(p.balance)])
	});
	const index = (px: number) => Math.round((px - padL) / Math.max(1, stepW));
</script>

<ChartFrame {label} {n} height={H + AX} bind:w bind:hover {index} at={hover === null ? null : x(hover)} band={Math.min(stepW, 24)} {table} empty="{name}: no balances in this range.">
	{#snippet plot()}
		<svg width={w} height={H + AX} aria-hidden="true">
			{#each yTicks as t, k (k)}
				<line class="grid" x1={padL} x2={w - padR} y1={y(t)} y2={y(t)} />
				<text class="axis" x={padL - 8} y={y(t) + 4} text-anchor="end">{tickBtc(t)}</text>
			{/each}
			<path class="line" class:down={trend < 0} d={line} data-testid="reserve-line" />
			{#if trend !== 0}<path class="trend" class:down={trend < 0} d={arrow(x(n - 1), y(vals[n - 1]), trend < 0)} data-testid="reserve-trend" data-trend={trend < 0 ? 'down' : 'up'} />{/if}
			{#each edgeTicks(n) as i (i)}
				<line class="tick" x1={x(i)} x2={x(i)} y1={H - 4} y2={H + 2} />
				<text class="axis sub" x={x(i)} y={H + AX - 4} text-anchor={tickAnchor(i, n)}>{heightLabel(points[i].height)}</text>
			{/each}
			{#if hover !== null}<circle class="dot" cx={x(hover)} cy={y(vals[hover])} r="3.5" />{/if}
		</svg>
	{/snippet}
	{#snippet tip(k: number)}
		<b>after {heightLabel(points[k].height)}</b>
		<span><i class="key line" style:background={trend < 0 ? COLOR.out : undefined}></i><em>{btc(points[k].balance)}</em> BTC</span>
	{/snippet}
</ChartFrame>
