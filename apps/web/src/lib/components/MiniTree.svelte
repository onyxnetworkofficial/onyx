<script lang="ts">
	// A Merkle tree over `leaves` rows with one proof path lit: the row, its
	// sibling hashes (amber) and the root. Odd nodes are promoted (ADR-0003).
	let { leaves = 8, lit = 0, broken = false, labels = false }: { leaves?: number; lit?: number; broken?: boolean; labels?: boolean } = $props();
	const W = 240;
	const levels = $derived.by(() => {
		const l = [Math.max(1, leaves)];
		while (l[l.length - 1] > 1) l.push(Math.ceil(l[l.length - 1] / 2));
		return l;
	});
	const top = $derived(levels.length - 1);
	const H = $derived(28 + top * 30 + (labels ? 18 : 0));
	const x = (lvl: number, i: number) => ((i + 0.5) * W) / levels[lvl];
	const y = (lvl: number) => H - 14 - (labels ? 18 : 0) - lvl * 30 + (labels ? 4 : 0);
	const onPath = (lvl: number, i: number) => i === lit >> lvl;
	const sibling = (lvl: number, i: number) => lvl < top && i === ((lit >> lvl) ^ 1);
	const color = (lvl: number, i: number) =>
		onPath(lvl, i) ? (broken ? 'var(--bad)' : 'var(--accent)') : sibling(lvl, i) ? 'var(--btc)' : 'var(--panel-solid)';
</script>

<svg viewBox="0 0 {W} {H}" width="100%" role="img" aria-label="Merkle proof over {leaves} rows: the row, its sibling hashes and the root">
	{#each levels.slice(0, top) as n, lvl (lvl)}
		{#each Array(n) as _, i (i)}
			<line x1={x(lvl, i)} y1={y(lvl)} x2={x(lvl + 1, i >> 1)} y2={y(lvl + 1)}
				style:stroke={onPath(lvl, i) ? (broken ? 'var(--bad)' : 'var(--accent)') : 'var(--line)'} stroke-width={onPath(lvl, i) ? 2 : 1} />
		{/each}
	{/each}
	{#each levels as n, lvl (lvl)}
		{#each Array(n) as _, i (i)}
			<circle cx={x(lvl, i)} cy={y(lvl)} r={lvl === top ? 9 : 6} style:fill={color(lvl, i)}
				style:stroke={onPath(lvl, i) || sibling(lvl, i) ? 'none' : 'var(--line)'} />
		{/each}
	{/each}
	{#if labels}
		<text x={x(0, lit)} y={y(0) + 22} text-anchor="middle" class="tl">row</text>
		{#each levels.slice(0, top) as n, lvl (lvl)}
			{#if ((lit >> lvl) ^ 1) < n}
				<text x={x(lvl, (lit >> lvl) ^ 1)} y={y(lvl) + 22} text-anchor="middle" class="tl">sibling</text>
			{/if}
		{/each}
		<text x={x(top, 0) + 16} y={y(top) + 4} class="tl">root</text>
	{/if}
</svg>

<style>
	.tl {
		font-family: var(--mono);
		font-size: var(--text-xs);
		fill: var(--muted);
	}
</style>
