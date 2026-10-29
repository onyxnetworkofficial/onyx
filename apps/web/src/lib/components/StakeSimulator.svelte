<script lang="ts">
	// Partner tracker T10 (ADR-0042): the staked-submission rules of the 2026-10-20 call, run in the
	// browser with the same integer rules as contracts/src/OnyxStakes.sol (src/lib/markets.ts,
	// checked against fixtures/markets/vectors.json). A simulation: no tokens, no chain, no network.
	import { minAttackStake, settleClaim, voteOutcome, type Outcome, type Side } from '#lib/markets.ts';

	// Placeholder parameters (OnyxStakes.defaultParams); amounts in hundredths of a test token.
	const U = 100n;
	const P = { bond: 10n * U, reward: 1n * U, minStake: 1n * U, maxPerClaim: 1000n * U, quorum: 20n * U, decisivePpm: 666_667n, feePpm: 20_000n };
	type Row = { who: string; side: Side; amount: string };
	type Kind = 'observed' | 'voted' | 'duplicate';
	type Seen = 'same' | 'different' | 'unavailable';
	type Arbiter = 'correct' | 'wrong' | 'silent';

	let kind = $state<Kind>('observed');
	let seen = $state<Seen>('same');
	let arbiter = $state<Arbiter>('correct');
	let rows = $state<Row[]>([
		{ who: 'Validator', side: 'correct', amount: '20' },
		{ who: 'Challenger', side: 'wrong', amount: '0' },
		{ who: 'Late staker', side: 'none', amount: '0' }
	]);
	let error = $state('');
	let result = $state<null | { by: string; outcome: Outcome; payouts: [string, string][]; treasury: string; note: string; duplicate?: boolean }>(null);

	const fmt = (v: bigint) => `${v / U}.${String(v % U).padStart(2, '0')}`;
	function toUnits(s: string): bigint | null {
		if (!/^\d{1,6}(\.\d{1,2})?$/.test(s.trim())) return null;
		const [w, f = ''] = s.trim().split('.');
		return BigInt(w) * U + BigInt((f + '00').slice(0, 2));
	}
	const reset = () => { result = null; error = ''; };

	function resolve() {
		reset();
		const stakes = [];
		let total = 0n;
		for (const r of rows) {
			const a = toUnits(r.amount);
			if (a === null) { error = `${r.who}: enter an amount like 20 or 12.5 (0 for no stake).`; return; }
			if (a === 0n) continue;
			if (a < P.minStake || a > 100n * U) { error = `${r.who}: a stake is 1 to 100 test tokens.`; return; }
			stakes.push({ who: r.who, amount: a, side: r.side });
			total += a;
		}
		if (total > P.maxPerClaim) { error = 'All stakes together are capped at 1,000 test tokens per claim.'; return; }
		if (kind === 'duplicate') {
			// OnyxStakes.submit: a value equal to the accepted one takes no bond and opens nothing.
			result = { by: 'Duplicate check: the same value was already accepted', outcome: 'void', payouts: [['Submitter (no bond taken)', '0.00']], treasury: '0.00', note: 'A copy earns nothing and costs nothing, and nobody can stake on it (call, 37:41: no reward for the same data again).', duplicate: true };
			return;
		}
		const correct = stakes.filter((s) => s.side === 'correct').reduce((n, s) => n + s.amount, 0n);
		const wrong = stakes.filter((s) => s.side === 'wrong').reduce((n, s) => n + s.amount, 0n);
		let outcome: Outcome;
		let by: string;
		let note = '';
		if (kind === 'observed' && seen !== 'unavailable') {
			outcome = seen === 'same' ? 'correct' : 'wrong';
			by = 'Re-observation: a majority of the observers re-read the source';
			note = 'Observers decide re-readable past data, whatever the stakes say. A vote cannot outweigh them.';
		} else {
			const v = voteOutcome(correct, wrong, P.quorum, P.decisivePpm);
			if (v) {
				outcome = v;
				by = 'Stake vote: one side holds at least 2/3 of at least 20 tokens revealed';
				const honest = v === 'correct' ? correct : wrong;
				const a = minAttackStake(honest, P.quorum, P.decisivePpm, P.minStake, P.maxPerClaim);
				note = a === 0n
					? 'Nobody can flip this vote alone within the 1,000-token cap per claim.'
					: `An attacker would need ${fmt(a)} tokens on the other side to flip this vote alone. A value decided by a vote is level 4 and never settles a market.`;
			} else if (arbiter === 'silent') {
				outcome = 'void';
				by = 'Fallback oracle did not answer in time: void';
				note = 'Everyone gets their bond and stake back.';
			} else {
				outcome = arbiter;
				by = 'Fallback panel (bonded validators drawn at random, none with a stake in it), because the vote was contested or too small';
				note = 'The partners agreed: try the stake vote; if it does not work, use an oracle (call, 56:26).';
			}
		}
		const s = settleClaim({ bond: P.bond, feePpm: P.feePpm, submitReward: P.reward, rewardPool: 100n * U, outcome, stakes });
		const payouts: [string, string][] = [['Submitter (bonded 10.00)', fmt(s.payouts.submitter)]];
		for (const r of stakes) payouts.push([`${r.who} (${r.side === 'none' ? 'never revealed' : r.side}, staked ${fmt(r.amount)})`, fmt(s.payouts[r.who])]);
		result = { by, outcome, payouts, treasury: fmt(s.treasury), note };
	}
</script>

<section class="sim" aria-labelledby="stake-sim-title" data-testid="stake-sim">
	<p class="eyebrow">Testnet simulation · staked submissions</p>
	<h2 id="stake-sim-title">Stake that your data is right.</h2>
	<p class="lede">The submitter bonds 10 test tokens on a value. Others stake for "correct" or "wrong". If the value is right, the bond comes back with a reward; if it is wrong, the bond goes to the people who said so. These are the rules of the testnet contracts (ADR-0042), run in your browser. No tokens move.</p>
	<div class="grid">
		<fieldset>
			<legend>What kind of data?</legend>
			<label><input type="radio" bind:group={kind} value="observed" onchange={reset} data-testid="stake-kind-observed" /> Past public data observers can re-read (a Treasury value)</label>
			<label><input type="radio" bind:group={kind} value="voted" onchange={reset} data-testid="stake-kind-voted" /> Own data nobody else can re-read (only a vote can decide)</label>
			<label><input type="radio" bind:group={kind} value="duplicate" onchange={reset} data-testid="stake-kind-duplicate" /> A copy of a value that was already accepted</label>
		</fieldset>
		{#if kind === 'observed'}
			<label class="field"><span>What the observers see</span>
				<select bind:value={seen} onchange={reset} data-testid="stake-seen">
					<option value="same">The same value as the submitter</option>
					<option value="different">A different value</option>
					<option value="unavailable">Source unavailable (the vote decides)</option>
				</select></label>
		{/if}
		<div class="rows">
			{#each rows as r, i (r.who)}
				<div class="row">
					<span class="who">{r.who}</span>
					<label class="field"><span>Side</span>
						<select bind:value={rows[i].side} onchange={reset} data-testid={`stake-side-${i}`}>
							<option value="correct">Correct</option><option value="wrong">Wrong</option><option value="none">Never reveals</option>
						</select></label>
					<label class="field"><span>Stake (tokens)</span><input bind:value={rows[i].amount} oninput={reset} inputmode="decimal" data-testid={`stake-amount-${i}`} /></label>
				</div>
			{/each}
		</div>
		<label class="field"><span>If it goes to the fallback oracle, it answers</span>
			<select bind:value={arbiter} onchange={reset} data-testid="stake-arbiter">
				<option value="correct">Correct</option><option value="wrong">Wrong</option><option value="silent">Nothing in time (void)</option>
			</select></label>
		<button type="button" class="primary" onclick={resolve} data-testid="stake-resolve">Resolve the claim</button>
	</div>
	{#if error}<p role="alert" data-testid="stake-error">{error}</p>{/if}
	{#if result}
		<div class="out" role="status" data-testid="stake-result">
			<p class="eyebrow">Simulated result</p>
			<h3 data-testid="stake-outcome">{result.duplicate ? 'Duplicate: nothing taken, nothing paid' : result.outcome === 'void' ? 'Void: everyone refunded' : result.outcome === 'correct' ? 'Correct: the value is accepted' : 'Wrong: the submitter is slashed'}</h3>
			<p class="fine" data-testid="stake-by">Decided by: {result.by}</p>
			<dl>
				{#each result.payouts as [k, v]}<div><dt>{k}</dt><dd>gets back {v}</dd></div>{/each}
				<div><dt>Treasury (2% fee, unrevealed stakes, rounding)</dt><dd data-testid="stake-treasury">{result.treasury}</dd></div>
			</dl>
			<p class="fine">{result.note}</p>
			<p class="fine">A validator who backs a correct value that nobody disputes earns nothing: there are no losers to pay it. Paying for that checking is an open question for the partners.</p>
		</div>
	{/if}
</section>

<style>
	.sim{padding:var(--sp-6);border:1px solid var(--line);border-radius:var(--radius-lg);background:var(--panel);margin-block:var(--sp-6)}
	h2{font-size:var(--text-h2);line-height:var(--lh-tight);margin:var(--sp-2) 0}
	.lede{max-width:68ch;color:var(--muted);line-height:var(--lh-body);margin-bottom:var(--sp-5)}
	.grid{display:grid;gap:var(--sp-4);max-width:46rem}
	fieldset{border:1px solid var(--line);border-radius:var(--radius-md);padding:var(--sp-4);display:grid;gap:var(--sp-2)}
	fieldset label{display:flex;gap:var(--sp-2);align-items:center;min-height:44px}
	.field{display:grid;gap:var(--sp-1)} .field>span{font-size:var(--text-sm);color:var(--muted)}
	input:not([type='radio']),select{width:100%;min-height:44px}
	.rows{display:grid;gap:var(--sp-3)}
	.row{display:grid;grid-template-columns:8rem minmax(0,1fr) minmax(0,1fr);gap:var(--sp-3);align-items:end}
	.who{font-size:var(--text-sm);padding-bottom:var(--sp-3)}
	.primary{justify-self:start;min-height:44px;border:1px solid var(--accent);border-radius:var(--radius-pill);padding:var(--sp-2) var(--sp-4);background:var(--accent);color:var(--accent-ink)}
	.out{margin-top:var(--sp-5);padding:var(--sp-4);border:1px solid var(--line-strong);border-radius:var(--radius-md)}
	.out h3{font-size:var(--text-lg);margin:var(--sp-2) 0}
	dl{display:grid;margin:var(--sp-3) 0} dl>div{display:flex;justify-content:space-between;gap:var(--sp-4);padding-block:var(--sp-2);border-bottom:1px solid var(--line)}
	dt{font-size:var(--text-sm);color:var(--muted)} dd{margin:0;font-size:var(--text-sm);text-align:right}
	@media(max-width:560px){.row{grid-template-columns:1fr}.who{padding-bottom:0}dl>div{flex-direction:column;gap:var(--sp-1)}dd{text-align:left}}
</style>
