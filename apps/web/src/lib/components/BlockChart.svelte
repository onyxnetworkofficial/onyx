<script lang="ts">
	// Live Bitcoin blocks, one column each: transactions (bars), then the gap
	// since the previous block's header time, then each header's
	// proof-of-work result. Two strips with their own axes, never a dual axis.
	// Style, keys, tooltip and table: ChartFrame and src/lib/chart.ts (W6).
	import type { LiveBlock } from '#lib/data.ts';
	import type { PillState } from '#lib/components/Pill.svelte';
	import { btc, int } from '#lib/format.ts';
	import { nice, utc } from '#lib/live.ts';
	import { barPath, barWidth, edgeTicks, gutterFor, heightLabel, padFor, tickAnchor } from '#lib/chart.ts';
	import ChartFrame from './ChartFrame.svelte';

	let { blocks, pow = {}, compact = false }: { blocks: LiveBlock[]; pow?: Record<number, PillState>; compact?: boolean } = $props();

	let w = $state(0);
	let hover = $state<number | null>(null);

	const n = $derived(blocks.length);
	const txMax = $derived(nice(Math.max(1, ...blocks.map((b) => b.txs))));
	const txTicks = $derived([0, txMax / 2, txMax]);

	// Minutes since the previous header. Can be negative: header times may run
	// up to two hours ahead, so a later block can carry an earlier time.
	const gaps = $derived(blocks.map((b, i) => (i > 0 && b.time && blocks[i - 1].time ? (b.time - blocks[i - 1].time!) / 60 : null)));
	const gMax = $derived(nice(Math.max(1, ...gaps.map((g) => g ?? 0))));
	const gMin = $derived(Math.min(0, ...gaps.map((g) => g ?? 0)) < 0 ? -nice(-Math.min(...gaps.map((g) => g ?? 0))) : 0);
	const gTicks = $derived([gMin, 0, gMax].filter((t, k, a) => a.indexOf(t) === k));
	const minus = (t: number) => `${t < 0 ? '−' : ''}${int(Math.abs(t))}`;

	const padL = $derived(compact ? 0 : padFor([...txTicks.map(int), ...gTicks.map(minus)]));
	// Compact (Home): no left axis; the scale (top value and zero) sits in a right gutter.
	const padR = $derived(compact ? gutterFor([int(txMax)]) : 4);
	const H1 = $derived(compact ? 76 : 172);
	const H2 = 84;
	const AX = 30;
	const band = $derived(Math.max(0, (w - padL - padR) / Math.max(1, n)));
	const bw = $derived(barWidth(band));
	const cx = (i: number) => padL + band * i + band / 2;
	const ty = (v: number) => 8 + (H1 - 8) * (1 - v / txMax);
	const gy = (v: number) => 6 + (H2 - 12) * ((gMax - v) / (gMax - gMin));

	const fmtGap = (g: number) => `${g < 0 ? '−' : ''}${Math.abs(g).toFixed(1)} min`;
	const label = $derived(
		n === 0
			? 'No blocks to draw.'
			: `${n} Bitcoin blocks, ${blocks[0].height}–${blocks[n - 1].height}: ${int(blocks.reduce((a, b) => a + b.txs, 0))} transactions, between ${int(Math.min(...blocks.map((b) => b.txs)))} and ${int(Math.max(...blocks.map((b) => b.txs)))} per block.`
	);
	// W3: the same numbers as the drawing and the tooltip.
	const table = $derived({
		caption: label,
		testid: 'blocks-table',
		columns: [{ label: 'Block' }, { label: 'Time (UTC)' }, { label: 'Transactions', right: true }, { label: 'BTC in outputs', right: true }, { label: 'After the previous', right: true }, { label: 'Proof of work' }],
		rows: blocks.map((b, i) => [
			int(b.height),
			b.time ? utc(b.time) : '–',
			int(b.txs),
			b.output_sats ? btc(b.output_sats) : '–',
			gaps[i] === null ? '–' : fmtGap(gaps[i]!),
			pow[b.height] === 'good' ? 'Passes' : pow[b.height] === 'bad' ? 'Fails' : 'Not checked'
		])
	});
	const index = (x: number) => Math.floor((x - padL) / Math.max(1, band));
</script>

<ChartFrame
	{label}
	{n}
	step="blocks"
	{compact}
	height={compact ? H1 + 2 : 22 + H1 + 2 + 22 + H2 + AX}
	bind:w
	bind:hover
	{index}
	at={hover === null ? null : cx(hover)}
	{band}
	tipTop={compact ? 0 : 22}
	{table}
	empty="No blocks in this range."
>
	{#snippet plot()}
		{#if !compact}<div class="chart-label" style:padding-left="{padL}px">Transactions per block</div>{/if}
		<svg width={w} height={H1 + 2} aria-hidden="true">
			{#if !compact}
				{#each txTicks as t (t)}
					<line class="grid" class:zero={t === 0} x1={padL} x2={w - padR} y1={ty(t)} y2={ty(t)} />
					<text class="axis" x={padL - 8} y={ty(t) + 4} text-anchor="end">{int(t)}</text>
				{/each}
			{:else}
				{#each [txMax, 0] as t (t)}
					<line class="grid" class:zero={t === 0} x1="0" x2={w - padR} y1={ty(t)} y2={ty(t)} />
					<text class="axis" x={w - padR + 6} y={ty(t) + 4}>{int(t)}</text>
				{/each}
			{/if}
			{#each blocks as b, i (b.height)}
				<path class="mark in tx" class:dim={hover !== null && hover !== i} d={barPath(cx(i), H1, ty(b.txs), bw)} />
			{/each}
		</svg>

		{#if !compact}
			<div class="chart-label" style:padding-left="{padL}px">Minutes since the previous block</div>
			<svg width={w} height={H2} aria-hidden="true">
				{#each gTicks as t (t)}
					<line class="grid" class:zero={t === 0} x1={padL} x2={w - padR} y1={gy(t)} y2={gy(t)} />
					{#if t === 0 || Math.abs(gy(t) - gy(0)) >= 12}<text class="axis" x={padL - 8} y={gy(t) + 4} text-anchor="end">{minus(t)}</text>{/if}
				{/each}
				{#each gaps as g, i (blocks[i].height)}
					{#if g !== null}
						<path class="mark gap" class:pos={g >= 0} class:neg={g < 0} class:dim={hover !== null && hover !== i} d={barPath(cx(i), gy(0), gy(g), bw)} />
					{/if}
				{/each}
			</svg>

			<!-- x axis: each header's proof-of-work result, then the first, middle and last height. -->
			<svg width={w} height={AX} aria-hidden="true">
				{#each blocks as b, i (b.height)}
					<rect class="pow {pow[b.height] ?? 'idle'}" x={cx(i) - 3} y="4" width="6" height="6" rx="1.5" />
				{/each}
				{#each edgeTicks(n) as i (i)}
					<text class="axis" x={cx(i)} y="26" text-anchor={tickAnchor(i, n)}>{heightLabel(blocks[i].height)}</text>
				{/each}
			</svg>
		{/if}
	{/snippet}
	{#snippet tip(k: number)}
		{@const b = blocks[k]}
		<b>{heightLabel(b.height)}</b>
		{#if b.time}<span>{utc(b.time)} UTC</span>{/if}
		<span><em>{int(b.txs)}</em> transactions</span>
		{#if b.output_sats}<span><em>{btc(b.output_sats).split('.')[0]}</em> BTC in outputs</span>{/if}
		{#if gaps[k] !== null}<span><em>{fmtGap(gaps[k]!)}</em> after the previous{gaps[k]! < 0 ? ' (earlier clock)' : ''}</span>{/if}
		{#if pow[b.height] === 'good'}<span class="ok">Proof of work passes</span>{:else if pow[b.height] === 'bad'}<span class="no">Header fails</span>{/if}
	{/snippet}
</ChartFrame>

<style>
	/* Proof-of-work marks: the status colours the Pill uses. */
	.pow {
		fill: var(--line-strong);
	}
	.pow.good {
		fill: var(--good);
	}
	.pow.bad {
		fill: var(--bad);
	}
	.pow.run {
		fill: var(--muted);
	}
</style>
