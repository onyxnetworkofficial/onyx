<script lang="ts">
	// Blocks -> recipe -> root, with real values. The seal lights up when the
	// browser's rerun matches the pinned root.
	import Blocks from '@lucide/svelte/icons/blocks';
	import Cpu from '@lucide/svelte/icons/cpu';
	import Check from '@lucide/svelte/icons/check';
	import X from '@lucide/svelte/icons/x';
	import type { PillState } from './Pill.svelte';
	import { group } from '#lib/format.ts';
	import { motion } from '#lib/motion.svelte.ts';
	let {
		blocks,
		recipe,
		root,
		status = 'idle'
	}: { blocks: { height: number; hash: string }[]; recipe: string; root: string; status?: PillState } = $props();

	// A slight 3D tilt toward the pointer.
	let rx = $state(0);
	let ry = $state(0);
	function tilt(e: PointerEvent) {
		if (e.pointerType !== 'mouse' || motion.off) return;
		const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
		ry = ((e.clientX - r.left) / r.width - 0.5) * 10;
		rx = -((e.clientY - r.top) / r.height - 0.5) * 8;
	}
</script>

<div class="viz" role="presentation" onpointermove={tilt} onpointerleave={() => ((rx = 0), (ry = 0))}
	style="transform: perspective(900px) rotateX({rx}deg) rotateY({ry}deg)">
	<div class="stack">
		{#each blocks as b, i (b.height)}
			<div class="block" style="--i: {i}">
				<Blocks size={18} />
				<span class="ht">#{group(b.height)}</span>
				<span class="hx">{b.hash.slice(0, 4)}…{b.hash.slice(-8)}</span>
			</div>
		{/each}
	</div>
	<div class="link" aria-hidden="true"><span></span></div>
	<div class="chip">
		<Cpu size={18} />
		<span>Recipe <em>{recipe.slice(0, 8)}…</em></span>
	</div>
	<div class="link" aria-hidden="true"><span style="animation-delay: 0.8s"></span></div>
	<div class="seal {status}">
		<span class="ring">
			{#if status === 'good'}<Check size={22} />{:else if status === 'bad'}<X size={22} />{:else}<span class="dot"></span>{/if}
		</span>
		<span class="lbl">Root</span>
		<span class="rt">{root.slice(0, 10)}…{root.slice(-6)}</span>
		<!-- The ring's icon and colour, in words (W3). -->
		<span class="sr-only">Root check: {status === 'good' ? 'passed, the pinned root' : status === 'bad' ? 'failed, not the pinned root' : status === 'run' ? 'checking' : 'not run yet'}</span>
	</div>
</div>

<style>
	.viz {
		display: grid;
		justify-items: center;
		gap: 0;
		padding: var(--sp-8) var(--sp-7);
		border-radius: var(--radius-lg);
		border: 1px solid var(--slab-line);
		background: linear-gradient(160deg, rgb(255 255 255 / 0.06), rgb(255 255 255 / 0.01));
		backdrop-filter: blur(10px);
		box-shadow: var(--shadow-pop);
		transition: transform var(--dur-slow) var(--ease);
		will-change: transform;
		font-family: var(--mono);
		font-size: var(--text-sm);
		color: var(--slab-ink);
	}
	/* W12 round 8: the drawing fills its 400 px column at 13 px (it was a 300 px stack of 12 px
	   text beside an 80 px title): taller blocks, longer links, a larger seal. */
	.stack {
		display: grid;
		gap: var(--sp-3);
		width: 100%;
	}
	.block {
		display: flex;
		align-items: center;
		gap: var(--sp-3);
		padding: var(--sp-3) var(--sp-4);
		border-radius: var(--radius-md);
		border: 1px solid var(--slab-line);
		background: var(--code-bg);
		margin-left: calc(var(--i) * var(--sp-4));
		margin-right: calc((2 - var(--i)) * var(--sp-4));
		color: var(--slab-accent);
	}
	.ht {
		color: var(--slab-ink);
	}
	.hx {
		margin-left: auto;
		color: var(--slab-muted);
		white-space: nowrap;
	}
	/* The column narrows to 300 px at 861-1100 px: less side padding and a smaller step between the
	   blocks, so each block's height and hash stay on one line. */
	@media (max-width: 1100px) {
		.viz {
			padding-inline: var(--sp-5);
		}
		.block {
			padding-inline: var(--sp-3);
			margin-left: calc(var(--i) * var(--sp-2));
			margin-right: calc((2 - var(--i)) * var(--sp-2));
		}
	}
	.link {
		position: relative;
		width: 2px;
		height: 40px;
		background: var(--slab-line);
		overflow: hidden;
	}
	.link span {
		position: absolute;
		inset: 0;
		background: linear-gradient(180deg, transparent, var(--slab-accent), transparent);
		transform: translateY(-100%);
	}
	@media (prefers-reduced-motion: no-preference) {
		.link span {
			animation: down 1.6s ease-in-out infinite;
		}
	}
	@keyframes down {
		to {
			transform: translateY(100%);
		}
	}
	.chip {
		display: inline-flex;
		align-items: center;
		gap: var(--sp-2);
		padding: var(--sp-3) var(--sp-5);
		border-radius: var(--radius-pill);
		border: 1px solid var(--accent-line);
		background: var(--accent-soft);
		color: var(--slab-accent);
		/* Round 8 review: the global .chip is 12 px; the drawing's text is 13 px throughout. */
		font-size: inherit;
	}
	.chip span {
		color: var(--slab-ink);
	}
	.chip em {
		font-style: normal;
		color: var(--slab-muted);
	}
	.seal {
		display: grid;
		justify-items: center;
		gap: var(--sp-2);
	}
	.ring {
		width: 76px;
		height: 76px;
		border-radius: 50%;
		display: grid;
		place-items: center;
		border: 2px solid var(--slab-line);
		color: var(--slab-ink);
		transition: all var(--dur-slow) var(--ease);
	}
	.seal.good .ring {
		border-color: var(--good);
		color: var(--good);
		box-shadow: var(--shadow-glow-good);
	}
	.seal.bad .ring {
		border-color: var(--bad);
		color: var(--bad);
	}
	.seal.run .ring {
		border-color: var(--slab-accent);
	}
	.dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: currentColor;
		opacity: 0.6;
	}
	@media (prefers-reduced-motion: no-preference) {
		.seal.run .dot {
			animation: pulse 0.9s ease-in-out infinite;
		}
	}
	@keyframes pulse {
		50% {
			opacity: 0.1;
		}
	}
	.lbl {
		font-family: var(--body);
		font-size: var(--text-xs);
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--slab-muted);
	}
	.rt {
		color: var(--slab-ink);
	}
</style>
