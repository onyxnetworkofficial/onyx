// e2e "a browser's POST through the dev proxy reaches the node as the page's own host"
// (THREATS PTR-R2-M1). The node refuses a browser's POST whose `Origin` is not the `Host` it
// receives (THREATS PTR-R1-M1). `pnpm dev` with ONYX_SUBMIT_PROXY puts Vite's proxy between the
// page and the node; its string shorthand rewrote `Host` to the node's own address, so every
// POST of the proposal page was refused with 403. This test runs a real page behind the real
// dev proxy against a real local node and requires the page's POSTs to be taken, and the same
// POST with the rewritten host (what the shorthand sent) to be refused, so the test would fail
// again if the proxy setting or the node's rule were lost.
//
// Run: start the local proposal server (no network, its own journal), then this file:
//   ONYX_SUBMIT_NO_FETCH=1 cargo run -p onyx-api --features http-client --example submit-dev -- \
//     "$(mktemp -d)/submissions.journal" 127.0.0.1:8788
//   node apps/web/tests/dev-proxy.mjs http://127.0.0.1:8788
// The same path is exercised by apps/web/tests/submission-live.mjs, which sends a proposal from the
// page through this proxy to an onyx-node: ONYX_SUBMIT_BROWSER=1 scripts/level3-book.sh local.
// Neither browser test is in check-all (both need a node and a dev server): run one after a
// change to the proxy setting or to the node's same-site rule.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { request } from 'node:http';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';

const [api] = process.argv.slice(2);
assert.match(api ?? '', /^http:\/\/127\.0\.0\.1:\d+$/, 'usage: node tests/dev-proxy.mjs http://127.0.0.1:PORT');
const root = fileURLToPath(new URL('..', import.meta.url));
const example = JSON.stringify(JSON.parse(await readFile(new URL('../../../fixtures/intake/vectors.json', import.meta.url), 'utf8')).example);
const config = await readFile(new URL('../vite.config.ts', import.meta.url), 'utf8');
assert.ok(config.includes("['/v1/submit', '/v1/publishers/pointer']") && config.includes('changeOrigin: false'), 'vite.config.ts proxies both routes with changeOrigin: false');

const server = spawn('pnpm', ['dev', '--host', '127.0.0.1', '--port', '0'], {
	cwd: root,
	env: { ...process.env, VITE_ONYX_SUBMIT_API: '1', ONYX_SUBMIT_PROXY: api },
	detached: true,
	stdio: ['ignore', 'pipe', 'pipe']
});
/** One POST with exactly these headers, as a proxy would send it (no browser in between). */
const post = (url, headers, body) =>
	new Promise((resolve, reject) => {
		const u = new URL(url);
		const rq = request({ host: u.hostname, port: u.port, path: u.pathname, method: 'POST', headers: { 'content-length': Buffer.byteLength(body), ...headers } }, (res) => {
			let text = '';
			res.on('data', (d) => (text += d));
			res.on('end', () => resolve({ status: res.statusCode, text }));
		});
		rq.on('error', reject);
		rq.end(body);
	});
let browser;
let failed = 0;
const ok = (cond, msg) => {
	console.log(`${cond ? 'ok  ' : 'FAIL'} ${msg}`);
	if (!cond) failed++;
};
try {
	const base = await new Promise((resolve, reject) => {
		const timeout = setTimeout(() => reject(new Error('the dev server did not start')), 60000);
		let log = '';
		server.stdout.on('data', (d) => {
			log = (log + d.toString()).slice(-8192);
			const url = log.match(/http:\/\/127\.0\.0\.1:\d+\//)?.[0];
			if (url) {
				clearTimeout(timeout);
				resolve(url);
			}
		});
		server.on('exit', (code) => {
			clearTimeout(timeout);
			reject(new Error(`the dev server exited ${code}`));
		});
	});
	const origin = new URL(base).origin;
	assert.notEqual(origin, api, 'the page and the node are two origins, joined only by the proxy');
	browser = await chromium.launch({ channel: 'chrome' });
	const page = await (await browser.newContext()).newPage();
	const sent = [];
	page.on('request', (r) => r.method() === 'POST' && sent.push({ url: r.url(), headers: r.headers() }));
	await page.goto(base + '#/submit');
	await page.getByTestId('add-paths').waitFor({ timeout: 90000 });
	// The page's own fetch, as the proposal form sends it: same origin, JSON.
	const send = (path) =>
		page.evaluate(
			async ([path, body]) => {
				const r = await fetch(path, { method: 'POST', headers: { 'content-type': 'application/json' }, body });
				return { status: r.status, body: await r.json().catch(() => null) };
			},
			[path, example]
		);
	const check = await send('/v1/submit/check');
	ok(check.status === 200 && check.body?.valid === true, `a browser's POST through the dev proxy reaches the node as the page's own host: POST /v1/submit/check is ${check.status} (${check.body?.error?.code ?? 'valid'})`);
	const receipt = await send('/v1/submit');
	ok(receipt.status === 201 && /^sub_[0-9a-f]{32}$/.test(receipt.body?.id ?? ''), `POST /v1/submit through the dev proxy is ${receipt.status} with a receipt (${receipt.body?.error?.code ?? receipt.body?.id})`);
	ok(sent.length === 2 && sent.every((s) => s.url.startsWith(origin + '/v1/submit')), `the browser sent both to the page's own origin (${origin}), not to the node`);
	// What the string shorthand did: the browser's Origin with the node's own Host. Still refused,
	// so the rule that makes this setting matter is in force on this node.
	const rewritten = await post(api + '/v1/submit/check', { 'content-type': 'application/json', origin, host: new URL(api).host, 'sec-fetch-site': 'same-origin' }, example);
	ok(rewritten.status === 403 && JSON.parse(rewritten.text).error?.code === 'cross_site_refused', `the same POST with the host rewritten to the node's (what the proxy's shorthand sent) is refused: ${rewritten.status}`);
	// And with the page's host passed on, without a browser: taken.
	const passed = await post(api + '/v1/submit/check', { 'content-type': 'application/json', origin, host: new URL(origin).host }, example);
	ok(passed.status === 200, `with the page's host passed on it is taken: ${passed.status}`);
	// The pointer route is proxied the same way: this node has none, so its own 404 comes back
	// (through the proxy, as JSON), never Vite's page.
	const pointer = await page.evaluate(async () => {
		const r = await fetch('/v1/publishers/pointer', { method: 'POST', headers: { 'content-type': 'application/json' }, body: '{}' });
		return { status: r.status, type: r.headers.get('content-type') ?? '' };
	});
	ok(pointer.status === 404 && !pointer.type.includes('html'), `the pointer route is proxied to the node too (its answer: ${pointer.status} ${pointer.type || 'no body type'})`);
} finally {
	await browser?.close();
	try {
		process.kill(-server.pid, 'SIGTERM');
	} catch {
		// already gone
	}
}
console.log(failed ? `DEV PROXY FAILED (${failed})` : 'DEV PROXY PASSED');
process.exitCode = failed ? 1 : 0;
