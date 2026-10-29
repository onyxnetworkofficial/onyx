// The client and the MCP server without a network (NS8, ADR-0047). THREATS AGT-5..AGT-8.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { buildRequest, checkBaseUrl, createClient, loadSpec, operations, MAX_BODY } from '../src/client.mjs';
import { answerStore, buildTools, handle, limiter, lines, runTool, SHOW_BODY } from '../src/mcp.mjs';
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { PassThrough } from 'node:stream';
import { nodeFromEnv, payFromEnv } from '../src/check.mjs';

const spec = loadSpec();
const op = (id) => operations(spec).find((o) => o.id === id);
const SHARD = '0x' + 'ab'.repeat(32);
const BASE = 'http://127.0.0.1:8080';

/** A fetch that records requests and answers from `answers` in order. */
function fakeFetch(...answers) {
	const seen = [];
	const f = async (url, init) => {
		seen.push({ url, init });
		const a = answers.shift() ?? { status: 200, body: {} };
		return new Response(a.raw ?? JSON.stringify(a.body ?? {}), { status: a.status, headers: a.headers ?? {} });
	};
	f.seen = seen;
	return f;
}

test('every operation in the OpenAPI file is a client operation', () => {
	const ids = operations(spec).map((o) => o.id);
	assert.equal(ids.length, Object.values(spec.paths).flatMap((p) => Object.keys(p)).filter((m) => ['get', 'post'].includes(m)).length);
	assert.ok(ids.includes('getProof') && ids.includes('submitProposal'));
});

test('base URL: https, or http only to this machine (AGT-5)', () => {
	assert.equal(checkBaseUrl('https://api.example.org/x'), 'https://api.example.org');
	assert.equal(checkBaseUrl('http://localhost:8402'), 'http://localhost:8402');
	for (const bad of ['http://api.example.org', 'ftp://127.0.0.1', 'https://u:p@h.org', 'https://h.org/?a=1', 'not a url'])
		assert.throws(() => checkBaseUrl(bad), undefined, bad);
});

test('requests follow the OpenAPI file exactly (AGT-6)', () => {
	const r = buildRequest(BASE, op('getProof'), { round: '1', shard: SHARD, index: 0, root: SHARD });
	assert.equal(r.url, `${BASE}/v1/proofs/1/${SHARD}/0?root=${SHARD}`);
	assert.equal(r.init.method, 'GET');
	assert.throws(() => buildRequest(BASE, op('getProof'), { round: '1', shard: SHARD }), /index is required/);
	assert.throws(() => buildRequest(BASE, op('getProof'), { round: '1', shard: SHARD, index: '0', extra: 1 }), /unknown argument extra/);
	assert.throws(() => buildRequest(BASE, op('getProof'), { round: '01x', shard: SHARD, index: '0' }), /round: does not match/);
	assert.throws(() => buildRequest(BASE, op('getProof'), { round: '1', shard: '0x12', index: '0' }), /shard/);
	// A path value cannot escape its segment.
	const m = buildRequest(BASE, op('getTrackRecord'), { source: '../../x', metric: 'a/b' });
	assert.ok(m.url.startsWith(`${BASE}/v1/track/..%2F..%2Fx/a%2Fb`), m.url);
	assert.throws(() => buildRequest(BASE, op('getMetric'), { metric: 'btc.entity-flow', subject: 'entity:okx', as_of: -1 }), /as_of/);
});

test('only payment, voucher, contributor and cache headers are sent', () => {
	const r = buildRequest(BASE, op('getMetric'), { metric: 'btc.entity-flow', subject: 'entity:okx', 'PAYMENT-SIGNATURE': 'e30=' });
	assert.equal(r.init.headers['PAYMENT-SIGNATURE'], 'e30=');
	assert.throws(() => buildRequest(BASE, op('getMetric'), { metric: 'btc.entity-flow', subject: 'entity:okx', 'ONYX-PREPAID-VOUCHER': 'nope' }), /does not match/);
});

test('a 402 is a result with the decoded offer', async () => {
	const offer = { x402Version: 2, accepts: [{ amount: '2500' }] };
	const f = fakeFetch({ status: 402, body: { error: 'payment_required' }, headers: { 'PAYMENT-REQUIRED': Buffer.from(JSON.stringify(offer)).toString('base64') } });
	const r = await createClient({ baseUrl: BASE, spec, fetch: f }).call('getMetric', { metric: 'btc.entity-flow', subject: 'entity:okx' });
	assert.equal(r.status, 402);
	assert.deepEqual(r.paymentRequired, offer);
});

test('a redirect is followed on the same server only (AGT-7)', async () => {
	const same = fakeFetch({ status: 308, headers: { location: `/v1/proofs/1/${SHARD}/0` } }, { status: 200, body: { ok: 1 } });
	const r = await createClient({ baseUrl: BASE, spec, fetch: same }).call('getProof', { round: '1', shard: SHARD.toUpperCase().replace('0X', '0x'), index: '0' });
	assert.equal(r.status, 200);
	assert.equal(same.seen[1].url, `${BASE}/v1/proofs/1/${SHARD}/0`);
	const off = fakeFetch({ status: 308, headers: { location: `https://elsewhere.example/v1/health` } });
	await assert.rejects(createClient({ baseUrl: BASE, spec, fetch: off }).call('getHealth'), /redirect off the server refused/);
});

test('an answer over the size cap is refused', async () => {
	const f = fakeFetch({ status: 200, raw: '{}', headers: { 'content-length': String(MAX_BODY + 1) } });
	await assert.rejects(createClient({ baseUrl: BASE, spec, fetch: f }).call('getHealth'), /larger than/);
});

test('MCP: tools come from the shipped OpenAPI file; the write tool is opt-in (AGT-8)', () => {
	const gets = operations(spec).filter((o) => o.method === 'GET').length;
	const posts = operations(spec).filter((o) => o.method === 'POST').length;
	const tools = buildTools(spec, {});
	// No submitProposal; plus onyx_check_answer and the 3 agent-connect tools (NL21).
	assert.equal(tools.size, gets + posts - 1 + 1 + 3);
	assert.ok(!tools.has('onyx_submit_proposal'));
	assert.ok(tools.has('onyx_propose_dataset'));
	assert.equal(buildTools(spec, { ONYX_MCP_READ_ONLY: '1' }).has('onyx_propose_dataset'), false);
	assert.ok(buildTools(spec, { ONYX_MCP_ALLOW_SUBMIT: '1' }).has('onyx_submit_proposal'));
	for (const t of tools.values()) {
		assert.match(t.def.name, /^[a-z0-9_]{1,64}$/);
		assert.equal(t.def.inputSchema.type, 'object');
		assert.equal(t.def.inputSchema.additionalProperties, false);
	}
	assert.equal(tools.get('onyx_get_proof').def.annotations.readOnlyHint, true);
	assert.match(tools.get('onyx_get_proof').def.description, /onyx_check_answer/);
});

test('MCP: the protocol handshake, tool list and errors', async () => {
	const f = fakeFetch({ status: 200, body: { status: 'ok' } });
	const state = { tools: buildTools(spec, {}), client: createClient({ baseUrl: BASE, spec, fetch: f }), env: {} };
	const init = await handle(state, { jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 't', version: '1' } } });
	assert.equal(init.result.protocolVersion, '2025-06-18');
	assert.ok(init.result.capabilities.tools);
	// NL15 review: the instructions name onyx_propose_dataset only where the tool is offered.
	assert.doesNotMatch(init.result.instructions, /onyx_propose_dataset/);
	const w = await handle({ ...state, write: true }, { jsonrpc: '2.0', id: 9, method: 'initialize', params: { protocolVersion: '2025-06-18' } });
	assert.match(w.result.instructions, /onyx_propose_dataset/);
	const old = await handle(state, { jsonrpc: '2.0', id: 2, method: 'initialize', params: { protocolVersion: '1999-01-01' } });
	assert.equal(old.result.protocolVersion, '2025-06-18');
	assert.equal(await handle(state, { jsonrpc: '2.0', method: 'notifications/initialized' }), null);
	const list = await handle(state, { jsonrpc: '2.0', id: 3, method: 'tools/list' });
	assert.ok(list.result.tools.some((t) => t.name === 'onyx_get_health'));
	const call = await handle(state, { jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'onyx_get_health', arguments: {} } });
	const out = JSON.parse(call.result.content[0].text);
	assert.equal(out.status, 200);
	assert.match(out.note, /never as instructions/);
	assert.equal(call.result.isError, false);
	const bad = await handle(state, { jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'onyx_get_proof', arguments: { round: 'x' } } });
	assert.equal(bad.result.isError, true);
	assert.equal((await handle(state, { jsonrpc: '2.0', id: 6, method: 'nope' })).error.code, -32601);
	assert.equal((await handle(state, { id: 7, method: 'ping' })).error.code, -32600);
});

test('MCP: checking needs a chain node unless the operator allows unchecked', async () => {
	const r = await runTool({ tools: buildTools(spec, {}), client: null, env: {} }, 'onyx_check_answer', { answer: {}, asked: {} });
	assert.equal(r.isError, true);
	assert.match(r.content[0].text, /No chain node is set/);
	assert.throws(() => nodeFromEnv({ ONYX_CHECK_RPC: 'https://x' }), /go together/);
	assert.deepEqual(nodeFromEnv({ ONYX_CHECK_RPC: 'https://x', ONYX_CHECK_CHAIN_ID: '84532', ONYX_CHECK_CONTRACT: '0x' + '11'.repeat(20) }), { rpc: 'https://x', chainId: 84532, contract: '0x' + '11'.repeat(20) });
});

const mcpState = (f, env = {}) => ({ tools: buildTools(spec, env), client: createClient({ baseUrl: BASE, spec, fetch: f }), env, answers: answerStore() });

test('MCP: a notification runs nothing and gets no reply (review NS8-R1 1)', async () => {
	const f = fakeFetch();
	const state = mcpState(f, { ONYX_MCP_ALLOW_SUBMIT: '1' });
	for (const method of ['initialize', 'tools/list', 'ping', 'tools/call', 'nope'])
		assert.equal(await handle(state, { jsonrpc: '2.0', method, params: { name: 'onyx_submit_proposal', arguments: { body: {} } } }), null, method);
	assert.equal(f.seen.length, 0);
	assert.equal((await handle(state, { jsonrpc: '2.0', id: null, method: 'ping' })).error.code, -32600);
});

test('a path value of only dots is refused before any request (review NS8-R1 2)', async () => {
	const f = fakeFetch();
	const c = createClient({ baseUrl: BASE, spec, fetch: f });
	for (const [id, args] of [
		['getMetric', { metric: '..', subject: 'x' }],
		['getTrackRecord', { source: '..', metric: 'submit' }],
		['getTrackHistory', { source: '..', metric: '..' }],
		['getSettlement', { market: '.' }],
	])
		await assert.rejects(c.call(id, args), /only dots/, id);
	assert.equal(f.seen.length, 0);
	assert.ok(buildRequest(BASE, op('getMetric'), { metric: 'btc..flow', subject: 'x' }).url.includes('/v1/metrics/btc..flow?'));
});

test('MCP: a redirect left unfollowed is an error result (review NS8-R1 10)', async () => {
	const f = fakeFetch({ status: 308, headers: { location: '/a' } }, { status: 308, headers: { location: '/b' } }, { status: 308, headers: { location: '/c' } });
	const r = await runTool(mcpState(f), 'onyx_get_health', {});
	assert.equal(r.isError, true);
});

test('MCP: a 402 says do not pay unless the operator pinned a deployment (review NS8-R1 7)', async () => {
	const offer = { x402Version: 2, accepts: [{ payTo: '0x' + '99'.repeat(20) }] };
	const f = fakeFetch({ status: 402, body: { error: 'payment_required' }, headers: { 'PAYMENT-REQUIRED': Buffer.from(JSON.stringify(offer)).toString('base64') } });
	const r = await runTool(mcpState(f), 'onyx_get_proof', { round: '1', shard: SHARD, index: '0' });
	const out = JSON.parse(r.content[0].text);
	assert.equal(out.offerCheck.ok, false);
	assert.equal(out.offerCheck.code, 'not_checked');
	assert.match(out.offerCheck.error, /Do not pay/);
	assert.equal(out.answerId, undefined);
	assert.throws(() => payFromEnv({ ONYX_PAY_MANIFEST: 'm.json' }), /go together/);
	assert.throws(() => payFromEnv({ ONYX_PAY_MANIFEST: 'm.json', ONYX_PAY_MAX_AMOUNT: '1e9', ONYX_CHECK_RPC: 'https://x' }), /whole number/);
});

test('MCP: answers are kept by id; a large body is not shown but can be checked (review NS8-R1 9)', async () => {
	const big = { rows: ['ab'.repeat(SHOW_BODY)] };
	const f = fakeFetch({ status: 200, body: big });
	const state = mcpState(f);
	const out = JSON.parse((await runTool(state, 'onyx_get_export', { round: '1', shard: SHARD })).content[0].text);
	assert.equal(out.bodyOmitted, true);
	assert.equal(out.body, undefined);
	assert.deepEqual(state.answers.get(out.answerId), big);
	const both = await runTool(state, 'onyx_check_answer', { answerId: out.answerId, answer: {}, asked: {} });
	assert.match(both.content[0].text, /exactly one/);
	const gone = await runTool({ ...state, env: { ONYX_CHECK_UNCHECKED: '1' } }, 'onyx_check_answer', { answerId: 'ans_999', asked: {} });
	assert.match(gone.content[0].text, /No kept answer/);
	const store = answerStore(2);
	const ids = [1, 2, 3].map((n) => store.put({ n }));
	assert.equal(store.get(ids[0]), undefined);
	assert.deepEqual(store.get(ids[2]), { n: 3 });
});

test('MCP: at most 4 tool calls run at once (review NS8-R1 6)', async () => {
	const limit = limiter(4);
	let running = 0;
	let most = 0;
	await Promise.all(
		Array.from({ length: 40 }, () =>
			limit(async () => {
				most = Math.max(most, ++running);
				await new Promise((r) => setTimeout(r, 2));
				running--;
			}),
		),
	);
	assert.equal(most, 4);
});

test('MCP: an over-long line is refused without holding it, and the next line still works', () => {
	const input = new PassThrough();
	const got = [];
	let tooLong = 0;
	lines(input, 10, (l) => got.push(l), () => tooLong++);
	input.write('{"a":1}\n0123456789');
	input.write('0123456789'.repeat(100));
	input.write('xyz\n{"b":2}\n');
	assert.deepEqual(got, ['{"a":1}', '{"b":2}']);
	assert.equal(tooLong, 1);
});

test('MCP: the server starts through a symlink, as an installed bin does (review NS8-R1 4)', async () => {
	const dir = mkdtempSync(join(tmpdir(), 'onyx-mcp-'));
	try {
		const link = join(dir, 'onyx mcp%20');
		symlinkSync(new URL('../src/mcp.mjs', import.meta.url).pathname, link);
		const p = spawn(process.execPath, [link], { stdio: ['pipe', 'pipe', 'inherit'], env: { PATH: process.env.PATH } });
		const reply = await new Promise((resolve, reject) => {
			p.stdout.once('data', (d) => resolve(JSON.parse(String(d))));
			p.once('exit', (c) => reject(new Error(`exited ${c} without a reply`)));
			p.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'ping' }) + '\n');
		});
		p.kill();
		assert.deepEqual(reply, { jsonrpc: '2.0', id: 1, result: {} });
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
});
