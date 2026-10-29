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

if (import.meta.url === `file://${process.argv[1]}`) console.log('ok', pp1Parser());
