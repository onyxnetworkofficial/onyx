<script lang="ts">
	// PRODUCT-PLAN.md U5 (pre-public loop PP4): the datasets first. The section opens with the way
	// to every dataset the node holds (a search and one button, both to All datasets); below it the
	// worked examples, each with its proof level, source, unit and rights; the sections after it
	// hold their checked values and the proof.
	import { DATASETS, dayText, groupDecimal, nodeSubjects, observation, observationUrl, parseFreshness, readsFromNode, type Dataset, type Freshness } from '#lib/catalog.ts';
	import { MAX_SEARCH, cleanSearch, countUrl, page as readPage, toLink } from '#lib/datasets.ts';
	import { group } from '#lib/format.ts';
	import { loadLadders } from '#lib/ladder.ts';
	import { FALLBACK } from '#lib/mode.ts';
	import LevelBadge from './LevelBadge.svelte';
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	let note = $state('');
	// PP11: each card's newest date, from the shipped data files (scripts/gen-freshness.py).
	let fresh = $state<Freshness>({});
	let freshTried = $state(false);
	// The search goes to All datasets, which asks the node; the count is the node's own total.
	let query = $state('');
	let all = $state<number | null>(null);
	// A card without a section here: its settled round on the hosted node (metric -> subject), and
	// the node's answer once the visitor asks for it.
	let subjects = $state<Record<string, string>>({});
	let read = $state<Record<string, { value: string; level: number } | 'loading' | 'none'>>({});
	let origin = $state('');
	onMount(() => {
		origin = location.origin;
		fetch('data/freshness.json')
			.then((r) => (r.ok ? r.json() : null))
			.then((v) => (fresh = parseFreshness(v)))
			.catch(() => {})
			.finally(() => (freshTried = true));
		fetch(countUrl(), { headers: { accept: 'application/json' } })
			.then((r) => (r.ok && (r.headers.get('content-type') ?? '').includes('json') ? r.json() : null))
			.then((v) => (all = readPage(v)?.total || null))
			.catch(() => {});
		loadLadders()
			.then((f) => (subjects = nodeSubjects(f)))
			.catch(() => {});
	});
	function search(e: SubmitEvent) {
		e.preventDefault();
		void goto(toLink(cleanSearch(query)));
	}
	async function readValue(d: Dataset) {
		const subject = subjects[d.id];
		if (!d.api || !subject || read[d.id] === 'loading') return;
		read[d.id] = 'loading';
		try {
			const r = await fetch(observationUrl(d.api, subject), { headers: { accept: 'application/json' } });
			read[d.id] = (r.ok && (r.headers.get('content-type') ?? '').includes('json') ? observation(await r.json(), d.id, subject) : null) ?? 'none';
		} catch {
			read[d.id] = 'none';
		}
	}
	const dayOf = (subject: string) => dayText(subject.slice(subject.lastIndexOf(':') + 1));
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
	<p class="lede">Every dataset this site’s node holds is on one list. Search it by name or meaning, or browse it all. Each dataset opens to its newest rows.</p>
	<form class="find" role="search" onsubmit={search} data-testid="catalog-search">
		<label for="catalog-q" class="muted">Names or meanings, for example <span class="mono">bitcoin holders</span>, <span class="mono">whale</span> or <span class="mono">eth supply</span></label>
		<div class="findrow">
			<input id="catalog-q" type="search" bind:value={query} maxlength={MAX_SEARCH} spellcheck="false" autocomplete="off" placeholder={all !== null ? `Search ${group(all)} datasets` : 'Search datasets'} data-testid="catalog-q" />
			<button type="submit" class="ghost" data-testid="catalog-go">Search</button>
		</div>
	</form>
	<a class="btn" href={resolve('/datasets')} data-testid="catalog-all">{all !== null ? `Browse all ${group(all)} datasets` : 'Browse all datasets'}</a>

	<h3 class="sub" id="catalog-examples">Worked examples</h3>
	<p class="lede" data-testid="catalog-lede">{DATASETS.length} datasets worked through on this page, each with the proof level it states. Open one to see its values and check them in your browser.</p>
	<ul class="cards" aria-labelledby="catalog-examples">
		{#each DATASETS as d (d.id)}
			{@const held = FALLBACK && d.labelDerived}
			<li class="card" data-testid="catalog-item" data-id={d.id}>
				<div class="top">
					<h4>{d.title}</h4>
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
				{:else if readsFromNode(d)}
					{@const subject = subjects[d.id]}
					{@const v = read[d.id]}
					<div class="node" data-testid="catalog-node">
						{#if !subject}
							<p class="fine" data-testid="catalog-node-how">This page holds no values for this dataset. The <a href={resolve('/api')}>API page</a> says how to read them from the node.</p>
						{:else if v && typeof v === 'object'}
							<p class="got" role="status" data-testid="catalog-node-value"><span class="mono">{groupDecimal(v.value)}</span> {d.unit} · {dayOf(subject)}</p>
							<p class="fine">The node’s answer for that day, at level {v.level}. Your browser did not check it. <a href={observationUrl(d.api!, subject)} target="_blank" rel="noopener" data-testid="catalog-node-full">The full answer</a> holds every observer’s signed reading.</p>
						{:else if v === 'none'}
							<p class="fine" role="status" data-testid="catalog-node-none">The node did not answer here. Read the value for {dayOf(subject)} yourself:</p>
							<pre class="code" data-testid="catalog-node-curl">curl -s "{origin}{observationUrl(d.api!, subject)}"</pre>
						{:else}
							<button type="button" class="ghost open" disabled={v === 'loading'} onclick={() => readValue(d)} data-testid="catalog-node-read">{v === 'loading' ? 'Asking the node' : 'Show the value'}</button>
						{/if}
					</div>
				{/if}
			</li>
		{/each}
	</ul>
	{#if note}<p class="fine" role="status">{note}</p>{/if}
</section>

<style>
	/* The way to the full list: the search field of All datasets, then one button. */
	.find {
		display: grid;
		gap: var(--sp-2);
		margin-block: var(--sp-3);
		max-width: 36rem;
	}
	.findrow {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-2);
	}
	.findrow input {
		flex: 1 1 14rem;
		min-width: 0;
		min-height: 44px;
	}
	.findrow button {
		min-height: 44px;
	}
	/* One button, as wide as its words (the section is a grid: it would stretch). */
	a.btn {
		justify-self: start;
		min-height: 44px;
	}
	.sub {
		margin-top: var(--sp-6);
	}
	.node .open {
		justify-self: start;
	}
	.node {
		margin-top: auto;
		display: grid;
		gap: var(--sp-2);
		min-width: 0;
	}
	.card .got {
		font-size: var(--text-base);
		color: var(--ink);
	}
	.node pre.code {
		margin: 0;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		font-size: var(--text-xs);
	}
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
	/* The card titles sit one level under "Worked examples" and keep the look they had as h3. */
	.card h4 {
		margin: 0;
		font-size: var(--text-base);
		font-weight: 600;
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
