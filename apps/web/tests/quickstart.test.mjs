// The API page's quick-start commands (src/lib/quickstart.ts). Review NEWC-R1 H1: the check
// commands must be ones the released onyx-check accepts: an oldest-first answer and whole-number
// bounds, never order=desc and never a date. Run: node --test apps/web/tests/quickstart.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const Q = await import(new URL('../src/lib/quickstart.ts', import.meta.url).href);
const ORIGIN = 'https://onyx.cryptagraph.com';
const DATE = /\d{4}-\d{2}-\d{2}/;

test('the check commands suit the released onyx-check: oldest first, whole-number bounds', () => {
	const [curl, check, ...rest] = Q.checkCommands(ORIGIN).split('\n');
	assert.equal(rest.length, 0);
	for (const line of [curl, check]) {
		assert.doesNotMatch(line, /order=|--order|--page|page=/, line);
		assert.doesNotMatch(line, DATE, line);
	}
	const u = new URL(/"([^"]+)"/.exec(curl)[1]);
	assert.equal(u.origin, ORIGIN);
	assert.equal(u.pathname, `/v1/bulk/${Q.EXAMPLE.publisher}/${Q.EXAMPLE.dataset}/rows`);
	assert.deepEqual([...u.searchParams.keys()], ['from', 'to', 'limit']);
	for (const k of ['from', 'to', 'limit']) assert.match(u.searchParams.get(k), /^\d+$/);
	assert.match(curl, /> rows\.json$/);
	// The check asks exactly the range the request asked, on the caller's own chain node.
	const args = check.split(' ');
	const opt = (k) => args[args.indexOf(k) + 1];
	assert.equal(args[0], 'onyx-check');
	assert.equal(opt('--bulk'), 'rows.json');
	assert.equal(opt('--publisher'), Q.EXAMPLE.publisher);
	assert.equal(opt('--dataset'), Q.EXAMPLE.dataset);
	assert.equal(opt('--from'), u.searchParams.get('from'));
	assert.equal(opt('--to'), u.searchParams.get('to'));
	assert.equal(opt('--chain-id'), '84532');
	assert.equal(opt('--book'), Q.BOOK);
	// The range is whole days in unix seconds: 2026-01-01 to 2026-01-31.
	assert.equal(new Date(Q.CHECK_RANGE.from * 1000).toISOString(), '2026-01-01T00:00:00.000Z');
	assert.equal(new Date(Q.CHECK_RANGE.to * 1000).toISOString(), '2026-01-31T00:00:00.000Z');
	// Every option is one the released onyx-check knows (crates/onyx-check at v0.3.1-testnet).
	const known = ['--bulk', '--publisher', '--dataset', '--from', '--to', '--rpc', '--chain-id', '--book'];
	assert.deepEqual(args.filter((a) => a.startsWith('--')), known);
});

test('three read commands against the given origin; the book is the deployment file\'s', () => {
	const cmds = Q.quickCommands('https://node.example');
	assert.equal(cmds.length, 3);
	for (const c of cmds) assert.match(c.cmd, /^curl "?https:\/\/node\.example\/v1\/bulk/);
	assert.match(cmds[2].cmd, /order=desc&limit=3/);
	// READINESS A20: the example is a dataset with data up to this month, not one that stopped
	// (btc.mvrv ended 2026-08-28), and the search command finds it by its own name.
	assert.deepEqual(Q.EXAMPLE, { publisher: 'onchain-archive', dataset: 'wbtc.mvrv' });
	assert.match(cmds[0].cmd, /\/v1\/bulk\?q=wbtc\.mvrv&limit=5"$/);
	const contracts = JSON.parse(readFileSync(new URL('../../../deploy/early-record/contracts.json', import.meta.url), 'utf8'));
	assert.equal(contracts.contracts.find((c) => c.name === 'OnyxSignedBook').address, Q.BOOK);
});

test('the page shows these commands and says what the released check does not read', () => {
	const page = readFileSync(new URL('../src/routes/api/+page.svelte', import.meta.url), 'utf8');
	assert.match(page, /checkCommands\(origin\)/);
	assert.match(page, /quickCommands\(origin\)/);
	// No check command written by hand in the page.
	assert.doesNotMatch(page, /onyx-check --bulk/);
	assert.match(page, /data-testid="api-quick-release"/);
});
