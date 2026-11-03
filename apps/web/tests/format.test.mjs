// format.ts: the shared formatters give the same text as the per-call `toLocale*` forms they
// replace (W4 numbers, L5 dates). Run: node --test apps/web/tests/format.test.mjs (Node 22.18+
// strips the TypeScript types of format.ts).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { dayMonth, group } from '../src/lib/format.ts';

test('dayMonth matches toLocaleDateString en-GB day month UTC', () => {
	const opts = { day: 'numeric', month: 'short', timeZone: 'UTC' };
	// Every day of 2026 and 2027 at three times of day, plus the epoch and far dates.
	const ts = [0, 1, 86399, 86400, 951782400, 4102444800, 1759622340];
	for (let d = 0; d < 730; d++) for (const s of [0, 43200, 86399]) ts.push(1767225600 + d * 86400 + s);
	for (const t of ts) assert.equal(dayMonth(t), new Date(t * 1000).toLocaleDateString('en-GB', opts), `t=${t}`);
});

test('group matches toLocaleString en-US', () => {
	for (const n of [0, 7, 999, 1000, 846000, -1234567, 2 ** 53 - 1]) assert.equal(group(n), n.toLocaleString('en-US'));
	assert.equal(group(123456789012345678901234567890n), (123456789012345678901234567890n).toLocaleString('en-US'));
});
