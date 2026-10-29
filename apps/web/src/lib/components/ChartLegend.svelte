<script lang="ts">
	// One legend style for every chart (W6): a swatch per series (bar square, line dash or
	// hatch, in the chart's own token), the unit at the end, then any controls (scale, range).
	// The swatches repeat what the chart's name and data table already say, so they are hidden
	// from screen readers; the unit and controls are not.
	import type { Snippet } from 'svelte';
	import type { LegendItem } from '#lib/chart.ts';
	let { items, unit, testid, children }: { items: LegendItem[]; unit?: string; testid?: string; children?: Snippet } = $props();
</script>

<div class="chart-legend" data-testid={testid}>
	{#each items as it (it.label)}
		<span aria-hidden="true"><i class="key {it.kind ?? 'bar'}" style:background={it.kind === 'hatch' ? undefined : it.color}></i>{it.label}</span>
	{/each}
	{#if unit}<span class="unit">{unit}</span>{/if}
	{#if children}<span class="controls">{@render children()}</span>{/if}
</div>
