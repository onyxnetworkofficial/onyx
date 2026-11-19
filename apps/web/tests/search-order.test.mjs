// One search order in three places: fixtures/search/order.json holds the website (rankSearch in
// src/lib/datasets.ts), the node (crates/onyx-api/src/bulk_names.rs) and the agent toolkit
// (apps/agent/src/connect.mjs) to the same order of the same ids. Each reads the file in a test of
// its own; this is the website's. Needs Node 22.18 or later (it imports the TypeScript file with
// its types stripped). Run: node --test apps/web/tests/search-order.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { MAJORS, STALE_DAYS, rankSearch, searchAssets } from '../src/lib/datasets.ts';

const fx = JSON.parse(readFileSync(new URL('../../../fixtures/search/order.json', import.meta.url), 'utf8'));
const item = (d, ageDays) => {
	const [publisher, dataset] = d.id.split('/');
	return { publisher, dataset, ageDays };
};
const ids = (list) => list.map((d) => `${d.publisher}/${d.dataset}`);

test('the fixture names the website\'s majors and its stale rule', () => {
	assert.deepEqual(MAJORS, fx.majors);
	assert.equal(STALE_DAYS, fx.staleDays);
	assert.ok(fx.cases.length >= 3);
	assert.ok(fx.cases.some((c) => c.named) && fx.cases.some((c) => !c.named));
	// A case with ages that change the order, so `orderByAge` pins more than `order` does.
	assert.ok(fx.cases.some((c) => c.order.join() !== c.orderByAge.join()));
});

for (const c of fx.cases) {
	test(`search order: ${c.name}`, () => {
		assert.equal(searchAssets(c.q).assets.length > 0, c.named);
		// Whatever order the node sent the matches in.
		for (const sent of [c.datasets, [...c.datasets].reverse()]) {
			assert.deepEqual(ids(rankSearch(sent.map((d) => item(d, d.ageDays)), c.q)), c.orderByAge, 'with the ages given');
			assert.deepEqual(ids(rankSearch(sent.map((d) => item(d, 1)), c.q)), c.order, 'every dataset fresh: the order of the ids alone, the node\'s');
		}
		// The same ids in both orders, each once.
		assert.deepEqual([...c.order].sort(), c.datasets.map((d) => d.id).sort());
		assert.deepEqual([...c.orderByAge].sort(), [...c.order].sort());
	});
}
