// Partner tracker T10 (ADR-0042): the browser's copy of the stake and futures contract rules
// (src/lib/markets.ts) runs every shared vector (fixtures/markets/vectors.json), the same file the
// real contracts run in contracts/test/OnyxMarketsVectors.t.sol (THREATS STK-8). Called by
// tests/e2e.mjs; also runs alone: node tests/t10.mjs.
import { readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';

const M = await import(new URL('../src/lib/markets.ts', import.meta.url).href);
const V = JSON.parse(await readFile(new URL('../../../fixtures/markets/vectors.json', import.meta.url), 'utf8'));
const B = (s) => BigInt(s);
const str = (o) => Object.fromEntries(Object.entries(o).map(([k, v]) => [k, String(v)]));
const same = (a, b) => JSON.stringify(Object.entries(a).sort()) === JSON.stringify(Object.entries(b).sort());

export function t10Vectors() {
	const bad = [];
	for (const c of V.votes) {
		const got = M.voteOutcome(B(c.correct), B(c.wrong), B(c.quorum), BigInt(c.decisivePpm));
		if ((got ?? 'fallback') !== c.expect) bad.push(`vote ${JSON.stringify(c)} -> ${got}`);
	}
	for (const c of V.attack) {
		const got = M.minAttackStake(B(c.honest), B(c.quorum), BigInt(c.decisivePpm), B(c.minStake), B(c.maxPerClaim));
		if (String(got) !== c.expect) bad.push(`attack ${JSON.stringify(c)} -> ${got}`);
	}
	for (const c of V.claims) {
		const r = M.settleClaim({
			bond: B(c.bond), feePpm: BigInt(c.feePpm), submitReward: B(c.submitReward), rewardPool: B(c.rewardPool), outcome: c.outcome,
			stakes: c.stakes.map((s) => ({ who: s.who, amount: B(s.amount), side: s.side }))
		});
		const e = c.expect;
		if (String(r.fee) !== e.fee || String(r.dust) !== e.dust || String(r.treasury) !== e.treasury || !same(str(r.payouts), e.payouts))
			bad.push(`claim "${c.name}": ${JSON.stringify({ ...str({ fee: r.fee, dust: r.dust, treasury: r.treasury }), payouts: str(r.payouts) })}`);
	}
	for (const m of V.markets) {
		const r = M.settleMarket({ edges: m.edges.map(B), feePpm: BigInt(m.feePpm), value: m.value === null ? null : B(m.value), bets: m.bets.map((b) => ({ ...b, amount: B(b.amount) })) });
		const e = m.expect;
		if (r.state !== e.state || r.winner !== e.winner || String(r.fee) !== e.fee || String(r.dust) !== e.dust || String(r.treasury) !== e.treasury || !same(str(r.payouts), e.payouts))
			bad.push(`market "${m.name}": ${JSON.stringify({ state: r.state, winner: r.winner, ...str({ fee: r.fee, dust: r.dust, treasury: r.treasury }), payouts: str(r.payouts) })}`);
	}
	for (const c of V.buckets) if (M.bucketOf(c.edges.map(B), B(c.value)) !== c.bucket) bad.push(`bucket ${JSON.stringify(c)}`);
	const n = V.votes.length + V.attack.length + V.claims.length + V.markets.length + V.buckets.length;
	return { bad, n };
}

const axe = await readFile(createRequire(import.meta.url).resolve('axe-core/axe.min.js'), 'utf8');

/** The Markets page's two simulators, in a real browser, give the contracts' numbers (the
 * demo story of contracts/test/OnyxMarketsDemo.t.sol), make no request, fit a phone and pass axe. */
export async function t10(browser, base, expect) {
	{
		const { bad, n } = t10Vectors();
		expect(bad.length === 0, `T10 markets.ts runs all ${n} shared vectors (${bad.slice(0, 2).join('; ')})`);
	}
	for (const width of [1280, 390]) {
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
		const pg = await ctx.newPage();
		const requests = [];
		pg.on('request', (r) => requests.push(r.url()));
		await pg.goto(base + '#/markets');
		await pg.getByTestId('arena-tab-verify').click();
		const text = async (id) => (await pg.getByTestId(id).innerText()).replace(/\s+/g, ' ');
		// 1. Honest past data, re-observed: the submitter's bond back plus the 1-token reward.
		await pg.getByTestId('stake-resolve').click();
		let r = await text('stake-result');
		expect(r.includes('Correct') && r.includes('Re-observation') && r.includes('Submitter (bonded 10.00) gets back 11.00') && r.includes('gets back 20.00'), `T10 ${width}: honest observed claim pays bond and reward (${r.slice(0, 160)})`);
		// 2. A wrong value with a challenger: the challenger wins the bond less 2%.
		await pg.getByTestId('stake-seen').selectOption('different');
		await pg.getByTestId('stake-side-0').selectOption('wrong');
		await pg.getByTestId('stake-resolve').click();
		r = await text('stake-result');
		expect(r.includes('Wrong') && r.includes('Submitter (bonded 10.00) gets back 0.00') && r.includes('gets back 29.80'), `T10 ${width}: wrong value is slashed to the challenger (${r.slice(0, 160)})`);
		// 3. Own data, contested 15 v 15: the fallback oracle decides.
		await pg.getByTestId('stake-kind-voted').check();
		await pg.getByTestId('stake-side-0').selectOption('correct');
		await pg.getByTestId('stake-amount-0').fill('15');
		await pg.getByTestId('stake-side-1').selectOption('wrong');
		await pg.getByTestId('stake-amount-1').fill('15');
		await pg.getByTestId('stake-arbiter').selectOption('wrong');
		await pg.getByTestId('stake-resolve').click();
		r = await text('stake-result');
		expect(r.includes('Fallback panel') && r.includes('gets back 39.50'), `T10 ${width}: contested vote goes to the drawn fallback panel (${r.slice(0, 200)})`);
		// 4. A one-sided vote shows its attack cost.
		await pg.getByTestId('stake-amount-1').fill('0');
		await pg.getByTestId('stake-amount-0').fill('30');
		await pg.getByTestId('stake-resolve').click();
		r = await text('stake-result');
		expect(r.includes('Stake vote') && r.includes('60.01 tokens'), `T10 ${width}: vote result states the attack cost (${r.slice(0, 220)})`);
		await pg.getByTestId('stake-kind-duplicate').check();
		await pg.getByTestId('stake-amount-0').fill('20');
		await pg.getByTestId('stake-resolve').click();
		r = await text('stake-result');
		expect(r.includes('Duplicate: nothing taken, nothing paid'), `T10 ${width}: a copy of an accepted value earns nothing`);
		await pg.getByTestId('stake-kind-voted').check();
		await pg.getByTestId('stake-amount-0').fill('abc');
		await pg.getByTestId('stake-resolve').click();
		expect((await pg.getByTestId('stake-error').count()) === 1 && (await pg.getByTestId('stake-result').count()) === 0, `T10 ${width}: a bad amount is refused`);
		expect(requests.every((u) => new URL(u).origin === new URL(base).origin), `T10 ${width}: the simulators make no outside request`);
		const overflow = await pg.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1);
		expect(!overflow, `T10 ${width}: page fits the viewport`);
		await pg.addScriptTag({ content: axe });
		{
			const v = await pg.evaluate(async () => (await window.axe.run(document, { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21aa', 'wcag22aa'] } })).violations.filter((x) => ['serious', 'critical'].includes(x.impact)).map((x) => x.id));
			expect(v.length === 0, `T10 ${width}: the stake simulator passes axe (${v.join(',')})`);
		}
		await ctx.close();
	}
}

if (import.meta.url === `file://${process.argv[1]}`) {
	const { bad, n } = t10Vectors();
	for (const b of bad) console.log('FAIL', b);
	console.log(bad.length ? `T10 FAILED (${bad.length} of ${n})` : `T10 vectors ok (${n})`);
	process.exit(bad.length ? 1 : 0);
}
