<script lang="ts">
	// Per-day share of blocks by mining pool, as stacked bars. Rows come from
	// the verifier's checked leaves only (verify/src/lib.rs `check_pools`).
	// Style, keys, tooltip and table: ChartFrame and src/lib/chart.ts (W6); the
	// arrow keys step through the days.
	type Row = { pool: string; blocks: number };
	type Day = { date: string; from: number; to: number; rows: Row[] };
	import { COLOR } from '#lib/chart.ts';
	import ChartFrame from './ChartFrame.svelte';
	let { days, named }: { days: Day[]; named: string[] } = $props();

	let w = $state(0);
	let h = $state(0);
	let hover = $state<number | null>(null);
	const n = $derived(days.length);
	const OTHER = 'Other listed pools';

	const color = (key: string) => {
		const i = named.indexOf(key);
		return i >= 0 ? COLOR.cat(i) : key === OTHER ? COLOR.other : undefined;
	};
	const name = (pool: string) => (pool === 'unknown' ? 'Unknown (not credited to a reviewed pool)' : pool);

	/** A day's segments in legend order: named pools, other listed pools, unknown. */
	function segments(d: Day) {
		const total = d.rows.reduce((a, r) => a + r.blocks, 0);
		const by = new Map(d.rows.map((r) => [r.pool, r.blocks]));
		const other = d.rows.filter((r) => !named.includes(r.pool) && r.pool !== 'unknown').reduce((a, r) => a + r.blocks, 0);
		const list = [
			...named.map((p) => ({ key: p, blocks: by.get(p) ?? 0 })),
			{ key: OTHER, blocks: other },
			{ key: 'unknown', blocks: by.get('unknown') ?? 0 }
		].filter((s) => s.blocks > 0);
		return { total, list };
	}
	const segs = $derived(days.map(segments));
	const pct = (k: number, t: number) => ((k * 100) / t).toFixed(1);
	const label = $derived(n === 0 ? 'No days to draw.' : `Blocks per mining pool, per UTC day, ${days[0].date} to ${days[n - 1].date}.`);
	// W3: every day's counts, as in each bar's label and the tooltip.
	const table = $derived({
		caption: 'Blocks per mining pool, per UTC day',
		testid: 'pool-table',
		columns: [{ label: 'Day (UTC)' }, ...named.map((p) => ({ label: p, right: true })), { label: OTHER, right: true }, { label: 'Unknown', right: true }, { label: 'Blocks', right: true }],
		rows: days.map((d, i) => {
			const of = (k: string) => segs[i].list.find((x) => x.key === k)?.blocks ?? 0;
			return [d.date, ...named.map(of), of(OTHER), of('unknown'), segs[i].total];
		})
	});
	// Rows are evenly spaced (12 px gap, --sp-3), so the row under the pointer is its share of the
	// height. W11: 30 px a row leaves room for the day tooltip (at most 9 lines) inside the chart.
	const GAP = 12;
	const index = (_x: number, y: number) => Math.floor((y / Math.max(1, h + GAP)) * n);
	const rowTop = (i: number) => ((h + GAP) / Math.max(1, n)) * i;
</script>

<ChartFrame {label} {n} step="days" height={n * 30 - GAP} bind:w bind:hover {index} at={hover === null ? null : w} tipTop={hover === null ? 0 : rowTop(hover) + 24} {table} empty="No days in this range.">
	{#snippet plot()}
		<div class="shares" data-testid="pool-shares" bind:clientHeight={h}>
			{#each days as d, i (d.date)}
				{@const s = segs[i]}
				<div class="day" class:on={hover === i}>
					<span class="axis-html mono">{d.date.slice(5)}</span>
					<div class="bar" aria-hidden="true">
						{#each s.list as seg (seg.key)}
							<span class="seg mark" class:hatch={seg.key === 'unknown'} class:dim={hover !== null && hover !== i} style:flex-grow={seg.blocks} style:background={color(seg.key)}></span>
						{/each}
					</div>
					<span class="axis-html mono r">{s.total}</span>
				</div>
			{/each}
		</div>
	{/snippet}
	{#snippet tip(k: number)}
		{@const s = segs[k]}
		<span><b>{days[k].date} UTC</b> <em>{s.total}</em> blocks</span>
		{#each s.list as seg (seg.key)}
			<span><i class="key" class:hatch={seg.key === 'unknown'} style:background={color(seg.key)}></i>{seg.key === 'unknown' ? 'Unknown' : seg.key} <em>{seg.blocks}</em> ({pct(seg.blocks, s.total)}%)</span>
		{/each}
	{/snippet}
</ChartFrame>

<style>
	.shares {
		display: grid;
		gap: var(--sp-3);
	}
	.day {
		display: grid;
		grid-template-columns: 44px minmax(0, 1fr) 34px;
		align-items: center;
		gap: var(--sp-3);
		border-radius: var(--radius-sm);
	}
	.day.on {
		background: var(--accent-soft);
	}
	/* The x-axis text style of the drawn charts, on HTML labels. */
	.axis-html {
		font-size: var(--text-xs);
		color: var(--muted);
		font-variant-numeric: tabular-nums;
	}
	.r {
		text-align: right;
	}
	.bar {
		display: flex;
		height: 18px;
		border-radius: var(--radius-xs);
		overflow: hidden;
		gap: var(--sp-0);
		background: var(--line);
	}
	.seg {
		min-width: 2px;
		flex-basis: 0;
	}
</style>
