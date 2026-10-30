// onyx-contrib (CL2) without a network: a fake provider and a fake Onyx intake on 127.0.0.1.
// The key must never reach Onyx, a tool result, a log line or a stored file (THREATS CL2-1..CL2-8).
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { PassThrough } from 'node:stream';
import { MAX_PROPOSALS } from '../../agent/src/connect.mjs';
import { buildProposal, endpointUrl, MAX_ANSWER } from '../src/contrib.mjs';
import { keccak256 } from '../src/keccak.mjs';
import { createState, runTool, serve, toolDefs } from '../src/mcp.mjs';
import { checkProvider, listEndpoints, registry } from '../src/providers.mjs';
import * as rules from '../src/rules.mjs';
import { memoryVault } from '../src/vault.mjs';
import { fakeIntake, fakeProvider, INTAKE, KEY, PROVIDER_ORIGIN, routedFetch, SPEC } from './fakes.mjs';

let provider, intake, dir;
const dirs = [];
before(async () => {
	provider = await fakeProvider();
	intake = await fakeIntake();
});
after(() => {
	provider.close();
	intake.close();
	for (const d of dirs) rmSync(d, { recursive: true, force: true });
});

/** A fresh state: a temp config folder, a memory vault holding KEY for "prices" (header) and "qprices" (query). */
async function setup() {
	dir = mkdtempSync(join(tmpdir(), 'onyx-contrib-test-'));
	dirs.push(dir);
	const reg = registry(dir);
	reg.add({ name: 'prices', baseUrl: PROVIDER_ORIGIN, auth: { in: 'header', name: 'x-api-key' }, openapi: `${PROVIDER_ORIGIN}/openapi.json`, docsUrl: 'https://www.example.com/api-terms' }, { addedBy: 'you' });
	reg.add({ name: 'qprices', baseUrl: `${PROVIDER_ORIGIN}/q`, auth: { in: 'query', name: 'api_key' }, docsUrl: 'https://www.example.com/api-terms' }, { addedBy: 'you' });
	const vault = memoryVault();
	await vault.set('prices', PROVIDER_ORIGIN, KEY);
	await vault.set('qprices', PROVIDER_ORIGIN, KEY);
	const logs = [];
	const fetch = routedFetch(provider.url);
	const state = createState({ env: { ONYX_API_URL: intake.url }, fetch, vault, dir, log: (l) => logs.push(l) });
	return { state, logs, fetch, vault };
}
const call = async (state, name, args) => {
	const r = await runTool(state, name, args);
	return { r, body: JSON.parse(r.content[0].text), text: r.content[0].text };
};
const ARGS = {
	provider: 'prices',
	path: '/simple/price',
	params: { ids: 'bitcoin', vs_currencies: 'usd' },
	pointer: '/bitcoin/usd',
	title: 'Bitcoin price in US dollars (example API)',
	description: 'The bitcoin price in US dollars as the example API serves it, updated every minute.',
	rights: 'not_sure',
	namesPerson: false,
	changes: 'live',
	unit: 'USD',
};
const intakeText = () => JSON.stringify(intake.seen).toLowerCase();
const noKeyIn = (s, what) => {
	for (const f of rules.keyForms(KEY)) assert.ok(!String(s).toLowerCase().includes(f.toLowerCase()), `the key reached ${what}`);
};

test('keccak256 matches known vectors (empty, abc, the 136-byte block edge, three blocks)', () => {
	assert.equal(keccak256(''), '0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470');
	assert.equal(keccak256('abc'), '0x4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45');
	assert.equal(keccak256('The quick brown fox jumps over the lazy dog'), '0x4d741b6f1eb29cb2a9b9911c82f56fa8d73b04959d3d9d222895df6c0b28aa15');
	const a = 'a'.repeat(136);
	assert.equal(keccak256(a), '0xa6c4d403279fe3e0af03729caada8374b5ca54d8065329a3ebcaeb4b60aa386e');
	assert.equal(keccak256(a.slice(1)), '0x34367dc248bbd832f4e3e69dfaac2f92638bd0bbd18f2912ba4ef454919cf446');
	assert.equal(keccak256(a + a + a), '0xf763fe56a104e5c3ec0e4f311124923905d7f8e1dd03bdc91f2e6845ea523677');
});

test('caps: the copied intake rules equal apps/web/src/lib/intake.ts (never raised)', () => {
	const src = readFileSync(new URL('../../web/src/lib/intake.ts', import.meta.url), 'utf8');
	const grab = (name) => {
		const m = new RegExp(`export const ${name}(?::[^=]+)? = ([^;]+);`).exec(src);
		assert.ok(m, `${name} in intake.ts`);
		return Function(`"use strict"; return (${m[1]});`)();
	};
	assert.deepEqual(rules.CAPS, grab('CAPS'));
	assert.equal(rules.KEY_PLACEHOLDER, grab('KEY_PLACEHOLDER'));
	assert.equal(rules.STUB_PROVER, grab('STUB_PROVER'));
	assert.deepEqual(rules.SECRET_PARAMS, grab('SECRET_PARAMS'));
	assert.deepEqual(rules.SECRET_PARTS, grab('SECRET_PARTS'));
	if (INTAKE) for (const u of ['https://a.example.com/x?api_key=1', 'https://a.example.com/x?ids=1&TOKEN=2', 'https://a.example.com/x?ids=1&api_key='])
		assert.equal(rules.hasSecretParam(u), INTAKE.hasSecretParam(u), u);
});

test('tools: no tool takes a key; every input schema is closed', () => {
	for (const t of toolDefs()) {
		assert.equal(t.inputSchema.additionalProperties, false, t.name);
		const names = JSON.stringify(t.inputSchema.properties).match(/"([A-Za-z_]+)":\{/g) ?? [];
		for (const n of names) assert.ok(!/key|secret|token|passw|credential/i.test(n), `${t.name} takes ${n}`);
	}
	assert.throws(() => checkProvider({ name: 'x', baseUrl: 'https://api.example.com', auth: { in: 'header', name: 'x-api-key' }, apiKey: KEY }), /unknown field apiKey.*key set/);
});

test('providers: https with a host name only; the assistant cannot replace an existing provider', async () => {
	const { state } = await setup();
	for (const baseUrl of ['http://api.example.com', 'https://127.0.0.1', 'https://user:pw@api.example.com', 'https://api.example.com/?a=1'])
		assert.throws(() => checkProvider({ name: 'x', baseUrl, auth: { in: 'header', name: 'x-api-key' } }), /baseUrl/, baseUrl);
	assert.throws(() => checkProvider({ name: 'x', baseUrl: 'https://api.example.com', auth: { in: 'header', name: 'Host' } }), /auth.name/);
	assert.throws(() => checkProvider({ name: 'x', baseUrl: 'https://api.example.com', auth: { in: 'header', name: 'x' }, openapi: '/etc/passwd' }), /openapi: an https URL/);
	const { body } = await call(state, 'contrib_add_provider', { name: 'prices', baseUrl: 'https://evil.example.net', auth: { in: 'header', name: 'x-api-key' } });
	assert.equal(body.ok, false);
	assert.match(body.error, /exists already/);
	assert.equal(state.registry.get('prices').baseUrl, PROVIDER_ORIGIN);
	const added = await call(state, 'contrib_add_provider', { name: 'newapi', baseUrl: 'https://api.example.org/v2', auth: { in: 'header', name: 'Authorization', prefix: 'Bearer ' } });
	assert.equal(added.body.ok, true);
	assert.equal(added.body.provider.keyStored, false);
	assert.match(added.body.next, /onyx-contrib key set newapi/);
	const listed = await call(state, 'contrib_providers', {});
	assert.deepEqual(listed.body.providers.map((p) => [p.name, p.keyStored]), [['prices', true], ['qprices', true], ['newapi', false]]);
	noKeyIn(listed.text, 'the provider list');
	noKeyIn(readFileSync(join(dir, 'providers.json'), 'utf8'), 'providers.json');
});

test('a key is bound to its origin: a provider moved to another host finds no key', async () => {
	const { state, vault } = await setup();
	state.registry.add({ name: 'prices', baseUrl: 'https://evil.example.net', auth: { in: 'header', name: 'x-api-key' } }, { addedBy: 'you', replace: true });
	const { body } = await call(state, 'contrib_preview', { provider: 'prices', path: '/simple/price', params: { ids: 'bitcoin' } });
	assert.equal(body.ok, false);
	assert.match(body.error, /No key is stored for prices \(for https:\/\/evil\.example\.net\)/);
	assert.equal(await vault.get('prices', 'https://evil.example.net'), null);
});

test('endpoints: GET endpoints from the OpenAPI file, $ref resolved, the key parameter left out', async () => {
	const { state } = await setup();
	const { body } = await call(state, 'contrib_endpoints', { provider: 'prices' });
	assert.equal(body.ok, true);
	assert.deepEqual(body.endpoints.map((e) => e.path), ['/account', '/coins/{id}', '/simple/price']);
	const price = body.endpoints.find((e) => e.path === '/simple/price');
	assert.deepEqual(price.params, [{ name: 'ids', in: 'query', required: true, type: 'string', description: 'Coin ids, comma separated.', example: 'bitcoin' }]);
	assert.match(price.keyParam, /left out/);
	const coin = body.endpoints.find((e) => e.path === '/coins/{id}');
	assert.deepEqual(coin.params.map((p) => p.name), ['id']);
	const p = state.registry.get('prices');
	assert.deepEqual(listEndpoints(p, SPEC, { search: 'coin' }).endpoints.map((e) => e.path), ['/coins/{id}', '/simple/price']);
	assert.equal(listEndpoints(p, SPEC, { offset: 2 }).endpoints.length, 1);
	assert.throws(() => listEndpoints(p, { openapi: '3' }), /no paths/);
	// The OpenAPI file was fetched without the key.
	const specReq = provider.seen.find((s) => s.url === '/openapi.json');
	noKeyIn(JSON.stringify(specReq), 'the OpenAPI request');
});

test('endpoint paths stay on the provider and never take a key parameter', async () => {
	const { state } = await setup();
	const p = state.registry.get('prices');
	assert.equal(endpointUrl(p, '/coins/{id}', { id: 'bit coin', vs: 'usd' }), 'https://api.example.com/coins/bit%20coin?vs=usd');
	for (const [path, params, re] of [
		['/../admin', {}, /path/],
		['//evil.example.net/x', {}, /path/],
		['/coins/{id}', { id: '..' }, /params.id/],
		['/coins/{id}', {}, /needs id/],
		['/x', { api_key: 'abc' }, /key parameter/],
		['/x', { access_token: 'abc' }, /key parameter/],
		['/x', { 'x-api-key': 'abc' }, /key parameter/],
	])
		assert.throws(() => endpointUrl(p, path, params), re, path);
});

test('preview: the value is read here, with the key in the header to the provider only, and removed from the answer', async () => {
	const { state, logs, fetch } = await setup();
	const { body, text } = await call(state, 'contrib_preview', { provider: 'prices', path: '/simple/price', params: { ids: 'bitcoin' }, pointer: '/bitcoin/usd' });
	assert.equal(body.ok, true);
	assert.equal(body.value, 67000);
	assert.equal(body.endpoint, 'https://api.example.com/simple/price?ids=bitcoin');
	assert.ok(body.pointers.includes('/bitcoin/usd'));
	const echo = await call(state, 'contrib_preview', { provider: 'prices', path: '/account' });
	assert.equal(echo.body.ok, true);
	assert.equal(echo.body.answer.api_key_used, '[key removed]');
	assert.equal(echo.body.keyRemovedFromAnswer, true);
	const err = await call(state, 'contrib_preview', { provider: 'prices', path: '/error' });
	assert.equal(err.body.ok, false);
	assert.match(err.body.error, /answered 500.*\[key removed\]/);
	for (const t of [text, echo.text, err.text, logs.join('\n')]) noKeyIn(t, 'a tool result or log');
	// The provider got the key, in its header; nothing else did.
	assert.ok(provider.seen.some((s) => s.headers['x-api-key'] === KEY));
	for (const c of fetch.calls) if (!c.url.startsWith(PROVIDER_ORIGIN)) noKeyIn(JSON.stringify(c), 'a request off the provider');
});

test('preview: no redirect is followed, so the key never reaches another host', async () => {
	const { state, fetch } = await setup();
	const { body } = await call(state, 'contrib_preview', { provider: 'prices', path: '/moved' });
	assert.equal(body.ok, false);
	assert.match(body.error, /redirect \(302\).*only to https:\/\/api\.example\.com/);
	assert.ok(!fetch.calls.some((c) => c.url.includes('elsewhere')));
});

test('contribute: check first (records nothing), then confirm sends exactly the checked body; the key never reaches Onyx', async () => {
	const { state, logs } = await setup();
	const before = intake.seen.length;
	const checked = await call(state, 'contrib_contribute', ARGS);
	assert.equal(checked.body.ok, true, checked.text);
	assert.equal(checked.body.recorded, false);
	assert.match(checked.body.checkId, /^chk_[0-9a-f]{16}$/);
	const sentSoFar = intake.seen.slice(before);
	assert.deepEqual(sentSoFar.map((s) => `${s.method} ${s.url}`), ['POST /v1/submit/check']);
	const w = checked.body.willSend;
	assert.equal(w.answers.what, 'keyed_api');
	assert.equal(w.endpointUrl, 'https://api.example.com/simple/price?ids=bitcoin&vs_currencies=usd');
	assert.equal(w.jsonPointer, '/bitcoin/usd');
	assert.match(w.description, /\/bitcoin\/usd = 67000$/);
	assert.match(w.description, /x-api-key header/);
	assert.equal(w.keyProof.prover, 'stub-s2');
	assert.equal(w.keyProof.real, false);
	assert.match(w.keyProof.responseKeccak256, /^0x[0-9a-f]{64}$/);
	assert.equal('dataPayload' in w, false);
	if (INTAKE) assert.equal(checked.body.check.route.status, 'held_keyed_source_review');
	assert.deepEqual(JSON.parse(sentSoFar[0].body), w);

	const sent = await call(state, 'contrib_contribute', { checkId: checked.body.checkId, confirm: true });
	assert.equal(sent.body.ok, true, sent.text);
	assert.match(sent.body.receipt.id, /^sub_[0-9a-f]{32}$/);
	const submit = intake.seen.at(-1);
	assert.equal(`${submit.method} ${submit.url}`, 'POST /v1/submit');
	assert.deepEqual(JSON.parse(submit.body), w);
	const again = await call(state, 'contrib_contribute', { checkId: checked.body.checkId, confirm: true });
	assert.match(again.body.error, /sent already/);
	const st = await call(state, 'contrib_status', { id: sent.body.receipt.id });
	assert.equal(st.body.ok, true);
	noKeyIn(intakeText(), 'the Onyx intake');
	for (const t of [checked.text, sent.text, st.text, logs.join('\n')]) noKeyIn(t, 'a tool result or log');
});

test('contribute: a query-key provider sends a template with {api_key} where the key goes', async () => {
	const { state } = await setup();
	const { body, text } = await call(state, 'contrib_contribute', { ...ARGS, provider: 'qprices', params: { ids: 'bitcoin' } });
	assert.equal(body.ok, true, text);
	assert.equal(body.willSend.endpointTemplate, 'https://api.example.com/q/simple/price?ids=bitcoin&api_key={api_key}');
	assert.equal('endpointUrl' in body.willSend, false);
	assert.ok(provider.seen.some((s) => s.url === `/q/simple/price?ids=bitcoin&api_key=${KEY}`));
	noKeyIn(intakeText(), 'the Onyx intake');
	noKeyIn(text, 'the tool result');
});

test('contribute: confirm needs a checkId from a check; unknown or missing ids are refused', async () => {
	const { state } = await setup();
	const before = intake.seen.length;
	for (const a of [{ ...ARGS, confirm: true }, { checkId: 'chk_0000000000000000', confirm: true }, { ...ARGS, checkId: 'chk_0000000000000000' }]) {
		const { body } = await call(state, 'contrib_contribute', a);
		assert.equal(body.ok, false);
	}
	assert.equal(intake.seen.length, before, 'nothing reached Onyx');
});

test('contribute: refused before any request when the key appears in the outgoing body', async () => {
	const { state, logs } = await setup();
	const before = intake.seen.length;
	// The provider echoes the key, and the pointer picks it.
	const echoed = await call(state, 'contrib_contribute', { ...ARGS, path: '/account', params: {}, pointer: '/api_key_used' });
	assert.equal(echoed.body.ok, false);
	assert.equal(echoed.body.code, 'key_in_submission');
	// The user's own text holds the key, in another letter case.
	const typed = await call(state, 'contrib_contribute', { ...ARGS, description: `My key is ${KEY.toLowerCase()} for this.` });
	assert.equal(typed.body.code, 'key_in_submission');
	// The key URL-encoded in a parameter value.
	const enc = await call(state, 'contrib_contribute', { ...ARGS, params: { ids: 'bitcoin', vs: encodeURIComponent(KEY) } });
	assert.equal(enc.body.code, 'key_in_submission');
	assert.equal(intake.seen.length, before, 'nothing reached Onyx');
	for (const t of [echoed.text, typed.text, enc.text, logs.join('\n')]) noKeyIn(t, 'a tool result or log');
	assert.equal(rules.keyGuard({ a: 'short' }, ['tooshort']).code, 'key_too_short');
	assert.equal(rules.keyGuard({ a: Buffer.from(KEY).toString('base64') }, [KEY]).code, 'key_in_submission');
	assert.equal(rules.keyGuard({ a: 'clean' }, [KEY]), null);
});

test('contribute: at most MAX_PROPOSALS per session', async () => {
	const { state } = await setup();
	const ids = [];
	for (let i = 0; i <= MAX_PROPOSALS; i++) ids.push((await call(state, 'contrib_contribute', ARGS)).body.checkId);
	for (let i = 0; i < MAX_PROPOSALS; i++) assert.equal((await call(state, 'contrib_contribute', { checkId: ids[i], confirm: true })).body.ok, true);
	const over = await call(state, 'contrib_contribute', { checkId: ids[MAX_PROPOSALS], confirm: true });
	assert.equal(over.body.ok, false);
	assert.match(over.body.error, new RegExp(`already sent ${MAX_PROPOSALS}`));
});

test('caps: the intake byte caps hold before anything is sent; a long sample is cut; a huge answer is refused', async () => {
	const { state } = await setup();
	const before = intake.seen.length;
	const long = await call(state, 'contrib_contribute', { ...ARGS, description: 'd'.repeat(rules.CAPS.description) });
	assert.match(long.body.error, /description: at most/);
	const title = await call(state, 'contrib_contribute', { ...ARGS, title: 't'.repeat(rules.CAPS.title + 1) });
	assert.match(title.body.error, /title: at most 120 bytes/);
	const unit = await call(state, 'contrib_contribute', { ...ARGS, unit: 'u'.repeat(rules.CAPS.unit + 1) });
	assert.match(unit.body.error, /unit: at most 32 bytes/);
	const big = await call(state, 'contrib_preview', { provider: 'prices', path: '/big' });
	assert.match(big.body.error, new RegExp(`larger than ${MAX_ANSWER} bytes`));
	const longUrl = await call(state, 'contrib_contribute', { ...ARGS, provider: 'qprices', params: { ids: 'b'.repeat(190), a: 'c'.repeat(190), b: 'e'.repeat(190) } });
	assert.equal(longUrl.body.ok, false);
	assert.equal(intake.seen.length, before, 'nothing reached Onyx');
	const cut = await call(state, 'contrib_contribute', { ...ARGS, path: '/long', params: {}, pointer: '/text' });
	assert.equal(cut.body.ok, true, cut.text);
	assert.ok(Buffer.byteLength(cut.body.willSend.description) <= rules.CAPS.description);
	assert.match(cut.body.willSend.description, /x…$/);
});

test('buildProposal refuses an endpoint whose URL is not one the intake takes', async () => {
	const p = { name: 'local', baseUrl: 'https://api.example.com', auth: { in: 'header', name: 'x-api-key' } };
	const callResult = { publicUrl: 'https://api.example.com/x?token=abc', raw: Buffer.from('{"v":1}'), json: { v: 1 } };
	assert.throws(() => buildProposal(p, callResult, { ...ARGS, pointer: '/v' }), /secret parameter/);
});

test('stdio: a whole MCP session carries no key in any message or log line', async () => {
	const { vault } = await setup();
	const input = new PassThrough();
	const output = new PassThrough();
	let transcript = '';
	output.on('data', (c) => (transcript += c));
	const logs = [];
	serve({ input, output, env: { ONYX_API_URL: intake.url }, fetch: routedFetch(provider.url), vault, dir, log: (l) => logs.push(l) });
	const msgs = [
		{ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-06-18' } },
		{ jsonrpc: '2.0', id: 2, method: 'tools/list' },
		{ jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'contrib_preview', arguments: { provider: 'prices', path: '/account' } } },
		{ jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'contrib_contribute', arguments: ARGS } },
	];
	for (const m of msgs) input.write(JSON.stringify(m) + '\n');
	const deadline = Date.now() + 30_000;
	while (transcript.trim().split('\n').length < msgs.length && Date.now() < deadline) await new Promise((r) => setTimeout(r, 20));
	const replies = transcript.trim().split('\n').map((l) => JSON.parse(l));
	assert.equal(replies.length, msgs.length);
	const byId = Object.fromEntries(replies.map((r) => [r.id, r]));
	assert.equal(byId[1].result.serverInfo.name, 'onyx-contrib');
	assert.deepEqual(byId[2].result.tools.map((t) => t.name), ['contrib_providers', 'contrib_add_provider', 'contrib_endpoints', 'contrib_preview', 'contrib_contribute', 'contrib_status', 'contrib_keyed_join', 'contrib_keyed_observe', 'contrib_keyed_status']);
	assert.equal(JSON.parse(byId[4].result.content[0].text).ok, true);
	noKeyIn(transcript, 'the MCP transcript');
	noKeyIn(logs.join('\n'), 'the log');
	assert.ok(logs.length >= 2);
	input.end();
});

test('the CLI stores a key from stdin into the vault and prints no key', async () => {
	const { main } = await import('../src/cli.mjs');
	const d = mkdtempSync(join(tmpdir(), 'onyx-contrib-cli-'));
	try {
		const out = [];
		const env = { ONYX_CONTRIB_HOME: d, ONYX_CONTRIB_VAULT: 'file', ONYX_CONTRIB_VAULT_PASSPHRASE: 'FAKE-passphrase-for-tests' };
		const o = { env, out: (s) => out.push(s), err: (s) => out.push(s) };
		assert.equal(await main(['provider', 'add', 'prices', '--base-url', 'https://api.example.com', '--header', 'x-api-key', '--openapi', 'https://api.example.com/openapi.json'], o), 0);
		const stdin = new PassThrough();
		stdin.end(KEY + '\n');
		assert.equal(await main(['key', 'set', 'prices'], { ...o, stdin }), 0);
		const short = new PassThrough();
		short.end('abc\n');
		assert.equal(await main(['key', 'set', 'prices'], { ...o, stdin: short }), 1);
		assert.equal(await main(['provider', 'list'], o), 0);
		assert.match(out.join('\n'), /prices {2}https:\/\/api\.example\.com {2}key stored/);
		assert.match(out.join('\n'), /will be sent only to https:\/\/api\.example\.com/);
		assert.equal(await main(['preview', 'prices', '/account'], { ...o, fetch: routedFetch(provider.url) }), 0);
		assert.match(out.join('\n'), /\[key removed\]/);
		noKeyIn(out.join('\n'), 'the CLI output');
		noKeyIn(readFileSync(join(d, 'vault.json'), 'utf8'), 'the vault file');
		noKeyIn(readFileSync(join(d, 'providers.json'), 'utf8'), 'providers.json');
		assert.equal(await main(['provider', 'remove', 'prices'], o), 0);
		assert.deepEqual(JSON.parse(readFileSync(join(d, 'vault.json'), 'utf8')).items, {});
	} finally {
		rmSync(d, { recursive: true, force: true });
	}
});

test('fakes sanity: the test key is an obvious fake', () => {
	assert.match(KEY, /^FAKE-/);
});

// Review CL2-R1 (fresh adversarial reviewer, 2026-10-29): each finding's exploit as a test.
test('review: an error body that echoes the key is redacted before it is cut (no half key)', async () => {
	const { state } = await setup();
	for (const n of [170, 180, 190, 200, 210]) {
		const { body, text } = await call(state, 'contrib_preview', { provider: 'qprices', path: '/nothing', params: { q: 'A'.repeat(n - 100), r: 'B'.repeat(100) } });
		assert.equal(body.ok, false);
		assert.match(body.error, /answered 404/);
		noKeyIn(text, 'the tool result');
		for (let i = 0; i + rules.KEY_WINDOW <= KEY.length; i++) assert.ok(!text.includes(KEY.slice(i, i + rules.KEY_WINDOW)), `a piece of the key at ${i} (pad ${n})`);
	}
});

test('review: every encoding of a key is caught, pieces of 10 characters included', () => {
	const odd = "FAKE~key!(odd)'0123456789";
	const forms = [
		new URLSearchParams({ k: odd }).toString().slice(2),
		new URLSearchParams({ k: odd }).toString().slice(2).toLowerCase(),
		encodeURIComponent(odd),
		Buffer.from('u:' + odd).toString('base64'),
		Buffer.from('xy:' + odd).toString('base64url'),
		Buffer.from(odd).toString('hex'),
	];
	for (const f of forms) {
		assert.equal(rules.keyGuard({ description: `echo ${f} end` }, [odd])?.code, 'key_in_submission', f);
		assert.ok(!rules.redact(`echo ${f} end`, [odd]).includes(f), f);
	}
	const slashed = 'FAKE/key/0123456789abc';
	assert.equal(rules.keyGuard({ d: JSON.stringify(slashed).slice(1, -1).replace(/\//g, '\\/') }, [slashed])?.code, 'key_in_submission');
	// A sample cut inside the key still holds a piece of it.
	assert.equal(rules.keyGuard({ description: `…zzzz${KEY.slice(0, 12)}…` }, [KEY])?.code, 'key_in_submission');
	// Percent-decoded strings are searched too.
	assert.equal(rules.keyGuard({ v: `a ${encodeURIComponent(odd).replace(/%/g, '%25')}` }, [odd])?.code, 'key_in_submission');
	const tree = rules.redactTree({ [KEY]: { a: `${'w'.repeat(285)}${KEY}` }, enc: encodeURIComponent(KEY) }, [KEY]);
	noKeyIn(JSON.stringify(tree), 'a redacted tree');
	assert.ok(rules.leafPointers(tree).every((p) => !p.toLowerCase().includes(KEY.slice(0, 10).toLowerCase())));
});

test("review: a guard checks every key read in the session, not only the current provider's", async () => {
	const { state, vault } = await setup();
	const OTHER = 'FAKE-second-key-abcdefghij0123';
	state.registry.add({ name: 'other', baseUrl: 'https://api.example.org', auth: { in: 'header', name: 'x-api-key' } }, { addedBy: 'you' });
	await vault.set('other', 'https://api.example.org', OTHER);
	await state.keyFor('other');
	const { body } = await call(state, 'contrib_contribute', { ...ARGS, description: `Notes ${OTHER}` });
	assert.equal(body.code, 'key_in_submission');
});

test('review: a path that resolves above the base URL is refused (%2e%2e)', async () => {
	const { state } = await setup();
	const q = state.registry.get('qprices');
	assert.throws(() => endpointUrl(q, '/%2e%2e/%2e%2e/admin/keys', {}), /under its base URL/);
	assert.throws(() => endpointUrl(q, '/%2E%2E/admin', {}), /under its base URL/);
	assert.equal(endpointUrl(q, '/simple/price', { ids: 'x' }), 'https://api.example.com/q/simple/price?ids=x');
});

test('review: an OpenAPI redirect is checked hop by hop (no IP literal); an unknown name is no provider', async () => {
	const { state } = await setup();
	state.registry.add({ name: 'moved', baseUrl: PROVIDER_ORIGIN, auth: { in: 'header', name: 'x-api-key' }, openapi: `${PROVIDER_ORIGIN}/openapi-moved` }, { addedBy: 'assistant' });
	const { body } = await call(state, 'contrib_endpoints', { provider: 'moved' });
	assert.equal(body.ok, false);
	assert.match(body.error, /redirects to a refused URL: a public host name, not an IP address/);
	assert.equal(state.registry.get('constructor'), null);
	assert.equal(state.registry.get('__proto__'), null);
});

test('review: a piped key set for a provider the assistant added needs --yes', async () => {
	const { main } = await import('../src/cli.mjs');
	const d = mkdtempSync(join(tmpdir(), 'onyx-contrib-cli-'));
	dirs.push(d);
	const out = [];
	const env = { ONYX_CONTRIB_HOME: d, ONYX_CONTRIB_VAULT: 'file', ONYX_CONTRIB_VAULT_PASSPHRASE: 'FAKE-passphrase-for-tests' };
	registry(d).add({ name: 'lookalike', baseUrl: 'https://api.examp1e.com', auth: { in: 'header', name: 'x-api-key' } }, { addedBy: 'assistant' });
	const o = { env, out: (s) => out.push(s), err: (s) => out.push(s) };
	const s1 = new PassThrough();
	s1.end(KEY);
	assert.equal(await main(['key', 'set', 'lookalike'], { ...o, stdin: s1 }), 1);
	assert.match(out.join('\n'), /added by an AI assistant: check its host, then run again with --yes/);
	const s2 = new PassThrough();
	s2.end(KEY);
	assert.equal(await main(['key', 'set', 'lookalike', '--yes'], { ...o, stdin: s2 }), 0);
	noKeyIn(out.join('\n'), 'the CLI output');
});
