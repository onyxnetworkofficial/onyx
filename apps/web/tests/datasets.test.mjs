// datasets.ts: the All datasets page keeps only answers in shape and formats keys, sizes and the
// node's search exactly as the node takes it. Run: node --test apps/web/tests/datasets.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { anyGroups, anyValue } from '../src/lib/glossary.ts';
import { cleanSearch, dayKeys, item, keyRange, keyText, listUrl, newestUrl, page, sample, size, trimDec, fromLink, toLink, isoDay, detailUrl, chartColumns, chartPoints, staleSince, STALE_DAYS, moved, infoUrl, countUrl } from '../src/lib/datasets.ts';

const good = {
	publisher: 'onchain-archive',
	dataset: 'btc.sth-net-position',
	version: 1,
	root: '0x' + '1'.repeat(64),
	rows: 5887,
	bytes: 1467114,
	shards: 1,
	stored_shards: 1,
	key_min: '1279324800',
	key_max: '1787875200',
	license: 'CC-BY-4.0',
	forked: false
};

test('a search is lowercase, only the characters the node takes, at most 64', () => {
	assert.equal(cleanSearch('  BTC   Realized<script> '), 'btc realized script');
	assert.equal(cleanSearch('eth.supply/x_y-z'), 'eth.supply/x_y-z');
	assert.equal(cleanSearch('a'.repeat(80)).length, 64);
	assert.equal(cleanSearch('%%%'), '');
});

test('the list URL carries the search, the page size and the cursor', () => {
	assert.equal(listUrl('', null), '/v1/bulk?limit=50');
	const u = new URL(listUrl('btc sth', 'onchain-archive/btc.a'), 'http://x');
	assert.deepEqual([u.searchParams.get('limit'), u.searchParams.get('after'), u.searchParams.get('q')], ['50', 'onchain-archive/btc.a', null]);
	assert.equal(u.searchParams.get('any'), anyValue(anyGroups('btc sth')));
	const [g1, g2] = u.searchParams.get('any').split(';').map((g) => g.split(','));
	assert.ok(g1[0] === 'btc*' && g2[0] === 'sth*');
});

test('an item out of shape is dropped', () => {
	assert.deepEqual(item(good)?.dataset, 'btc.sth-net-position');
	for (const bad of [
		{ ...good, dataset: '<img>' },
		{ ...good, publisher: 'Upper' },
		{ ...good, rows: -1 },
		{ ...good, rows: '5887' },
		{ ...good, key_min: '12.5' },
		{ ...good, stored_shards: undefined },
		null,
		'x'
	])
		assert.equal(item(bad), null, JSON.stringify(bad));
	assert.equal(item({ ...good, license: '<b>' })?.license, '');
	assert.equal(item({ ...good, created_unix: 1793727822 })?.created, 1793727822);
	assert.equal(item({ ...good, created_unix: 'soon' })?.created, null);
	assert.equal(isoDay(1793727822), '2026-11-03');
	assert.equal(isoDay(null), '');
	assert.equal(isoDay(9e12), '');
	const p = page({ datasets: [good, { ...good, dataset: 'BAD' }], total: 9026, next: 'onchain-archive/btc.sth-net-position' });
	assert.equal(p.items.length, 1);
	assert.equal(p.total, 9026);
	assert.equal(p.next, 'onchain-archive/btc.sth-net-position');
	assert.equal(page({ datasets: [good], next: 'javascript:x' }).next, null);
	assert.equal(page({ nope: 1 }), null);
});

test('day keys read as dates, other keys as grouped numbers', () => {
	assert.equal(dayKeys('1279324800', '1787875200'), true);
	assert.equal(dayKeys('1279324801', '1787875200'), false);
	assert.equal(dayKeys('0', '86400'), false);
	assert.equal(keyRange('1279324800', '1787875200'), '2010-07-17 to 2026-08-28');
	assert.equal(keyRange('1787875200', '1787875200'), '2026-08-28');
	assert.equal(keyRange('1500000000', '1501764888'), '1,500,000,000 to 1,501,764,888');
	assert.equal(keyRange('-5', '170141183460469231731687303715884105727'), '-5 to 170,141,183,460,469,231,731,687,303,715,884,105,727');
	assert.equal(keyText('1787875200', true), '2026-08-28');
	assert.equal(keyText('x', true), '');
});

test('decimals lose only their padding zeros', () => {
	assert.deepEqual(
		['2959.157427629921600000', '3.000', '-0.50', '100', '1e5', '10.0100', 'text 1.0'].map(trimDec),
		['2959.1574276299216', '3', '-0.5', '100', '1e5', '10.01', 'text 1.0']
	);
});

test('sizes', () => {
	assert.deepEqual([999, 1467114, 7221973879, 12345].map(size), ['999 B', '1.5 MB', '7.2 GB', '12 KB']);
});

test('the newest rows are the last page', () => {
	const d = item(good);
	assert.equal(newestUrl(d), '/v1/bulk/onchain-archive/btc.sth-net-position/rows?limit=10&page=588');
	assert.equal(newestUrl({ ...d, rows: 10 }), '/v1/bulk/onchain-archive/btc.sth-net-position/rows?limit=10&page=0');
	assert.equal(newestUrl({ ...d, rows: 0 }), '/v1/bulk/onchain-archive/btc.sth-net-position/rows?limit=10&page=0');
});

test('a rows answer becomes a table; anything out of shape is dropped', () => {
	const answer = {
		anchor: { book: '0x04bb31d80c8ce9df02c8a03486b5812ec6e5ccc5' },
		manifest: { schema: { columns: [{ name: 'day', kind: 'int' }, { name: 'sth_btc', kind: 'decimal', scale: 2 }] } },
		rows: [
			{ key: '1787788800', values: { day: '1787788800', sth_btc: '1.50' } },
			{ key: '1787875200', values: { day: '1787875200', sth_btc: null } },
			{ key: 'bad', values: { day: 'x' } }
		]
	};
	const s = sample(answer);
	assert.deepEqual(s.columns, ['day', 'sth_btc']);
	assert.deepEqual(s.rows, [
		['1787788800', '1.5'],
		['1787875200', '']
	]);
	assert.equal(s.from, '1787788800');
	assert.equal(s.to, '1787875200');
	assert.equal(s.book, '0x04bb31d80c8ce9df02c8a03486b5812ec6e5ccc5');
	assert.equal(sample({ ...answer, anchor: { book: 'evil' } }).book, '');
	assert.equal(sample({ ...answer, manifest: { schema: { columns: [{ name: '<b>' }] } } }), null);
	assert.equal(sample({ ...answer, rows: [] }), null);
});

// ADR-0059 amendment 5: a widened dataset's answer shows old rows padded to the newest scale (an
// added column null); the table shows the same text as before the widening, and a padded value
// past 80 characters is trimmed of its zeros before any cut (THREATS BULK-WIDEN-3).
test('sample of a widened answer shows the same text as before', () => {
	const before = {
		manifest: { schema: { columns: [{ name: 'day', kind: 'int' }, { name: 'v', kind: 'decimal', scale: 2 }] } },
		rows: [
			{ key: '1787788800', values: { day: '1787788800', v: '711.50' } },
			{ key: '1787875200', values: { day: '1787875200', v: '-3.00' } }
		]
	};
	const z = (n) => '0'.repeat(n);
	const after = {
		manifest: {
			format: 2,
			schema: { columns: [{ name: 'day', kind: 'int' }, { name: 'v', kind: 'decimal', scale: 22 }, { name: 'w', kind: 'int', nullable: true }] }
		},
		rows: [
			{ key: '1787788800', schema: 0, values: { day: '1787788800', v: '711.5' + z(21), w: null } },
			{ key: '1787875200', schema: 0, values: { day: '1787875200', v: '-3.' + z(22), w: null } }
		]
	};
	const a = sample(before);
	const b = sample(after);
	assert.deepEqual(b.columns, ['day', 'v', 'w']);
	assert.deepEqual(b.rows.map((r) => r.slice(0, 2)), a.rows);
	assert.deepEqual(b.rows.map((r) => r[2]), ['', '']);
	const long = '9'.repeat(77) + '.25';
	const wide = { ...after, rows: [{ key: '1', values: { day: '1', v: long + z(36), w: null } }] };
	assert.equal(sample(wide).rows[0][1], long);
});

test('a page link carries the search and the open dataset, and nothing else', () => {
	assert.equal(toLink(''), '#/datasets');
	assert.equal(toLink('btc sth'), '#/datasets?q=btc+sth');
	assert.equal(toLink('', 'onchain-archive/btc.a'), '#/datasets?d=onchain-archive%2Fbtc.a');
	assert.deepEqual(fromLink('#/datasets?q=BTC+sth&d=onchain-archive%2Fbtc.a'), { q: 'btc sth', d: 'onchain-archive/btc.a' });
	assert.deepEqual(fromLink('#/datasets'), { q: '', d: null });
	assert.deepEqual(fromLink('#/datasets?q=%3Cscript%3E&d=javascript:alert(1)'), { q: 'script', d: null });
	assert.deepEqual(fromLink('#/datasets?d=a/b/c'), { q: '', d: null });
});

test('a dataset opens with its newest 90 days (day keys) or its newest 10 rows', () => {
	const d = item(good);
	assert.equal(detailUrl(d), '/v1/bulk/onchain-archive/btc.sth-net-position/rows?from=1780185600&to=1787875200&limit=100');
	const young = { ...d, keyMin: '1787788800' };
	assert.equal(detailUrl(young), '/v1/bulk/onchain-archive/btc.sth-net-position/rows?from=1787788800&to=1787875200&limit=100');
	assert.equal(detailUrl({ ...d, keyMin: '5', keyMax: '9' }), newestUrl({ ...d, keyMin: '5', keyMax: '9' }));
});

test('a chart draws only plain-number columns, never a table of series', () => {
	const s = { columns: ['day', 'price', 'note', 'one'], rows: [['1787788800', '1.5', 'x', ''], ['1787875200', '-2', 'y', '3']], from: '', to: '', book: '' };
	assert.deepEqual(chartColumns(s), [1]);
	assert.deepEqual(chartPoints(s, 1, true), [
		{ v: 1.5, text: '1.5', head: '2026-08-27' },
		{ v: -2, text: '-2', head: '2026-08-28' }
	]);
	assert.deepEqual(chartColumns({ ...s, columns: ['day', 'series', 'i', 'v00'] }), []);
});

test('freshness: the newest day the node says and age, both or neither; stale only past 30 days (A20)', () => {
	assert.equal(STALE_DAYS, 30);
	const d = item({ ...good, newest: '2026-08-28', newest_age_days: 80 });
	assert.deepEqual([d.newest, d.ageDays], ['2026-08-28', 80]);
	assert.equal(staleSince(d), '2026-08-28');
	// The 30-day edge: 30 whole days behind is not stale, 31 is.
	assert.equal(staleSince(item({ ...good, newest: '2026-10-17', newest_age_days: 30 })), null);
	assert.equal(staleSince(item({ ...good, newest: '2026-10-16', newest_age_days: 31 })), '2026-10-16');
	assert.equal(staleSince(item({ ...good, newest: '2026-11-13', newest_age_days: 0 })), null);
	// A key that is not a time: the node sends neither field; nothing is shown, never stale.
	const plain = item({ ...good, key_min: '0', key_max: '900000' });
	assert.deepEqual([plain.newest, plain.ageDays, staleSince(plain)], [null, null, null]);
	// The page never reads a day out of the key itself: only the node says the key is a time.
	assert.equal(staleSince(item(good)), null);
	// Review FRESH-R1 M1: the node says the newest key is dated in the future. No day is kept
	// (even one the node sends beside it), and it is neither fresh nor stale.
	for (const more of [{}, { newest: '2099-12-31', newest_age_days: 0 }, { newest: '2026-08-28', newest_age_days: 80 }]) {
		const f = item({ ...good, key_max: '4102358400', newest_in_future: true, ...more });
		assert.deepEqual([f.future, f.newest, f.ageDays, staleSince(f)], [true, null, null, null], JSON.stringify(more));
	}
	assert.equal(d.future, false);
	assert.equal(item({ ...good, newest_in_future: 'yes' }).future, false);
	// Out of shape: both dropped, the dataset stays listed.
	for (const bad of [
		{ newest: '2026-08-28' },
		{ newest_age_days: 80 },
		{ newest: '<b>2026</b>', newest_age_days: 80 },
		{ newest: '2026-13-01', newest_age_days: 80 },
		{ newest: 20260828, newest_age_days: 80 },
		{ newest: '2026-08-28', newest_age_days: -1 },
		{ newest: '2026-08-28', newest_age_days: '80' },
		{ newest: '2026-08-28', newest_age_days: 1.5 }
	]) {
		const x = item({ ...good, ...bad });
		assert.deepEqual([x.dataset, x.newest, x.ageDays, staleSince(x)], [good.dataset, null, null, null], JSON.stringify(bad));
	}
});

test('the list URL asks the node for fresh datasets only when told to (A20)', () => {
	assert.equal(new URL(listUrl('btc', null), 'http://x').searchParams.get('fresh'), null);
	const u = new URL(listUrl('btc', 'onchain-archive/btc.a', 50, true), 'http://x');
	assert.equal(u.searchParams.get('fresh'), '30d');
	assert.equal(u.searchParams.get('after'), 'onchain-archive/btc.a');
	assert.equal(listUrl('', null, 50, true), '/v1/bulk?fresh=30d&limit=50');
});

// R-BULK-17: the node's 410 for a removed dataset names its successor (the body the hosted node
// gives for a moved dataset, copied in shape).
const removed = (successor) => ({ catalog: 15, erase: { due: 1797598136, phase: 'waiting' }, error: { code: 'bulk_dataset_removed', message: 'the publisher removed this dataset in its catalog 15' }, ...(successor === undefined ? {} : { successor }) });
const link = (extra = {}) => ({ dataset: 'trx.staking-flows-v2', final: 'trx.staking-flows-v2', generation: 1, hops: 1, proof: '/v1/bulk/onchain-archive/trx.staking-flows/successor', row: '1', status: 'live', ...extra });
const OLD = 'onchain-archive/trx.staking-flows';

test('a removed dataset with a live successor moved there, under the same publisher', () => {
	assert.equal(infoUrl(OLD), '/v1/bulk/onchain-archive/trx.staking-flows');
	assert.deepEqual(moved(410, removed(link()), OLD), { to: 'onchain-archive/trx.staking-flows-v2', live: true });
	// Two links: the chain's last id is the one opened, not the first.
	assert.deepEqual(moved(410, removed(link({ final: 'trx.staking-flows-v3', hops: 2 })), OLD), { to: 'onchain-archive/trx.staking-flows-v3', live: true });
});

test('a successor that is not live is named and never opened', () => {
	for (const status of ['removed', 'retracted', 'not_listed', 'no_links', 'cycle', 'too_long', 'pending', undefined, 7])
		assert.deepEqual(moved(410, removed(link({ status })), OLD), { to: 'onchain-archive/trx.staking-flows-v2', live: false }, String(status));
	// No id to name: a pending answer, an id out of shape, or a chain that ends where it began.
	for (const s of [{ status: 'pending' }, link({ final: '<img>' }), link({ final: 'a/b' }), link({ final: null }), link({ final: 'trx.staking-flows' })])
		assert.deepEqual(moved(410, removed(s), OLD), { to: null, live: false }, JSON.stringify(s));
});

test('only the node\'s 410 with a successor counts as moved', () => {
	assert.equal(moved(410, removed(), OLD), null);
	assert.equal(moved(410, removed(null), OLD), null);
	assert.equal(moved(410, removed('trx.staking-flows-v2'), OLD), null);
	assert.equal(moved(404, removed(link()), OLD), null);
	assert.equal(moved(200, removed(link()), OLD), null);
	assert.equal(moved(410, { error: { code: 'bulk_unknown_dataset' }, successor: link() }, OLD), null);
	assert.equal(moved(410, null, OLD), null);
	assert.equal(moved(410, 'gone', OLD), null);
});

test('the count is asked as an unfiltered list of one', () => {
	assert.equal(countUrl(), '/v1/bulk?limit=1');
	assert.equal(page({ datasets: [good], total: 9054, next: null }).total, 9054);
});

// ---- Newcomer test 2026-11-18: order, counts, headline column, CSV. ----
import { rankSearch, searchAssets, defaultOrder, defaultSegments, isTestSet, isFresh, freshCounts, freshIn, freshCountUrl, headlineColumn, newestFirst, csv, csvName, historyUrl, pythonSnippet, anyUrl, HISTORY_PAGE } from '../src/lib/datasets.ts';
import { datasetTitle } from '../src/lib/glossary.ts';

const D = (dataset, ageDays = 2, publisher = 'onchain-archive') => ({ publisher, dataset, ageDays });
const ids = (list) => list.map((d) => d.dataset);

test('a search names its assets by id or exact name, and its other words', () => {
	assert.deepEqual(searchAssets('Bitcoin MVRV'), { assets: ['btc'], rest: ['mvrv'] });
	assert.deepEqual(searchAssets('btc price'), { assets: ['btc'], rest: ['price'] });
	assert.deepEqual(searchAssets('usdt-trx supply'), { assets: ['usdt-trx'], rest: ['supply'] });
	assert.deepEqual(searchAssets('tron staking flows'), { assets: ['trx'], rest: ['staking', 'flows'] });
	// Part of a name is not an asset: "bitc" stays a word.
	assert.deepEqual(searchAssets('bitc'), { assets: [], rest: ['bitc'] });
	assert.deepEqual(searchAssets(''), { assets: [], rest: [] });
});

test('search ranking: the exact asset first, then the exact metric; a stale exact asset before a fresh other asset', () => {
	const list = [D('wbtc.mvrv', 1), D('btc.sth-mvrv', 1), D('btc.mvrv', 82), D('btc.mvrv-z-score', 82), D('wbtc.btc-mvrv', 1), D('btc.active-mvrv', 82)];
	assert.deepEqual(ids(rankSearch(list, 'bitcoin mvrv')), ['btc.mvrv', 'btc.sth-mvrv', 'btc.active-mvrv', 'btc.mvrv-z-score', 'wbtc.mvrv', 'wbtc.btc-mvrv']);
	// The same with the id typed, and whatever order the node sent.
	assert.deepEqual(ids(rankSearch([...list].reverse(), 'btc mvrv')), ids(rankSearch(list, 'bitcoin mvrv')));
	const price = [D('wbtc.price', 0), D('btc.price-floor-levels', 0), D('btc.realized-price', 0), D('btc.price', 82)];
	assert.deepEqual(ids(rankSearch(price, 'bitcoin price')), ['btc.price', 'btc.realized-price', 'btc.price-floor-levels', 'wbtc.price']);
	// No asset searched: only the exact metric, freshness, the shorter name and the id decide.
	assert.deepEqual(ids(rankSearch([D('eth.mvrv-bands', 1), D('eth.mvrv', 90), D('btc.mvrv', 1)], 'mvrv')), ['btc.mvrv', 'eth.mvrv', 'eth.mvrv-bands']);
	// A key that is no time (no age) counts as not fresh, never as an error.
	assert.deepEqual(ids(rankSearch([D('btc.b', null), D('btc.a', 5)], 'btc')), ['btc.a', 'btc.b']);
	assert.deepEqual(rankSearch([], 'btc'), []);
});

test('the opening order: updated in the last 30 days first, majors first, test data last', () => {
	const list = [D('testnet.early-contributors', null, 'early-record'), D('1inch.acdd', 2), D('btc.mvrv', 82), D('eth.supply', 3), D('aave.price', 40), D('btc.price', 1), D('sol.price', 30), D('eth.wide', null), D('testnet.x', 1)];
	const want = ['btc.price', 'eth.supply', '1inch.acdd', 'sol.price', 'btc.mvrv', 'eth.wide', 'aave.price', 'testnet.early-contributors', 'testnet.x'];
	assert.deepEqual(ids([...list].sort(defaultOrder)), want);
	assert.equal(isTestSet(D('testnet.x')), true);
	assert.equal(isTestSet(D('btc.price', 1, 'early-record')), true);
	assert.equal(isTestSet(D('btc.testnet-fees')), false);
	assert.deepEqual([isFresh(D('a.b', 30)), isFresh(D('a.b', 31)), isFresh(D('a.b', null))], [true, false, false]);
});

test('the opening list is read in steps that give the same order', () => {
	const list = [D('testnet.early-contributors', null, 'early-record'), D('1inch.acdd', 2), D('btc.mvrv', 82), D('eth.supply', 3), D('aave.price', 40), D('btc.price', 1), D('eth.btc-ratio', 1), D('eth.wide', null)];
	// A fake node: id order, the `fresh` filter, and whole-word `any` terms.
	const node = (seg) =>
		[...list]
			.sort((a, b) => (`${a.publisher}/${a.dataset}` < `${b.publisher}/${b.dataset}` ? -1 : 1))
			.filter((d) => !seg.fresh || isFresh(d))
			.filter((d) => !seg.any || seg.any.split(',').some((t) => `${d.publisher}/${d.dataset}`.split(/[^a-z0-9]+/).join(' ').includes(t.replace('*', '').replace('-', ' '))));
	const read = (freshOnly) => defaultSegments(freshOnly).flatMap((seg) => node(seg).filter(seg.keep));
	assert.deepEqual(ids(read(false)), ids([...list].sort(defaultOrder)));
	assert.deepEqual(ids(read(true)), ['btc.price', 'eth.btc-ratio', 'eth.supply', '1inch.acdd']);
	// Each step is a list call the node takes.
	assert.deepEqual(defaultSegments(true).map((s) => anyUrl(s.any, null, 50, s.fresh)), ['/v1/bulk?any=btc&fresh=30d&limit=50', '/v1/bulk?any=eth&fresh=30d&limit=50', '/v1/bulk?fresh=30d&limit=50', '/v1/bulk?any=testnet*%2Cearly-record&fresh=30d&limit=50']);
	assert.equal(defaultSegments(false).length, 7);
});

test('staleness counts come from the node\'s two totals, or not at all', () => {
	assert.deepEqual(freshCounts(9054, 6302), { all: 9054, fresh: 6302, stale: 2752 });
	assert.deepEqual(freshCounts(5, 5), { all: 5, fresh: 5, stale: 0 });
	assert.deepEqual(freshCounts(5, 0), { all: 5, fresh: 0, stale: 5 });
	for (const [a, f] of [[null, 3], [9054, null], [3, 4], [0, 0], [-1, 0], [1.5, 1], [5, -1]]) assert.equal(freshCounts(a, f), null, `${a}, ${f}`);
	assert.equal(freshIn([D('a.b', 1), D('a.c', 31), D('a.d', null), D('a.e', 30)]), 2);
	assert.equal(freshCountUrl(), '/v1/bulk?fresh=30d&limit=1');
});

test('the chart opens with the column named like the metric', () => {
	const S = (columns) => ({ columns, rows: [columns.map((_, i) => String(i + 1)), columns.map((_, i) => String(i + 2))], from: '', to: '', book: '', nextPage: null });
	const pick = (cols, id) => cols[headlineColumn(S(cols), id)];
	const mvrv = ['day', 'c_4year_minus_1_sigma', 'c_4year_plus_1_sigma', 'lth_mvrv', 'market_cap_usd', 'mvrv', 'realized_cap_usd', 'sth_mvrv'];
	assert.equal(pick(mvrv, 'btc.mvrv'), 'mvrv');
	assert.equal(pick(['day', 'price_usd'], 'btc.price'), 'price_usd');
	assert.equal(pick(['day', 'market_cap_usd', 'mvrv', 'realized_cap_usd', 'mvrv_zscore'], 'btc.mvrv-z-score'), 'mvrv_zscore');
	assert.equal(pick(['day', 'lth_percentage', 'lth_sopr', 'lth_supply_native', 'sopr'], 'btc.lth-sopr'), 'lth_sopr');
	assert.equal(pick(['day', 'mvrv', 'realized_price_usd', 'raw_mvrv'], 'btc.entity-adjusted-mvrv'), 'mvrv');
	assert.equal(pick(['day', 'sth_btc_in_profit', 'sth_net_position_change', 'sth_accumulation'], 'btc.sth-net-position'), 'sth_net_position_change');
	// Nothing named like the metric: the first column a chart can draw (never the key).
	assert.equal(pick(['day', 'net_staked', 'staked'], 'trx.staking-flows-v2'), 'net_staked');
	assert.equal(pick(['day', 'a', 'b'], 'btc'), 'a');
	// A column of text is never picked, and a table of series has no chart.
	const text = { ...S(['day', 'mvrv', 'x']), rows: [['1', 'high', '2'], ['2', 'low', '3']] };
	assert.equal(text.columns[headlineColumn(text, 'btc.mvrv')], 'x');
	assert.equal(headlineColumn(S(['day', 'series', 'v']), 'btc.v'), -1);
});

test('the newest rows are listed newest first', () => {
	const rows = Array.from({ length: 12 }, (_, i) => [String(i)]);
	assert.deepEqual(newestFirst(rows).map((r) => r[0]), ['11', '10', '9', '8', '7', '6', '5', '4', '3', '2']);
	assert.deepEqual(newestFirst([['1'], ['2']]), [['2'], ['1']]);
	assert.equal(rows[0][0], '0', 'the sample itself keeps its order');
});

test('CSV: a header, one line a row, quoting, dates for day keys, no formulas', () => {
	assert.equal(csv(['day', 'price_usd'], [['1787788800', '1.5'], ['1787875200', '-2']], true), 'day,price_usd\r\n2026-08-27,1.5\r\n2026-08-28,-2\r\n');
	assert.equal(csv(['height', 'v'], [['900000', '']], false), 'height,v\r\n900000,\r\n');
	assert.equal(csv(['k', 'note'], [['1', 'a,b'], ['2', 'say "hi"'], ['3', 'two\nlines']]), 'k,note\r\n1,"a,b"\r\n2,"say ""hi"""\r\n3,"two\nlines"\r\n');
	// A text a spreadsheet would run is made plain text; a negative number is left alone.
	assert.equal(csv(['k', 'v'], [['1', '=1+1'], ['2', '@x'], ['3', '-cmd'], ['4', '-0.5'], ['5', '+1']]), "k,v\r\n1,'=1+1\r\n2,'@x\r\n3,'-cmd\r\n4,-0.5\r\n5,'+1\r\n");
	assert.equal(csv(['k'], []), 'k\r\n');
	assert.equal(csvName({ publisher: 'onchain-archive', dataset: 'btc.mvrv' }), 'onchain-archive_btc.mvrv.csv');
});

test('a full history is read 1,000 rows a call, pinned to one version', () => {
	assert.equal(HISTORY_PAGE, 1000);
	const d = item({ ...good, root: '0x' + 'ab'.repeat(32) });
	assert.equal(historyUrl(d, 0), `/v1/bulk/onchain-archive/btc.sth-net-position/rows?limit=1000&page=0&root=0x${'ab'.repeat(32)}`);
	assert.equal(historyUrl({ ...d, root: '' }, 3), '/v1/bulk/onchain-archive/btc.sth-net-position/rows?limit=1000&page=3');
	assert.equal(item({ ...good, root: 'javascript:x' }).root, '');
	assert.equal(sample({ manifest: { schema: { columns: [{ name: 'day' }] } }, rows: [{ key: '1', values: { day: '1' } }], next_page: 4 }).nextPage, 4);
	assert.equal(sample({ manifest: { schema: { columns: [{ name: 'day' }] } }, rows: [{ key: '1', values: { day: '1' } }], next_page: null }).nextPage, null);
	const py = pythonSnippet('https://node.example', d).split('\n');
	assert.equal(py.length, 5);
	assert.ok(py[1].includes('https://node.example/v1/bulk/onchain-archive/btc.sth-net-position/rows') && py[2].includes('"limit": 1000') && py[2].includes('"order": "desc"'));
});

test('"z-score" reads as one term in a title', () => {
	assert.equal(datasetTitle('btc.mvrv-z-score'), 'Bitcoin · MVRV Z-score');
	assert.equal(datasetTitle('btc.sth-mvrv-z-score'), 'Bitcoin · Short-term holder MVRV Z-score');
	assert.equal(datasetTitle('eth.z-score-bands'), 'Ethereum · Z-score bands');
});
