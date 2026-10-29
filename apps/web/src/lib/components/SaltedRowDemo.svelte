<script lang="ts">
	// ROADMAP D30 (pre-public loop PP3): a buyer's check of one salted (format-3) label row, on the
	// synthetic set of fixtures/dated/salted-vectors.json, against the pin compiled into the verifier.
	import Pill, { type PillState } from '#lib/components/Pill.svelte';
	import { verifier } from '#lib/verifier.ts';
	type Case = { case: string; answer: unknown; expect: string };
	type Vectors = { cases: Case[]; guess: { row: string; sibling: string } };
	let pill = $state<{ state: PillState; text: string }>({ state: 'idle', text: 'Not checked' });
	let lines = $state<string[]>([]);
	let busy = $state(false);
	let data: Vectors | null = null;

	async function load(): Promise<Vectors> {
		if (data) return data;
		const r = await fetch(new URL('data/salted-vectors.json', document.baseURI));
		if (!r.ok) throw new Error('could not load the demo rows');
		data = (await r.json()) as Vectors;
		return data;
	}

	async function run(kind: 'row' | 'flip' | 'guess') {
		if (busy) return;
		busy = true;
		pill = { state: 'run', text: 'Checking' };
		try {
			const v = await load();
			const pin = await verifier.saltedPin();
			if (kind === 'guess') {
				const confirms = await verifier.saltedGuess(v.guess.row, v.guess.sibling);
				lines = [
					'The buyer of row 0 guesses its neighbour exactly (address, company and heights) and hashes the guess the old way, without a salt.',
					confirms ? 'The guess matches the sibling hash: the neighbour is exposed.' : 'The guess does not match the sibling hash in the proof: the neighbour stays hidden.'
				];
				pill = confirms ? { state: 'bad', text: 'Guess confirmed' } : { state: 'good', text: 'Guess not confirmed' };
			} else {
				const c = v.cases.find((x) => x.case === (kind === 'row' ? 'row 1' : 'salt bit flipped'));
				if (!c) throw new Error('demo row missing');
				const got = await verifier.saltedAnswer(JSON.stringify(c.answer), pin);
				if (got.ok) {
					lines = [`${got.address} belonged to ${got.entity} from block ${got.from}${got.to === null ? ' on' : ` to ${got.to}`}.`, 'The row, its own salt and its proof lead to the root, and the header hashes to the pin in this site\'s code.'];
					pill = { state: 'good', text: 'Row checks' };
				} else {
					lines = ['One bit of the salt was changed before the check.', `Refused: ${got.error}`];
					pill = { state: 'good', text: 'Tampered row refused' };
				}
			}
		} catch (e) {
			lines = [e instanceof Error ? e.message : String(e)];
			pill = { state: 'bad', text: 'Could not check' };
		} finally {
			busy = false;
		}
	}
</script>

<div class="panel" data-testid="salted-demo">
	<div class="sh"><h2>Rows you cannot guess</h2><Pill live state={pill.state} text={pill.text} /></div>
	<p>
		A label set whose rows are not public is sold by the network's nodes one row, one address, at a time. Each row carries its own salt, so the proof that comes with it does
		not let a buyer confirm a guessed neighbour.
	</p>
	<div class="row">
		<button type="button" onclick={() => run('row')} disabled={busy} data-testid="salted-check">Check a bought row</button>
		<button type="button" class="ghost" onclick={() => run('flip')} disabled={busy} data-testid="salted-flip">Change the salt, then check</button>
		<button type="button" class="ghost" onclick={() => run('guess')} disabled={busy} data-testid="salted-guess">Try to guess a neighbour</button>
	</div>
	{#if lines.length}
		<div class="saltlog" data-testid="salted-result" role="status">{#each lines as l, i (i)}<p>{l}</p>{/each}</div>
	{/if}
	<div class="pfoot">
		<p class="fine">Example companies and test addresses only. Real label sets are not served this way until their rights are cleared.</p>
		<details class="more">
			<summary>For developers</summary>
			<p class="fine">A node serves one row at <code>GET /v1/labels/{'{set}'}/{'{address}'}</code>.</p>
		</details>
	</div>
</div>

<style>
	.saltlog {
		margin-top: var(--sp-3);
		font-size: var(--text-sm);
		line-height: var(--lh-body);
		max-width: 68ch;
	}
	.saltlog p {
		margin: 0 0 var(--sp-1);
	}
</style>
