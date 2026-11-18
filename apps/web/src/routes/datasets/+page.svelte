<script lang="ts">
	// ROADMAP S22: every bulk dataset the node holds, searched and paged by the node
	// (`GET /v1/bulk?q=&limit=&after=`, 50 a page). A dataset opens to its newest rows (one rows
	// answer, with its proofs) and the two commands that check them against Base Sepolia. Every value
	// comes through #lib/datasets.ts, which drops anything out of shape.
	import { onMount } from 'svelte';
	import { replaceState } from '$app/navigation';
	import OnyxSlab from '#lib/components/OnyxSlab.svelte';
	import Pill, { type PillState } from '#lib/components/Pill.svelte';
	import DatasetChart from '#lib/components/DatasetChart.svelte';
	import { group } from '#lib/format.ts';
	import { datasetTitle, meanings, split } from '#lib/glossary.ts';
	import { MAX_SEARCH, STALE_DAYS, staleSince, chartColumns, chartPoints, cleanSearch, dayKeys, detailUrl, fromLink, isoDay, toLink, keyRange, keyText, infoUrl, listUrl, moved as readMoved, page as readPage, sample as readSample, size, type Dataset, type Moved, type Sample } from '#lib/datasets.ts';

	let query = $state('');
	// READINESS A20: only the datasets with data from the last STALE_DAYS days (the node's `fresh`).
	let freshOnly = $state(false);
	// Review FRESH-R1 M2: said when the node does not take the filter (an older node answers 400).
	let filterOff = $state(false);
	let items = $state<Dataset[]>([]);
	let total = $state<number | null>(null);
	let all = $state<number | null>(null);
	let next = $state<string | null>(null);
	let pill = $state<{ state: PillState; text: string }>({ state: 'run', text: 'Asking the node' });
	let busy = $state(false);
	let none = $state(false);
	let open = $state<string | null>(null);
	let samples = $state<Record<string, Sample | 'none' | 'loading'>>({});
	// The column each open dataset's chart draws (an index into its sample's columns).
	let drawn = $state<Record<string, number>>({});
	let origin = $state('');
	// A dataset link whose dataset this node does not hold (the search shows the closest names).
	let missing = $state<string | null>(null);
	// R-BULK-17: what the node's 410 says about a linked dataset its publisher removed: the id it
	// moved to (opened when live) or a successor that is not live (said, nothing opened).
	let gone = $state<{ from: string } & Moved | null>(null);
	let seq = 0;
	let timer: ReturnType<typeof setTimeout> | undefined;

	const idOf = (d: Dataset) => `${d.publisher}/${d.dataset}`;

	async function load(q: string, after: string | null) {
		const my = ++seq;
		busy = true;
		if (!after) pill = { state: 'run', text: 'Asking the node' };
		try {
			const r = await fetch(listUrl(q, after, undefined, freshOnly), { headers: { accept: 'application/json' } });
			if (my !== seq) return;
			if (r.status === 429) {
				pill = { state: 'warn', text: 'Busy: try again in a minute' };
				return;
			}
			const p = r.ok && (r.headers.get('content-type') ?? '').includes('json') ? readPage(await r.json()) : null;
			if (my !== seq) return;
			if (!p && freshOnly) {
				// The filtered list failed (a node without `fresh` refuses the parameter): that says
				// nothing about the node's datasets. Say the filter is off and list all of them.
				freshOnly = false;
				filterOff = true;
				return void load(q, null);
			}
			if (!p) {
				none = true;
				pill = { state: 'idle', text: 'No datasets here' };
				return;
			}
			none = false;
			items = after ? [...items, ...p.items] : p.items;
			total = p.total;
			next = p.next;
			if (!q && !freshOnly) all = p.total;
			pill = { state: 'good', text: `${group(p.total)} ${p.total === 1 ? 'dataset' : 'datasets'}${q ? ' found' : ''}` };
		} catch {
			if (my === seq) {
				none = true;
				pill = { state: 'idle', text: 'No datasets here' };
			}
		} finally {
			if (my === seq) busy = false;
		}
	}

	// The address bar keeps the search and the open dataset, so a link shows the same view.
	function remember() {
		try {
			replaceState(toLink(cleanSearch(query), open), {});
		} catch {
			// Before the router is ready: the next change writes it.
		}
	}

	function search() {
		clearTimeout(timer);
		timer = setTimeout(() => {
			open = null;
			missing = null;
			gone = null;
			remember();
			load(cleanSearch(query), null);
		}, 350);
	}

	function show(fresh: boolean) {
		if (freshOnly === fresh) return;
		freshOnly = fresh;
		filterOff = false;
		clearTimeout(timer);
		open = null;
		missing = null;
		gone = null;
		remember();
		load(cleanSearch(query), null);
	}

	async function toggle(d: Dataset) {
		const id = idOf(d);
		if (open === id) {
			open = null;
			remember();
			return;
		}
		open = id;
		remember();
		if (samples[id] && samples[id] !== 'none') return;
		samples[id] = 'loading';
		try {
			const r = await fetch(detailUrl(d), { headers: { accept: 'application/json' } });
			const s = (r.ok ? readSample(await r.json()) : null) ?? 'none';
			samples[id] = s;
			if (s !== 'none') drawn[id] = chartColumns(s)[0] ?? -1;
		} catch {
			samples[id] = 'none';
		}
	}

	// A linked dataset that is not in the list: the node's own answer about it says whether its
	// publisher removed it and which dataset it named in its place.
	async function successor(pair: string): Promise<Moved | null> {
		try {
			const r = await fetch(infoUrl(pair), { headers: { accept: 'application/json' } });
			return r.status === 410 && (r.headers.get('content-type') ?? '').includes('json') ? readMoved(r.status, await r.json(), pair) : null;
		} catch {
			return null;
		}
	}

	onMount(() => {
		origin = location.origin;
		const link = fromLink(location.hash);
		// A dataset link searches for its name and opens it.
		query = link.q || (link.d ? cleanSearch(link.d.split('/')[1]) : '');
		load(cleanSearch(query), null).then(async () => {
			const d = link.d ? items.find((x) => idOf(x) === link.d) : undefined;
			if (d) return void toggle(d);
			if (!link.d || none) return;
			const m = await successor(link.d);
			if (!m) return void (missing = link.d);
			gone = { from: link.d, ...m };
			if (!m.live || !m.to) return;
			// The dataset moved: search for the id it moved to and open that one.
			query = cleanSearch(m.to.split('/')[1]);
			await load(cleanSearch(query), null);
			const to = items.find((x) => idOf(x) === m.to);
			if (to) await toggle(to);
			else gone = { from: link.d, to: m.to, live: false };
		});
		return () => clearTimeout(timer);
	});

	const curl = (d: Dataset, s: Sample) => `curl -s "${origin}/v1/bulk/${d.publisher}/${d.dataset}/rows?from=${s.from}&to=${s.to}&limit=100" > rows.json`;
	const check = (d: Dataset, s: Sample) =>
		`onyx-check --bulk rows.json --publisher ${d.publisher} --dataset ${d.dataset} --from ${s.from} --to ${s.to} --rpc https://sepolia.base.org --chain-id 84532 --book ${s.book}`;
</script>

<svelte:head><title>All datasets · Onyx</title></svelte:head>

<OnyxSlab>
	<h1>All datasets</h1>
	<div class="slabside">
		<p class="slablede">Every dataset this node holds, to search and read. Each version is signed by its publisher and anchored on Base Sepolia, and every row comes with its proof.</p>
		<p class="meta slabmeta">Test network only · <span class="nw">each dataset under its own license</span></p>
	</div>
</OnyxSlab>

<div class="wrap">
	<section class="section" data-testid="datasets">
		<div class="sh">
			<h2>Search</h2>
			<Pill live state={pill.state} text={pill.text} />
		</div>
		<div class="find">
			<label for="ds-q" class="muted">Names or meanings, for example <span class="mono">bitcoin holders</span>, <span class="mono">whale</span> or <span class="mono">eth supply</span></label>
			<input id="ds-q" type="search" bind:value={query} oninput={search} maxlength={MAX_SEARCH} spellcheck="false" autocomplete="off" placeholder={all !== null ? `Search ${group(all)} datasets` : 'Search datasets'} data-testid="datasets-q" />
			<div class="seg" role="group" aria-label="Which datasets to list">
				<button type="button" class="chipbtn" aria-pressed={!freshOnly} onclick={() => show(false)} data-testid="datasets-all">All</button>
				<button type="button" class="chipbtn" aria-pressed={freshOnly} onclick={() => show(true)} data-testid="datasets-fresh">Updated in the last {STALE_DAYS} days</button>
			</div>
		</div>

		{#if filterOff}
			<p class="muted" role="status" data-testid="datasets-filter-off">The {STALE_DAYS}-day filter is not available on this node right now. All datasets are listed.</p>
		{/if}
		{#if gone}
			<p class="muted" role="status" data-testid="datasets-moved" data-live={gone.live}>
				{#if gone.live && gone.to}
					This dataset moved to <span class="mono">{gone.to}</span>. It is open below. The old name <span class="mono">{gone.from}</span> was removed by its publisher.
				{:else if gone.to}
					The publisher removed <span class="mono">{gone.from}</span> and named <span class="mono">{gone.to}</span> in its place. This node does not serve that dataset now, so nothing is opened.
				{:else}
					The publisher removed <span class="mono">{gone.from}</span>. No dataset that this node serves is named in its place.
				{/if}
			</p>
		{/if}
		{#if missing}
			<p class="muted" data-testid="datasets-missing">This node holds no dataset <span class="mono">{missing}</span>. {total ? 'The closest names are below.' : ''}</p>
		{/if}
		{#if none}
			<p class="muted" data-testid="datasets-none">This site’s node shows no datasets.</p>
		{:else if items.length}
			<ul class="list" aria-label="Datasets" data-testid="datasets-list">
				{#each items as d (idOf(d))}
					{@const id = idOf(d)}
					{@const since = staleSince(d)}
					<li class="panel item" data-testid="datasets-row">
						<button type="button" class="name" aria-expanded={open === id} aria-controls="ds-{id}" onclick={() => toggle(d)} data-testid="datasets-open">
							<span class="ttl" data-testid="datasets-title">{datasetTitle(d.dataset)}</span>
							<span class="muted pub"><span class="mono" data-testid="datasets-id">{d.dataset}</span> · {d.publisher} · version {d.version}</span>
						</button>
						{#each meanings(split(d.dataset).words) as m (m)}<p class="about" data-testid="datasets-about">{m}</p>{/each}
						<dl class="dsfacts">
							<div><dt>Rows</dt><dd class="mono">{group(d.rows)}</dd></div>
							<div><dt>Covers</dt><dd class="mono">{keyRange(d.keyMin, d.keyMax)}</dd></div>
							<div><dt>Size</dt><dd class="mono">{size(d.bytes)}</dd></div>
							<div><dt>Stored</dt><dd class="mono">{d.stored} of {d.shards} {d.shards === 1 ? 'shard' : 'shards'}</dd></div>
							{#if d.newest}<div><dt>Newest day</dt><dd class="mono" data-testid="datasets-newest">{d.newest}</dd></div>{/if}
							{#if isoDay(d.created)}<div><dt>Version made</dt><dd class="mono" data-testid="datasets-updated">{isoDay(d.created)}</dd></div>{/if}
						</dl>
						{#if since}<p class="stale" data-testid="datasets-stale"><span class="stalemark" aria-hidden="true">!</span><span>No data after <span class="mono">{since}</span></span></p>{/if}
						{#if d.future}<p class="stale" data-testid="datasets-future"><span class="stalemark" aria-hidden="true">!</span><span>Dates in the future: the newest day is not known</span></p>{/if}
						{#if open === id}
							{@const s = samples[id]}
							<div class="more" id="ds-{id}" data-testid="datasets-detail">
								{#if s === 'loading' || s === undefined}
									<p class="muted">Reading the newest rows…</p>
								{:else if s === 'none'}
									<p class="muted" data-testid="datasets-detail-none">The node did not answer with rows. Try again in a minute.</p>
								{:else}
									{@const cols = chartColumns(s)}
									{@const days = dayKeys(d.keyMin, d.keyMax)}
									{#if cols.length && (drawn[id] ?? -1) >= 0}
										<div class="chartrow">
											{#if cols.length > 1}
												<label class="muted">Chart <select bind:value={drawn[id]} data-testid="datasets-chart-col">{#each cols as c (c)}<option value={c}>{s.columns[c]}</option>{/each}</select></label>
											{:else}
												<span class="muted">Chart: <span class="mono">{s.columns[cols[0]]}</span></span>
											{/if}
										</div>
										<div class="panel chartbox"><DatasetChart points={chartPoints(s, drawn[id], days)} name={s.columns[drawn[id]]} testid="datasets-chart" /></div>
									{/if}
									{@const shown = s.rows.slice(-10)}
									{#if since}<p class="fine" data-testid="datasets-detail-stale">This dataset has no data after {since}: the chart and the rows below end there, and are not current values.</p>{/if}
									<p class="fine">The newest {shown.length} {shown.length === 1 ? 'row' : 'rows'}{d.license ? ` · license ${d.license}` : ''} · <a href={toLink('', id)} data-testid="datasets-link">link to this dataset</a></p>
									<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
									<div class="tablebox sample" tabindex="0" role="region" aria-label="Newest rows of {d.dataset}">
										<table data-testid="datasets-sample">
											<thead><tr>{#each s.columns as c, i (c)}<th class:r={i > 0}>{c}</th>{/each}</tr></thead>
											<tbody>
												{#each shown as r, ri (ri)}
													<tr>{#each r as v, i (i)}<td class="mono" class:r={i > 0}>{i === 0 ? keyText(v, dayKeys(d.keyMin, d.keyMax)) : v}</td>{/each}</tr>
												{/each}
											</tbody>
										</table>
									</div>
									{#if s.book}
										<p class="fine">Check these rows yourself: the first command saves them with their proofs, the second checks every proof against the signed book on Base Sepolia.</p>
										<pre class="code" data-testid="datasets-commands">{curl(d, s)}
{check(d, s)}</pre>
									{/if}
								{/if}
							</div>
						{/if}
					</li>
				{/each}
			</ul>
			<div class="pager">
				<span class="muted" data-testid="datasets-count">Showing {group(items.length)} of {group(total ?? items.length)}</span>
				{#if next}
					<button type="button" class="btn ghost" disabled={busy} onclick={() => load(cleanSearch(query), next)} data-testid="datasets-more">Show more</button>
				{/if}
			</div>
		{:else if total === 0}
			<p class="muted" data-testid="datasets-empty">{freshOnly ? `No dataset${cleanSearch(query) ? ' with every word of this search in its name' : ''} has data from the last ${STALE_DAYS} days.` : 'No dataset has every word of this search in its name.'}</p>
		{/if}
	</section>
</div>

<style>
	.find {
		display: grid;
		gap: var(--sp-2);
		margin-block: var(--sp-3) var(--sp-4);
		max-width: 36rem;
	}
	.find input {
		width: 100%;
	}
	.find .seg {
		justify-self: start;
		flex-wrap: wrap;
		max-width: 100%;
	}
	/* No data in 30 days, or dates in the future: said in words, with a mark and a dashed frame (never colour alone). */
	.stale {
		display: inline-flex;
		align-items: baseline;
		gap: var(--sp-2);
		justify-self: start;
		max-width: 100%;
		margin: 0;
		padding: var(--sp-1) var(--sp-2);
		border: 1px dashed var(--warn);
		border-radius: var(--radius-sm);
		font-size: var(--text-sm);
		color: var(--ink);
		overflow-wrap: anywhere;
	}
	.stalemark {
		font-family: var(--mono);
		font-weight: 700;
		color: var(--warn);
	}
	/* The dataset's name opens it: a plain link-coloured line, not a pill button. */
	.name {
		display: grid;
		gap: 2px;
		justify-content: start;
		justify-items: start;
		width: 100%;
		text-align: left;
		background: none;
		border: 0;
		border-radius: var(--radius-sm);
		box-shadow: none;
		padding: 0;
		color: var(--accent);
		cursor: pointer;
		font: inherit;
	}
	.name:hover:not(:disabled) {
		transform: none;
		box-shadow: none;
	}
	.name:hover .ttl {
		text-decoration: underline;
	}
	.ttl {
		font-weight: 500;
		overflow-wrap: anywhere;
	}
	.name .mono {
		overflow-wrap: anywhere;
		white-space: normal;
	}
	.name[aria-expanded='true'] .ttl {
		text-decoration: underline;
	}
	.about {
		margin: 0;
		font-size: var(--text-sm);
		color: var(--muted);
		max-width: 70ch;
	}
	.pub {
		font-size: var(--text-xs);
	}
	.list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		gap: var(--sp-2);
	}
	.item {
		display: grid;
		gap: var(--sp-2);
		min-width: 0;
	}
	.dsfacts {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-1) var(--sp-4);
		margin: 0;
		font-size: var(--text-sm);
	}
	.dsfacts div {
		display: flex;
		gap: var(--sp-1);
	}
	.dsfacts dt {
		color: var(--muted);
	}
	.dsfacts dd {
		margin: 0;
	}
	.more {
		min-width: 0;
		border-top: 1px solid var(--line);
		padding-top: var(--sp-2);
	}
	.sample {
		max-width: 100%;
		margin-block: var(--sp-2) var(--sp-3);
	}
	.sample td,
	.sample th {
		font-size: var(--text-xs);
	}
	pre.code {
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}
	.chartrow {
		margin-block: var(--sp-2);
		font-size: var(--text-sm);
	}
	.chartrow select {
		margin-left: var(--sp-1);
	}
	.chartbox {
		margin-bottom: var(--sp-3);
		padding: var(--sp-3);
	}
	.pager {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-3);
		align-items: center;
		justify-content: space-between;
		margin-top: var(--sp-3);
	}
</style>
