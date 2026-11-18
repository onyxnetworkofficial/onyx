// markets.ts: a futures market's badge on a fixed clock. Past its refund date (`resolveBy`) with
// no observed value a market no longer says it is waiting: OnyxFutures.resolve then voids it.
// Run: node --test apps/web/tests/markets.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { marketStatus, refundDue } from '../src/lib/markets.ts';

// 2026-11-11 00:00 UTC, the refund date of the test market this rule was found on.
const BY = Date.UTC(2026, 10, 11) / 1000;
const closed = { state: 'open', trading: false, resolveBy: BY, value: null };
const WAITING = 'Trading closed, waiting for the observed value';
const DUE = 'No value by the refund date';

test('before the refund date a closed market waits for its value', () => {
	assert.equal(marketStatus(closed, BY - 1), WAITING);
	assert.equal(refundDue(closed, BY - 1), false);
	assert.equal(marketStatus({ ...closed, trading: true }, BY - 86_400), 'Trading open');
});

test('from the refund date on, with no value, the badge says so (the contract compares >=)', () => {
	for (const now of [BY, BY + 1, BY + 7 * 86_400]) {
		assert.equal(marketStatus(closed, now), DUE, String(now));
		assert.equal(refundDue(closed, now), true, String(now));
	}
	assert.equal(refundDue({ ...closed, value: undefined }, BY), true);
});

test('a value on chain keeps the market waiting to be settled, whatever the date', () => {
	const m = { ...closed, value: '38500000000000.00' };
	assert.equal(marketStatus(m, BY + 86_400), WAITING);
	assert.equal(refundDue(m, BY + 86_400), false);
});

test('a settled or void market says its own state, and nothing is due without a date', () => {
	assert.equal(marketStatus({ ...closed, state: 'resolved' }, BY + 1), 'Settled');
	assert.equal(marketStatus({ ...closed, state: 'void' }, BY + 1), 'Void, every bet refunded');
	assert.equal(refundDue({ ...closed, state: 'void' }, BY + 1), false);
	assert.equal(refundDue({ ...closed, state: 'resolved' }, BY + 1), false);
	assert.equal(refundDue({ ...closed, resolveBy: undefined }, BY + 1), false);
	assert.equal(refundDue({ ...closed, resolveBy: 0 }, BY + 1), false);
	assert.equal(refundDue({ ...closed, trading: true }, BY + 1), false);
});

// Newcomer test 2026-11-18: the practice markets' dates roll with today (never a day that passed).
import { practiceDay, practiceDayLabel } from '../src/lib/arena.ts';

test('a practice market names a day counted from today, in one date style', () => {
	const now = Date.UTC(2026, 10, 18, 15, 30);
	assert.equal(practiceDayLabel(now), '18 Nov 2026');
	assert.equal(practiceDay(0, now), '18 Nov 2026');
	assert.equal(practiceDay(12, now), '30 Nov 2026');
	assert.equal(practiceDay(43, now), '31 Dec 2026');
	assert.equal(practiceDay(2, Date.UTC(2026, 11, 31)), '2 Jan 2027');
	// Whatever today is, a day two or more days ahead has not passed.
	assert.ok(Date.parse(`${practiceDay(2)} UTC`) > Date.now());
});
