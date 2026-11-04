// glossary.ts: plain-English titles and meanings for bulk dataset ids, and the meaning search it
// builds for the node's `any` (bulk_serve.rs AnyTerm). Run: node --test apps/web/tests/glossary.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { ASSETS, MEANINGS, WORDS, anyGroups, anyValue, datasetTitle, meanings, metricTitle, split } from '../src/lib/glossary.ts';

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
