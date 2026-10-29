<script lang="ts">
	// ROADMAP C61: a level-3 answer (a value the source signed itself) with its check state.
	// The browser does not take the API's word: the Rust verifier (verify/src/signed_view.rs)
	// recomputes every hash from the signed payload and compares it with OnyxSignedBook's
	// `settleable` read at a finalized block. No live book is deployed yet (ROADMAP C10, a lead
	// step), so this shows one answer recorded in the local-anvil rehearsal with the chain answers
	// recorded at the same moment, both pinned in the verifier, and says so everywhere. The reader
	// can also read the chain through an RPC of their choice (a public Base Sepolia RPC by default,
	// no API key): against the rehearsal's local chain that is refused, as it must be.
	// Every figure comes from the verifier's checked output, never from the JSON.
	import { onMount } from 'svelte';
	import Pill, { type PillState } from '#lib/components/Pill.svelte';
	import Skeleton from '#lib/components/Skeleton.svelte';
	import { verifier, type SignedCheck } from '#lib/verifier.ts';
	import { dayText } from '#lib/catalog.ts';

	const FILE = 'level3/signed-rehearsal.json';
	const DEFAULT_RPC = 'https://sepolia.base.org';

	let pill = $state<{ state: PillState; text: string }>({ state: 'idle', text: 'Waiting' });
	let check = $state<SignedCheck | null>(null);
	let doc = '';
	let forged = $state<SignedCheck | null>(null);
	let rpc = $state(DEFAULT_RPC);
	let live = $state<SignedCheck | null>(null);
	let liveError = $state('');
	let livePill = $state<{ state: PillState; text: string }>({ state: 'idle', text: 'Not read' });

	const short = (h: string) => `${h.slice(0, 10)}…${h.slice(-4)}`;
	/** "4 Oct 2026, 12:21 UTC" (owner review: one date style in prose and labels). */
	const stamp = (t: number) => {
		const iso = new Date(t * 1000).toISOString();
		return `${dayText(iso.slice(0, 10))}, ${iso.slice(11, 16)} UTC`;
	};
	const grouped = (d: string) => d.replace(/\B(?=(\d{3})+(?!\d))/g, ',');

	async function run() {
		pill = { state: 'run', text: 'Checking' };
		check = null;
		try {
			const r = await fetch(FILE);
			if (!r.ok) throw new Error(`${FILE}: HTTP ${r.status}`);
			doc = await r.text();
			const c = await verifier.signedRehearsal(doc);
			check = c;
			// Data review: a rehearsal answer that checks out gets a neutral badge, not the green "Checks out"
			// of a live value (the section says no live book exists).
			pill = c.ok ? { state: 'idle', text: 'Rehearsal' } : { state: 'bad', text: c.problems[0] ?? 'Does not check out' };
		} catch (e) {
			pill = { state: 'bad', text: String(e) };
		}
	}

	async function tryForged() {
		forged = null;
		if (!doc) return;
		try {
			forged = await verifier.signedRehearsal(await verifier.signedForgedValue(doc));
		} catch (e) {
			forged = { ok: false, problems: [String(e)] } as unknown as SignedCheck;
		}
	}

	/** Only an https RPC, or one on this machine (a local node); never with credentials. */
	function rpcUrl(s: string): URL {
		const u = new URL(s.trim());
		const local = u.protocol === 'http:' && (u.hostname === '127.0.0.1' || u.hostname === 'localhost');
		if (u.protocol !== 'https:' && !local) throw new Error('the RPC must be an https:// URL (or http on 127.0.0.1)');
		if (u.username || u.password) throw new Error('the RPC URL must not carry a user name or password');
		return u;
	}

	async function call(url: URL, method: string, params: unknown[]): Promise<unknown> {
		const ctl = new AbortController();
		const t = setTimeout(() => ctl.abort(), 15000);
		try {
			const r = await fetch(url, {
				method: 'POST',
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
				credentials: 'omit',
				referrerPolicy: 'no-referrer',
				signal: ctl.signal
			});
			if (!r.ok) throw new Error(`${method}: HTTP ${r.status}`);
			const j = (await r.json()) as { result?: unknown; error?: { message?: string } };
			if (j.error) throw new Error(`${method}: ${j.error.message ?? 'error'}`);
			return j.result ?? null;
		} finally {
			clearTimeout(t);
		}
	}

	// The page only carries the reads; the verifier names the calls and decides.
	async function readChain() {
		live = null;
		liveError = '';
		livePill = { state: 'run', text: 'Reading' };
		try {
			const url = rpcUrl(rpc);
			const body = JSON.stringify(JSON.parse(doc).body);
			const calls = await verifier.signedLiveCalls(body);
			const chainId = await call(url, 'eth_chainId', []);
			const fin = (await call(url, 'eth_getBlockByNumber', ['finalized', false])) as { number?: string; hash?: string; timestamp?: string } | null;
			let reads: Record<string, unknown> = { chainId, finalized: fin && { number: fin.number, hash: fin.hash, timestamp: fin.timestamp }, code: '0x', settleable: '0x', factView: '0x' };
			// The book's address is on the pinned chain only: read it there, at the block's hash.
			if (fin?.hash && chainId === `0x${calls.chainId.toString(16)}`) {
				// EIP-1898: the block must be on the RPC's canonical chain (review C61-R1-7).
				const at = { blockHash: fin.hash, requireCanonical: true };
				reads = {
					...reads,
					code: await call(url, 'eth_getCode', [calls.to, at]),
					settleable: await call(url, 'eth_call', [{ to: calls.to, data: calls.settleable }, at]),
					factView: await call(url, 'eth_call', [{ to: calls.to, data: calls.factView }, at])
				};
			}
			const c = await verifier.signedLive(body, JSON.stringify(reads), Math.floor(Date.now() / 1000));
			live = c;
			// Review C61-R2-2: never the rehearsal card's "Checks out": this is one RPC's answer.
			livePill = c.ok ? { state: 'good', text: 'Your RPC agrees' } : { state: 'bad', text: 'Refused' };
		} catch (e) {
			liveError = e instanceof Error ? e.message : String(e);
			livePill = { state: 'bad', text: 'Could not read' };
		}
	}

	onMount(run);
</script>

<section class="section" data-testid="level3">
	<!-- Data review: folded by default (a less central section of a long page). -->
	<details class="secfold" data-testid="level3-fold">
	<summary class="sh">
		<h2>Signed by the source</h2>
		<Pill live state={pill.state} text={pill.text} />
		<p class="foldnote">Values a source signs with its own published key (level 3). No live record yet: a local rehearsal is shown.</p>
	</summary>
	<p class="muted lede">
		Level 3: the source signs each value with a key it publishes on its own website. A book on chain (OnyxSignedBook) records every signed
		report and refuses two values for one fact. The API serves a value only once nobody contradicted it for an hour and a half of chain time,
		and your browser does not take the API's word: it recomputes every hash from the signed value and asks the book, through a chain RPC at a
		finalized block, whether the value may be settled on (for the chain's state it still takes that one RPC's word).
	</p>
	<div class="panel" data-testid="level3-norecord">
		<p class="tight"><b>No live level-3 book yet.</b></p>
		<p class="tight muted">
			The book is not deployed on Base Sepolia yet, and no real source signs its data yet. The answer below
			was recorded in a rehearsal on a local test chain that no longer runs, with test keys and an invented test source.
		</p>
	</div>
	{#if check?.ok}
		{@const c = check}
		<div class="panel l3" data-testid="level3-card">
			<p class="tight label"><span class="mono">{c.metric}</span> of <span class="mono">{c.subject}</span></p>
			<p class="tight big mono">
				<span data-testid="level3-value">{grouped(c.value)}</span>
				<span class="ex-tag" data-testid="level3-tag">rehearsal · test data</span>
			</p>
			<dl class="kv">
				<dt>Level</dt>
				<dd>3, signed by the source</dd>
				<dt>Source</dt>
				<dd class="mono" data-testid="level3-source">{c.source} (a test source)</dd>
				<dt>Signed by</dt>
				<dd class="mono" title={c.signer}>{short(c.signer)}, the key the book registered for it</dd>
				<dt>Observed</dt>
				<dd>{stamp(c.observed_unix)}</dd>
				<dt>Accepted</dt>
				<dd>block time <span class="nw">{stamp(c.accepted_at)}</span>; servable from <span class="nw">{stamp(c.servable_at)}</span></dd>
				<dt>Checked at</dt>
				<dd class="mono" data-testid="level3-block">finalized block {c.block} ({short(c.block_hash)})</dd>
				<dt>Book</dt>
				<dd class="mono" data-testid="level3-book" title={c.book}>{short(c.book)}, local anvil (chain {c.chain_id}), rehearsal</dd>
			</dl>
			<p class="muted tight" data-testid="level3-copy">
				Your browser decoded the signed report, checked the source's signature and that its key is the one the report names, recomputed the
				fact, report and value hashes, and compared them with the book's <span class="mono">settleable</span> answer at the finalized block,
				read with the code at the book's address hashed against the pinned build. <span data-testid="level3-recorded">The chain answers were
				recorded in the rehearsal on <span class="nw">{stamp(c.recorded_unix ?? 0)}</span> and are pinned in the verifier; the rehearsal moved its chain's clock
				on by 90 minutes, and so does this check.</span> No market settles on a signed value yet.
			</p>
			<div class="rerun">
				<button type="button" class="btn ghost" data-testid="level3-forge" onclick={tryForged}>Try a forged value</button>
				{#if forged}
					<p class="tight muted" data-testid="level3-forged">
						{forged.ok ? 'The forged value passed: this is a bug.' : `Refused: ${forged.problems[0]}`}
					</p>
				{/if}
			</div>
			<form class="rpc" onsubmit={(e) => { e.preventDefault(); void readChain(); }}>
				<label for="level3-rpc">Read the chain yourself, through an RPC you choose (no key; nothing is sent but the reads)</label>
				<div class="rpcrow">
					<input id="level3-rpc" class="mono" data-testid="level3-rpc" type="url" bind:value={rpc} spellcheck="false" autocomplete="off" />
					<button type="submit" class="btn ghost" data-testid="level3-read">Read</button>
					<Pill live state={livePill.state} text={livePill.text} />
				</div>
			</form>
			{#if live}
				<p class="tight muted" data-testid="level3-live">
					{#if live.ok}
						Your RPC's finalized block {live.block} says the book settles this fact on this report. Your browser takes that
						RPC's word for the chain's state (one RPC; reading two, or a proof anchored on L1, is still to come).{#if live.chain_id === check?.chain_id}
							<span data-testid="level3-live-local"> Your RPC answers for chain {live.chain_id}, the rehearsal's local test chain, not a live
								deployment: anyone can run that chain with the rehearsal's answers, so this is not a live level-3 value.</span>{/if}
					{:else}
						Refused: {live.problems[0]}.{#if live.problems.some((p) => p.includes('your RPC is on chain'))}
							This answer comes from the rehearsal's local chain, so a live Base Sepolia RPC cannot confirm it; once the book is deployed,
							this button checks a live answer.{/if}
					{/if}
				</p>
			{:else if liveError}
				<p class="tight muted" data-testid="level3-live">Could not read your RPC: {liveError}</p>
			{/if}
		</div>
	{:else if check && !check.ok}
		<p class="panel muted" data-testid="level3-failed">The recorded answer did not pass the check, so nothing is shown.</p>
	{:else}
		<div class="panel"><Skeleton rows={2} label="Checking the level-3 rehearsal" /></div>
	{/if}
	</details>
</section>

<style>
	.tight {
		margin: 0;
	}
	.l3 {
		display: grid;
		gap: var(--sp-3);
	}
	.label {
		color: var(--muted);
		font-size: var(--text-sm);
	}
	.big {
		font-size: var(--text-md);
	}
	.rerun {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--sp-3);
	}
	.rpc {
		display: grid;
		gap: var(--sp-2);
	}
	.rpc label {
		color: var(--muted);
		font-size: var(--text-sm);
	}
	.rpcrow {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--sp-3);
	}
	.rpcrow input {
		flex: 1 1 16rem;
		min-width: 0;
		/* A primary control: at least 44 px tall on a phone (the a11y target-size check). */
		min-height: 44px;
		font-family: var(--mono);
		font-size: var(--text-sm);
		color: var(--ink);
		background: var(--panel-2);
		border: 1px solid var(--line-strong);
		border-radius: var(--radius-sm);
		padding: var(--sp-2) var(--sp-3);
		transition: border-color var(--dur) var(--ease);
	}
	/* Every control changes on hover (W10). */
	.rpcrow input:hover {
		border-color: var(--line-bright);
	}
	.ex-tag {
		font-family: inherit;
		font-size: var(--text-xs);
		font-weight: 500;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--muted);
		border: 1px solid currentColor;
		border-radius: var(--radius-pill);
		padding: var(--sp-0) var(--sp-2);
		vertical-align: middle;
	}
.nw {
		white-space: nowrap;
	}
</style>
