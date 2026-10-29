<script lang="ts">
	// North-star loop NL9 (docs/spec/bounty-board.md): the bounty board. Coarse categories of wanted
	// data from the node's `GET /v1/wants`, asked only when the reader presses the button. A category
	// is shown only once enough prepaid accounts want it, as a bucket; there is no field for an exact
	// query, so none can be shown. Every value is mapped through the closed lists below: anything
	// else in the answer is dropped, never printed.
	import Pill, { type PillState } from '#lib/components/Pill.svelte';

	const KINDS: Record<string, string> = {
		chain: 'Chain data',
		public_api: 'A public page or API',
		keyed_api: 'An API that needs a key',
		signed: 'Signed by the source',
		own: 'Own data',
		forecast: 'A forecast'
	};
	const DOMAINS: Record<string, string> = {
		finance: 'finance',
		energy: 'energy',
		weather_climate: 'weather and climate',
		earth_science: 'earth science',
		health_statistics: 'health statistics',
		transport: 'transport',
		public_spending: 'public spending',
		other: 'other'
	};
	const REGIONS: Record<string, string> = {
		global: 'the whole world',
		africa: 'Africa',
		asia: 'Asia',
		europe: 'Europe',
		north_america: 'North America',
		south_america: 'South America',
		oceania: 'Oceania'
	};
	const CADENCES: Record<string, string> = { once: 'once', daily: 'daily', monthly: 'monthly' };
	const BUCKETS = new Set(['1-4', '5-19', '20+']);

	type Row = { text: string; bucket: string };
	let pill = $state<{ state: PillState; text: string }>({ state: 'idle', text: 'Not loaded' });
	let rows = $state<Row[] | null>(null);
	let min = $state(5);
	let asked = $state(false);

	function row(c: Record<string, unknown>): Row | null {
		const [k, d, r, ca, b] = [c.kind, c.domain, c.region, c.cadence, c.bucket].map((x) => (typeof x === 'string' ? x : ''));
		if (!(k in KINDS) || !(d in DOMAINS) || !(r in REGIONS) || !(ca in CADENCES) || !BUCKETS.has(b)) return null;
		return { text: `${KINDS[k]}: ${DOMAINS[d]}, ${REGIONS[r]}, ${CADENCES[ca]}`, bucket: b };
	}

	async function ask() {
		asked = true;
		rows = null;
		pill = { state: 'run', text: 'Asking the node' };
		try {
			const r = await fetch('/v1/wants', { headers: { accept: 'application/json' } });
			if (!r.ok || !(r.headers.get('content-type') ?? '').includes('json')) {
				pill = { state: 'idle', text: 'No board here' };
				return;
			}
			const b = await r.json();
			if (typeof b.minAccounts === 'number') min = b.minAccounts;
			const list = Array.isArray(b.categories) ? b.categories : [];
			const shown: Row[] = list.map(row).filter((x: Row | null): x is Row => x !== null);
			rows = shown;
			pill = { state: 'good', text: shown.length ? `${shown.length} wanted` : 'Nothing shown yet' };
		} catch {
			pill = { state: 'idle', text: 'No board here' };
		}
	}
</script>

<section class="section side" data-testid="wanted">
	<div class="sh">
		<h2>Wanted data</h2>
		<!-- The status shows only once the node was asked: an idle "Not loaded" read as unfinished. -->
		{#if asked}<Pill live state={pill.state} text={pill.text} />{/if}
	</div>
	<p class="muted">
		What buyers want, in broad categories only: the kind of data, its field, a continent and how often. Never an exact question. A category
		shows once at least {min} prepaid accounts want it, as a range, and the list changes once a week.
	</p>
	<button type="button" class="btn ghost" data-testid="wanted-ask" onclick={ask}>Show what buyers want</button>
	{#if rows && rows.length}
		<ul class="wl" data-testid="wanted-list">
			{#each rows as r (r.text)}
				<li><span>{r.text}</span> <span class="mono muted" data-testid="wanted-bucket">{r.bucket} buyers</span></li>
			{/each}
		</ul>
	{:else if rows}
		<p class="muted" data-testid="wanted-empty">No category has enough buyers to show yet.</p>
	{:else if asked && pill.state === 'idle'}
		<p class="muted" data-testid="wanted-none">This site’s node shows no board.</p>
	{/if}
</section>

<style>
	.wl {
		margin: var(--sp-3) 0 0;
		padding-left: var(--sp-4);
		display: grid;
		gap: var(--sp-2);
	}
	.wl li {
		overflow-wrap: anywhere;
	}
</style>
