<script lang="ts">
	// Share of labels per group (kind of company).
	import { group } from '#lib/format.ts';
	let { parts }: { parts: { label: string; value: number }[] } = $props();
	const colors = ['var(--accent)', 'var(--btc)', 'var(--muted)'];
	const total = $derived(parts.reduce((a, p) => a + p.value, 0) || 1);
	const R = 40;
	const C = 2 * Math.PI * R;
	const arcs = $derived.by(() => {
		let off = 0;
		return parts.map((p, i) => {
			const len = (p.value / total) * C;
			const a = { ...p, len, off, color: colors[i % colors.length] };
			off += len;
			return a;
		});
	});
</script>

<div class="donut">
	<svg viewBox="0 0 100 100" width="132" height="132" role="img" aria-label="Labels per kind of company">
		{#each arcs as a (a.label)}
			<circle cx="50" cy="50" r={R} fill="none" stroke={a.color} stroke-width="14"
				stroke-dasharray="{a.len} {C}" stroke-dashoffset={-a.off} transform="rotate(-90 50 50)" />
		{/each}
		<text x="50" y="49" text-anchor="middle" style:font-family="var(--mono)" style:font-size="var(--text-sm)" style:fill="var(--ink)">{group(total)}</text>
		<text x="50" y="65" text-anchor="middle" style:font-family="var(--body)" style:font-size="var(--text-xs)" style:fill="var(--muted)">labels</text>
	</svg>
	<ul>
		{#each arcs as a (a.label)}
			<li><i style="background: {a.color}"></i><span>{a.label}</span><b>{group(a.value)}</b></li>
		{/each}
	</ul>
</div>

<style>
	.donut {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-4);
		align-items: center;
	}
	ul {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		gap: var(--sp-2);
	}
	li {
		display: flex;
		gap: var(--sp-2);
		align-items: center;
		font-size: var(--text-sm);
	}
	li i {
		width: 10px;
		height: 10px;
		border-radius: var(--radius-xs);
	}
	li b {
		font-family: var(--mono);
		font-weight: 500;
	}
</style>
