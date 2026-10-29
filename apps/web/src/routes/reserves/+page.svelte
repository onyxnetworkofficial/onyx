<script lang="ts">
	import { onMount } from 'svelte';
	import Pill, { type PillState } from '#lib/components/Pill.svelte';
	import Skeleton from '#lib/components/Skeleton.svelte';
	import OnyxSlab from '#lib/components/OnyxSlab.svelte';
	import ReserveChart from '#lib/components/ReserveChart.svelte';
	import ChartLegend from '#lib/components/ChartLegend.svelte';
	import { RESERVE_KEYS } from '#lib/chart.ts';
	import { loadReserves, loadRosters, whenIdle, type Rosters } from '#lib/data.ts';
	import { verifier, type ReservesCheck } from '#lib/verifier.ts';
	import { entityName } from '#lib/live.ts';
	import { btc, int } from '#lib/format.ts';
	import { OKX_SIGNED_SUBSET, RESERVE_MATCHES } from '#lib/reserves.ts';
	import { FALLBACK } from '#lib/mode.ts';
	import { HELD_BACK, HELD_WHY } from '#lib/components/HeldBack.svelte';
	import { resolve } from '$app/paths';
	import ArrowRight from '@lucide/svelte/icons/arrow-right';
	import TableGroup from '#lib/components/TableGroup.svelte';

	// The published figures and Onyx's, shared with the home page's proof card (W5).
	const okxMatch = RESERVE_MATCHES.find((m) => m.entity === 'entity:okx')!;
	const bitbMatch = RESERVE_MATCHES.find((m) => m.entity === 'entity:bitwise-bitb')!;

	let check = $state.raw<ReservesCheck | null>(null);
	let pill = $state<{ state: PillState; text: string }>({ state: 'run', text: 'Checking' });
	let loadError = $state('');
	// Label set v3: the issuers' own rosters (ROADMAP P2a). Not checked in the browser: the export
	// is rerun by examples/export-rosters.rs (Python recount per shard) and the set by labels_v3.
	let rosters = $state.raw<Rosters | null>(null);
	let rostersError = $state('');

	onMount(() => {
		// A rights-fallback build ships no reserve history (ROADMAP L14).
		if (FALLBACK) return;
		loadRosters().then(
			(r) => (rosters = r),
			(e) => (rostersError = String(e))
		);
		whenIdle(async () => {
			try {
				check = await verifier.reserves(await loadReserves());
				pill = check.ok ? { state: 'good', text: 'Checks out' } : { state: 'bad', text: 'Does not check out' };
			} catch (e) {
				loadError = String(e);
				pill = { state: 'bad', text: 'Could not check' };
			}
		});
	});

	// Who stands behind each address list (fixtures/labels/PROVENANCE.md, ADR-0034).
	const source: Record<string, string> = {
		'entity:okx': 'Addresses OKX signed in its proof-of-reserves files (signatures checked)',
		'entity:binance': 'Addresses Binance published for proof of reserves (no signatures)',
		'entity:bitmex': 'Addresses BitMEX published for proof of liabilities and reserves',
		'entity:bitwise-bitb': 'Wallets the issuer lists on bitbetf.com (custodian: Coinbase Custody)'
	};
	// W8: where the lists without a cross-check link were read (docs/ops/data-sources.md S7, S8;
	// fixtures/labels/PROVENANCE.md). OKX and BITB link theirs in the cross-check line. The bare
	// folder URLs open an empty object (BitMEX's 404s), so: the bucket's file listing of the
	// PoRL folder, and Binance's newest wallet file, the one the indexer re-downloaded (20260801).
	const sourceLink: Record<string, { label: string; href: string }> = {
		'entity:binance': { label: 'source file (zip)', href: 'https://public.bnbstatic.com/static/proof-of-reserve/wallet_address_20260801.zip' },
		'entity:bitmex': { label: 'source', href: 'https://s3-eu-west-1.amazonaws.com/public.bitmex.com/?prefix=data/porl/' }
	};
	// Independent cross-checks of the base (THREATS RES-1): only these two have a source that is
	// not Onyx's own node. The others rest on one node's scan.
	const crossCheck: Record<string, { label: string; href: string }> = {
		'entity:okx': { label: "OKX's proof-of-reserves file", href: 'https://www.okx.com/proof-of-reserves' },
		'entity:bitwise-bitb': { label: "Bitwise's wallet list", href: 'https://bitbetf.com/' }
	};
	const series = $derived(
		(check?.ok ? check.entities : []).map((e) => {
			const points = e.points.map((p) => ({ height: p.height, balance: BigInt(p.balance_sats) }));
			const first = points[0]?.balance ?? 0n;
			const last = points[points.length - 1]?.balance ?? 0n;
			return { entity: e.entity, points, first, last, change: last - first };
		})
	);
	// Serving through the paid API (ADR-0034 Amendment 1): a row is served only when its entity's
	// base has an independent cross-check (fixtures/reserves/cross-checks.json); others are refused.
	const apiEntities = ['entity:binance', 'entity:bitmex', 'entity:bitwise-bitb', 'entity:okx'];
	const apiBasis: Record<string, string> = {
		'entity:okx': 'published-por-file',
		'entity:bitwise-bitb': 'issuer-wallet-list'
	};
	// Per roster entity over the fixture blocks: summed flows and the last day's balance (if any).
	const rosterWeek = $derived(
		(rosters?.entities ?? []).map((e) => {
			let received = 0n;
			let sent = 0n;
			let txs = 0;
			let balance: string | null = null;
			let balanceAt = 0;
			let seen = false;
			for (const d of rosters?.days ?? [])
				for (const r of d.rows)
					if (r.entity === e.entity) {
						seen = true;
						received += BigInt(r.received_sats);
						sent += BigInt(r.sent_sats);
						txs += r.tx_count;
						// The last day the entity still had listed addresses at its end.
						if (r.balance_sats !== null) {
							balance = r.balance_sats;
							balanceAt = d.to;
						}
					}
			return { ...e, seen, received, sent, txs, bal: balance, balanceAt };
		})
	);
	const scanTotal = (e: string) => BigInt(check?.scan_totals.find((t) => t.entity === e)?.balance_sats ?? '0');
	const slug = (e: string) => e.replace(/^entity:/, '');
</script>

<svelte:head><title>Reserves · Onyx</title></svelte:head>

<OnyxSlab>
	<h1>Reserves</h1>
	<div class="slabside" data-testid="slabside">
		{#if FALLBACK}
			<!-- The rights-fallback build shows no balance: the lede says what will be here, and that it waits. -->
			<p class="slablede">This page will show the bitcoin held at each company's published addresses, rerunnable from the blocks.</p>
			<p class="slablede">Those balances wait for data rights to be cleared; until then they are held back.</p>
		{:else}
			<p class="slablede">On-chain bitcoin at each company's published addresses, block by block.</p>
			<p class="slablede">The balance is rerunnable from the blocks. Who owns the addresses stays the company's claim.</p>
		{/if}
	</div>
</OnyxSlab>

<div class="wrap">
	{#if FALLBACK}
		<!-- A rights-fallback build: the page's one section says what is waiting and why, and points to
		     what is live. -->
		<section class="section" data-testid="reserves-held-back">
			<div class="sh"><h2>Company reserves: waiting for data rights</h2><span class="state">Held back</span></div>
			<div class="panel waiting">
				<p class="why">
					<b>{HELD_BACK}</b> Once they are, this page shows each company's on-chain bitcoin, its weekly history and how it compares
					with the company's own figures. {HELD_WHY}
				</p>
				<div class="live">
					<p class="muted">Live now:</p>
					<div class="links">
						<a class="btn" href={resolve('/data')} data-testid="reserves-to-data">Browse data <ArrowRight size={18} /></a>
						<a class="btn ghost" href={resolve('/markets')} data-testid="reserves-to-markets">Markets <ArrowRight size={18} /></a>
					</div>
				</div>
			</div>
		</section>
	{:else}
	<section class="section" data-testid="reserves">
		<div class="sh">
			<h2>Balance over the week</h2>
			<Pill live state={pill.state} text={pill.text} />
		</div>
		<!-- W12 round 6: the lede reads in two columns on a wide screen (app.css .duo). -->
		<div class="duo">
		<p class="muted lede">
			A chain of <span class="mono">btc-entity-reserve</span> shards (ADR-0034): each shard starts on the previous shard's end balances and adds
			that range's flows; the first starts on a base taken from a scan of the unspent outputs at every listed address. Same week and shard
			boundaries as the flow history on the Data page.
		</p>
		</div>
		{#if loadError}
			<p class="panel muted" data-testid="reserves-failed">Could not check the reserve history: {loadError}</p>
		{:else if check && !check.ok}
			<div class="panel" data-testid="reserves-failed">
				<p class="muted">The reserve history did not pass the check, so nothing is shown.</p>
				<ul class="muted">
					{#each check.problems.slice(0, 5) as p, i (i)}<li>{p}</li>{/each}
				</ul>
			</div>
		{:else if check}
			<!-- W12 round 5: one "Data tables" switch in the legend row opens the four cards' tables (each
			     stays right under its chart); there was a "Data table (67 rows)" fold in every card. -->
			<TableGroup testid="reserve-tables">
			{#snippet children(tables)}
			<div class="legendrow">
				<ChartLegend items={RESERVE_KEYS} unit="BTC; each axis runs from the week's low to its high" testid="reserve-legend">{@render tables()}</ChartLegend>
			</div>
			<div class="grid" data-testid="reserve-cards">
				{#each series as s (s.entity)}
					<div class="panel card" data-testid="reserve-{slug(s.entity)}">
						<div class="head">
							<h3 class="ptitle">{entityName(s.entity)}</h3>
							<span class="mono big" data-testid="reserve-end-{slug(s.entity)}">{btc(s.last)} BTC</span>
						</div>
						<p class="muted small">{source[s.entity] ?? s.entity}{#if sourceLink[s.entity]}{' '}· <a href={sourceLink[s.entity].href} target="_blank" rel="noopener">{sourceLink[s.entity].label}</a>{/if}</p>
						<p class="small" data-testid="reserve-xcheck-{slug(s.entity)}">
							{#if crossCheck[s.entity]}
								<span class="chip xok">Cross-checked</span>
								<span class="muted"
									>against <a href={crossCheck[s.entity].href} target="_blank" rel="noopener">{crossCheck[s.entity].label}</a></span
								>
							{:else}
								<span class="chip xno">Not independently cross-checked</span>
								<span class="muted">the base is one node's scan</span>
							{/if}
						</p>
						<ReserveChart points={s.points} name={entityName(s.entity)} />
						<!-- W12 round 5: the week's change is the card's footer row. Round 6: its two figures sit side
						     by side, each under its label (the mono line wrapped to two lines at 1,280 px); the text is
						     the same, the dot between them is in the text but not drawn (each figure has its label). -->
						<div class="pfoot">
							<p class="wk" data-testid="reserve-change-{slug(s.entity)}">
								<span class="fig"><span class="lbl">Week:</span> <span class="val nw" class:down={s.change < 0n} class:up={s.change > 0n}>{s.change < 0n ? '▼ ' : s.change > 0n ? '▲ +' : ''}{btc(s.change)} BTC</span></span><span class="sep"> · </span><span class="fig"><span class="lbl">at the scan (#{int(check!.scan_height)}):</span> <span class="val nw">{btc(scanTotal(s.entity))} BTC</span></span>
							</p>
						</div>
					</div>
				{/each}
			</div>
			{/snippet}
			</TableGroup>
			<div class="notebar duo">
			<p class="muted small" data-testid="reserves-note">
				{check.week_shards} week shards and {check.shards - check.week_shards} more up to the scan at block {int(check.scan_height)}. Your browser
				rebuilt every shard's root from its leaves, checked each shard starts on the previous one's end balances and root, checked the chain lands
				on the scan's totals exactly, derived each company's starting balance from the pinned scan and the shards' flows (the client serving
				check an API client runs too), and recomputed the commitment this page pins from Onyx's own run. It did not rerun the recipe: that needs
				the raw blocks (<span class="mono">examples/export-reserves.rs</span>, <span class="mono">tests/golden_reserve.rs</span>).
			</p>
			</div>
		{:else}
			<div class="panel"><Skeleton rows={4} label="Checking the reserve history" /></div>
		{/if}
	</section>

	<section class="section" data-testid="rosters">
		<div class="sh"><h2>Issuer rosters (label set v3)</h2></div>
		<div class="panel">
			<div class="duo">
			<p class="muted lede">
				Eleven companies' own dated address lists, one membership per stretch of blocks between their publications
				(<span class="mono">fixtures/rosters</span>, ADR-0037 Amendment 1). A listing counts for 8,928 blocks (62 days) after the
				snapshot that lists it, so an old list stops counting. "Signed" means the company's signature over the address checked out
				in Onyx; "published" means the company listed it. Flows below are the <span class="mono">btc-entity-balance</span> recipe over
				each stretch of the fixture blocks, recounted in Python; a balance shows only where the committed scan holds every address.
				Test data, not served.
			</p>
			</div>
			{#if rostersError}
				<p class="muted" data-testid="rosters-failed">Could not load the roster export: {rostersError}</p>
			{:else if rosters}
				<div class="tablebox"><table class="live stack">
					<thead><tr><th>Company</th><th>Source</th><th>Grade</th><th class="r">Snapshots</th><th class="r">Addresses</th><th>Blocks {int(rosters.range.from)} to {int(rosters.range.to)}</th></tr></thead>
					<tbody>
						{#each rosterWeek as e (e.entity)}
							<tr data-testid="roster-{slug(e.entity)}">
								<td data-label="Company">{entityName(e.entity)}</td>
								<td data-label="Source">Listed in <a href={e.source_url} target="_blank" rel="noopener">{e.source}</a>, {e.first_issued} to {e.last_issued}</td>
								<td data-label="Grade" data-testid="roster-grade-{slug(e.entity)}">{e.grade}</td>
								<td class="mono r" data-label="Snapshots">{int(e.snapshots)}</td>
								<td class="mono r" data-label="Addresses">{int(e.addresses)}</td>
								<td data-label="Fixture blocks" data-testid="roster-week-{slug(e.entity)}">
									{#if e.seen}
										<span class="mono">in {btc(e.received)} · out {btc(e.sent)} BTC</span>{#if e.bal !== null}<span class="mono"> · balance {btc(BigInt(e.bal))} BTC at #{int(e.balanceAt)}</span>{:else}<span class="muted small"> · balance not computed (no scan holds these addresses)</span>{/if}
									{:else}
										<span class="muted small">{e.balance}</span>
									{/if}
								</td>
							</tr>
						{/each}
					</tbody>
				</table></div>
				<p class="muted small" data-testid="rosters-note">
					Label set {rosters.label_set.name} v{rosters.label_set.version}, published at block {int(rosters.label_set.published)}, content hash
					<span class="mono">{rosters.label_set.content_hash.slice(0, 18)}…</span>. Left out: wBTC (its site's terms forbid redistribution) and Coinbase cbBTC
					(terms unread). Rerun: <span class="mono">examples/export-rosters.rs</span>.
				</p>
			{:else}
				<Skeleton rows={4} label="Loading the issuer rosters" />
			{/if}
		</div>
	</section>
	<section class="section two">
		<div class="panel">
			<div class="sh"><h2>Where the base comes from</h2></div>
			<p class="muted">
				One node's read of the unspent outputs (<span class="mono">scantxoutset</span>) at every listed address, at block 971,199, committed
				per address. A scan only reads the tip, so the base before the week is that scan minus the flows in between, and the recipe chain lands
				on the scan again, to the satoshi. That checks the arithmetic, not the scan. A second scan at block 971,202 reconciles address by address,
				but it is the same node 3 blocks later, so a node that missed an address in both would still pass.
			</p>
			<p class="muted">
				Independent checks: OKX's proof-of-reserves file and Bitwise's wallet list agree to the satoshi (below). Binance and BitMEX have no
				independent check yet, so the API does not serve their reserves until a scan on a second node reconciles with this one.
			</p>
		</div>
		<div class="panel">
			<div class="sh"><h2>What each number proves</h2></div>
			<ul class="muted proof">
				<li>
					<b>Balance of the listed addresses:</b> level 1 given the base. Anyone with the blocks reruns it. The base is one node's scan,
					cross-checked independently only for OKX (its proof-of-reserves file covers 99.8% of OKX's labelled value, checked at block 965,949) and BITB (issuer list).
				</li>
				<li><b>That OKX controls its addresses:</b> OKX's own signatures (a signed source, level 3).</li>
				<li><b>That Binance, BitMEX or Bitwise control theirs:</b> their published lists, a claim (level 4).</li>
				<li><b>Incoming dust:</b> counted. It is on chain at the address, and it costs the sender what it adds.</li>
			</ul>
		</div>
	</section>

	<section class="section" data-testid="reserve-api">
		<div class="sh"><h2>Served through the API</h2></div>
		<div class="panel">
			<!-- W12 round 6: the intro reads in two columns on a wide screen (app.css .duo). -->
			<div class="duo">
			<p class="muted">
				Each company's balance is a paid metric, one row per company with its Merkle proof, served only for shards whose root the chain
				finalized. Before any row is served, the server runs a serving check: it rebuilds the base from the committed scan and the finalized
				rows (never from the shard's own parameters), and refuses any shard on another base. A row is served only if its base is
				independently cross-checked; the others are refused before you are asked to pay.
			</p>
			</div>
			<!-- W12 round 4: the four facts as cards under the rule (they ran in a column beside it,
			     the rule's column half empty below it). -->
			<dl class="facts stack apifacts">
				<div class="fact">
					<dt>Endpoint</dt>
					<dd class="mono" data-testid="reserve-api-endpoint">GET /v1/metrics/btc.entity-reserve?subject=entity:okx</dd>
				</div>
				<div class="fact">
					<dt>Price</dt>
					<dd data-testid="reserve-api-price">
						Set by each operator, per row, paid with x402 while the data is fresh. The local rehearsal charges 2,500 micro-USDC (0.0025 test USDC)
						for 24 hours.
					</dd>
				</div>
				<div class="fact">
					<dt>Proof level</dt>
					<dd data-testid="reserve-api-level">
						4 overall, registered on chain: the balance is rerun from the blocks (level 1), but the base and the published address lists are
						claims (level 4). The registry's <span class="mono">verifyRowLeveled</span> returns 4 for a served row.
					</dd>
				</div>
				<div class="fact">
					<dt>Answer</dt>
					<dd data-testid="reserve-api-answer">
						The row, its proof and <span class="mono cmd">servingCheck: {'{'} gate, crossChecked, basis, source {'}'}</span>, the operator's claim.
						A client that does not trust it re-runs the check: <span class="mono">/v1/served/btc.entity-reserve</span> lists the chain, and
						<span class="mono">onyx_reserve_serve::client</span> derives the base from the client's own pinned scan.
					</dd>
				</div>
			</dl>
			<div class="tablebox"><table class="live stack">
				<thead><tr><th>Company</th><th>Served</th><th>Cross-checked</th></tr></thead>
				<tbody>
					{#each apiEntities as e (e)}
						<tr data-testid="reserve-api-{slug(e)}">
							<td data-label="Company">{entityName(e)}</td>
							{#if apiBasis[e]}
								<td data-label="Served">Yes</td>
								<td class="mono" data-label="Cross-checked">crossChecked: true ({apiBasis[e]})</td>
							{:else}
								<td data-label="Served">No: 403 <span class="mono">not_cross_checked</span></td>
								<td class="mono" data-label="Cross-checked">no independent check</td>
							{/if}
						</tr>
					{/each}
				</tbody>
			</table></div>
		</div>
	</section>

	<section class="section" data-testid="reserve-live">
		<div class="sh"><h2>Checked against the companies' own figures</h2></div>
		<div class="panel">
			<div class="tablebox"><table class="live stack">
				<thead><tr><th>Published figure</th><th class="r">Theirs</th><th class="r">Onyx</th><th>Difference</th></tr></thead>
				<tbody>
					<tr>
						<td data-label="Published figure"
							>OKX proof of reserves, 2026-09-08, block 965,949, all {int(okxMatch.addresses)} BTC addresses (<a
								href="https://www.okx.com/proof-of-reserves"
								target="_blank"
								rel="noopener">okx.com</a
							>)</td
						>
						<td class="mono r" data-label="Theirs">{okxMatch.theirs}</td>
						<td class="mono r" data-label="Onyx">{okxMatch.onyx}</td>
						<td class="mono" data-label="Difference">0 ({int(okxMatch.addresses)} of {int(okxMatch.addresses)} addresses exact)</td>
					</tr>
					<tr>
						<td data-label="Published figure">Same snapshot, the 3,558 addresses also in Onyx's signed OKX set</td>
						<td class="mono r" data-label="Theirs">{OKX_SIGNED_SUBSET}</td>
						<td class="mono r" data-label="Onyx">{OKX_SIGNED_SUBSET}</td>
						<td class="mono" data-label="Difference">0</td>
					</tr>
					<tr>
						<td data-label="Published figure"
							>Bitwise BITB wallet list, 2026-10-13 (<a href="https://bitbetf.com/" target="_blank" rel="noopener">bitbetf.com</a>), {bitbMatch.addresses}
							wallets, at block 971,199</td
						>
						<td class="mono r" data-label="Theirs">{bitbMatch.theirs}</td>
						<td class="mono r" data-label="Onyx">{bitbMatch.onyx}</td>
						<td class="mono" data-label="Difference">0 ({bitbMatch.addresses} of {bitbMatch.addresses} wallets exact)</td>
					</tr>
				</tbody>
			</table></div>
			<div class="pfoot duo"><p class="muted small">
				Recorded 2026-10-13 with <span class="mono">scripts/reserve-live-check.py</span> (docs/ops/indexer-integration.md). Onyx's OKX figure is
				the scan at block 971,199 minus each address's flows over the 5,250 blocks after the snapshot. Nothing was tuned to match.
			</p></div>
		</div>
	</section>
	{/if}
</div>

<style>
	/* The rights-fallback state: a plain status beside the heading, the reason, then the live pages. */
	.state {
		display: inline-flex;
		align-items: center;
		gap: var(--sp-2);
		font-size: var(--text-sm);
		color: var(--warn);
	}
	.state::before {
		content: '';
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: currentColor;
	}
	.waiting {
		display: grid;
		gap: var(--sp-5);
	}
	/* The held-back build is one short section: the layout's full-screen minimum (kept for pages
	   whose content loads after first paint) left a band of empty page above the footer. */
	:global(main:has([data-testid='reserves-held-back'])) {
		min-height: 0;
	}
	.why {
		margin: 0;
		max-width: 68ch;
		line-height: var(--lh-body);
		text-wrap: pretty;
	}
	.live {
		display: grid;
		gap: var(--sp-3);
		align-content: start;
		padding-top: var(--sp-4);
		border-top: 1px solid var(--line);
	}
	/* Wide: the reason at the left, the live pages beside it (the right half stood empty). */
	@media (min-width: 900px) {
		.waiting {
			grid-template-columns: minmax(0, 3fr) minmax(0, 2fr);
			column-gap: var(--sp-7);
		}
		.live {
			padding-top: 0;
			padding-left: var(--sp-6);
			border-top: 0;
			border-left: 1px solid var(--line);
		}
	}
	.live p {
		margin: 0;
	}
	.links {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-3);
	}
	.legendrow {
		margin-bottom: var(--sp-4);
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 340px), 1fr));
		gap: var(--sp-4);
	}
	.card {
		display: grid;
		gap: var(--sp-2);
		min-width: 0;
	}
	.head {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
		gap: var(--sp-2);
		flex-wrap: wrap;
	}
	/* W12 round 11: each company's name is a panel title (app.css .ptitle, the display face): it was
	   bold 15 px sans beside the serif section and panel titles. */
	h3 {
		margin: 0;
	}
	/* Phones: the name over the figure on every card (some fit side by side, some did not). */
	@media (max-width: 560px) {
		.head {
			display: grid;
			gap: var(--sp-1);
		}
	}
	/* W12 round 2: each company's balance is the card's figure (was body size, level with the
	   name); it drops under the name when both do not fit. */
	.big {
		font-size: var(--text-lg);
		font-weight: 500;
		line-height: var(--lh-tight);
		letter-spacing: -0.02em;
		font-variant-numeric: tabular-nums;
	}
	.small {
		font-size: var(--text-sm);
		margin: 0;
	}
	/* W12 round 6: the week's change and the scan total as two labelled figures; the change in the
	   chart's colour for its direction (the triangle says it without colour). Phones: one figure a row,
	   label left, value right. */
	/* Two equal columns when both figures fit; when they do not (cards about 350 px wide, near
	   761 px), the second figure wraps under the first instead of running into it (round 7 review:
	   a 1fr grid let the nowrap week value overlap "at the scan"). */
	.wk {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-1) var(--sp-3);
		max-width: none;
	}
	.wk .sep {
		display: none;
	}
	.wk .fig {
		display: grid;
		gap: var(--sp-0);
		flex: 1 1 0;
		min-width: max-content;
	}
	.wk .lbl {
		color: var(--muted);
		font-size: var(--text-xs);
		line-height: var(--lh-snug);
	}
	.wk .val {
		font-family: var(--mono);
		font-size: var(--text-sm);
		font-weight: 500;
		font-variant-numeric: tabular-nums;
	}
	.wk .val.down {
		color: var(--btc);
	}
	.wk .val.up {
		color: var(--line-net);
	}
	@media (max-width: 560px) {
		.wk {
			display: grid;
			grid-template-columns: minmax(0, 1fr);
			gap: var(--sp-1);
		}
		.wk .fig {
			min-width: 0;
			display: flex;
			flex-wrap: wrap;
			justify-content: space-between;
			align-items: baseline;
			gap: var(--sp-0) var(--sp-3);
		}
	}
	/* The API facts: two cards a row on a wide screen (the endpoint needs the width). */
	@media (min-width: 761px) {
		.apifacts {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
	}
	.apifacts dd.mono {
		overflow-wrap: anywhere;
	}
	.chip.xok {
		color: var(--good);
	}
	.chip.xno {
		color: var(--warn);
	}
	.proof {
		display: grid;
		gap: var(--sp-2);
		padding-left: var(--sp-4);
	}
	.live {
		width: 100%;
		border-collapse: collapse;
		font-size: var(--text-sm);
	}
	/* Wide screens: the table scrolls inside its box if it must (W12 round 3: the box scrolls, not
	   the table, so a narrow table still spans its panel); phones stack each row (app.css). */
	.live td {
		vertical-align: top;
	}
	/* W12: the figure's name and the difference note wrap, so the four columns fit the panel
	   at 1280 px (the last one was cut at the panel's edge). */
	.live td:first-child,
	.live td:last-child {
		white-space: normal;
	}
	.live td:first-child {
		min-width: 24ch;
	}
	@media (max-width: 560px) {
		.live td {
			padding: var(--sp-1) 0 var(--sp-1) 42%;
			border: 0;
		}
	}
</style>
