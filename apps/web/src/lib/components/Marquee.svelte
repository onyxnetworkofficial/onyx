<script lang="ts">
	// Endless strip of blocks (Magic UI "marquee" idea). Paused on hover.
	import type { PillState } from './Pill.svelte';
	import { group, short } from '#lib/format.ts';
	import { motion } from '#lib/motion.svelte.ts';
	import ArrowLeft from '@lucide/svelte/icons/arrow-left';
	import ArrowRight from '@lucide/svelte/icons/arrow-right';
	let { items, states = {} }: { items: { height: number; hash: string; txs: number }[]; states?: Record<number, PillState> } = $props();
	// W12 round 9: with motion off (reduced motion, or the header's pause button) the strip stood
	// still and clipped, so every block past the sixth (at 1,280 px; the second at 390) could not be
	// seen at all. Still, it is one row that scrolls sideways (swipe, trackpad, or the arrow, Home and
	// End keys once it has focus), snapping to whole blocks; an edge fades only where more are hidden.
	// Phones and tablets (up to 860 px): the blocks wrap into a grid, two a row on a phone, so no card
	// is cut in half at the panel's edge; nothing moves there, with motion on or off. A phone shows the
	// first PHONE_MAX blocks and a button for the rest.
	const mq = (q: string) => typeof matchMedia === 'function' && matchMedia(q).matches;
	let narrow = $state(mq('(max-width: 860px)'));
	let phone = $state(mq('(max-width: 560px)'));
	let all = $state(false);
	const PHONE_MAX = 8;
	const shown = $derived(phone && !all ? items.slice(0, PHONE_MAX) : items);
	$effect(() => {
		const a = matchMedia('(max-width: 860px)');
		const b = matchMedia('(max-width: 560px)');
		const f = () => {
			narrow = a.matches;
			phone = b.matches;
		};
		f();
		a.addEventListener('change', f);
		b.addEventListener('change', f);
		return () => {
			a.removeEventListener('change', f);
			b.removeEventListener('change', f);
		};
	});
	let box = $state<HTMLElement>();
	let less = $state(false);
	let more = $state(false);
	function edges() {
		if (!box) return;
		less = box.scrollLeft > 1;
		more = box.scrollLeft + box.clientWidth < box.scrollWidth - 1;
	}
	// Keys on the focused row: the arrows step one block, Home and End go to either end (a browser's
	// own arrow step is 40 px, two dozen presses for one screen of blocks).
	function keys(e: KeyboardEvent) {
		// Review: modified keys stay the browser's (Alt+Left is Back, Ctrl+Home the page top).
		if (!box || !motion.off || e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
		const blk = box.querySelector('.blk');
		const step = blk ? blk.getBoundingClientRect().width + parseFloat(getComputedStyle(blk.parentElement!).columnGap || '0') : 160;
		const to = { ArrowRight: box.scrollLeft + step, ArrowLeft: box.scrollLeft - step, Home: 0, End: box.scrollWidth }[e.key];
		if (to === undefined) return;
		e.preventDefault();
		box.scrollTo({ left: to });
	}
	// 2026-10-29 review: the still row's arrows, so the cut last card reads as "more this way".
	function page(dir: 1 | -1) {
		if (!box) return;
		box.scrollBy({ left: dir * box.clientWidth * 0.8 });
	}
	$effect(() => {
		if (!box) return;
		// Review: back to the start when motion comes back on (a scrolled row would slide offset).
		if (!motion.off) {
			box.scrollLeft = 0;
			return;
		}
		edges();
		// The row and its blocks: a block arriving changes the scroll width, not the row's size.
		const ro = new ResizeObserver(edges);
		ro.observe(box);
		if (box.firstElementChild) ro.observe(box.firstElementChild);
		return () => ro.disconnect();
	});
</script>

<!-- W3: one list of blocks for screen readers (the second copy only makes the loop seamless);
     each block's check state in words, not only its border colour. It stops with the header's
     pause button or reduced motion (app.css), and on hover. -->
{#snippet card(b: { height: number; hash: string; txs: number }, copy: number)}
	{@const st = states[b.height] ?? 'idle'}
	<div class="blk {st}" role={copy === 0 ? 'listitem' : undefined}>
		<span class="h">#{group(b.height)}</span>
		<!-- One truncation for every block hash on the Explorer (4 + 6, the input blocks too): it fits a phone grid card whole. -->
		<span class="x" title={b.hash}>{short(b.hash, 4, 6)}</span>
		<span class="t">{group(b.txs)} tx</span>
		{#if st !== 'idle'}<span class="sr-only">{st === 'good' ? 'header passes' : st === 'bad' ? 'header fails' : 'checking'}</span>{/if}
	</div>
{/snippet}
{#if narrow}
<div class="strip">
	<div class="bgrid" role="list" aria-label="Live blocks" data-testid="live-strip">
		{#each shown as b (b.height)}{@render card(b, 0)}{/each}
	</div>
	{#if phone && items.length > PHONE_MAX}
		<button type="button" class="ghost allbtn" aria-expanded={all} onclick={() => (all = !all)}>{all ? 'Show fewer blocks' : `Show all ${items.length} blocks`}</button>
	{/if}
</div>
{:else}
<div class="strip">
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div
	class="marquee"
	class:still={motion.off}
	class:less={motion.off && less}
	class:more={motion.off && more}
	bind:this={box}
	onscroll={edges}
	onkeydown={keys}
	tabindex={motion.off ? 0 : undefined}
	role={motion.off ? 'region' : undefined}
	aria-label={motion.off ? 'Live blocks' : undefined}
	data-testid="live-strip"
>
	<div class="track">
		{#each [0, 1] as copy (copy)}
			<div class="group" role={copy === 0 ? 'list' : undefined} aria-label={copy === 0 && !motion.off ? 'Live blocks' : undefined} aria-hidden={copy === 1 ? 'true' : undefined}>
				{#each items as b (b.height)}{@render card(b, copy)}{/each}
			</div>
		{/each}
	</div>
</div>
{#if motion.off}
	<!-- Mouse users' cue and control; keyboard users scroll the focused row itself (arrows, Home, End). -->
	{#if less}<button type="button" class="nudge prev" tabindex="-1" aria-label="Earlier blocks" onclick={() => page(-1)}><ArrowLeft size={18} /></button>{/if}
	{#if more}<button type="button" class="nudge next" tabindex="-1" aria-label="More blocks" onclick={() => page(1)}><ArrowRight size={18} /></button>{/if}
{/if}
</div>
{/if}

<style>
	/* Narrow screens: a grid of whole cards (two a row on a phone). */
	.bgrid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
		gap: var(--sp-3);
	}
	@media (max-width: 560px) {
		.bgrid {
			grid-template-columns: repeat(2, minmax(0, 1fr));
			gap: var(--sp-2);
		}
	}
	.bgrid .blk {
		min-width: 0;
	}
	.bgrid .blk > span {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.allbtn {
		margin-top: var(--sp-3);
	}
	.marquee {
		overflow: hidden;
		mask-image: linear-gradient(90deg, transparent, #000 8%, #000 92%, transparent);
	}
	/* Still: a row that scrolls sideways and snaps to whole blocks; the loop's second copy goes
	   (nothing moves, so nothing needs to join up), the first block is not faded, and an edge fades
	   only while blocks are hidden past it (the cue that the row scrolls). The focus ring is the
	   wrapper's, since the fade would mask a ring drawn on the row itself. */
	.strip {
		position: relative;
		border-radius: var(--radius-md);
	}
	/* A round arrow over the faded edge: the cut card is the row continuing, not a layout slip. */
	.nudge {
		position: absolute;
		top: calc(50% - var(--sp-3) / 2);
		translate: 0 -50%;
		display: grid;
		place-items: center;
		width: var(--sp-8);
		height: var(--sp-8);
		padding: 0;
		border-radius: var(--radius-pill);
		border: 1px solid var(--line-bright);
		background: var(--panel-2);
		color: var(--ink);
		font-family: var(--mono);
		font-size: var(--text-sm);
		box-shadow: var(--shadow);
		cursor: pointer;
	}
	.nudge:hover {
		border-color: var(--ink);
	}
	.nudge.next {
		right: 0;
	}
	.nudge.prev {
		left: 0;
	}
	.marquee.still {
		overflow-x: auto;
		overscroll-behavior-x: contain;
		scroll-snap-type: x mandatory;
		scroll-padding-inline: 0;
		scrollbar-width: thin;
		scrollbar-color: var(--line-bright) transparent;
		padding-bottom: var(--sp-3);
		mask-image: none;
	}
	.marquee.still.more {
		mask-image: linear-gradient(90deg, #000 calc(100% - 160px), transparent);
	}
	.marquee.still.less {
		mask-image: linear-gradient(90deg, transparent, #000 160px);
	}
	.marquee.still.less.more {
		mask-image: linear-gradient(90deg, transparent, #000 160px, #000 calc(100% - 160px), transparent);
	}
	.marquee.still:focus-visible {
		outline: none;
	}
	.strip:has(> .marquee.still:focus-visible) {
		outline: 2px solid var(--accent);
		outline-offset: 4px;
	}
	.still .group[aria-hidden='true'] {
		display: none;
	}
	.still .group {
		padding-right: 0;
	}
	.still .blk {
		scroll-snap-align: start;
	}
	.track {
		display: flex;
		width: max-content;
	}
	@media (prefers-reduced-motion: no-preference) {
		.track {
			animation: slide 60s linear infinite;
		}
		.marquee:hover .track {
			animation-play-state: paused;
		}
	}
	@keyframes slide {
		to {
			transform: translateX(-50%);
		}
	}
	.group {
		display: flex;
		gap: var(--sp-3);
		padding-right: var(--sp-3);
	}
	/* NL15 review: the block's position holds its screen-reader text (absolute, app.css .sr-only)
	   inside the scrolling row; unpositioned, it escaped the row and widened the page after a check. */
	.blk {
		position: relative;
		display: grid;
		gap: var(--sp-0);
		min-width: 150px;
		padding: var(--sp-3) var(--sp-3);
		border-radius: var(--radius-md);
		border: 1px solid var(--line);
		background: var(--panel);
		font-family: var(--mono);
		font-size: var(--text-xs);
	}
	.blk .h {
		font-size: var(--text-sm);
		color: var(--ink);
	}
	.blk .x,
	.blk .t {
		color: var(--muted);
	}
	.blk.good {
		border-color: var(--good);
		box-shadow: var(--shadow-underline-good);
	}
	.blk.bad {
		border-color: var(--bad);
	}
	.blk.run {
		border-color: var(--accent);
	}
</style>
