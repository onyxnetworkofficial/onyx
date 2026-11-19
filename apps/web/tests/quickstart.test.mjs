// The API page's quick-start commands (src/lib/quickstart.ts). Review NEWC-R1 H1: the check
// commands must be ones the released onyx-check accepts. Since v0.4.0-testnet (READINESS A21) that
// is a newest-first page with --order desc; one oldest-first example with whole-number bounds
// stays, and no example uses a date. Run: node --test apps/web/tests/quickstart.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const Q = await import(new URL('../src/lib/quickstart.ts', import.meta.url).href);
const ORIGIN = 'https://onyx.cryptagraph.com';
const DATE = /\d{4}-\d{2}-\d{2}/;

const R = await import(new URL('../src/lib/release.ts', import.meta.url).href);
const opts = (line) => {
	const args = line.split(' ');
	return { args, opt: (k) => args[args.indexOf(k) + 1], flags: args.filter((a) => a.startsWith('--')) };
};

test('the newest-first check: the same read as command 3, checked with --order desc and no --to', () => {
	const [curl, check, ...rest] = Q.checkCommands(ORIGIN).split('\n');
	assert.equal(rest.length, 0);
	for (const line of [curl, check]) assert.doesNotMatch(line, DATE, line);
	const u = new URL(/"([^"]+)"/.exec(curl)[1]);
	assert.equal(u.origin, ORIGIN);
	assert.equal(u.pathname, `/v1/bulk/${Q.EXAMPLE.publisher}/${Q.EXAMPLE.dataset}/rows`);
	// Page 0 with no bounds: the check then proves the page starts at the newest row.
	assert.deepEqual([...u.searchParams.keys()], ['order', 'limit']);
	assert.equal(u.searchParams.get('order'), 'desc');
	assert.match(u.searchParams.get('limit'), /^\d+$/);
	assert.match(curl, /> rows\.json$/);
	const { args, opt, flags } = opts(check);
	assert.equal(args[0], 'onyx-check');
	assert.equal(opt('--bulk'), 'rows.json');
	assert.equal(opt('--publisher'), Q.EXAMPLE.publisher);
	assert.equal(opt('--dataset'), Q.EXAMPLE.dataset);
	assert.equal(opt('--order'), 'desc');
	assert.equal(opt('--chain-id'), '84532');
	assert.equal(opt('--book'), Q.BOOK);
	assert.deepEqual(flags, ['--bulk', '--publisher', '--dataset', '--order', '--rpc', '--chain-id', '--book']);
	// --order is an option of the release the page links: v0.4.0-testnet or later.
	const [major, minor] = R.versionOf(R.RELEASE);
	assert.ok(major > 0 || minor >= 4, R.RELEASE);
});

test('the range check: oldest first, whole-number bounds, the form every released onyx-check reads', () => {
	const [curl, check, ...rest] = Q.checkRangeCommands(ORIGIN).split('\n');
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
	const { args, opt, flags } = opts(check);
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
	assert.deepEqual(flags, ['--bulk', '--publisher', '--dataset', '--from', '--to', '--rpc', '--chain-id', '--book']);
});

test('three read commands against the given origin; the book is the deployment file\'s', () => {
	const cmds = Q.quickCommands('https://node.example');
	assert.equal(cmds.length, 3);
	for (const c of cmds) assert.match(c.cmd, /^curl "?https:\/\/node\.example\/v1\/bulk/);
	assert.match(cmds[2].cmd, /order=desc&limit=3/);
	// READINESS A20: the example is a dataset with data up to this month, not one that stopped
	// (btc.mvrv ended 2026-08-28), and the search command finds it by its own name. Newcomer test
	// 2026-11-18: a Bitcoin dataset (btc.*), not Wrapped Bitcoin (wbtc.*).
	assert.deepEqual(Q.EXAMPLE, { publisher: 'onchain-archive', dataset: 'btc.entity-adjusted-mvrv' });
	assert.match(cmds[0].cmd, /\/v1\/bulk\?q=btc\.entity-adjusted-mvrv&limit=5"$/);
	assert.equal(cmds[1].cmd, 'curl https://node.example/v1/bulk/onchain-archive/btc.entity-adjusted-mvrv');
	assert.equal(cmds[2].cmd, 'curl "https://node.example/v1/bulk/onchain-archive/btc.entity-adjusted-mvrv/rows?order=desc&limit=3"');
	const contracts = JSON.parse(readFileSync(new URL('../../../deploy/early-record/contracts.json', import.meta.url), 'utf8'));
	assert.equal(contracts.contracts.find((c) => c.name === 'OnyxSignedBook').address, Q.BOOK);
});

test('the page shows these commands and names the release a newest-first check needs', () => {
	const page = readFileSync(new URL('../src/routes/api/+page.svelte', import.meta.url), 'utf8');
	assert.match(page, /checkCommands\(origin\)/);
	assert.match(page, /checkRangeCommands\(origin\)/);
	assert.doesNotMatch(page, /next release/);
	assert.match(page, /quickCommands\(origin\)/);
	// No check command written by hand in the page.
	assert.doesNotMatch(page, /onyx-check --bulk/);
	assert.match(page, /data-testid="api-quick-release"/);
});

// Newcomer test 2026-11-18: the page says rows come 1,000 a call at most and how to page, with
// the node's own parameter names, and names the release file that holds onyx-check.
test('the paging command uses the node\'s limit and page; the check file is the publisher bundle', () => {
	const u = new URL(Q.pageCommand('https://node.example').replace(/^curl "|"$/g, ''));
	assert.equal(u.pathname, `/v1/bulk/${Q.EXAMPLE.publisher}/${Q.EXAMPLE.dataset}/rows`);
	assert.deepEqual([...u.searchParams.keys()], ['limit', 'page']);
	assert.equal(u.searchParams.get('limit'), '1000');
	assert.equal(Q.MAX_ROWS, 1000);
	// The same limit and parameter names as the node (crates/onyx-api/src/bulk_serve.rs).
	const node = readFileSync(new URL('../../../crates/onyx-api/src/bulk_serve.rs', import.meta.url), 'utf8');
	assert.match(node, /pub const MAX_PAGE_ROWS: u64 = 1_000;/);
	assert.match(node, /"page" => \{/);
	assert.match(node, /"next_page"/);
	// The release script puts onyx-check in exactly this file.
	const build = readFileSync(new URL('../../../scripts/release-build.sh', import.meta.url), 'utf8');
	assert.ok(build.includes(`${Q.CHECK_FILE} `) && /onyx-publish and onyx-check/.test(build));
	const page = readFileSync(new URL('../src/routes/api/+page.svelte', import.meta.url), 'utf8');
	assert.match(page, /pageCommand\(origin\)/);
	assert.match(page, /Everything on this page is free to read today: no key, no sign-up/);
	assert.match(page, /CHECK_FILE/);
});
