// glossary.ts: plain-English titles and meanings for bulk dataset ids, and the meaning search it
// builds for the node's `any` (bulk_serve.rs AnyTerm). Run: node --test apps/web/tests/glossary.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { ASSETS, MEANINGS, WORDS, anyGroups, anyValue, datasetTitle, meanings, metricTitle, split } from '../src/lib/glossary.ts';
import * as G from '../src/lib/glossary.ts';

test('titles read as plain English; unknown abbreviations stay as written', () => {
	const cases = {
		'btc.sth-net-position': 'Bitcoin · Short-term holder net position',
		'eth.realized-price-1d-1w': 'Ethereum · Realized price 1 day to 1 week',
		'ltc.sth-lth-cdd': 'Litecoin · Short- and long-term holder coin days destroyed',
		'btc.op-return-data-volume': 'Bitcoin · OP_RETURN data volume',
		'btc.entity-adjusted-mvrv': 'Bitcoin · Entity-adjusted MVRV',
		'sol.a2vr-momentum': 'Solana · A2VR momentum',
		'btc.rainbow-q35': 'Bitcoin · Rainbow Q35',
		'usdt-trx.price': 'Tether on Tron · Price',
		'btc.supply-1yr-plus': 'Bitcoin · Supply 1 year plus',
		'zzz.lth-sopr-7d': 'ZZZ · Long-term holder SOPR 7 days'
	};
	for (const [id, want] of Object.entries(cases)) assert.equal(datasetTitle(id), want, id);
	assert.equal(metricTitle([]), '');
});

test('meanings: standard terms only, longest first, in the order of the name, at most two', () => {
	assert.deepEqual(meanings(split('btc.price').words), ['The daily price in US dollars.']);
	assert.deepEqual(meanings(split('btc.price-floor-levels').words), []);
	const m = meanings(split('btc.cost-basis-heatmap-sth').words);
	assert.equal(m.length, 2);
	assert.ok(m[0].startsWith('Cost basis:') && m[1].startsWith('Heatmap:'));
	assert.ok(meanings(split('btc.realized-price').words)[0].startsWith('Realized price:'), 'realized-price, not realized or price');
	assert.deepEqual(meanings(split('btc.rps-is-above').words), []);
	for (const [k, v] of Object.entries(MEANINGS)) {
		assert.match(k, /^[a-z0-9]+(-[a-z0-9]+)*$/, k);
		assert.ok(v.length < 200 && /\.$/.test(v) && !/https?:|inspect|watch/i.test(v), k);
	}
	for (const [k, v] of Object.entries(WORDS)) assert.ok(/^[a-z0-9]+$/.test(k) && v.length > 0, k);
	for (const [k, a] of Object.entries(ASSETS)) assert.ok(/^[a-z0-9]+(-[a-z0-9]+)*$/.test(k) && a.name.length > 0, k);
});

test('meaning search: names, kinds and meanings become id words the node takes', () => {
	assert.deepEqual(anyGroups(''), []);
	assert.deepEqual(anyGroups('eth'), [['eth*']]);
	const [btc, holders] = anyGroups('Bitcoin holders');
	assert.deepEqual(btc, ['bitcoin*', 'btc'], 'exactly a name: that asset only');
	assert.ok(['btc', 'bch', 'wbtc'].every((x) => anyGroups('bitc')[0].includes(x)), 'part of a name: every asset it starts');
	assert.deepEqual(anyGroups('ethereum')[0], ['ethereum*', 'eth']);
	assert.ok(['lth', 'sth'].every((x) => holders.includes(x)));
	assert.ok(anyGroups('stablecoin')[0].includes('usdt') && !anyGroups('stablecoin')[0].includes('btc'));
	assert.ok(anyGroups('destroyed')[0].includes('cdd'));
	assert.ok(anyGroups('whale')[0].includes('balance-distribution'));
	// Within the node's caps for any input: 8 groups, 200 terms, its term grammar.
	const wide = anyGroups('s t a b c d e f g h stablecoin profit holders whale bitcoin ethereum solana price');
	assert.ok(wide.length <= 8);
	const terms = wide.flat();
	assert.ok(terms.length <= 200);
	for (const t of terms) assert.match(t, /^[a-z0-9]+(-[a-z0-9]+)*\*?$/, t);
	assert.equal(anyValue([['a*', 'b'], ['c*']]), 'a*,b;c*');
});

// Newcomer test 3 (2026-11-18): "eth.gas-analytics" was titled and explained as gas, and holds no
// gas column (checked against the live columns of every chain's copy).
test('a metric whose id would mislead is titled and explained by what its columns are', () => {
	assert.equal(G.datasetTitle('eth.gas-analytics'), 'Ethereum · Network activity');
	assert.equal(G.datasetTitle('bnb.gas-analytics'), 'BNB · Network activity');
	const [m] = G.meanings(G.split('eth.gas-analytics').words);
	assert.match(m, /^Network activity by day: active addresses, transactions/);
	assert.match(m, /coins burned/);
	assert.match(m, /no gas column/);
	assert.equal(G.meanings(G.split('eth.gas-analytics').words).length, 1);
	// A metric that is about gas keeps the plain meaning and its own words.
	assert.equal(G.datasetTitle('hype.gas-used-today'), 'Hyperliquid · Gas used today');
	assert.match(G.meanings(['gas', 'used', 'today'])[0], /^Gas: what transactions pay/);
	// btc.gini holds percentiles, not one coefficient.
	assert.match(G.meanings(['gini'])[0], /percentiles \(p25 to p95\), not one coefficient/);
	assert.match(G.meanings(['gini', 'volatility'])[0], /^Gini coefficient/);
	// Worded so they hold for every dataset they are shown on.
	assert.match(G.MEANINGS['hodl-waves'], /supply split into age bands/);
	assert.match(G.MEANINGS.exchange, /addresses the publisher labels as exchanges/);
});

test('the unit legend explains the endings the columns carry, and only those', () => {
	assert.deepEqual(G.unitLegend(['day', 'price_usd', 'transfer_volume_native', 'daily_tx_count', 'supply_in_profit_pct']), [
		'_usd: in US dollars',
		'_native: in the asset’s own coin or token, not in dollars',
		'_count: a count (a whole number)',
		'_pct: a percentage (50 means 50%)'
	]);
	assert.deepEqual(G.unitLegend(['day', 'liveliness_percent']), ['_pct: a percentage (50 means 50%)']);
	assert.deepEqual(G.unitLegend(['day', 'mvrv', 'usd_price', 'count_x']), []);
	assert.deepEqual(G.unitLegend([]), []);
});

// Newcomer test 3 (2026-11-18): "bitcon" and "etherium gas price" found nothing and offered nothing.
test('did you mean: the nearest known word, by edit distance on the site\'s own names', () => {
	assert.equal(G.editDistance('bitcon', 'bitcoin'), 1);
	assert.equal(G.editDistance('etherium', 'ethereum'), 1);
	assert.equal(G.editDistance('hodlers', 'holders'), 1, 'two neighbours swapped is one edit');
	assert.equal(G.editDistance('abc', 'abc'), 0);
	assert.equal(G.editDistance('abcdef', 'uvwxyz'), 3, 'stops counting above the cap');
	assert.equal(G.nearestWord('bitcon'), 'bitcoin');
	assert.equal(G.nearestWord('etherium'), 'ethereum');
	assert.equal(G.nearestWord('solona'), 'solana');
	assert.equal(G.nearestWord('suply'), 'supply');
	// A known word, a word start (the node matches starts), a short word and a duration stay.
	for (const w of ['bitcoin', 'bitc', 'mvrv', 'btc', 'eth', '30d', '2024', 'price']) assert.equal(G.nearestWord(w), w, w);
	assert.equal(G.nearestWord('zzzzqq'), null);
	assert.ok(G.KNOWN_WORDS.includes('bitcoin') && G.KNOWN_WORDS.includes('mvrv') && G.KNOWN_WORDS.includes('supply') && new Set(G.KNOWN_WORDS).size === G.KNOWN_WORDS.length);
	for (const w of G.KNOWN_WORDS) assert.match(w, /^[a-z0-9]{3,}$/, w);
});

test('did you mean: the corrected search first, then with words dropped from the end', () => {
	assert.deepEqual(G.suggestSearches('bitcon'), ['bitcoin']);
	assert.deepEqual(G.suggestSearches('etherium gas price'), ['ethereum gas price', 'ethereum gas', 'ethereum']);
	assert.deepEqual(G.suggestSearches('Ethereum Gas Price'), ['ethereum gas', 'ethereum']);
	assert.deepEqual(G.suggestSearches('btc suply'), ['btc supply', 'btc']);
	// Nothing to offer: one known word, an unknown word, nothing typed.
	for (const q of ['mvrv', 'bitcoin', 'zzzzqq', '', '   ']) assert.deepEqual(G.suggestSearches(q), [], q);
	// An unknown word is dropped, the rest kept; never more than 4, never the typed search.
	assert.deepEqual(G.suggestSearches('bitcoin zzzzqq'), ['bitcoin']);
	const many = G.suggestSearches('bitcoin holders supply price market cap ratio');
	assert.ok(many.length === 4 && !many.includes('bitcoin holders supply price market cap ratio'));
	// Every suggestion is a search the node takes.
	for (const q of [...many, ...G.suggestSearches('etherium gas price')]) assert.match(q, /^[a-z0-9]+( [a-z0-9]+)*$/, q);
});
