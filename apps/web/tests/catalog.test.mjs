// catalog.ts: the Browse data cards without a section on the page read one value from the hosted
// node; only an answer for the asked metric and subject with a plain decimal value is shown.
// Run: node --test apps/web/tests/catalog.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { DATASETS, groupDecimal, nodeSubjects, observation, observationUrl, readsFromNode } from '../src/lib/catalog.ts';

const M = 'us.treasury.tga_closing_balance';
const S = 'us.treasury.operating_cash:2026-10-27';
const answer = { metric: M, subjectText: S, value: '1052530', proofLevel: 2 };

test('every card leads somewhere: a section, a page, or a value read from the node', () => {
	for (const d of DATASETS) assert.ok(d.section || d.page || readsFromNode(d), d.id);
	assert.ok(DATASETS.some(readsFromNode));
	for (const d of DATASETS.filter(readsFromNode)) assert.equal(d.api, `/v1/observations/${d.id}`, d.id);
});

test('the shipped level history names a settled round for every card read from the node', async () => {
	const file = JSON.parse(await readFile(new URL('../static/level2/ladders-base-sepolia.json', import.meta.url), 'utf8'));
	const subjects = nodeSubjects(file);
	for (const d of DATASETS.filter(readsFromNode)) assert.match(subjects[d.id] ?? '', /:[0-9]{4}-[0-9]{2}-[0-9]{2}$/, d.id);
	assert.equal(observationUrl(`/v1/observations/${M}`, subjects[M]), `/v1/observations/${M}?subject=${subjects[M]}`);
});

test('a subject out of shape is dropped', () => {
	assert.deepEqual(nodeSubjects({ values: [{ metric: M, subject: S }, { metric: 'x', subject: 'x:2026-10-27&as_of=1' }, { metric: 'y', subject: '"><b>:2026-10-27' }, { metric: 3, subject: S }, null] }), { [M]: S });
	for (const bad of [null, 'x', {}, { values: 'no' }]) assert.deepEqual(nodeSubjects(bad), {});
});

test('an answer is shown only for the asked metric and subject, with a plain decimal value', () => {
	assert.deepEqual(observation(answer, M, S), { value: '1052530', level: 2 });
	assert.deepEqual(observation({ ...answer, value: '-0.5' }, M, S), { value: '-0.5', level: 2 });
	for (const bad of [
		{ ...answer, metric: 'us.treasury.other' },
		{ ...answer, subjectText: 'us.treasury.operating_cash:2026-10-26' },
		{ ...answer, value: 1052530 },
		{ ...answer, value: '<b>1</b>' },
		{ ...answer, value: '1e9' },
		{ ...answer, value: '' },
		{ ...answer, proofLevel: 9 },
		{ ...answer, proofLevel: '2' },
		{ error: { code: 'unknown_subject' } },
		null,
		'x'
	])
		assert.equal(observation(bad, M, S), null, JSON.stringify(bad));
});

test('a value keeps its digits: only the whole part is grouped', () => {
	assert.equal(groupDecimal('1052530'), '1,052,530');
	assert.equal(groupDecimal('40.1'), '40.1');
	assert.equal(groupDecimal('-1234567.8900'), '-1,234,567.8900');
	assert.equal(groupDecimal('0.0'), '0.0');
	assert.equal(groupDecimal('n/a'), 'n/a');
});
