// datasets.ts: the All datasets page keeps only answers in shape and formats keys, sizes and the
// node's search exactly as the node takes it. Run: node --test apps/web/tests/datasets.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { anyGroups, anyValue } from '../src/lib/glossary.ts';
import { cleanSearch, dayKeys, item, keyRange, keyText, listUrl, newestUrl, page, sample, size, trimDec, fromLink, toLink, isoDay, detailUrl, chartColumns, chartPoints } from '../src/lib/datasets.ts';

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
