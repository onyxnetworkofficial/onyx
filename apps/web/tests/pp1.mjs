// Pre-public loop PP1 (docs/pre-public-loop.md, ADR-0039 amendment 1): the receipt parser accepts
// a fed receipt's observer evidence only in its exact shape, only on an approved receipt, and the
// page's words keep what it shows separate from what it does not prove. Called by tests/e2e.mjs;
// also runs alone: node tests/pp1.mjs.
import assert from 'node:assert/strict';

const R = await import(new URL('../src/lib/submission-receipt.ts', import.meta.url).href);
const id = 'sub_' + 'a'.repeat(32);
const check = { level: 2, recordDate: '2026-10-08', roundId: '0x' + '1a'.repeat(32), result: '0x' + '2b'.repeat(32), value: '40305316210829.72', registry: 'simulated' };

export function pp1Parser() {
	assert.deepEqual(R.parseReceipt({ id, status: 'approved' }), { id, status: 'approved' });
	assert.deepEqual(R.parseReceipt({ id, status: 'approved', check }), { id, status: 'approved', check });
	assert.deepEqual(R.parseReceipt({ id, status: 'approved', check: { ...check, registry: 'eip155:84532' } }).check.registry, 'eip155:84532');
	const refused = [
		{ id, status: 'pending_review', check },
		{ id, status: 'rejected', check },
		{ id, status: 'approved', check: null },
		{ id, status: 'approved', check: [] },
		{ id, status: 'approved', check: { ...check, level: 1 } },
		{ id, status: 'approved', check: { ...check, roundId: '0xABC' } },
		{ id, status: 'approved', check: { ...check, result: '0x' + 'G'.repeat(64) } },
		{ id, status: 'approved', check: { ...check, recordDate: '<img src=x>' } },
		{ id, status: 'approved', check: { ...check, registry: 'eip155:0' } },
		{ id, status: 'approved', check: (({ value, ...rest }) => rest)(check) },
		{ id, status: 'approved', check: { ...check, value: '1e3' } },
		{ id, status: 'approved', check: { ...check, value: '040.5' } },
		{ id, status: 'approved', check: { ...check, value: '<b>1</b>' } },
		{ id, status: 'approved', check: { ...check, value: 40305316210829.72 } },
		{ id, status: 'approved', check: { ...check, reason: 'operator note' } }
	];
	for (const r of refused) assert.throws(() => R.parseReceipt(r), /valid receipt/, JSON.stringify(r));
	assert.equal(R.groupedValue('40305316210829.72'), '40,305,316,210,829.72');
	assert.equal(R.groupedValue('-1000'), '-1,000');
	assert.equal(R.groupedValue('0.5'), '0.5');
	const s = R.checkSummary(check);
	assert.match(s.meaning, /not from your proposal/);
	assert.match(s.limit, /does not prove the source itself is correct/);
	assert.match(s.where, /not posted on a chain/);
	assert.match(R.checkSummary({ ...check, registry: 'eip155:84532' }).where, /chain 84532/);
	return 'PP1 receipt parser: exact evidence shape on approved receipts only; limits stated';
}

export function b2Parser() {
	// READINESS B2: a reviewer's decision. The state in plain words; a reason only as a code from the
	// fixed list, only on a hold or a drop, never a reviewer's text and never beside observer evidence.
	assert.deepEqual(R.parseReceipt({ id, status: 'cleared' }), { id, status: 'cleared' });
	assert.match(R.RECEIPT_STATUS.cleared, /cleared it for the next step.*not been verified or published/);
	assert.match(R.RECEIPT_STATUS.on_hold, /on hold.*not published/);
	assert.match(R.RECEIPT_STATUS.dropped, /will not proceed/);
	const codes = ['terms_forbid', 'personal_data', 'not_entities', 'duplicate', 'unreachable_source', 'out_of_scope', 'spam', 'other'];
	assert.deepEqual(Object.keys(R.RECEIPT_REASON), codes, 'the reason codes of docs/api/openapi.json, each with its words');
	for (const reason of codes) {
		for (const status of ['on_hold', 'dropped']) assert.deepEqual(R.parseReceipt({ id, status, reason }), { id, status, reason });
		assert.match(R.RECEIPT_REASON[reason], /^Reason: [a-z].*\.$/);
		assert.doesNotMatch(R.RECEIPT_REASON[reason], /_/, 'plain words, not the code');
	}
	assert.deepEqual(R.parseReceipt({ id, status: 'on_hold' }), { id, status: 'on_hold' });
	const refusedReasons = [
		{ id, status: 'cleared', reason: 'spam' },
		{ id, status: 'pending_review', reason: 'spam' },
		{ id, status: 'approved', reason: 'spam' },
		{ id, status: 'rejected', reason: 'duplicate' },
		{ id, status: 'dropped', reason: 'other:private_note' },
		{ id, status: 'dropped', reason: 'the reviewer wrote this' },
		{ id, status: 'dropped', reason: '<img src=x onerror=alert(1)>' },
		{ id, status: 'dropped', reason: 7 },
		{ id, status: 'dropped', reason: null },
		{ id, status: 'dropped', reason: 'constructor' },
		{ id, status: 'on_hold', reason: 'spam', check },
		{ id, status: 'cleared', check },
		{ id, status: 'withdrawn' }
	];
	for (const r of refusedReasons) assert.throws(() => R.parseReceipt(r), /valid receipt/, JSON.stringify(r));
	return 'B2 receipt parser: a reviewer\'s decision in plain words, a reason only as a code from the fixed list';
}

// READINESS B2: the receipt section of the Add data page, in a real browser, against canned answers of
// `GET /v1/submit/{id}` (the site's intake.json answered "on"; no request leaves the test). A cleared,
// held or dropped proposal reads in plain words, a reason shows as this page's sentence for its code,
// and an answer carrying a reviewer's text is refused whole: none of it reaches the page.
export async function b2Receipt(browser, base, expect) {
	const { settled } = await import('./wait.mjs');
	const sub = (c) => 'sub_' + c.repeat(32);
	const forged = 'call the reviewer about this';
	const answers = {
		[sub('1')]: { status: 'cleared' },
		[sub('2')]: { status: 'on_hold', reason: 'unreachable_source' },
		[sub('3')]: { status: 'dropped', reason: 'personal_data' },
		[sub('4')]: { status: 'dropped', reason: 'other' },
		[sub('5')]: { status: 'dropped', reason: forged },
		[sub('6')]: { status: 'cleared', reason: 'spam' }
	};
	const ctx = await browser.newContext({ viewport: { width: 390, height: 800 }, reducedMotion: 'reduce' });
	try {
		const pg = await ctx.newPage();
		await pg.route('**/intake.json', (r) => r.fulfill({ contentType: 'application/json', body: '{"submit":true}' }));
		await pg.route('**/v1/submit/sub_*', (r) => {
			const id = new URL(r.request().url()).pathname.split('/').pop();
			return answers[id]
				? r.fulfill({ contentType: 'application/json', body: JSON.stringify({ id, ...answers[id] }) })
				: r.fulfill({ status: 404, contentType: 'application/json', body: '{"error":{"code":"not_found"}}' });
		});
		await pg.goto(base + '#/submit');
		await settled(pg);
		const ask = async (id) => {
			await pg.getByTestId('receipt-id').fill(id);
			await pg.getByTestId('receipt-refresh').click();
		};
		const shown = async (id, words) => {
			await ask(id);
			await pg.getByTestId('receipt-status').filter({ hasText: words }).waitFor();
			return pg.getByTestId('receipt-status').innerText();
		};
		let text = await shown(sub('1'), 'cleared it for the next step');
		expect(/not been verified or published yet/.test(text) && (await pg.getByTestId('receipt-reason').count()) === 0, 'B2 receipt: a cleared proposal says it is cleared for the next step, not verified or published, with no reason line');
		text = await shown(sub('2'), 'put your proposal on hold');
		expect(/Reason: the reviewer could not open the source\./.test(text) && !/unreachable_source/.test(text), 'B2 receipt: a held proposal shows its reason in plain words, not the code');
		text = await shown(sub('3'), 'will not proceed');
		expect(/Reason: the proposal holds or points to data about a person/.test(text) && /submit a corrected proposal/.test(text), 'B2 receipt: a dropped proposal says so, with its reason and what the proposer can do');
		text = await shown(sub('4'), 'will not proceed');
		expect(/Reason: another reason, which this page cannot show\./.test(text), 'B2 receipt: the reviewer\'s own code behind "other" is never shown');
		for (const id of [sub('5'), sub('6')]) {
			await ask(id);
			await pg.getByTestId('receipt-error').filter({ hasText: 'valid receipt' }).waitFor();
			expect((await pg.getByTestId('receipt-status').count()) === 0 && !(await pg.locator('body').innerText()).includes(forged), `B2 receipt: an answer with ${id === sub('5') ? 'a reviewer\'s text as the reason' : 'a reason on a cleared proposal'} is refused whole and nothing of it is shown`);
		}
		expect(await pg.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), 'B2 receipt: no sideways scroll at phone width with a reason shown');
	} finally {
		await ctx.close();
	}
}

if (import.meta.url === `file://${process.argv[1]}`) { console.log('ok', pp1Parser()); console.log('ok', b2Parser()); }
