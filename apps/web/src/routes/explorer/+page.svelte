<script lang="ts">
	import { onMount } from 'svelte';
	import Pill, { type PillState } from '#lib/components/Pill.svelte';
	import { FALLBACK, PUBLIC } from '#lib/mode.ts';
	import { HELD_BACK } from '#lib/components/HeldBack.svelte';
	import Skeleton from '#lib/components/Skeleton.svelte';
	import Hash from '#lib/components/Hash.svelte';
	import OnyxSlab from '#lib/components/OnyxSlab.svelte';
	import MiniTree from '#lib/components/MiniTree.svelte';
	import Marquee from '#lib/components/Marquee.svelte';
	import FlowBars from '#lib/components/FlowBars.svelte';
	import Blocks from '@lucide/svelte/icons/blocks';
	import Lock from '@lucide/svelte/icons/lock';
	import Cpu from '@lucide/svelte/icons/cpu';
	import ShieldCheck from '@lucide/svelte/icons/shield-check';
	import GitMerge from '@lucide/svelte/icons/git-merge';
	import Activity from '@lucide/svelte/icons/activity';
	import { loadDemo, loadInput, loadRecipe, type Demo } from '#lib/data.ts';
	import { checkLiveHeaders } from '#lib/live.ts';
	import { verifier, type FlowRow, type Inspected, type Pinned, type Run } from '#lib/verifier.ts';
	import { btc, int, short } from '#lib/format.ts';

	let demo = $state.raw<Demo | null>(null);
	let rerun = $state<Run | null>(null);
	let rerunMs = $state(0);
	let rerunPill = $state<{ state: PillState; text: string }>({ state: 'idle', text: 'Not run' });
	let headerState = $state<Record<number, PillState>>({});
	let headerPill = $state<{ state: PillState; text: string }>({ state: 'idle', text: 'Not checked' });
	let proofRow = $state(0);
	let proofPill = $state<{ state: PillState; text: string }>({ state: 'idle', text: 'Not checked' });
	let proofLog = $state('');
	let scenario = $state('honest');
	let valPill = $state<{ state: PillState; text: string }>({ state: 'idle', text: 'Not run' });
	let valLog = $state<string[]>([]);

	let pin = $state.raw<Pinned | null>(null);
	let inspected = $state.raw<Inspected | null>(null);
	let liveRows = $state.raw<FlowRow[]>([]);
	let loadError = $state('');

	// Tables show what is decoded from the hashed bytes, not the export's own fields.
	onMount(() => {
		loadDemo()
			.then(async (d) => {
				pin = await verifier.pinned();
				// The public build ships no recipe input: it holds the label rows (AUD-2-F1).
				if (!PUBLIC) inspected = await verifier.inspectInput(await loadInput(d.runs[0].input));
				// A rights-fallback build ships no live rows (ROADMAP L14).
				if (d.live && !FALLBACK) liveRows = await Promise.all(d.live.rows.map((r) => verifier.decodeLeaf(r.leaf)));
				demo = d;
			})
			.catch((e) => (loadError = String(e)));
	});
	const fail = (e: unknown) => ({ state: 'bad' as PillState, text: e instanceof Error ? e.message : String(e) });

	async function rerunRecipe() {
		if (!demo) return;
		rerunPill = { state: 'run', text: 'Running the canonical engine' };
		const t = performance.now();
		try {
			const r = await verifier.runRecipe(await loadRecipe(), await loadInput(demo.runs[0].input));
			rerunMs = performance.now() - t;
			rerun = r;
			const ok = r.outcome === 'Ok' && r.root === pin?.root;
			rerunPill = ok ? { state: 'good', text: 'Same root as the pinned one' } : { state: 'bad', text: 'Root differs' };
		} catch (e) {
			rerunPill = fail(e);
		}
	}

	async function checkHeaders() {
		if (!demo?.live || !pin) return;
		headerPill = { state: 'run', text: 'Checking' };
		const r = await checkLiveHeaders(demo, pin, (h, st) => (headerState[h] = st));
		headerPill = r.state === 'good' ? { state: 'good', text: `${r.text}, ending at the pinned tip` } : r;
	}

	async function checkProof(tamper: boolean) {
		if (!demo || !pin) return;
		try {
		const run = demo.runs[0];
		const p = run.proofs![proofRow];
		let leaf = run.rows[proofRow].leaf;
		if (tamper) {
			// Flip the lowest bit of net_sats (the 25th byte from the end): one satoshi.
			const b = leaf.slice(2);
			const i = b.length - 50;
			const byte = (parseInt(b.slice(i, i + 2), 16) ^ 1).toString(16).padStart(2, '0');
			leaf = '0x' + b.slice(0, i) + byte + b.slice(i + 2);
		}
		const ok = await verifier.verifyProof(pin.root, leaf, p.index, p.leaf_count, p.siblings);
		proofLog = `Row ${run.rows[proofRow].entity}${tamper ? ', changed by 1 sat' : ''} · ${p.siblings.length} sibling hash(es) · pinned root ${short(pin.root)}`;
		proofPill = ok
			? { state: 'good', text: 'Valid: this row is in the committed data' }
			: { state: 'bad', text: 'Fails: the row was changed' };
		} catch (e) {
			proofPill = fail(e);
		}
	}

	async function validate() {
		if (!demo || !pin) return;
		valPill = { state: 'run', text: 'Checking' };
		try {
		const pinned = pin.root;
		const input = await loadInput(demo.runs[0].input);
		if (scenario === 'blocks') {
			const t = input.slice();
			t[8 + 5000] ^= 1;
			const h = await verifier.sourceHash(t);
			valLog = [`Hash of the submitted blocks: ${short(h)}`, `Pinned snapshot hash: ${short(demo.snapshot.content_hash)}`];
			valPill = h === demo.snapshot.content_hash
				? { state: 'bad', text: 'Not caught' }
				: { state: 'good', text: 'Refused before running: the blocks are not the pinned ones' };
			return;
		}
		const claimed = scenario === 'honest' ? pinned : demo.runs[1].root;
		const r = await verifier.runRecipe(await loadRecipe(), input);
		valLog = [`Miner's root: ${short(claimed)}`, `Validator's rerun: ${short(r.root)}`];
		valPill = r.root === claimed
			? { state: 'good', text: 'Match: the miner is paid' }
			: { state: 'bad', text: 'Mismatch: the miner loses its bond' };
		} catch (e) {
			valPill = fail(e);
		}
	}
</script>

<svelte:head><title>Explorer · Onyx</title></svelte:head>

<OnyxSlab>
	<h1>Shard explorer</h1>
	<div class="slabside" data-testid="slabside">
		<p class="meta slabmeta">Round {demo?.commitment.round ?? 1} · shard {demo ? short(demo.runs[0].shard_id, 8, 6) : '…'}{#if demo?.blocks.length}{' '}· blocks {int(demo.blocks[0].height)}–{int(demo.blocks[demo.blocks.length - 1].height)}{/if}</p>
	</div>
</OnyxSlab>

<div class="wrap">
	{#if loadError}
		<p class="section">Could not load the demo data: {loadError}</p>
	{:else if !demo || !pin || (!PUBLIC && !inspected)}
		<Skeleton rows={4} label={PUBLIC ? 'Loading the shard and its pinned roots' : 'Loading the shard and decoding its inputs'} />
	{:else}
		<ol class="timeline">
			{#if PUBLIC || !inspected}
			<li class="step">
				<span class="rail {demo.runs[0].root === pin.root ? 'good' : 'bad'}"><Blocks size={18} /></span>
				<div class="panel" data-testid="public-input">
					<div class="sh"><h2>Input blocks</h2>{#if demo.runs[0].root === pin.root}<Pill state="good" text="Pinned root" />{:else}<Pill state="bad" text="Not the pinned root" />{/if}</div>
					<div class="cards">
						{#each demo.blocks as b (b.height)}
							<div class="bcard"><span class="mono h">#{int(b.height)}</span><span class="mono x" title={b.hash}>{short(b.hash, 4, 6)}</span><span class="mono t">{int(b.txs)} tx</span></div>
						{/each}
					</div>
					<!-- W12 round 7: the panel's note is its footer row (it floated as small print under the cards). -->
					<div class="pfoot">
					{#if FALLBACK}
						<p class="fine" data-testid="explorer-held-back">The recipe input holds the label rows, so this build does not ship it and does not rerun the recipe here. {HELD_BACK} The shard's rows (company flows) are left out; its root and commitment stay.</p>
					{:else}
						<p class="fine">The recipe input holds the label rows, so this public build does not ship it and does not rerun the recipe here: the test labels' sharing rights are not cleared. The rows below are checked against the pinned root.</p>
					{/if}
					</div>
				</div>
			</li>
			{:else}
			<li class="step">
				<span class="rail {inspected.input_keccak === pin.input_keccak ? 'good' : 'bad'}"><Blocks size={18} /></span>
				<div class="panel">
					<div class="sh"><h2>Input blocks</h2>{#if inspected.input_keccak === pin.input_keccak}<Pill state="good" text="Pinned input" />{:else}<Pill state="bad" text="Not the pinned input" />{/if}</div>
					<div class="cards">
						{#each inspected.blocks as b (b.height)}
							<div class="bcard"><span class="mono h">#{int(b.height)}</span><span class="mono x" title={b.hash}>{short(b.hash, 4, 6)}</span><span class="mono t">{int(b.txs)} tx</span></div>
						{/each}
					</div>
					<details class="more"><summary>Hashes</summary>
						<dl class="kv">
							<dt>Block data</dt><dd><Hash value={inspected.source_hash} /></dd>
							<dt>Recipe input</dt><dd><Hash value={inspected.input_keccak} /></dd>
						</dl>
					</details>
				</div>
			</li>
			{/if}

			<li class="step">
				<span class="rail {''}"><Lock size={18} /></span>
				<div class="panel">
					<div class="sh"><h2>Commit, then reveal</h2></div>
					<div class="seal-row">
						<div class="sealbox"><span class="meta">Commit hash</span><span class="mono">{short(demo.commitment.commit_hash, 10, 6)}</span></div>
						<span class="arrow">→</span>
						<div class="sealbox open"><span class="meta">Revealed root</span><span class="mono">{short(demo.runs[0].root, 10, 6)}</span></div>
					</div>
					<details class="more"><summary>Details (as published)</summary>
						<dl class="kv">
							<dt>Shard</dt><dd><Hash value={demo.runs[0].shard_id} /></dd>
							<dt>Recipe</dt><dd><Hash value={demo.recipe_id} /></dd>
							<dt>Engine</dt><dd class="mono">{demo.host_id}</dd>
							<dt>Miner (test key)</dt><dd><Hash value={demo.commitment.miner} /></dd>
						</dl>
					</details>
				</div>
			</li>

			{#if !PUBLIC}
			<!-- W12 round 9: the rerun and the validator check are one button each; on a wide screen they
			     sit side by side (each a full-width card with its right half empty), until the rerun's
			     company bars need the width. -->
			<li class="step" class:half={!rerun} data-testid="step-rerun">
				<span class="rail {rerunPill.state}"><Cpu size={18} /></span>
				<div class="panel">
					<div class="sh"><h2>Rerun in this browser</h2><Pill live state={rerunPill.state} text={rerunPill.text} /></div>
					<div class="row">
						<button type="button" onclick={rerunRecipe} disabled={rerunPill.state === 'run'}>Rerun the recipe</button>
						{#if rerun}<span class="mono muted">{int(rerun.fuel)} fuel · {(rerunMs / 1000).toFixed(1)} s</span>{/if}
					</div>
					{#if rerun}
						<div class="flows">
							{#each rerun.rows as r (r.entity)}
								<div class="fl"><span class="mono">{r.entity}</span><FlowBars received={r.received_sats} sent={r.sent_sats} net={r.net_sats} /></div>
							{/each}
						</div>
					{/if}
				</div>
			</li>

			<li class="step" class:half={!rerun} data-testid="step-validate">
				<span class="rail {valPill.state === 'idle' || valPill.state === 'run' ? valPill.state : 'good'}"><ShieldCheck size={18} /></span>
				<div class="panel">
					<div class="sh"><h2>Validator check</h2><Pill live state={valPill.state} text={valPill.text} /></div>
					<div class="row">
						<label for="scenario" class="muted">The miner</label>
						<select id="scenario" bind:value={scenario}>
							<option value="honest">did the work honestly</option>
							<option value="labels">used a smaller label set</option>
							<option value="blocks">changed one byte of the blocks</option>
						</select>
						<button type="button" onclick={validate} disabled={valPill.state === 'run'}>Validate</button>
					</div>
					{#if valLog.length}<div class="log">{#each valLog as l (l)}<span>{l}</span>{/each}</div>{/if}
				</div>
			</li>

			{/if}

			{#if !FALLBACK}
			<li class="step">
				<span class="rail {proofPill.state}"><GitMerge size={18} /></span>
				<div class="panel">
					<div class="sh"><h2>Row proof</h2><Pill live state={proofPill.state} text={proofPill.text} /></div>
					<div class="proof">
						<div class="tree"><MiniTree labels leaves={demo.runs[0].rows.length} lit={proofRow} broken={proofPill.state === 'bad'} /></div>
						<div class="ctl">
							<select id="proofRow" bind:value={proofRow} aria-label="Row">
								{#each demo.runs[0].rows as r, i (r.entity)}<option value={i}>{r.entity}</option>{/each}
							</select>
							<button type="button" onclick={() => checkProof(false)}>Check the proof</button>
							<button type="button" class="ghost" onclick={() => checkProof(true)}>Change 1 sat, then check</button>
							{#if proofLog}<p class="log">{proofLog}</p>{/if}
						</div>
					</div>
				</div>
			</li>
			{/if}

			{#if demo.live}
				<li class="step">
					<span class="rail {headerPill.state}"><Activity size={18} /></span>
					<div class="panel">
						<div class="sh"><h2>Live blocks</h2><Pill live state={headerPill.state} text={headerPill.text} /></div>
						<Marquee items={demo.live.blocks} states={headerState} />
						<div class="row"><button type="button" onclick={checkHeaders} disabled={headerPill.state === 'run'}>Check all headers</button></div>
						{#if liveRows.some((r) => r.tx_count > 0)}
						<div class="flows" data-testid="live-flows">
						{#each liveRows.filter((r) => r.tx_count > 0) as r (r.entity)}
							<div class="fl"><span class="mono">{r.entity} · {r.tx_count} tx</span><FlowBars received={r.received_sats} sent={r.sent_sats} net={r.net_sats} /></div>
						{/each}
						</div>
						{/if}
						<div class="pfoot">
						{#if FALLBACK}
							<p class="fine">Headers only. {HELD_BACK}</p>
						{:else}
							<p class="fine">Rows checked against the pinned root; this 16 MB range is not rerun here. Test labels.</p>
						{/if}
						</div>
					</div>
				</li>
			{/if}
		</ol>
	{/if}
</div>

<style>
	/* On phones the line wraps once the shard id loads; reserve both lines. */
	@media (max-width: 560px) {
		.slabmeta {
			min-height: 3.1em;
		}
	}
	.timeline {
		list-style: none;
		margin: 0;
		padding: var(--sp-8) 0 var(--sp-6);
		display: grid;
		gap: var(--sp-4);
		position: relative;
	}
	.timeline::before {
		content: '';
		position: absolute;
		left: 19px;
		top: 40px;
		bottom: 40px;
		width: 2px;
		background: linear-gradient(180deg, var(--accent), var(--line));
	}
	.step {
		display: grid;
		grid-template-columns: 40px minmax(0, 1fr);
		gap: var(--sp-4);
		align-items: start;
	}
	@media (min-width: 961px) {
		.timeline {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
		.step {
			grid-column: 1 / -1;
		}
		.step.half {
			grid-column: auto;
			align-items: stretch;
		}
		.step.half > .rail {
			align-self: start;
		}
		.step.half > .panel {
			align-content: start;
		}
		/* W12 round 10: the paired panels end on their controls: each control row sits at its panel's
		   foot, so the two buttons share a line (the rerun panel's half stood empty under its button
		   beside the validator's two rows). The validator's label sits over its select like a form
		   field, so the select and its button share one row. */
		.step.half > .panel {
			display: flex;
			flex-direction: column;
		}
		.step.half > .panel > .row {
			margin-top: auto;
		}
		.step.half > .panel > .row:has(> select) {
			display: grid;
			grid-template-columns: auto auto;
			justify-content: start;
			gap: var(--sp-2) var(--sp-3);
		}
		.step.half > .panel > .row:has(> select) > label {
			grid-column: 1 / -1;
			font-size: var(--text-sm);
		}
		/* W12 round 10 review: once the validator writes its log, the paired panels share three rows
		   (title, controls, log) through a subgrid, so the two buttons stay on one line (with the
		   controls at each panel's foot the rerun button dropped 60 px under Validate). */
		.timeline:has(> .step.half .log) > .step.half {
			grid-row: span 3;
			grid-template-rows: subgrid;
			row-gap: 0;
		}
		.timeline:has(> .step.half .log) > .step.half > .rail {
			grid-row: 1;
		}
		.timeline:has(> .step.half .log) > .step.half > .panel {
			grid-row: 1 / -1;
			display: grid;
			grid-template-rows: subgrid;
			row-gap: 0;
		}
		.timeline:has(> .step.half .log) > .step.half > .panel > .row {
			margin-top: var(--sp-4);
			align-self: end;
		}
		.timeline:has(> .step.half .log) > .step.half > .panel > .log {
			margin-top: var(--sp-4);
		}
	}
	.rail.good {
		color: var(--good);
		border-color: var(--good);
		box-shadow: var(--shadow-ring-good);
	}
	.rail.bad {
		color: var(--bad);
		border-color: var(--bad);
	}
	.rail.run {
		border-color: var(--accent);
	}
	.rail {
		transition:
			color var(--dur-slow) var(--ease),
			border-color var(--dur-slow) var(--ease),
			box-shadow var(--dur-slow) var(--ease);
		position: relative;
		width: 40px;
		height: 40px;
		border-radius: var(--radius-md);
		display: grid;
		place-items: center;
		background: var(--panel);
		border: 1px solid var(--line);
		color: var(--accent);
		box-shadow: var(--shadow);
	}
	.cards {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
		gap: var(--sp-3);
	}
	.bcard {
		display: grid;
		gap: var(--sp-0);
		padding: var(--sp-3);
		border-radius: var(--radius-md);
		border: 1px solid var(--line);
		background: var(--panel-2);
	}
	.bcard .h {
		font-size: var(--text-base);
		color: var(--ink);
	}
	.bcard .x {
		color: var(--muted);
		font-size: var(--text-xs);
	}
	/* One style for a block's transaction count, as on the live blocks ("3,868 tx"). */
	.bcard .t {
		font-size: var(--text-xs);
		color: var(--muted);
	}
	.seal-row {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--sp-3);
	}
	.sealbox {
		display: grid;
		gap: var(--sp-1);
		padding: var(--sp-3) var(--sp-4);
		border-radius: var(--radius-md);
		border: 1px dashed var(--line);
		background: var(--panel-2);
		min-width: 0;
	}
	.sealbox.open {
		border: 1px solid var(--accent);
		background: var(--accent-soft);
	}
	.arrow {
		color: var(--muted);
		font-size: var(--text-lg);
	}
	@media (max-width: 560px) {
		.seal-row {
			flex-direction: column;
			align-items: flex-start;
		}
		.arrow {
			transform: rotate(90deg);
			margin-left: var(--sp-4);
		}
	}
	/* W12 round 6: each company's in / out bars take a column on a wide screen (three full-width
	   bar pairs stacked 380 px deep); each pair keeps its own scale, as before. */
	.flows {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 260px), 1fr));
		gap: var(--sp-5) var(--sp-6);
	}
	.fl {
		display: grid;
		gap: var(--sp-2);
	}
	/* W12 round 6: the tree at the left with its controls and result beside it under a hairline (the
	   tree sat centred in two thirds of the panel, the controls at the far right, the left third
	   empty). Phones: the tree over the controls. */
	.proof {
		display: grid;
		grid-template-columns: minmax(0, 340px) minmax(0, 1fr);
		gap: var(--sp-4) var(--sp-8);
		align-items: center;
	}
	@media (max-width: 640px) {
		.proof {
			grid-template-columns: minmax(0, 1fr);
		}
	}
	.tree {
		width: 100%;
		max-width: 340px;
		justify-self: start;
	}
	@media (max-width: 640px) {
		.tree {
			max-width: 300px;
			justify-self: center;
		}
	}
	.ctl {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-2) var(--sp-3);
		align-items: center;
		align-content: center;
		min-width: 0;
	}
	@media (min-width: 641px) {
		.ctl {
			align-self: stretch;
			padding-left: var(--sp-6);
			border-left: 1px solid var(--line);
		}
	}
	.ctl > .log {
		flex-basis: 100%;
	}
	/* W12 round 11: the row's select on a line of its own, then the two checks side by side (the
	   second button wrapped alone under the select and the first). */
	.ctl > select {
		flex: 1 1 100%;
	}
	/* Where the two checks cannot share a line (a phone, and the narrow right column at 761-910 px,
	   where they wrapped to two rows of different widths), they stack at the select's width, like
	   Labels'. Measured on the row itself (a container query), not the screen: the column's width,
	   not the viewport's, decides whether 142 + 12 + 206 px fit. */
	.ctl {
		container-type: inline-size;
	}
	@container (max-width: 420px) {
		.ctl > button {
			flex: 1 1 100%;
			justify-content: center;
		}
	}
</style>
