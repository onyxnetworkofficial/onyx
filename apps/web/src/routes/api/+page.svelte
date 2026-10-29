<script lang="ts">
	import { onMount } from 'svelte';
	import Pill, { type PillState } from '#lib/components/Pill.svelte';
	import OnyxSlab from '#lib/components/OnyxSlab.svelte';
	import Wallet from '@lucide/svelte/icons/wallet';
	import GitMerge from '@lucide/svelte/icons/git-merge';
	import Link from '@lucide/svelte/icons/link';
	import Terminal from '@lucide/svelte/icons/terminal';
	import { loadDemo, type Demo } from '#lib/data.ts';
	import { verifier, type Pinned } from '#lib/verifier.ts';
	import { FALLBACK } from '#lib/mode.ts';
	import ApiReference from '#lib/components/ApiReference.svelte';
	import spec from '../../../static/openapi.json';
	import { operations } from '#lib/openapi.ts';

	let demo = $state.raw<Demo | null>(null);
	let pill = $state<{ state: PillState; text: string }>({ state: 'idle', text: 'Not checked' });
	let pin = $state.raw<Pinned | null>(null);
	let loadError = $state('');
	// W4: the sample shows from the small demo file; the verifier (1.8 MiB gzip) loads only
	// when the button below asks it to check the proof.
	onMount(() => {
		loadDemo()
			.then((d) => (demo = d))
			.catch((e) => (loadError = String(e)));
	});

	// The sample response is built from the real shard in the export. Where each dataset was
	// served is a short table; how each run was paid sits in the folded "Run notes" (M7L3-R2-4).
	const sample = $derived.by(() => {
		// A rights-fallback build ships no row to show (ROADMAP L14).
		if (!demo || FALLBACK) return null;
		const run = demo.runs[0];
		// A demo file whose run carries no proofs (the small first-paint file) shows no sample yet.
		const p = run.proofs?.[0];
		if (!p) return null;
		return {
			round: demo.commitment.round,
			shard: run.shard_id,
			index: p.index,
			row: run.rows[0].leaf,
			proof: { index: p.index, leaf_count: p.leaf_count, siblings: p.siblings },
			root: run.root,
			recipe_id: demo.recipe_id,
			label_set: demo.label_set.content_hash,
			tx: null
		};
	});

	let tab = $state<'request' | 'response'>('response');
	// PP6: the paid data routes, from the OpenAPI file (the Reference below lists every route).
	const endpoints = operations(spec as Record<string, unknown>)
		.filter((o) => o.paid)
		.map((o) => [o.path, o.summary]);
	// A path breaks only after a "/", so the method stays on the line with its path's start.
	const segs = (path: string) => path.split(/(?<=\/)/);
	// Where each dataset has been served (facts from the run notes below).
	const served = [
		{ id: 'btc.entity-flow', what: 'Company flows', where: 'Internal test (mock payments); Base Sepolia test network (test USDC)', when: '2026-10-12, 10-13, 10-14' },
		{ id: 'btc.entity-reserve', what: 'Company reserves', where: 'Internal test (mock payments); Base Sepolia test network (test USDC)', when: '2026-10-14' },
		{ id: '/v1/observations', what: 'U.S. public debt (level-2 observations)', where: 'Internal test only (mock payments)', when: '' }
	];

	async function check() {
		if (!sample) return;
		pill = { state: 'run', text: 'Checking' };
		try {
			pin ??= await verifier.pinned();
			// Checked against the root pinned in the code, not the "root" field the server sent.
			const ok = sample.root === pin.root && (await verifier.verifyProof(pin.root, sample.row, sample.proof.index, sample.proof.leaf_count, sample.proof.siblings));
			pill = ok ? { state: 'good', text: 'Valid against the pinned root' } : { state: 'bad', text: 'Proof fails' };
		} catch (e) {
			pill = { state: 'bad', text: e instanceof Error ? e.message : String(e) };
		}
	}
</script>

<svelte:head><title>API · Onyx</title></svelte:head>

<OnyxSlab>
	<h1>Query API</h1>
	<div class="slabside wide" data-testid="slabside">
		<p class="meta slabmeta" data-testid="api-status">Hosted test node at <a href="https://onyx.cryptagraph.com/v1/health" target="_blank" rel="noopener">onyx.cryptagraph.com/v1</a> · <span class="nw">testnet only</span> · <span>data is paid in test USDC on Base Sepolia, not switched on yet</span> · <span class="nw">no real money</span></p>
		<div class="perks">
			<span><Wallet size={16} /> Testnet USDC per call (x402, HTTP 402)</span>
			<span><GitMerge size={16} /> A proof with every row</span>
			<span><Link size={16} /> Roots on Base Sepolia since 2026-10-12</span>
		</div>
	</div>
</OnyxSlab>

<div class="wrap">
	{#if loadError}<p class="section">Could not load the demo data: {loadError}</p>{/if}
	<section class="section grid">
		<h2 class="span">Paid data routes</h2>
		<div class="eps" class:solo={!sample}>
			{#each endpoints as [path, what] (path)}
				<div class="ep"><span class="mp"><span class="get">GET</span> <code>{#each segs(path) as sg, i (i)}{sg}<wbr />{/each}</code></span><span class="muted">{what}</span></div>
			{/each}
		</div>

		{#if sample}
			<div class="term">
				<!-- W3: the two views are toggle buttons that say which is shown; the sample is a
				     named region that takes focus, so a keyboard can scroll it. -->
				<div class="tbar" role="group" aria-label="Sample">
					<Terminal size={16} />
					<button type="button" class="tab" class:on={tab === 'request'} aria-pressed={tab === 'request'} onclick={() => (tab = 'request')}>Request</button>
					<button type="button" class="tab" class:on={tab === 'response'} aria-pressed={tab === 'response'} onclick={() => (tab = 'response')}>Response</button>
				</div>
				{#if tab === 'request'}
					<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
					<pre class="tcode" tabindex="0" role="region" aria-label="Sample request">curl https://api.onyx.example/v1/proofs/{sample.round}/{sample.shard.slice(0, 10)}…/{sample.index}</pre>
				{:else}
					<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
					<pre class="tcode" tabindex="0" role="region" aria-label="Sample response">{JSON.stringify(sample, null, 2)}</pre>
				{/if}
				<div class="tfoot">
					<button type="button" onclick={check}>Check the proof against the pinned root</button>
					<Pill live state={pill.state} text={pill.text} />
					<span class="tnote" data-testid="api-check-scope">A real row and proof from the Explorer's shard. This button checks the proof in your browser against the root pinned in this site's code. It does not read the chain; the client library does that on your own node (below).</span>
				</div>
			</div>
		{/if}
		<!-- W12 round 5: where each dataset was served is its own panel across both columns (it floated as a
		     long grey list under the endpoints, beside an empty column); the four datasets sit two a row. -->
		<div class="panel scope" data-testid="api-scope">
			<h3 class="ptitle">Where each dataset has been served</h3>
			<p class="muted">All served by the Onyx API server (<code>onyx-api</code>); paid calls use x402.</p>
			<div class="tablebox">
				<table class="servedt">
					<thead><tr><th>Dataset</th><th>Where served</th><th>Date</th></tr></thead>
					<tbody>
						{#each served as d (d.what)}
							<tr>
								<td>{d.what}{#if d.id}<br /><code class="id">{d.id}</code>{/if}</td>
								<td data-label="Where served">{d.where}</td>
								<td class="mono nw" data-label="Date">{d.when || '—'}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
			{#if FALLBACK}
				<p class="fine" data-testid="api-held-back">
					Company metrics (<code>btc.entity-flow</code>, <code>btc.entity-reserve</code> and the others) are held back until data rights
					clear: a node declines them before any payment. Pools and level-2 observations are served as before.
				</p>
			{/if}
			<details class="more runnotes" data-testid="api-run-notes">
				<summary>Run notes</summary>
				<ul class="served">
					<li data-testid="api-scope-flow">
						<code>btc.entity-flow</code>: in the local rehearsal (a local anvil chain; paid with x402 through a mock
						facilitator in mock USDC, so no token moves) and in each Base Sepolia run (2026-10-12, 2026-10-13, 2026-10-14). Each
						run sold the same row twice with real testnet USDC: once settled by <code>onyx-api</code> itself
						(x402 self-settle) into <code>OnyxRevenue</code>, and once through a standard x402 facilitator (a local
						stand-in that submits the token's <code>transferWithAuthorization</code>): that USDC reached
						<code>OnyxRevenue</code> unallocated, the receipt was on disk before the answer and survived a stop of the API,
						and the reporter credited it to the shard.
					</li>
					<li data-testid="api-scope-reserve">
						<code>btc.entity-reserve</code>: in the local rehearsal (mock facilitator, mock USDC: no token moves) and in the Base
						Sepolia run of 2026-10-14: after its serving check cleared the reserve shard, one OKX row was sold with real testnet
						USDC, self-settled into <code>OnyxRevenue</code> and booked to the reserve shard; Binance was refused (403
						<code>not_cross_checked</code>) before any payment.
					</li>
					<li data-testid="api-scope-level2">
						<code>/v1/observations</code> (level-2 observations; the U.S. total public debt outstanding): in the local
						rehearsal only, paid with x402 through the mock facilitator (mock USDC: no token moves).
					</li>
					<li data-testid="api-scope-tests">
						Mining pools: only in <code>onyx-api</code>'s test suites; no rehearsal has served them.
					</li>
					{#if FALLBACK}<li>A node withholds company metrics when started with <code>ONYX_RIGHTS=fallback</code>.</li>{/if}
				</ul>
			</details>
			<div class="pfoot"><p class="fine">Testnet only. Routes and answers may still change.</p></div>
		</div>
	</section>

	<ApiReference />

	<!-- W12 round 6: heading beside its paragraph on a wide screen (app.css .section.side). -->
	<section class="section side verifynote" data-testid="api-verify-default">
		<h2>Your node checks the root</h2>
		<div class="vtext">
			<p>
				Before your code uses a row, the client library asks your own chain node whether that row's root is final. The server reads
				roots through its operator's node, so only your own read catches a server that lies.
			</p>
			<p class="small muted">
				By default <code>ClientNode::verify</code> reads <code>getFinalizedRoot(round, shard)</code> at the <code>finalized</code> block
				and refuses a root that is not final there. Skipping the check takes an explicit call to <code>verify_response_unchecked</code>,
				and its result is labelled <code>NOT VERIFIED ON CHAIN</code>.
			</p>
		</div>
	</section>
</div>

<style>
	.perks {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-3) var(--sp-6);
		margin-top: var(--sp-4);
		color: var(--slab-ink);
		font-size: var(--text-sm);
	}
	.perks span {
		display: inline-flex;
		align-items: center;
		gap: var(--sp-2);
	}
	.perks :global(svg) {
		color: var(--slab-accent);
	}
	.grid {
		display: grid;
		grid-template-columns: minmax(0, 0.9fr) minmax(0, 1.1fr);
		gap: var(--sp-6) var(--sp-4);
		align-items: start;
	}
	@media (max-width: 860px) {
		.grid {
			grid-template-columns: minmax(0, 1fr);
		}
	}
	.eps {
		display: grid;
		gap: var(--sp-3);
	}
	/* Without the sample beside them (a rights-fallback build) the cards fill the row, two a row. */
	.eps.solo {
		grid-column: 1 / -1;
		grid-template-columns: repeat(auto-fill, minmax(min(100%, 22rem), 1fr));
	}
	.ep {
		display: grid;
		gap: var(--sp-1);
		align-content: start;
		padding: var(--sp-4);
		border-radius: var(--radius-lg);
		border: 1px solid var(--line);
		background: var(--panel);
		box-shadow: var(--shadow);
	}
	/* Method and path on one line; the path breaks only after a "/" (a <wbr>). */
	.mp code {
		overflow-wrap: normal;
	}
	.ep .muted {
		font-size: var(--text-sm);
	}
	.get {
		font-family: var(--mono);
		font-size: var(--text-xs);
		font-weight: 600;
		color: var(--accent);
		background: var(--accent-soft);
		border-radius: var(--radius-sm);
		padding: var(--sp-0) var(--sp-2);
		margin-right: var(--sp-1);
	}
	.term {
		border-radius: var(--radius-lg);
		overflow: hidden;
		background: var(--slab);
		color: var(--slab-ink);
		border: 1px solid var(--slab-line);
		box-shadow: var(--shadow);
		min-width: 0;
	}
	.tbar {
		display: flex;
		align-items: center;
		gap: var(--sp-2);
		padding: var(--sp-3) var(--sp-3);
		border-bottom: 1px solid var(--slab-line);
		color: var(--slab-muted);
	}
	.tab {
		background: transparent;
		border: 0;
		color: var(--slab-muted);
		padding: var(--sp-1) var(--sp-3);
		font-size: var(--text-sm);
	}
	.tab.on {
		color: var(--slab-ink);
		background: var(--line);
	}
	.tab:hover:not(:disabled) {
		transform: none;
		box-shadow: none;
		color: var(--slab-ink);
	}
	.tcode {
		margin: 0;
		padding: var(--sp-4);
		font-family: var(--mono);
		font-size: var(--text-xs);
		overflow-x: auto;
		max-height: 380px;
		color: var(--ink);
	}
	/* Phones: long hashes wrap instead of scrolling sideways. */
	@media (max-width: 560px) {
		.tcode {
			white-space: pre-wrap;
			overflow-wrap: anywhere;
			overflow-x: hidden;
		}
	}
	.tfoot {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-3);
		align-items: center;
		padding: var(--sp-3);
		border-top: 1px solid var(--slab-line);
		background: var(--panel-2);
	}
	.tnote {
		flex-basis: 100%;
		font-size: var(--text-sm);
		color: var(--slab-muted);
	}
	.tfoot :global(.pill.idle) {
		color: var(--slab-muted);
	}
	/* W12 round 5: the datasets served so far, one panel across both columns: a short table, the
	   per-run detail folded under "Run notes". */
	.scope {
		grid-column: 1 / -1;
	}
	.scope .ptitle {
		margin: 0;
	}
	.servedt td {
		white-space: normal;
		vertical-align: top;
	}
	.servedt td.nw {
		white-space: nowrap;
	}
	/* Phones: each dataset is a small block (name, where, date), not three squeezed columns. */
	@media (max-width: 560px) {
		.servedt thead {
			display: none;
		}
		.servedt tr {
			display: grid;
			gap: var(--sp-1);
			padding: var(--sp-3) 0;
			border-bottom: 1px solid var(--line);
		}
		.servedt tr:last-child {
			border-bottom: 0;
		}
		.servedt td {
			display: block;
			padding: 0;
			border: 0;
		}
		.servedt td[data-label]::before {
			content: attr(data-label) ': ';
			color: var(--muted);
			font-family: var(--body);
		}
	}
	.servedt .id {
		font-size: var(--text-xs);
		color: var(--muted);
		overflow-wrap: anywhere;
	}
	.runnotes {
		min-width: 0;
	}
	.served {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: var(--sp-5) var(--sp-7);
	}
	@media (max-width: 760px) {
		.served {
			grid-template-columns: minmax(0, 1fr);
			gap: var(--sp-4);
		}
	}
	.served li {
		padding-top: var(--sp-3);
		border-top: 1px solid var(--line);
		font-size: var(--text-sm);
		line-height: var(--lh-body);
		color: var(--muted);
	}
	.served li > code:first-child {
		color: var(--ink);
	}
	.scope code {
		overflow-wrap: anywhere;
	}
	.vtext {
		display: grid;
		gap: var(--sp-3);
		min-width: 0;
	}
	.vtext .small {
		font-size: var(--text-sm);
	}
	.verifynote p {
		margin: 0;
		line-height: var(--lh-body);
		max-width: 68ch;
	}
	/* W12: the page's two sections open like every other page's (number, rule, h2). */
	.span {
		grid-column: 1 / -1;
	}
	.verifynote code {
		overflow-wrap: anywhere;
	}
</style>
