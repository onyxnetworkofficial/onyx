<script lang="ts">
	// Five roles joined by moving beams (Magic UI "animated beam" idea).
	import type { Component } from 'svelte';
	type Step = { icon: Component; title: string; note: string; when?: string };
	let { steps }: { steps: Step[] } = $props();
</script>

<ol class="pipe">
	{#each steps as s, i (s.title)}
		<li class="node">
			<span class="icon"><s.icon size={22} /></span>
			<b class="ititle">{s.title}</b>
			<span class="note">{s.note}</span>
			{#if s.when}<span class="when">{s.when}</span>{/if}
		</li>
		{#if i < steps.length - 1}
			<li class="beam" aria-hidden="true"><span style="animation-delay: {i * 0.45}s"></span></li>
		{/if}
	{/each}
</ol>

<style>
	.pipe {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		grid-template-columns: repeat(4, minmax(0, 1fr) 40px) minmax(0, 1fr);
		align-items: start;
	}
	.node {
		display: grid;
		justify-items: center;
		text-align: center;
		gap: var(--sp-2);
		min-width: 0;
	}
	.icon {
		width: 56px;
		height: 56px;
		/* W12 round 10: an icon tile inside a panel takes the inner radius and no shadow of its own,
		   like the proof levels' and Explorer's icon tiles (it was 18 px with a drop shadow). */
		border-radius: var(--radius-md);
		display: grid;
		place-items: center;
		background: var(--panel);
		border: 1px solid var(--line);
		color: var(--accent);
	}
	/* W12 round 13: a note takes its column's width (it was capped at 18ch, 140 px in a 166 px column,
	   so "Rerun a sample; cheaters lose their bond" ran to three lines at 1,000-1,600 px), its lines
	   balanced. */
	.note {
		font-size: var(--text-sm);
		color: var(--muted);
		text-wrap: balance;
	}
	/* W12 round 14: at 761-999 px five equal columns are 102-145 px, so "Rerun a sample; cheaters lose
	   their bond" ran to three lines (16 px beams would only fix it from 861 px, round 13). Here each
	   role takes a width in proportion to its text (a flex row, each node's basis its one-line width,
	   shrinking by that basis), the beams keep 40 px: the long notes get the room the short ones do not
	   need, and every note sits on two lines. From 1,000 px the even grid stays. */
	@media (min-width: 761px) and (max-width: 999px) {
		.pipe {
			display: flex;
		}
		.node {
			flex: 1 1 auto;
		}
		.beam {
			flex: 0 0 40px;
		}
	}
	.when {
		font-family: var(--mono);
		font-size: var(--text-xs);
		color: var(--warn);
	}
	.beam {
		position: relative;
		height: 2px;
		margin-top: var(--sp-6);
		background: var(--line);
		overflow: hidden;
		border-radius: var(--radius-xs);
	}
	.beam span {
		position: absolute;
		inset: 0;
		background: linear-gradient(90deg, transparent, var(--accent), transparent);
		transform: translateX(-100%);
	}
	@media (prefers-reduced-motion: no-preference) {
		.beam span {
			animation: run 2.2s ease-in-out infinite;
		}
	}
	@keyframes run {
		to {
			transform: translateX(100%);
		}
	}
	@media (max-width: 760px) {
		/* Phones: a compact vertical list, icon left, text right. */
		.pipe {
			grid-template-columns: minmax(0, 1fr);
		}
		.node {
			grid-template-columns: 44px minmax(0, 1fr);
			justify-items: start;
			text-align: left;
			column-gap: var(--sp-4);
			row-gap: 0;
		}
		.icon {
			grid-row: span 3;
			width: 44px;
			height: 44px;
			border-radius: var(--radius-md);
		}
		.note {
			max-width: none;
		}
		.beam {
			width: 2px;
			height: 16px;
			margin: var(--sp-1) 0 var(--sp-1) var(--sp-5);
		}
		.beam span {
			background: linear-gradient(180deg, transparent, var(--accent), transparent);
			transform: translateY(-100%);
		}
		@media (prefers-reduced-motion: no-preference) {
			.beam span {
				animation-name: down;
			}
		}
	}
	@keyframes down {
		to {
			transform: translateY(100%);
		}
	}
</style>
