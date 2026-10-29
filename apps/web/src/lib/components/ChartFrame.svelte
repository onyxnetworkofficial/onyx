<script lang="ts">
	// The frame every chart shares (W6): one Tab stop with a name; Left/Right/Home/End move the
	// tooltip, Esc hides it (W3); the pointer picks the nearest point; one highlight band, one
	// tooltip style, one empty state; the data table folded underneath (built when opened).
	// Compact (inside a link on the overview): one image with a summary, pointer tooltip only.
	import type { Snippet } from 'svelte';
	import { stepKey } from '#lib/chartkeys.ts';
	import { TIP_W } from '#lib/chart.ts';
	import DataTable from './DataTable.svelte';

	type Table = { caption: string; columns: { label: string; right?: boolean }[]; rows: (string | number)[][]; testid?: string };
	let {
		label,
		n,
		step = 'points',
		compact = false,
		height,
		w = $bindable(0),
		hover = $bindable(null),
		index,
		at = null,
		band = 0,
		tipTop = 4,
		table,
		empty = 'Nothing to draw in this range.',
		plot,
		tip
	}: {
		/** What the chart shows, in a sentence (its accessible name and the table caption). */
		label: string;
		n: number;
		/** What the arrow keys step through, for the name ("blocks", "days", ...). */
		step?: string;
		compact?: boolean;
		/** Height the drawing takes, kept while the width is measured. */
		height: number;
		w?: number;
		hover?: number | null;
		/** Point under the pointer, from its offset in the frame. */
		index: (x: number, y: number) => number | null;
		/** Horizontal centre of the hovered point (tooltip and highlight), or null. */
		at?: number | null;
		/** Width of the highlight band around `at` (0: none). */
		band?: number;
		tipTop?: number;
		table?: Table;
		empty?: string;
		plot: Snippet;
		tip?: Snippet<[number]>;
	} = $props();

	function key(e: KeyboardEvent) {
		const k = stepKey(e, hover, n);
		if (k !== undefined) hover = k;
	}
	function pick(e: PointerEvent) {
		if (n === 0) return;
		const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
		const i = index(e.clientX - r.left, e.clientY - r.top);
		hover = i === null ? null : Math.max(0, Math.min(n - 1, i));
	}
	// Beside the point, inside the chart; flips left near the right edge.
	let fh = $state(0);
	let th = $state(0);
	// Never past the chart's bottom (W11: the pool tooltip on the last days ran over the edge).
	const top = $derived(Math.max(0, Math.min(tipTop, fh - th)));
	const left = $derived(at === null ? 0 : at + band / 2 + 6 + TIP_W <= w ? at + band / 2 + 6 : Math.max(0, at - band / 2 - 6 - TIP_W));
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<div
	class="chart"
	class:compact
	bind:clientWidth={w}
	bind:clientHeight={fh}
	role={compact ? 'img' : 'group'}
	aria-label={compact || n === 0 ? label : `${label} Left and Right arrow keys step through the ${step}; the data table below lists every value.`}
	tabindex={compact || n === 0 ? undefined : 0}
	onkeydown={compact ? undefined : key}
	onblur={() => (hover = null)}
	onpointermove={pick}
	onpointerdown={pick}
	onpointerleave={() => (hover = null)}
>
	{#if n === 0}
		<p class="chart-empty" data-testid="chart-empty">{empty}</p>
	{:else if w > 0}
		{#if hover !== null && at !== null && band > 0}
			<i class="hl" style:left="{at - band / 2}px" style:width="{band}px" aria-hidden="true"></i>
		{/if}
		{@render plot()}
		{#if hover !== null && tip}
			<div class="tip" style:left="{left}px" style:top="{top}px" style:width="{TIP_W}px" role="status" bind:offsetHeight={th}>{@render tip(hover)}</div>
		{/if}
	{:else}
		<div style:height="{height}px"></div>
	{/if}
</div>
{#if !compact && table && n > 0}
	<DataTable {...table} />
{/if}
