<script lang="ts">
	// PRODUCT-PLAN.md U5 (pre-public loop PP4): the datasets first, each with its proof level,
	// source, unit and rights; the section below it holds the checked values and the proof.
	import { DATASETS, dayText, parseFreshness, type Freshness } from '#lib/catalog.ts';
	import { FALLBACK } from '#lib/mode.ts';
	import LevelBadge from './LevelBadge.svelte';
	import { onMount } from 'svelte';
	let note = $state('');
	// PP11: each card's newest date, from the shipped data files (scripts/gen-freshness.py).
	let fresh = $state<Freshness>({});
	let freshTried = $state(false);
	onMount(() => {
		fetch('data/freshness.json')
			.then((r) => (r.ok ? r.json() : null))
			.then((v) => (fresh = parseFreshness(v)))
			.catch(() => {})
			.finally(() => (freshTried = true));
	});
	function show(section: string) {
		const el = document.querySelector<HTMLElement>(`[data-testid="${section}"]`);
		if (!el) {
			note = 'That section is not in this build.';
			return;
		}
		note = '';
		el.scrollIntoView({ behavior: matchMedia('(prefers-reduced-motion: reduce)').matches ? 'auto' : 'smooth', block: 'start' });
		// Move focus with the view, so a keyboard or screen reader lands on the section too.
		const h = el.querySelector<HTMLElement>('h2');
		if (h) {
			h.tabIndex = -1;
			h.focus({ preventScroll: true });
		}
	}
</script>

<section class="section" data-testid="catalog" aria-labelledby="catalog-heading">
	<h2 id="catalog-heading">Datasets</h2>
	<p class="lede" data-testid="catalog-lede">Every dataset on this site, with the proof level it states. Open one to see its values and check them in your browser.</p>
	<ul class="cards">
		{#each DATASETS as d (d.id)}
			{@const held = FALLBACK && d.labelDerived}
			<li class="card" data-testid="catalog-item" data-id={d.id}>
				<div class="top">
					<h3>{d.title}</h3>
					<LevelBadge level={d.level} />
				</div>
				<p>{d.what}</p>
				<dl>
					<dt>Source</dt><dd>{d.source}</dd>
					<dt>Unit</dt><dd>{d.unit}</dd>
					<dt>Rights</dt><dd>{d.rights}</dd>
					{#if !d.noDate}
						<dt>Data as of</dt>
						<dd data-testid="catalog-asof">{#if fresh[d.id]}{dayText(fresh[d.id].asOf)} <span class="what">({fresh[d.id].what})</span>{:else if freshTried}Not given{:else}<span aria-hidden="true">–</span><span class="sr-only">Loading</span>{/if}</dd>
					{/if}
					{#if d.api}<dt>API</dt><dd>Query API{#if d.apiWhere}, {d.apiWhere}{/if}</dd>{/if}
				</dl>
				{#if held}
					<p class="fine" data-testid="catalog-held">Held back in this build until the label rights are cleared.</p>
				{:else if d.page}
					<a class="open" href={d.page}>Open on the Reserves page</a>
				{:else if d.section}
					<button type="button" class="ghost open" onclick={() => show(d.section!)}>Show the values</button>
				{/if}
			</li>
		{/each}
	</ul>
	{#if note}<p class="fine" role="status">{note}</p>{/if}
</section>

<style>
	.what {
		color: var(--muted);
	}
	.cards {
		list-style: none;
		margin: var(--sp-4) 0 0;
		padding: 0;
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(min(100%, 300px), 1fr));
		gap: var(--sp-3);
	}
	.card {
		display: flex;
		flex-direction: column;
		gap: var(--sp-2);
		border: 1px solid var(--line);
		border-radius: var(--radius-md);
		background: var(--panel);
		padding: var(--sp-4);
		min-width: 0;
	}
	/* Owner review: the level badge always takes its own line under the title. */
	.top {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: var(--sp-2);
	}
	.card h3 {
		margin: 0;
	}
	.card p {
		margin: 0;
		font-size: var(--text-sm);
		line-height: var(--lh-body);
		color: var(--muted);
	}
	dl {
		display: grid;
		grid-template-columns: max-content minmax(0, 1fr);
		gap: var(--sp-1) var(--sp-3);
		margin: 0;
		font-size: var(--text-sm);
	}
	dt {
		color: var(--muted);
	}
	dd {
		margin: 0;
		overflow-wrap: anywhere;
		min-width: 0;
	}
	.open {
		margin-top: auto;
		align-self: flex-start;
	}
	/* A phone target like every button (44 px, W2). */
	a.open {
		display: inline-flex;
		align-items: center;
		min-height: 44px;
		min-width: 44px;
	}
</style>
