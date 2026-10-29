// The MCP server as an agent runs it, against the demo servers (scripts/agent-smoke.sh starts
// them): ask, check, refuse a changed answer, get a payment offer. Skipped without
// ONYX_AGENT_FREE_URL, ONYX_AGENT_PAID_URL and ONYX_CHECK_BIN.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';

const { ONYX_AGENT_FREE_URL: FREE, ONYX_AGENT_PAID_URL: PAID, ONYX_CHECK_BIN: BIN } = process.env;
const skip = !(FREE && PAID && BIN) && 'set ONYX_AGENT_FREE_URL, ONYX_AGENT_PAID_URL and ONYX_CHECK_BIN (scripts/agent-smoke.sh)';
const SHARD = '0xacbcbfa203b99d9cba691cc1958b3c6563f04f8eab94db5a0402597b1ca34244';

/** Start the MCP server as a child process; `rpc(method, params)` sends one request. */
function mcp(apiUrl) {
	const p = spawn(process.execPath, [new URL('../src/mcp.mjs', import.meta.url).pathname], {
		env: { PATH: process.env.PATH, ONYX_API_URL: apiUrl, ONYX_CHECK_BIN: BIN, ONYX_CHECK_UNCHECKED: '1' },
		stdio: ['pipe', 'pipe', 'inherit'],
	});
	const waiting = new Map();
	createInterface({ input: p.stdout }).on('line', (l) => {
		const m = JSON.parse(l);
		waiting.get(m.id)?.(m);
		waiting.delete(m.id);
	});
	let id = 0;
	const rpc = (method, params) =>
		new Promise((resolve) => {
			const n = ++id;
			waiting.set(n, resolve);
			p.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: n, method, params }) + '\n');
		});
	const tool = async (name, args) => {
		const r = await rpc('tools/call', { name, arguments: args });
		return { isError: r.result.isError, out: JSON.parse(r.result.content[0].text) };
	};
	return { rpc, tool, stop: () => p.kill() };
}

test('an agent asks for a row, checks it, and a changed row is refused', { skip }, async () => {
	const m = mcp(FREE);
	try {
		const init = await m.rpc('initialize', { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'smoke', version: '1' } });
		assert.equal(init.result.serverInfo.name, 'onyx');
		const tools = (await m.rpc('tools/list', {})).result.tools.map((t) => t.name);
		assert.ok(tools.includes('onyx_get_proof') && tools.includes('onyx_check_answer'));

		const health = await m.tool('onyx_get_health', {});
		assert.equal(health.out.status, 200);

		const proof = await m.tool('onyx_get_proof', { round: '1', shard: SHARD, index: '0' });
		assert.equal(proof.out.status, 200);
		const asked = { kind: 'proof', round: 1, shard: SHARD, index: 0 };
		// Without a chain node the rest passes, but it is not called verified (review NS8-R1 3).
		const ok = await m.tool('onyx_check_answer', { answerId: proof.out.answerId, asked });
		assert.equal(ok.out.outcome, 'unverified_on_chain');
		assert.equal(ok.isError, true);
		assert.equal(ok.out.ok, true);
		assert.equal(ok.out.chainChecked, false);
		assert.match(ok.out.warning, /NOT VERIFIED ON CHAIN/);
		const other = await m.tool('onyx_check_answer', { answerId: proof.out.answerId, asked: { ...asked, index: 1 } });
		assert.equal(other.out.code, 'not_requested');

		const row = proof.out.body.row;
		const changed = { ...proof.out.body, row: row.slice(0, -1) + (row.endsWith('0') ? '1' : '0') };
		const bad = await m.tool('onyx_check_answer', { answer: changed, asked });
		assert.equal(bad.isError, true);
		assert.equal(bad.out.code, 'proof_invalid');

		const settle = await m.tool('onyx_get_settlement', { market: 'okx-net-flow-921080-921082' });
		const s = await m.tool('onyx_check_answer', {
			answer: settle.out.body,
			asked: { kind: 'settlement', market: 'okx-net-flow-921080-921082', round: 1, shard: SHARD, subject: 'entity:okx' },
		});
		assert.equal(s.out.outcome, 'unverified_on_chain');
		assert.equal(s.out.ok, true);
	} finally {
		m.stop();
	}
});

test('a paid server answers with a payment offer first', { skip }, async () => {
	const m = mcp(PAID);
	try {
		const r = await m.tool('onyx_get_proof', { round: '1', shard: SHARD, index: '0' });
		assert.equal(r.out.status, 402);
		assert.equal(r.isError, false);
		assert.ok(r.out.paymentRequired?.accepts?.length > 0, JSON.stringify(r.out.paymentRequired));
		// The offer names this server's own address (the bound port) and a testnet (Base Sepolia).
		assert.ok(r.out.paymentRequired.resource.url.startsWith(`${PAID}/v1/proofs/1/`), r.out.paymentRequired.resource.url);
		assert.equal(r.out.paymentRequired.accepts[0].network, 'eip155:84532');
		// No deployment manifest is set here: the offer is not to be paid.
		assert.equal(r.out.offerCheck.code, 'not_checked');
	} finally {
		m.stop();
	}
});
