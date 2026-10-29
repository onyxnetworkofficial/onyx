#!/usr/bin/env node
// The steps of docs/agents.md, as an AI client runs them (NL21), against a node at ONYX_API_URL:
// start the remote MCP server, connect, ask "do you have X?", get "not yet", propose it (check,
// then confirm), follow the receipt, and connect over stdio too. scripts/agent-connect-test.sh
// runs it in a fresh container with only apps/agent (no repository, no Rust) and the mock node.
// ONYX_QUICKSTART_LIVE=1: read-only steps only (find and status), for the hosted node.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { createHttpServer } from '../src/http.mjs';

const live = process.env.ONYX_QUICKSTART_LIVE === '1';
const env = { ...process.env, ONYX_MCP_ALLOW_PROPOSE: live ? '0' : '1', ONYX_USER_AGENT: process.env.ONYX_USER_AGENT || 'onyx-dev' };
const say = (s) => console.log(`  ${s}`);

const { server } = createHttpServer({ env });
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const url = `http://127.0.0.1:${server.address().port}/mcp`;
let sid = null;
let n = 0;
async function rpc(method, params) {
	const headers = { 'content-type': 'application/json', accept: 'application/json, text/event-stream' };
	if (sid) Object.assign(headers, { 'mcp-session-id': sid, 'mcp-protocol-version': '2025-06-18' });
	const r = await fetch(url, { method: 'POST', headers, body: JSON.stringify({ jsonrpc: '2.0', id: ++n, method, params }) });
	sid ??= r.headers.get('mcp-session-id');
	const b = await r.json();
	assert.ok(!b.error, JSON.stringify(b.error));
	return b.result;
}
const tool = async (name, args) => JSON.parse((await rpc('tools/call', { name, arguments: args })).content[0].text);

try {
	console.log(`agent connect quickstart against ${env.ONYX_API_URL} (${live ? 'live, read-only' : 'mock node'})`);
	const init = await rpc('initialize', { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'quickstart', version: '1' } });
	assert.equal(init.serverInfo.name, 'onyx');
	say(`1. connected over HTTP (session ${sid.slice(0, 8)}…)`);
	const names = (await rpc('tools/list')).tools.map((t) => t.name);
	assert.ok(names.includes('onyx_find_dataset'));
	say(`2. ${names.length} tools, starting with onyx_find_dataset`);

	const have = await tool('onyx_find_dataset', { query: 'earthquakes per day' });
	assert.equal(have.found, true);
	assert.equal(have.matches[0].id, 'usgs.earthquakes.m5_count');
	say(`3. "earthquakes per day": found ${have.matches[0].id}, level ${have.matches[0].level}, data as of ${have.matches[0].dataAsOf}`);
	if (live) assert.notEqual(have.matches[0].dataAsOf, 'not given', 'the live freshness file was not read');

	const lack = await tool('onyx_find_dataset', { query: 'daily rainfall in Tokyo' });
	assert.equal(lack.found, false);
	say('4. "daily rainfall in Tokyo": not yet, propose it');

	if (live) {
		const st = await tool('onyx_proposal_status', { id: 'sub_' + '0'.repeat(32) });
		assert.equal(st.ok, false);
		say(`5. status of an unknown receipt: ${st.error}`);
	} else {
		const proposal = {
			title: 'Daily rainfall, Tokyo',
			description: 'Millimetres of rain per day at one station, from the national weather office.',
			sourceName: 'A national weather office',
			sourceUrl: 'https://example.org/rain',
			answers: { what: 'public_api', noLogin: true, rights: 'open_license', licenseId: 'CC-BY-4.0', namesPerson: false, changes: 'daily' },
		};
		const checked = await tool('onyx_propose_dataset', proposal);
		assert.equal(checked.recorded, false);
		assert.equal(checked.check.valid, true);
		say('5. proposal checked by the node, nothing recorded');
		const rec = await tool('onyx_propose_dataset', { ...proposal, confirm: true });
		assert.equal(rec.recorded, true);
		say(`6. confirmed: receipt ${rec.receipt.id} (${rec.receipt.status})`);
		const st = await tool('onyx_proposal_status', { id: rec.receipt.id });
		assert.equal(st.status.status, 'pending_review');
		say(`7. status: ${st.status.status}`);
		const keyed = await tool('onyx_propose_dataset', { ...proposal, answers: { ...proposal.answers, what: 'keyed_api' } });
		assert.equal(keyed.ok, false);
		say('8. a source that needs an API key is refused before any request');
	}

	// The same server over stdio, as a local client starts it.
	const p = spawn(process.execPath, [new URL('../src/mcp.mjs', import.meta.url).pathname], { env, stdio: ['pipe', 'pipe', 'inherit'] });
	const lines = createInterface({ input: p.stdout });
	const next = () => new Promise((r) => lines.once('line', (l) => r(JSON.parse(l))));
	p.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'q', version: '1' } } }) + '\n');
	assert.equal((await next()).result.serverInfo.name, 'onyx');
	p.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'onyx_find_dataset', arguments: { query: 'Denver temperature' } } }) + '\n');
	const r = JSON.parse((await next()).result.content[0].text);
	assert.equal(r.matches[0].id, 'noaa.ghcnd.usw00003017.tmax');
	p.kill();
	say('9. stdio: "Denver temperature" found noaa.ghcnd.usw00003017.tmax');
	console.log('AGENT CONNECT PASSED');
} finally {
	server.close();
}
