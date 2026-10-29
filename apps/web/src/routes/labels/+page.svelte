<script lang="ts">
	import { onMount } from 'svelte';
	import Pill, { type PillState } from '#lib/components/Pill.svelte';
	import { FALLBACK, PUBLIC } from '#lib/mode.ts';
	import Skeleton from '#lib/components/Skeleton.svelte';
	import Hash from '#lib/components/Hash.svelte';
	import OnyxSlab from '#lib/components/OnyxSlab.svelte';
	import Donut from '#lib/components/Donut.svelte';
	import SaltedRowDemo from '#lib/components/SaltedRowDemo.svelte';
	import Bitcoin from '@lucide/svelte/icons/bitcoin';
	import KeyRound from '@lucide/svelte/icons/key-round';
	import { loadDemo, loadInput, whenIdle, type Demo } from '#lib/data.ts';
	import { verifier, type Inspected, type Vector } from '#lib/verifier.ts';
	import { entityName } from '#lib/live.ts';
	import { group, short } from '#lib/format.ts';

	let demo = $state.raw<Demo | null>(null);
	let filter = $state('');
	let attPill = $state<{ state: PillState; text: string }>({ state: 'idle', text: 'Not checked' });
	let attNote = $state('');
	let vectors = $state<Vector[] | null>(null);
	let vecPill = $state<{ state: PillState; text: string }>({ state: 'idle', text: 'Not run' });

	// Scoring playground (ADR-0016): Brier over K registered companies, from Rust.
	const K = 50;
	let truth = $state(70);
	let pA = $state(70);
	let pB = $state(99);
	let scores = $state<{ a: number; b: number; best: number } | null>(null);

	// The label table is decoded from the pinned recipe input (the verifier
	// refuses any label that does not name an entity), not read from the export.
	let set = $state.raw<Inspected['label_set'] | null>(null);
	let pinnedInput = $state(false);
	let pinnedSet = $state<boolean | null>(null);
	let loadError = $state('');
	onMount(() => {
		loadDemo()
			.then(async (d) => {
				// Attestation, vectors and scoring show at once; only the label
				// table waits for the verifier to decode the pinned input.
				demo = d;
				whenIdle(async () => {
					scoring = true;
					try {
						if (PUBLIC) {
							// No label rows in the public build (AUD-2-F1): check the set's hash only.
							const pin = await verifier.pinned();
							pinnedSet = d.label_set.content_hash === pin.label_set;
							return;
						}
						const [pin, i] = await Promise.all([verifier.pinned(), verifier.inspectInput(await loadInput(d.runs[0].input))]);
						set = i.label_set;
						pinnedInput = i.input_keccak === pin.input_keccak;
					} catch (e) {
						loadError = String(e);
					}
				});
			})
			.catch((e) => (loadError = String(e)));
	});
	const fail = (e: unknown) => ({ state: 'bad' as PillState, text: e instanceof Error ? e.message : String(e) });

	const shown = $derived(set ? set.labels.filter((l) => !filter || l.address.includes(filter) || l.entity.includes(filter)) : []);
	// Ten thousand rows would stall the page; the filter narrows, the table shows the first ones.
	const LIMIT = 200;
	const visible = $derived(shown.slice(0, LIMIT));
	const matchText = $derived(`${group(shown.length)} match${shown.length > LIMIT ? ` · first ${LIMIT} shown` : ''}`);
	const kindName: Record<string, string> = { Exchange: 'Exchanges', Etf: 'ETFs', Fund: 'Funds', Protocol: 'Protocols', Custodian: 'Custodians' };
	// Evidence level per label (ADR-0031): a first-party signature (OKX PoR, the
	// signed rows at 990,000 ppm) is level 3; a list a company published
	// without a signature is a claim, level 4. A shard's level is its weakest
	// input, so every flow over this set is level 4 (IDX-R1-F5).
	const SIGNED_PPM = 990_000;
	const byEntity = $derived.by(() => {
		if (!set) return [];
		const m = new Map<string, { kind: string; n: number; signed: number }>();
		for (const l of set.labels) {
			const e = m.get(l.entity) ?? { kind: l.kind, n: 0, signed: 0 };
			e.n++;
			if (l.confidence_ppm >= SIGNED_PPM) e.signed++;
			m.set(l.entity, e);
		}
		return [...m.entries()].sort((a, b) => (a[1].kind === b[1].kind ? b[1].n - a[1].n : a[1].kind.localeCompare(b[1].kind)));
	});
	// Labels and companies per kind, biggest first.
	const groups = $derived.by(() => {
		if (!set) return [];
		const m = new Map<string, { n: number; companies: Set<string> }>();
		for (const l of set.labels) {
			const g = m.get(l.kind) ?? { n: 0, companies: new Set<string>() };
			g.n++;
			g.companies.add(l.entity);
			m.set(l.kind, g);
		}
		return [...m.entries()].sort((a, b) => b[1].n - a[1].n);
	});
	const companies = $derived(groups.reduce((a, [, g]) => a + g.companies.size, 0));

	async function checkAtt(tamper: boolean) {
		if (!demo) return;
		const a = demo.attestation;
		const statement = tamper ? a.statement.replace('nonce: 09', 'nonce: 0a') : a.statement;
		if (tamper && statement === a.statement) {
			attPill = { state: 'bad', text: 'Could not change the text: unexpected statement' };
			return;
		}
		attPill = { state: 'run', text: 'Checking' };
		try {
			const v = await verifier.verifyAttestation(statement, a.address_sig, a.entity_key, a.entity_sig, 921080, 921082);
			attPill = v === 'valid' ? { state: 'good', text: 'Valid: both signatures check out' } : { state: 'bad', text: `Refused: ${v}` };
			attNote = tamper ? 'One character of the signed text was changed after signing.' : `Company key pinned in the verifier: ${await verifier.demoEntityKey()}`;
		} catch (e) {
			attPill = fail(e);
		}
	}

	async function runVectors() {
		vecPill = { state: 'run', text: 'Running' };
		try {
			const v = await verifier.bip322Vectors();
			vectors = v;
			const bad = v.filter((x) => x.got !== x.expect).length;
			vecPill = bad || !v.length ? { state: 'bad', text: `${bad} differ` } : { state: 'good', text: `All ${v.length} match` };
		} catch (e) {
			vecPill = fail(e);
		}
	}

	async function mean(pct: number, truth: number): Promise<number> {
		const ppm = pct * 10000;
		const hit = BigInt(await verifier.brierTerm(ppm, true, K));
		const miss = BigInt(await verifier.brierTerm(ppm, false, K));
		const m = (BigInt(truth) * hit + BigInt(100 - truth) * miss) / 100n;
		return Number(m / 1000000n) / 1e6;
	}
	// Results arrive asynchronously; keep only the latest while a slider moves.
	let seq = 0;
	// W4: the first scores wait for the browser to be idle (after first paint), like the table.
	let scoring = $state(false);
	$effect(() => {
		const [t, a, b] = [truth, pA, pB];
		if (!scoring) return;
		const mine = ++seq;
		Promise.all([mean(a, t), mean(b, t), mean(t, t)]).then(([x, y, z]) => {
			if (mine === seq) scores = { a: x, b: y, best: z };
		});
	});
</script>

<svelte:head><title>Labels · Onyx</title></svelte:head>

<OnyxSlab>
	<h1>Address labels</h1>
	<div class="slabside" data-testid="slabside">
		<p class="slablede">Exchanges, ETFs, funds, protocols. Never people.</p>
		<p class="slablede">Test labels. Sources are first-party, but sharing rights are not cleared yet.</p>
	</div>
</OnyxSlab>

<div class="wrap">
	{#if loadError}
		<p class="section">Could not load the demo data: {loadError}</p>
	{:else if !demo}
		<Skeleton rows={4} label="Loading the label set" />
	{:else}
		<section class="section">
			<!-- Fixed slot: the decoded table replaces the placeholder without moving the page. -->
			<div class="setslot">
			{#if PUBLIC}
			<div class="panel" data-testid="public-label-set">
				<div class="sh">
					<h2>Label set</h2>
					{#if pinnedSet === null}<Pill state="idle" text="Checking" />{:else if pinnedSet}<Pill state="good" text="Pinned label set" />{:else}<Pill state="bad" text="Not the pinned label set" />{/if}
				</div>
				<div class="pubrow">
				<div class="setmeta">
					<span class="chip">{demo.label_set.name} v{demo.label_set.version}</span>
					<span class="muted">{group(demo.label_set.count)} labels</span>
					<Hash value={demo.label_set.content_hash} short />
				</div>
				<p class="muted">This public build ships no label rows: the test labels' sharing rights are not cleared. The page checks the set's hash against the one pinned in the code; {FALLBACK ? 'company totals computed from it are held back until data rights are cleared.' : 'flows over it are checked against pinned roots.'}</p>
				</div>
			</div>
			{:else if !set}
				<!-- The heading shows while the set decodes: the section opener (app.css) waits for an
				     h2, and when it came with the table it moved this slot 28 px (W9, CLS 0.014). -->
				<div class="panel decoding"><div class="sh"><h2>Label set</h2></div><Skeleton rows={1} label="Decoding the label set" /></div>
			{:else}
			<div class="panel">
				<div class="sh">
					<h2>Label set</h2>
					{#if pinnedInput}<Pill state="good" text="Decoded from the pinned input" />{:else}<Pill state="bad" text="Not the pinned input" />{/if}
				</div>
				<div class="setrow">
					<Donut parts={groups.map(([k, g]) => ({ label: `${kindName[k] ?? k} · ${g.companies.size}`, value: g.n }))} />
					<div class="setmeta">
						<span class="chip">{set.name} v{set.version}</span>
						<span class="muted">{companies} companies</span>
						<Hash value={set.content_hash} short />
					</div>
				</div>
				<div class="tablebox groups" data-testid="label-groups">
					<table>
						<caption class="muted" data-testid="label-groups-caption"
							>Evidence level of each company's labels. A flow computed with this set uses the unsigned lists too, so every flow is a
							claim, level 4.</caption
						>
						<thead><tr><th>Company</th><th class="kindcol">Kind</th><th class="r">Labels</th><th>Label evidence</th></tr></thead>
						<tbody>
							{#each byEntity as [entity, e] (entity)}
								<tr>
									<td>{entityName(entity)}</td>
									<td class="kindcol">{kindName[e.kind] ?? e.kind}</td>
									<td class="r mono">{group(e.n)}</td>
									<td class="lvl">{e.signed === e.n ? 'Level 3 · signed' : e.signed > 0 ? 'Level 3 and 4' : 'Level 4 · published (a claim)'}</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
				<details class="more">
					<summary>Browse all {group(set.labels.length)} labels</summary>
					<div class="row">
						<input id="filter" type="search" placeholder="Filter by address or company" bind:value={filter} aria-label="Filter labels" />
						<span class="muted">{matchText}</span>
					</div>
					<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
					<div class="tablebox scroll" tabindex="0" role="region" aria-label="Labels{filter ? ' matching the filter' : ''}">
						<table class="stack">
							<thead><tr><th>Address</th><th>Company</th><th>Kind</th><th class="r">Confidence</th></tr></thead>
							<tbody>
								{#each visible as l (l.address)}
									<tr><td class="mono" data-label="Address">{short(l.address, 22, 8)}</td><td class="mono" data-label="Company">{l.entity}</td><td data-label="Kind">{l.kind}</td><td class="r" data-label="Confidence">{(l.confidence_ppm / 10000).toFixed(0)}%</td></tr>
								{/each}
							</tbody>
						</table>
					</div>
				</details>
			</div>
			{/if}
			</div>
		</section>

		<section class="section">
			<div class="panel attpanel">
				<div class="sh"><h2>Attestation</h2><Pill live state={attPill.state} text={attPill.text} /></div>
				<div class="attest">
					<figure class="stmt" data-testid="att-message">
						<figcaption>Signed message <span class="muted">· the text both keys signed</span></figcaption>
						<pre class="code">{demo.attestation.statement}</pre>
					</figure>
					<div class="seals {attPill.state}">
						<div class="seal"><Bitcoin size={22} /><b>Address</b><small>BIP-322</small></div>
						<span class="plus">+</span>
						<div class="seal"><KeyRound size={22} /><b>Company key</b><small>EIP-191</small></div>
					</div>
				</div>
				<div class="row">
					<button type="button" onclick={() => checkAtt(false)}>Check both signatures</button>
					<button type="button" class="ghost" onclick={() => checkAtt(true)}>Change the text, then check</button>
				</div>
				{#if attNote}<p class="log">{attNote}</p>{/if}
				<!-- W12 round 7: each panel's note is its footer row (it floated as small print). -->
				<div class="pfoot"><p class="fine">Test company and test keys: this shows the checker works.</p></div>
			</div>
		</section>

		<!-- ROADMAP D30 (PP3): one salted row, checked against the pin in the verifier. -->
		<section class="section">
			<SaltedRowDemo />
		</section>

		<section class="section two">
			<div class="panel">
				<div class="sh"><h2>BIP-322 test vectors</h2><Pill live state={vecPill.state} text={vecPill.text} /></div>
				<!-- W3: each dot's result in words too (colour alone is not enough). -->
				<ul class="dots" aria-label="Test vector results" aria-hidden={vectors ? undefined : 'true'}>
					{#each vectors ?? Array(29).fill(null) as v, i (i)}
						<li class="d {v ? (v.got === v.expect ? 'ok' : 'no') : ''}" title={v ? `${v.variant} ${v.body}: ${v.got}` : 'not run'}>
							<span class="sr-only">{v ? `${v.variant} ${v.body}: ${v.got}, ${v.got === v.expect ? 'matches' : `expected ${v.expect}`}` : `Vector ${i + 1}: not run`}</span>
						</li>
					{/each}
				</ul>
				<!-- The idle tiles are dashed and the pill says "Not run"; a caption here made the panel taller than the Brier panel beside it (W12). -->
				<div class="row"><button type="button" onclick={runVectors} disabled={vecPill.state === 'run'}>Run the vectors</button></div>
				<div class="pfoot"><p class="fine">The <a href="https://github.com/bitcoin/bips/tree/master/bip-0322" target="_blank" rel="noopener">official BIP-322 vectors</a> plus our edge cases. Same answer on every machine.</p></div>
			</div>

			<div class="panel">
				<div class="sh"><h2>Brier score</h2></div>
				<div class="sliders">
					<label for="truth">Label right {truth}% of the time</label>
					<input id="truth" type="range" min="5" max="95" bind:value={truth} />
					<label for="pA">A says {pA}% sure</label>
					<input id="pA" type="range" min="5" max="99" bind:value={pA} />
					<label for="pB">B says {pB}% sure</label>
					<input id="pB" type="range" min="5" max="99" bind:value={pB} />
				</div>
				{#if scores}
					{@const worst = Math.max(scores.a, scores.b, scores.best) || 1}
					<div class="score">
						{#each [['A', scores.a], ['B', scores.b], ['Honest', scores.best]] as const as [k, v] (k)}
							<div class="sb"><span>{k}</span><div class="track"><i class:best={k === 'Honest'} style="width: {(v / worst) * 100}%"></i></div><b>{v.toFixed(3)}</b></div>
						{/each}
					</div>
					<div class="pfoot"><p class="fine">The Brier score is the squared gap between how sure a labeler said it was and what turned out true. Lower is better: saying exactly how sure you are wins.</p></div>
				{/if}
			</div>
		</section>
	{/if}
</div>

<style>
	/* Two panels side by side: the shared .section.two (app.css). */
	.two {
		padding-top: 0;
	}
	.section + .section {
		padding-top: 0;
	}
	.setrow {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-6);
		align-items: center;
	}
	/* W12 round 8: on a wide screen the set's name, company count and hash are a card that fills the
	   right half beside the donut (they sat next to it, the right third of the panel empty). Same
	   height as the donut, so the panel keeps its measured slot. */
	@media (min-width: 641px) {
		.setrow {
			display: grid;
			grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
			align-items: stretch;
		}
		.setrow > :global(.donut) {
			align-self: center;
		}
		.setrow .setmeta {
			align-content: center;
			/* W12 round 10: the inner card's padding (was 16 / 24 px). */
			padding: var(--sp-3) var(--sp-4);
			border: 1px solid var(--line);
			border-radius: var(--radius-md);
			background: var(--panel-2);
		}
	}
	/* The public set: its name, count and hash a card at the left, what this build checks beside it
	   (the right half of the panel stood empty). Under it on a phone. */
	.pubrow {
		display: grid;
		gap: var(--sp-4);
		align-items: center;
	}
	.pubrow > p {
		margin: 0;
		text-wrap: pretty;
	}
	@media (min-width: 641px) {
		.pubrow {
			grid-template-columns: minmax(0, 2fr) minmax(0, 3fr);
		}
		.pubrow .setmeta {
			padding: var(--sp-3) var(--sp-4);
			border: 1px solid var(--line);
			border-radius: var(--radius-md);
			background: var(--panel-2);
		}
	}
	.setmeta {
		display: grid;
		gap: var(--sp-2);
		min-width: 0;
		flex: 1 1 260px;
		justify-items: start;
	}
	/* Measured panel heights: 270 px wide, 431 px at phone width. */
	.setslot {
		--slot: 272px;
		min-height: var(--slot);
		/* A grid child: without this the label table's width pushes the page sideways on phones. */
		min-width: 0;
	}
	@media (max-width: 640px) {
		.setslot {
			--slot: 432px;
		}
	}
	/* The public build draws its set panel at once (no table decodes into it): no slot to hold. */
	.setslot:has(> [data-testid='public-label-set']) {
		min-height: 0;
	}
	.setslot :global(.sk) {
		padding-block: 0;
	}
	.decoding {
		min-height: var(--slot);
		align-content: start;
	}
	/* Grid child of the panel: let the table scroll inside it instead of widening the page. */
	.more {
		min-width: 0;
	}
	.scroll {
		max-height: 340px;
		overflow-y: auto;
	}
	.groups {
		min-width: 0;
		margin-top: var(--sp-3);
	}
	.groups td {
		white-space: normal;
	}
	.groups caption {
		caption-side: top;
		text-align: left;
		padding: 0 0 var(--sp-2);
		font-size: var(--text-sm);
	}
	.groups td.r {
		white-space: nowrap;
	}
	/* Phones: the kind is in the donut legend above; the table keeps company, count and level. */
	@media (max-width: 560px) {
		.groups .kindcol {
			display: none;
		}
	}
	.attest {
		display: grid;
		grid-template-columns: minmax(0, 1.3fr) minmax(0, 1fr);
		gap: var(--sp-4);
		align-items: center;
	}
	@media (max-width: 760px) {
		.attest {
			grid-template-columns: minmax(0, 1fr);
		}
	}
	/* W12 round 10: from 761 px the two buttons sit under the seals in the right column (they sat under
	   the signed text at the left, the seals alone in the middle of an empty right half), so the
	   signed text faces what checks it. Reading order: text, seals, buttons, result, note. */
	@media (min-width: 761px) {
		.attpanel {
			grid-template-columns: repeat(2, minmax(0, 1fr));
			column-gap: var(--sp-4);
		}
		.attpanel > :global(*) {
			grid-column: 1 / -1;
		}
		.attpanel > .attest {
			display: contents;
		}
		.attpanel .stmt {
			grid-column: 1;
			grid-row: 2 / span 2;
		}
		.attpanel .seals {
			grid-column: 2;
			grid-row: 2;
			align-self: end;
		}
		.attpanel > .row {
			grid-column: 2;
			grid-row: 3;
			align-self: start;
			justify-content: center;
		}
	}
	/* W12 round 11: where the two buttons cannot share a line (the right column at 761-1,000 px, a
	   phone), they stack at one width (they wrapped to two rows of different widths). */
	@media (min-width: 761px) and (max-width: 1000px), (max-width: 560px) {
		.attpanel > .row {
			flex-direction: column;
			align-items: stretch;
		}
		.attpanel > .row > button {
			justify-content: center;
		}
	}
	/* The attestation's signed text, framed and named. */
	.stmt {
		margin: 0;
		min-width: 0;
		display: grid;
		gap: var(--sp-2);
		align-content: start;
	}
	.stmt figcaption {
		font-family: var(--mono);
		font-size: var(--text-xs);
		text-transform: uppercase;
		letter-spacing: 0.08em;
		color: var(--ink);
	}
	.stmt figcaption .muted {
		text-transform: none;
		letter-spacing: 0;
		font-family: var(--body);
	}
	.stmt pre {
		margin: 0;
		white-space: pre-wrap;
		word-break: break-all;
		border: 1px solid var(--line);
		border-radius: var(--radius-md);
	}
	/* Status that is not a result yet ("Not checked", "Not run") reads as plain text with a dot, not as
	   a button: no outline, regular weight. Results keep their coloured pill. */
	.wrap :global(.pill.idle) {
		border-color: transparent;
		padding-inline: 0;
		font-weight: 400;
	}
	.wrap :global(.pill.idle)::before {
		content: '';
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: currentColor;
		opacity: 0.6;
	}
	.seals {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: var(--sp-3);
	}
	.seal {
		width: 108px;
		height: 108px;
		border-radius: 50%;
		display: grid;
		place-items: center;
		align-content: center;
		gap: var(--sp-0);
		border: 2px dashed var(--line);
		color: var(--muted);
		text-align: center;
		transition: all var(--dur-slow) var(--ease);
	}
	.seal b {
		font-size: var(--text-sm);
		color: var(--ink);
	}
	.seal small {
		font-family: var(--mono);
		font-size: var(--text-xs);
	}
	.seals.good .seal {
		border: 2px solid var(--good);
		color: var(--good);
		background: var(--good-soft);
		box-shadow: var(--shadow-ring-good);
	}
	.seals.bad .seal {
		border-color: var(--bad);
		color: var(--bad);
	}
	.plus {
		color: var(--muted);
		font-size: var(--text-lg);
	}
	.dots {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		grid-template-columns: repeat(auto-fill, 22px);
		gap: var(--sp-2);
	}
	/* W12 round 8: beside the taller Brier card the 29 results are a grid that fills the card's width
	   (two rows of 22 px squares left half the card empty above its footer). */
	/* W12 round 12: the panel ends on its control. Beside the taller Brier panel the spare height sat
	   between the run button and the footer (62 px at 1,280 px); the results' grid now takes it, its
	   squares staying under the title, so the button sits one gap above the footer like Explorer's
	   paired panels (round 10). From 1,100 px 10 a row stays: 8 a row would make this panel 50 px
	   taller than the Brier panel and move the empty band there. */
	@media (min-width: 861px) {
		.dots {
			grid-template-columns: repeat(10, minmax(0, 1fr));
			gap: var(--sp-3);
			flex-grow: 1;
			align-content: start;
		}
		/* Between 861 and 1,058 px the panels are narrower and the Brier panel taller, so 8 a row (four
		   rows of larger squares) fill the height the 10-a-row grid left empty (93 px at 1,000 px).
		   Review of round 12: from 1,059 px the Brier note fits on two lines and that panel is 20 px
		   shorter, so 8 a row (squares 46-47 px) made this panel up to 50 px taller and left the empty
		   band in the Brier panel; 10 a row from there, as from 1,100 px. */
		@media (max-width: 1058px) {
			.dots {
				grid-template-columns: repeat(8, minmax(0, 1fr));
			}
		}
		.dots .d {
			width: auto;
			height: auto;
			aspect-ratio: 1;
		}
	}
	.d {
		width: 22px;
		height: 22px;
		border-radius: var(--radius-sm);
		background: var(--code-bg);
		border: 1px solid var(--line);
		transition: background var(--dur-slow) var(--ease);
	}
	/* Not run yet: an empty dashed slot waiting for its result (it read as a broken grey tile). */
	.d:not(.ok, .no) {
		background: transparent;
		border-style: dashed;
	}
	.d.ok {
		background: var(--good);
		border-color: var(--good);
	}
	.d.no {
		background: var(--bad);
		border-color: var(--bad);
	}
	/* W12 round 8 review: each result also carries a shape, a tick for a match and a cross for a
	   difference (green and red alone fail a colour-blind reader, W3); a square not run stays empty.
	   Drawn as a mask, so no font is needed and no text reaches the accessibility tree (the
	   sr-only line names the vector and its result). */
	.d {
		display: grid;
		place-items: center;
	}
	.d.ok::before,
	.d.no::before {
		content: '';
		width: 64%;
		height: 64%;
		background: var(--accent-ink);
		mask: var(--glyph) center / contain no-repeat;
	}
	.d.ok {
		--glyph: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='black' stroke-width='3.5' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='M20 6 9 17l-5-5'/%3E%3C/svg%3E");
	}
	.d.no {
		--glyph: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='black' stroke-width='3.5' stroke-linecap='round'%3E%3Cpath d='M18 6 6 18M6 6l12 12'/%3E%3C/svg%3E");
	}
	.sliders {
		display: grid;
		gap: var(--sp-1);
	}
	.sliders label {
		font-size: var(--text-sm);
		color: var(--muted);
	}
	.score {
		display: grid;
		gap: var(--sp-2);
	}
	.sb {
		display: grid;
		grid-template-columns: 56px minmax(0, 1fr) 54px;
		gap: var(--sp-3);
		align-items: center;
		font-size: var(--text-sm);
	}
	/* W12 round 14: from 1,059 px (where the vectors run 10 a row) the Brier panel is a form: each
	   slider's label beside its slider, and the scores' names in the same column, so the sliders' tracks
	   and the score bars start on one edge. Its labels each took a line over their slider, so this panel
	   stood 45-49 px taller than the test vectors beside it and left that band empty above the vectors'
	   button (round 13 tried grid counts for the squares; each one moved the band). Now the vectors panel
	   is the taller of the two; the Brier panel's few spare pixels go between its slider rows. */
	@media (min-width: 1059px) {
		.section.two {
			--lab: 11rem;
		}
		.sliders {
			grid-template-columns: var(--lab) minmax(0, 1fr);
			column-gap: var(--sp-3);
			row-gap: var(--sp-2);
			align-items: center;
			flex-grow: 1;
			align-content: space-between;
		}
		.sb {
			grid-template-columns: var(--lab) minmax(0, 1fr) 54px;
		}
	}
	.sb b {
		font-family: var(--mono);
		font-weight: 500;
		text-align: right;
	}
	.sb .track {
		height: 10px;
		border-radius: var(--radius-xs);
		background: var(--code-bg);
		overflow: hidden;
	}
	.sb i {
		display: block;
		height: 100%;
		background: var(--btc);
		border-radius: var(--radius-xs);
	}
	.sb i.best {
		background: var(--good);
	}
</style>
