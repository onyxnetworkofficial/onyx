// markets.ts: a futures market's badge on a fixed clock. From its `resolveBy` on with
// no observed value a market no longer says it is waiting: OnyxFutures.resolve then voids it.
// Run: node --test apps/web/tests/markets.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { marketStatus, voidable } from '../src/lib/markets.ts';

// 2026-11-11 00:00 UTC, the `resolveBy` of the test market this rule was found on.
const BY = Date.UTC(2026, 10, 11) / 1000;
const closed = { state: 'open', trading: false, resolveBy: BY, value: null };
const WAITING = 'Trading closed, waiting for the observed value';
const DUE = 'No value yet, can be voided';

test('before `resolveBy` a closed market waits for its value', () => {
	assert.equal(marketStatus(closed, BY - 1), WAITING);
	assert.equal(voidable(closed, BY - 1), false);
	assert.equal(marketStatus({ ...closed, trading: true }, BY - 86_400), 'Trading open');
});

test('from `resolveBy` on, with no value, the badge says so (the contract compares >=)', () => {
	for (const now of [BY, BY + 1, BY + 7 * 86_400]) {
		assert.equal(marketStatus(closed, now), DUE, String(now));
		assert.equal(voidable(closed, now), true, String(now));
	}
	assert.equal(voidable({ ...closed, value: undefined }, BY), true);
});

test('a value on chain keeps the market waiting to be settled, whatever the date', () => {
	const m = { ...closed, value: '38500000000000.00' };
	assert.equal(marketStatus(m, BY + 86_400), WAITING);
	assert.equal(voidable(m, BY + 86_400), false);
});

test('a settled or void market says its own state, and nothing is due without a date', () => {
	assert.equal(marketStatus({ ...closed, state: 'resolved' }, BY + 1), 'Settled');
	assert.equal(marketStatus({ ...closed, state: 'void' }, BY + 1), 'Void, bets can be withdrawn');
	assert.equal(voidable({ ...closed, state: 'void' }, BY + 1), false);
	assert.equal(voidable({ ...closed, state: 'resolved' }, BY + 1), false);
	assert.equal(voidable({ ...closed, resolveBy: undefined }, BY + 1), false);
	assert.equal(voidable({ ...closed, resolveBy: 0 }, BY + 1), false);
	assert.equal(voidable({ ...closed, trading: true }, BY + 1), false);
});

// Newcomer test 3 (2026-11-18): a void card gave no reason, and the page said "Open markets"
// while none was open.
import { marketTally, voidReason } from '../src/lib/markets.ts';

test('a void market says why, and the tally names only the states the chain gave', () => {
	assert.equal(voidReason({ ...closed, state: 'void', value: null }), 'No value was recorded by the date, so the market was voided. Every bet can be withdrawn in full.');
	assert.match(voidReason({ ...closed, state: 'void', value: '4051234567890123.45' }), /a range nobody had bet on, so the market was voided\. Every bet can be withdrawn in full\.$/);
	for (const state of ['open', 'resolved']) assert.equal(voidReason({ ...closed, state }), '');
	assert.equal(marketTally([]), '');
	assert.equal(marketTally([{ state: 'resolved' }, { state: 'void' }]), '2 test markets read: none open for bets, 1 settled, 1 void.');
	assert.equal(marketTally([{ state: 'open', trading: true }]), '1 test market read: 1 open for bets.');
	assert.equal(marketTally([{ state: 'open', trading: false }, { state: 'open', trading: true }, { state: 'resolved' }]), '3 test markets read: 1 open for bets, 1 closed and waiting for a value, 1 settled.');
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
