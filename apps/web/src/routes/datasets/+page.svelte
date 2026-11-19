<script lang="ts">
	// ROADMAP S22: every bulk dataset the node holds, searched by the node (`GET /v1/bulk?any=`). A
	// search loads its matches (up to SEARCH_CAP) and ranks them here (rankSearch: the searched
	// asset first); with no search the list opens in defaultOrder, read from the node in steps
	// (defaultSegments), 50 shown at a time. A dataset opens to its newest rows (one rows answer,
	// with its proofs), a chart, a CSV download and the commands that check the rows against Base
	// Sepolia. Every value comes through #lib/datasets.ts, which drops anything out of shape.
	import { onMount, tick } from 'svelte';
	import { afterNavigate, pushState, replaceState } from '$app/navigation';
	import OnyxSlab from '#lib/components/OnyxSlab.svelte';
	import Pill, { type PillState } from '#lib/components/Pill.svelte';
	import DatasetChart from '#lib/components/DatasetChart.svelte';
	import { group } from '#lib/format.ts';
	import { datasetTitle, meanings, split, suggestSearches, unitLegend } from '#lib/glossary.ts';
	import {
		HISTORY_CAP,
		LIST_MAX,
		MAX_SEARCH,
		PAGE,
		SEARCH_CAP,
		STALE_DAYS,
		anyUrl,
		chartColumns,
		chartPoints,
		cleanSearch,
		countUrl,
		csv,
		csvName,
		dayKeys,
		defaultSegments,
		detailUrl,
		freshCountUrl,
		freshCounts,
		freshIn,
		fromLink,
		headlineColumn,
		historyUrl,
		infoUrl,
		isTestSet,
		isoDay,
		keyRange,
		keyText,
		listUrl,
		mainPublisher,
		pageTitle,
		publisherCounts,
		sourceNote,
		textColumns,
		moved as readMoved,
		removed as readRemoved,
		newestFirst,
		page as readPage,
		pythonSnippet,
		rankSearch,
		sample as readSample,
		size,
		staleSince,
		toLink,
		type Dataset,
		type Moved,
		type Page,
		type Sample,
		type Segment
	} from '#lib/datasets.ts';

	let query = $state('');
	// READINESS A20: only the datasets with data from the last STALE_DAYS days (the node's `fresh`).
	let freshOnly = $state(false);
	// Review FRESH-R1 M2: said when the node does not take the filter (an older node answers 400).
	let filterOff = $state(false);
	// Everything loaded for this view, in the order shown; the page shows the first `shown`.
	let found = $state<Dataset[]>([]);
	let shown = $state(PAGE);
	let total = $state<number | null>(null);
	// The node's two counts: every dataset, and those with data from the last STALE_DAYS days.
	let all = $state<number | null>(null);
	let fresh = $state<number | null>(null);
	// More matches than SEARCH_CAP: only the loaded ones are ranked (said on the page).
	let capped = $state(false);
	// The search the list on screen answers (the count line speaks about it).
	let listed = $state('');
	let pill = $state<{ state: PillState; text: string }>({ state: 'run', text: 'Asking the node' });
	let busy = $state(false);
	let none = $state(false);
	let open = $state<string | null>(null);
	let samples = $state<Record<string, Sample | 'none' | 'loading'>>({});
	// The column each open dataset's chart draws (an index into its sample's columns).
	let drawn = $state<Record<string, number>>({});
	// A full-history download: rows read so far and how it ended.
	type Download = { state: 'run' | 'done' | 'capped' | 'busy' | 'fail'; rows: number };
	let history = $state<Record<string, Download>>({});
	let origin = $state('');
	// A dataset link whose dataset this node does not hold (the search shows the closest names).
	let missing = $state<string | null>(null);
	// R-BULK-17: what the node's 410 says about a linked dataset its publisher removed: the id it
	// moved to (opened when live) or a successor that is not live (said, nothing opened).
	let gone = $state<{ from: string } & Moved | null>(null);
	// A linked dataset its publisher removed with nothing named in its place (the node's 410
	// without a successor), and the catalog that removed it when the node says which.
	let ended = $state<{ from: string; catalog: number | null } | null>(null);
	// Newcomer test 3 (2026-11-18): a search that found nothing offers the nearest known words that
	// do find something ("bitcon": did you mean bitcoin?), with the node's own count.
	let suggest = $state<{ q: string; total: number } | null>(null);
	// Each publisher's dataset count, from the node's count answer (its catalogs).
	let publishers = $state.raw<{ publisher: string; datasets: number }[]>([]);
	const main = $derived(mainPublisher(publishers, all));
	// The open dataset has a history entry of its own (Back closes it).
	let pushed = false;
	// The link the page is loading now (a second signal for the same address is dropped).
	let arriving: string | null = null;
	let seq = 0;
	let timer: ReturnType<typeof setTimeout> | undefined;
	// The opening list's steps (no search), the step being read and its cursor. `plain`: a node
	// without the `fresh` filter, listed in its own order.
	let segs = $state.raw<Segment[]>([]);
	let segAt = $state(0);
	let cursor: string | null = null;
	let plain = false;

	const idOf = (d: Dataset) => `${d.publisher}/${d.dataset}`;
	const items = $derived(found.slice(0, shown));
	const counts = $derived(freshCounts(all, fresh));
	const more = $derived(shown < found.length || segAt < segs.length);

	async function ask(url: string): Promise<Page | 'busy' | null> {
		const r = await fetch(url, { headers: { accept: 'application/json' } });
		if (r.status === 429) return 'busy';
		return r.ok && (r.headers.get('content-type') ?? '').includes('json') ? readPage(await r.json()) : null;
	}

	const say = (n: number | null, q: string) => (pill = { state: 'good', text: n === null ? 'Datasets listed' : `${group(n)} ${n === 1 ? 'dataset' : 'datasets'}${q ? ' found' : ''}` });

	// The node does not take `fresh`: say so when the reader asked for the filter, and list all.
	function withoutFilter(q: string) {
		if (freshOnly) filterOff = true;
		freshOnly = false;
		plain = true;
		return load(q);
	}

	// Reads the opening list's next steps until `shown` datasets are loaded or the steps end.
	async function fill(my: number): Promise<void> {
		while (found.length < shown && segAt < segs.length) {
			const seg = segs[segAt];
			const p = await ask(anyUrl(seg.any, cursor, plain ? PAGE : seg.any === '' && !seg.fresh ? LIST_MAX : 2 * PAGE, seg.fresh));
			if (my !== seq) return;
			if (p === 'busy') return void (pill = { state: 'warn', text: 'Busy: try again in a minute' });
			if (!p && seg.fresh && !plain) return withoutFilter('');
			if (!p) {
				if (!found.length) {
					none = true;
					pill = { state: 'idle', text: 'No datasets here' };
				}
				return;
			}
			none = false;
			if (plain) total = p.total;
			found.push(...p.items.filter(seg.keep));
			cursor = p.next;
			if (!cursor) segAt++;
		}
		say(total, '');
	}

	// A search: every match (up to SEARCH_CAP), ranked here.
	async function searchFor(q: string, my: number): Promise<void> {
		const got: Dataset[] = [];
		let after: string | null = null;
		let matches = 0;
		do {
			const p = await ask(listUrl(q, after, LIST_MAX, freshOnly));
			if (my !== seq) return;
			if (p === 'busy') return void (pill = { state: 'warn', text: 'Busy: try again in a minute' });
			if (!p && freshOnly) return withoutFilter(q);
			if (!p) {
				none = true;
				pill = { state: 'idle', text: 'No datasets here' };
				return;
			}
			got.push(...p.items);
			matches = p.total;
			after = p.next;
		} while (after && got.length < SEARCH_CAP);
		none = false;
		capped = after !== null;
		found = rankSearch(got, q);
		total = matches;
		say(matches, q);
		if (matches === 0) await offer(q, my);
	}

	// Nothing found: the first of the nearest searches (#lib/glossary.ts suggestSearches) that the
	// node has matches for, asked with a list of one. Nothing is offered when none has any.
	async function offer(q: string, my: number): Promise<void> {
		for (const s of suggestSearches(q)) {
			const p = await ask(listUrl(s, null, 1, freshOnly)).catch(() => null);
			if (my !== seq || !p || p === 'busy') return;
			if (p.total > 0) return void (suggest = { q: s, total: p.total });
		}
	}

	function take(q: string) {
		clearTimeout(timer);
		query = q;
		open = null;
		pushed = false;
		missing = null;
		remember();
		void load(cleanSearch(q));
	}

	async function load(q: string) {
		const my = ++seq;
		busy = true;
		pill = { state: 'run', text: 'Asking the node' };
		found = [];
		shown = PAGE;
		capped = false;
		suggest = null;
		listed = q;
		segs = q ? [] : plain ? [{ any: '', fresh: false, keep: () => true }] : defaultSegments(freshOnly);
		segAt = 0;
		cursor = null;
		try {
			if (q) await searchFor(q, my);
			else {
				total = plain ? null : freshOnly ? fresh : all;
				await fill(my);
			}
		} catch {
			if (my === seq) {
				none = true;
				pill = { state: 'idle', text: 'No datasets here' };
			}
		} finally {
			if (my === seq) busy = false;
		}
	}

	async function showMore() {
		shown += PAGE;
		if (!segs.length || found.length >= shown) return;
		const my = seq;
		busy = true;
		try {
			await fill(my);
		} catch {
			// The rows already listed stay; the button asks again.
		} finally {
			if (my === seq) busy = false;
		}
	}

	// The node's own counts, for the line that says how many datasets are being updated.
	async function count() {
		const one = async (url: string): Promise<unknown> => {
			try {
				const r = await fetch(url, { headers: { accept: 'application/json' } });
				return r.ok && (r.headers.get('content-type') ?? '').includes('json') ? await r.json() : null;
			} catch {
				return null;
			}
		};
		const [a, f] = await Promise.all([one(countUrl()), one(freshCountUrl())]);
		[all, fresh] = [readPage(a)?.total || null, readPage(f)?.total || null];
		publishers = publisherCounts(a);
	}

	// The address bar keeps the search and the open dataset, so a link shows the same view.
	// `push`: a new history entry (a dataset opened), so Back closes it and stays on the list.
	function remember(push = false) {
		try {
			(push ? pushState : replaceState)(toLink(cleanSearch(query), open), {});
		} catch {
			// Before the router is ready: the next change writes it.
		}
	}

	function search() {
		clearTimeout(timer);
		timer = setTimeout(() => {
			open = null;
			pushed = false;
			missing = null;
			gone = null;
			ended = null;
			remember();
			load(cleanSearch(query));
		}, 350);
	}

	function show(onlyFresh: boolean) {
		if (freshOnly === onlyFresh) return;
		freshOnly = onlyFresh;
		filterOff = false;
		// Asked again: an updated node may take the filter now.
		plain = false;
		clearTimeout(timer);
		open = null;
		pushed = false;
		missing = null;
		gone = null;
		ended = null;
		remember();
		load(cleanSearch(query));
	}

	// Opening a dataset adds a history entry (newcomer test 3: Back left the list); closing it by
	// hand goes back over that entry. A dataset opened from a link, or in place of an open one,
	// rewrites the entry it is on.
	async function toggle(d: Dataset, viaLink = false) {
		const id = idOf(d);
		if (open === id) {
			open = null;
			if (pushed) {
				pushed = false;
				window.history.back();
			} else remember();
			return;
		}
		const fresh = !viaLink && !pushed;
		open = id;
		remember(fresh);
		if (fresh) pushed = true;
		if (samples[id] && samples[id] !== 'none') return;
		samples[id] = 'loading';
		try {
			const r = await fetch(detailUrl(d), { headers: { accept: 'application/json' } });
			const s = (r.ok ? readSample(await r.json()) : null) ?? 'none';
			samples[id] = s;
			if (s !== 'none') drawn[id] = headlineColumn(s, d.dataset);
		} catch {
			samples[id] = 'none';
		}
	}

	// Brings an opened card to the top of the screen (a dataset link, opened or pressed).
	async function reveal(id: string) {
		await tick();
		document.getElementById(`card-${id}`)?.scrollIntoView({ block: 'start' });
	}

	// A linked dataset: shown (past the first 50 too), opened and scrolled to.
	async function openLinked(d: Dataset) {
		shown = Math.max(shown, found.indexOf(d) + 1);
		if (open !== idOf(d)) await toggle(d, true);
		await reveal(idOf(d));
	}

	// A linked dataset that is not in the list: the node's own answer about it says whether its
	// publisher removed it and which dataset it named in its place.
	async function successor(pair: string): Promise<{ moved: Moved | null; removed: { catalog: number | null } | null }> {
		try {
			const r = await fetch(infoUrl(pair), { headers: { accept: 'application/json' } });
			if (r.status !== 410 || !(r.headers.get('content-type') ?? '').includes('json')) return { moved: null, removed: null };
			const v = await r.json();
			return { moved: readMoved(r.status, v, pair), removed: readRemoved(r.status, v) };
		} catch {
			return { moved: null, removed: null };
		}
	}

	function save(name: string, text: string) {
		const url = URL.createObjectURL(new Blob([text], { type: 'text/csv' }));
		const a = document.createElement('a');
		a.href = url;
		a.download = name;
		document.body.append(a);
		a.click();
		a.remove();
		setTimeout(() => URL.revokeObjectURL(url), 1000);
	}

	// The whole history, 1,000 rows a call (the node's limit), oldest first, up to HISTORY_CAP rows.
	async function saveHistory(d: Dataset) {
		const id = idOf(d);
		if (history[id]?.state === 'run') return;
		history[id] = { state: 'run', rows: 0 };
		const rows: string[][] = [];
		let columns: string[] = [];
		let at: number | null = 0;
		try {
			while (at !== null && rows.length < HISTORY_CAP) {
				const r = await fetch(historyUrl(d, at), { headers: { accept: 'application/json' } });
				if (r.status === 429) return void (history[id] = { state: 'busy', rows: rows.length });
				const s = r.ok ? readSample(await r.json()) : null;
				if (!s || (columns.length && s.columns.join() !== columns.join())) return void (history[id] = { state: 'fail', rows: rows.length });
				columns = s.columns;
				rows.push(...s.rows);
				at = s.nextPage;
				history[id] = { state: 'run', rows: rows.length };
			}
			save(csvName(d, true), csv(columns, rows, dayKeys(d.keyMin, d.keyMax)));
			history[id] = { state: at === null ? 'done' : 'capped', rows: rows.length };
		} catch {
			history[id] = { state: 'fail', rows: rows.length };
		}
	}

	// A page link: the search it names, then its dataset opened. A `d` that is no
	// "publisher/dataset" gets the same not-found line as an unknown id.
	async function arrive(link: ReturnType<typeof fromLink>, hash: string) {
		arriving = hash;
		clearTimeout(timer);
		open = null;
		pushed = false;
		missing = null;
		gone = null;
		ended = null;
		// A dataset link searches for its name and opens it.
		query = link.q || (link.d ? cleanSearch(link.d.split('/')[1]) : link.bad && link.bad !== '?' ? link.bad : '');
		try {
			await load(cleanSearch(query));
			if (link.bad) return void (missing = link.bad);
			const d = link.d ? found.find((x) => idOf(x) === link.d) : undefined;
			if (d) return void (await openLinked(d));
			if (!link.d || none) return;
			const { moved: m, removed } = await successor(link.d);
			if (!m) return void (removed ? (ended = { from: link.d, catalog: removed.catalog }) : (missing = link.d));
			gone = { from: link.d, ...m };
			if (!m.live || !m.to) return;
			// The dataset moved: search for the id it moved to and open that one.
			query = cleanSearch(m.to.split('/')[1]);
			await load(cleanSearch(query));
			const to = found.find((x) => idOf(x) === m.to);
			if (to) await openLinked(to);
			else gone = { from: link.d, to: m.to, live: false };
		} finally {
			if (arriving === hash) arriving = null;
		}
	}

	// The address changed under the page: Back or Forward, the header search, a dataset link.
	async function follow() {
		const hash = location.hash;
		if (!hash.startsWith('#/datasets') || hash === arriving) return;
		const link = fromLink(hash);
		pushed = false;
		if (link.d && link.d === open) return;
		const d = link.d ? found.find((x) => idOf(x) === link.d) : undefined;
		if (d) return void (await openLinked(d));
		// The same search without a dataset: only close the open one.
		if (!link.d && !link.bad && link.q === listed) return void (open = null);
		await arrive(link, hash);
	}

	afterNavigate((nav) => {
		if (nav.type !== 'enter' && nav.from?.route.id === nav.to?.route.id) void follow();
	});

	onMount(() => {
		origin = location.origin;
		const hash = location.hash;
		void count().then(() => arrive(fromLink(hash), hash));
		const back = () => void follow();
		addEventListener('popstate', back);
		return () => {
			clearTimeout(timer);
			removeEventListener('popstate', back);
		};
	});

	const curl = (d: Dataset, s: Sample) => `curl -s "${origin}/v1/bulk/${d.publisher}/${d.dataset}/rows?from=${s.from}&to=${s.to}&limit=100" > rows.json`;
	const check = (d: Dataset, s: Sample) =>
		`onyx-check --bulk rows.json --publisher ${d.publisher} --dataset ${d.dataset} --from ${s.from} --to ${s.to} --rpc https://sepolia.base.org --chain-id 84532 --book ${s.book}`;
	const SIGNED = 'The publisher signed this version with its own key. Onyx did not check the numbers.';
</script>

<svelte:head><title>{pageTitle(open)}</title></svelte:head>

<OnyxSlab>
	<h1>All datasets</h1>
	<div class="slabside">
		<p class="slablede">Every dataset this node holds, to search and read. Each version is signed by its publisher and anchored on Base Sepolia, and every row comes with its proof.</p>
		<p class="slablede proofnote" data-testid="datasets-proof-note">The proof shows these are exactly the rows the publisher signed and anchored on the test chain. It does not show that the numbers are right.</p>
		<p class="meta slabmeta">Test network only · <span class="nw">each dataset under its own license</span></p>
	</div>
</OnyxSlab>

<div class="wrap">
	<section class="section" data-testid="datasets">
		<div class="sh">
			<h2>Search</h2>
			<Pill live state={pill.state} text={pill.text} />
		</div>
		{#if counts}
			<p class="updated" data-testid="datasets-freshness"><b>{group(counts.fresh)} of {group(counts.all)}</b> datasets were updated in the last {STALE_DAYS} days. {#if counts.stale}The other {group(counts.stale)} were not: each of them shows its last day.{/if}</p>
		{/if}
		<!-- Newcomer test 3 (2026-11-18): one publisher holds nearly every dataset and its method is
		     not published; said here and on every opened card (#lib/datasets.ts sourceNote). -->
		<p class="fine where" data-testid="datasets-where">
			<b>Where these numbers come from.</b>
			{#if main}{group(main.datasets)} of {group(main.total)} datasets here come from one publisher, <span class="mono">{main.publisher}</span>.{/if}
			{sourceNote(main?.publisher ?? '').join(' ')}
		</p>
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
		{#if ended}
			<p class="muted" role="status" data-testid="datasets-removed">The publisher removed <span class="mono">{ended.from}</span>{ended.catalog !== null ? ` in its catalog ${ended.catalog}` : ''}. {total ? 'The closest names are below.' : ''}</p>
		{/if}
		{#if missing}
			<p class="muted" role="status" data-testid="datasets-missing">This node holds no dataset {#if missing === '?'}with that name{:else}<span class="mono">{missing}</span>{/if}. {total ? 'The closest names are below.' : ''}</p>
		{/if}
		{#if none}
			<p class="muted" data-testid="datasets-none">This site’s node shows no datasets.</p>
		{:else if items.length}
			<p class="fine order" data-testid="datasets-order">
				{#if listed}
					Best match first: datasets of the asset you searched come before others.{!freshOnly ? ` ${group(freshIn(found))} of ${group(found.length)} ${capped ? 'loaded' : 'found'} were updated in the last ${STALE_DAYS} days.` : ''}{capped ? ` Only the first ${group(SEARCH_CAP)} matches are loaded: add a word to narrow the search.` : ''}
				{:else if segs.length > 1}
					{freshOnly ? 'Bitcoin and Ethereum first, then by name.' : `Datasets updated in the last ${STALE_DAYS} days first (Bitcoin and Ethereum, then by name), then the rest. Test data is last.`}
				{:else}
					Listed by name.
				{/if}
			</p>
			<ul class="list" aria-label="Datasets" data-testid="datasets-list">
				{#each items as d (idOf(d))}
					{@const id = idOf(d)}
					{@const since = staleSince(d)}
					<li class="panel item" id="card-{id}" data-testid="datasets-row">
						<button type="button" class="name" aria-expanded={open === id} aria-controls="ds-{id}" onclick={() => toggle(d)} data-testid="datasets-open">
							<span class="ttl" data-testid="datasets-title">{datasetTitle(d.dataset)}</span>
							<span class="muted pub"><span class="mono" data-testid="datasets-id">{d.dataset}</span> · version {d.version}</span>
						</button>
						{#if isTestSet(d)}<p class="chip testtag" data-testid="datasets-test">test data</p>{/if}
						{#each meanings(split(d.dataset).words) as m (m)}<p class="about" data-testid="datasets-about">{m}</p>{/each}
						<dl class="dsfacts">
							<div title={SIGNED}><dt>Published by</dt><dd class="mono" data-testid="datasets-publisher">{d.publisher}</dd></div>
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
								<p class="fine" data-testid="datasets-signed">Published by <span class="mono">{d.publisher}</span>. {SIGNED}</p>
								<p class="fine" data-testid="datasets-where-card"><b>Where these numbers come from.</b> {sourceNote(d.publisher).join(' ')}</p>
								{#if s === 'loading' || s === undefined}
									<p class="muted">Reading the newest rows…</p>
								{:else if s === 'none'}
									<p class="muted" data-testid="datasets-detail-none">The node did not answer with rows. Try again in a minute.</p>
								{:else}
									{@const cols = chartColumns(s)}
									{@const days = dayKeys(d.keyMin, d.keyMax)}
									{@const h = history[id]}
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
									{@const newest = newestFirst(s.rows)}
									{#if since}<p class="fine" data-testid="datasets-detail-stale">This dataset has no data after {since}: the chart and the rows below end there, and are not current values.</p>{/if}
									<p class="fine">The newest {newest.length} {newest.length === 1 ? 'row' : 'rows'}, newest first{d.license ? ` · license ${d.license}` : ''} · <a href={toLink('', id)} onclick={() => reveal(id)} data-testid="datasets-link">link to this dataset</a></p>
									<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
									<div class="tablebox sample" tabindex="0" role="region" aria-label="Newest rows of {d.dataset}">
										<table data-testid="datasets-sample">
											<thead><tr>{#each s.columns as c, i (c)}<th class:r={i > 0}>{c}</th>{/each}</tr></thead>
											<tbody>
												{#each newest as r, ri (ri)}
													<tr>{#each r as v, i (i)}<td class="mono" class:r={i > 0}>{i === 0 ? keyText(v, days) : v}</td>{/each}</tr>
												{/each}
											</tbody>
										</table>
									</div>
									{#if unitLegend(s.columns).length}
										<p class="fine legend" data-testid="datasets-units">Column name endings: {#each unitLegend(s.columns) as u, ui (u)}{ui ? ' · ' : ''}<span class="nw"><span class="mono">{u.split(': ')[0]}</span>: {u.split(': ')[1]}</span>{/each}</p>
									{/if}
									<div class="dl" data-testid="datasets-download">
										<button type="button" class="btn ghost" onclick={() => save(csvName(d), csv(s.columns, s.rows, days))} data-testid="datasets-csv">Download CSV ({group(s.rows.length)} {s.rows.length === 1 ? 'row' : 'rows'})</button>
										{#if d.rows > s.rows.length}
											<button type="button" class="btn ghost" disabled={h?.state === 'run'} onclick={() => saveHistory(d)} data-testid="datasets-csv-all">Download full history (CSV)</button>
										{/if}
									</div>
									<p class="fine" role="status" data-testid="datasets-csv-status">
										{#if h?.state === 'run'}Reading {group(h.rows)} of {group(d.rows)} rows, 1,000 a call…
										{:else if h?.state === 'done'}Saved {group(h.rows)} rows, oldest first.
										{:else if h?.state === 'capped'}Saved the first {group(h.rows)} rows, oldest first. A download here stops at {group(HISTORY_CAP)} rows; the API pages through the rest in the same way.
										{:else if h?.state === 'busy'}The node is busy. Nothing was saved: try again in a minute.
										{:else if h?.state === 'fail'}The node did not answer with every row. Nothing was saved: try again in a minute.
										{:else if d.rows > s.rows.length}The first button saves the rows of the chart. The full history is read 1,000 rows a call and stops at {group(HISTORY_CAP)} rows.{/if}
										{#if days}Days are written as dates (UTC).{/if}
									</p>
									{#if s.book}
										<p class="fine">Check these rows yourself: the first command saves them with their proofs, the second checks every proof against the signed book on Base Sepolia.</p>
										<pre class="code" data-testid="datasets-commands">{curl(d, s)}
{check(d, s)}</pre>
									{/if}
									<p class="fine">Or read the newest rows in Python (needs <span class="mono">requests</span> and <span class="mono">pandas</span>; at most 1,000 rows a call). The node sends every value as text{days ? ' and each day as unix seconds' : ''}: the snippet turns them into numbers{days ? ' and dates' : ''} (floats, so not exact to the last digit).</p>
									<pre class="code" data-testid="datasets-python">{pythonSnippet(origin, d, { key: s.columns[0], days, text: textColumns(s) })}</pre>
								{/if}
							</div>
						{/if}
					</li>
				{/each}
			</ul>
			<div class="pager">
				<span class="muted" data-testid="datasets-count">Showing {group(items.length)} of {group(total ?? found.length)}</span>
				{#if more}
					<button type="button" class="btn ghost" disabled={busy} onclick={showMore} data-testid="datasets-more">Show more</button>
				{/if}
			</div>
		{:else if total === 0 || (!busy && pill.state === 'good')}
			<p class="muted" data-testid="datasets-empty">{freshOnly ? `No dataset${cleanSearch(query) ? ' with every word of this search in its name' : ''} has data from the last ${STALE_DAYS} days.` : 'No dataset has every word of this search in its name.'}</p>
			{#if suggest}
				<p class="suggest" role="status" data-testid="datasets-suggest">Did you mean <button type="button" class="linkbtn" onclick={() => take(suggest!.q)} data-testid="datasets-suggest-go">{suggest.q}</button>? ({group(suggest.total)} {suggest.total === 1 ? 'dataset' : 'datasets'})</p>
			{/if}
		{/if}
	</section>
</div>

<style>
	.where {
		margin: var(--sp-2) 0 0;
		max-width: 68ch;
	}
	.legend {
		margin: 0 0 var(--sp-3);
	}
	.suggest {
		margin: var(--sp-2) 0 0;
	}
	/* A suggested search: a link-coloured word, not a pill button (44 px tall for a finger). */
	.linkbtn {
		display: inline-flex;
		align-items: center;
		min-height: 44px;
		padding: 0 var(--sp-1);
		background: none;
		border: 0;
		border-radius: var(--radius-sm);
		box-shadow: none;
		color: var(--accent);
		font: inherit;
		font-weight: 500;
		text-decoration: underline;
		cursor: pointer;
	}
	.linkbtn:hover:not(:disabled) {
		transform: none;
		box-shadow: none;
	}
	.updated {
		margin: var(--sp-3) 0 0;
		max-width: 68ch;
		line-height: var(--lh-body);
	}
	.proofnote {
		margin-top: var(--sp-2);
	}
	.order {
		margin: 0 0 var(--sp-3);
	}
	.testtag {
		justify-self: start;
		margin: 0;
	}
	.dl {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-2);
		margin-bottom: var(--sp-2);
	}
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
		/* A dataset link scrolls its card to the top: clear of the fixed header. */
		scroll-margin-top: 5rem;
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
