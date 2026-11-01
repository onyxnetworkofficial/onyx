// `onyx-contrib ui` (CL10, THREATS CL10): the local app server. Real HTTP on 127.0.0.1 with a fake
// provider and a fake Onyx intake; no outside network. Every route, the launch token, the Host,
// Origin and Sec-Fetch-Site gates, the Claude Desktop merge, and that no key (in any form, any
// 10-character piece) reaches any response, header, log line, page file or stored file.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { request } from 'node:http';
import { chmodSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { PassThrough } from 'node:stream';
import { claudeConfigPath, connectClaude, mcpEntry, SERVER_NAME } from '../src/claude.mjs';
import { doctor, secretToolAdvice } from '../src/doctor.mjs';
import { registry } from '../src/providers.mjs';
import * as rules from '../src/rules.mjs';
import { startUi } from '../src/ui.mjs';
import { memoryVault } from '../src/vault.mjs';
import { fakeIntake, fakeProvider, KEY, OTHER_KEY, PROVIDER_ORIGIN, routedFetch } from './fakes.mjs';

const PASSPHRASE = 'FAKE-ui-vault-pw-7Qx29';
let provider, intake;
const dirs = [];
const apps = [];
before(async () => {
	provider = await fakeProvider();
	intake = await fakeIntake();
});
after(async () => {
	for (const a of apps) await a.close();
	provider.close();
	intake.close();
	for (const d of dirs) rmSync(d, { recursive: true, force: true });
});

const tmp = (p) => {
	const d = mkdtempSync(join(tmpdir(), p));
	dirs.push(d);
	return d;
};

/** Everything any test saw leave the server: bodies, headers, log lines. Scanned for keys at the end. */
const seen = [];

async function app({ withProviders = true } = {}) {
	const dir = tmp('onyx-ui-');
	const claude = join(tmp('onyx-claude-'), 'Claude', 'claude_desktop_config.json');
	if (withProviders) {
		const reg = registry(dir);
		reg.add({ name: 'prices', baseUrl: PROVIDER_ORIGIN, auth: { in: 'header', name: 'x-api-key' }, openapi: `${PROVIDER_ORIGIN}/openapi.json`, docsUrl: 'https://www.example.com/api-terms' }, { addedBy: 'you' });
	}
	const vault = memoryVault();
	const logs = [];
	const a = await startUi({ env: { ONYX_API_URL: intake.url, ONYX_CLAUDE_CONFIG: claude, ONYX_CONTRIB_HOME: dir }, fetch: routedFetch(provider.url), vault, dir, log: (l) => logs.push(l), platform: 'linux', execPath: '/opt/onyx/onyx-contrib', sea: true, run: async () => ({ code: 1, stdout: '', stderr: '' }) });
	apps.push(a);
	seen.push(logs);
	return { a, dir, claude, vault, logs };
}

/** One raw HTTP request (so Host and Origin can be anything). */
function http(a, { method = 'GET', path = '/', headers = {}, body, token = a.token } = {}) {
	const h = { host: `127.0.0.1:${a.port}`, ...(token ? { 'x-onyx-token': token } : {}), ...(method !== 'GET' && method !== 'HEAD' ? { origin: `http://127.0.0.1:${a.port}`, 'content-type': 'application/json' } : {}), ...headers };
	for (const [k, v] of Object.entries(h)) if (v === null) delete h[k];
	const data = body === undefined ? undefined : typeof body === 'string' ? body : JSON.stringify(body);
	return new Promise((ok, fail) => {
		const r = request({ host: '127.0.0.1', port: a.port, method, path, headers: h, setHost: false }, (res) => {
			const chunks = [];
			res.on('data', (c) => chunks.push(c));
			res.on('end', () => {
				const text = Buffer.concat(chunks).toString('utf8');
				seen.push([JSON.stringify(res.headers), text]);
				let json = null;
				try {
					json = JSON.parse(text);
				} catch {}
				ok({ status: res.statusCode, headers: res.headers, text, json });
			});
		});
		r.on('error', fail);
		if (data !== undefined) r.write(data);
		r.end();
	});
}

const noKeyIn = (s, what) => {
	for (const k of [KEY, OTHER_KEY, PASSPHRASE]) {
		assert.equal(rules.redact(String(s), [k]), String(s), `a piece of a key reached ${what}`);
		for (const f of rules.keyForms(k)) assert.ok(!String(s).toLowerCase().includes(f.toLowerCase()), `the key reached ${what}`);
	}
};

const ARGS = { provider: 'prices', path: '/simple/price', params: { ids: 'bitcoin', vs_currencies: 'usd' }, pointer: '/bitcoin/usd', title: 'Bitcoin price in US dollars (example API)', description: 'The bitcoin price in US dollars as the example API serves it, updated every minute.', rights: 'not_sure', namesPerson: false, changes: 'live', unit: 'USD' };

test('ui: listens on 127.0.0.1 only, a random port, a new 256-bit token per launch in the URL fragment', async () => {
	const { a } = await app();
	const { a: b } = await app();
	assert.match(a.url, /^http:\/\/127\.0\.0\.1:\d+\/#t=[0-9a-f]{64}$/);
	assert.notEqual(a.port, b.port);
	assert.notEqual(a.token, b.token);
	assert.ok(a.url.endsWith(a.token));
	// Only the loopback address answers: the server is bound to it, not to every interface.
	assert.equal((await http(a, { path: '/' })).status, 200);
});

test('ui: the token is required on every /api route; a wrong or missing one is refused', async () => {
	const { a } = await app();
	for (const token of [null, '', 'f'.repeat(64), a.token.slice(0, 63), a.token + '0']) {
		const r = await http(a, { path: '/api/state', token: token ?? undefined, headers: token === null ? { 'x-onyx-token': null } : {} });
		assert.equal(r.status, 401, `token ${token}`);
		assert.equal(r.json.ok, false);
	}
	const b = await app();
	assert.equal((await http(a, { path: '/api/state', token: b.a.token })).status, 401, "another launch's token");
	assert.equal((await http(a, { path: '/api/state' })).status, 200);
});

test('ui: Host must be this server (DNS rebinding), on every path, the page too', async () => {
	const { a } = await app();
	for (const host of ['evil.example.com', `evil.example.com:${a.port}`, `127.0.0.1:${a.port + 1}`, `127.0.0.1`, `[::1]:${a.port}`, `127.0.0.1.nip.io:${a.port}`])
		for (const path of ['/', '/app.js', '/api/state']) assert.equal((await http(a, { path, headers: { host } })).status, 403, `${host} ${path}`);
	assert.equal((await http(a, { path: '/api/state', headers: { host: `localhost:${a.port}` } })).status, 200);
});

test('ui: other origins are refused: Origin, Sec-Fetch-Site, a write without Origin, preflight, plain-text bodies', async () => {
	const { a } = await app();
	const post = (headers, body = { name: 'x' }) => http(a, { method: 'POST', path: '/api/key/remove', headers, body });
	assert.equal((await post({ origin: 'http://evil.example.com' })).status, 403);
	assert.equal((await post({ origin: 'null' })).status, 403);
	assert.equal((await post({ origin: `http://127.0.0.1:${a.port + 1}` })).status, 403);
	assert.equal((await post({ origin: null })).status, 403, 'a write without Origin');
	assert.equal((await post({ 'sec-fetch-site': 'cross-site' })).status, 403);
	assert.equal((await post({ 'sec-fetch-site': 'same-site' })).status, 403);
	assert.equal((await http(a, { path: '/api/state', headers: { origin: 'https://evil.example.com' } })).status, 403, 'a cross-origin read');
	assert.equal((await post({ 'content-type': 'text/plain' }, 'name=x')).status, 415);
	assert.equal((await post({ 'content-type': 'application/x-www-form-urlencoded' }, 'name=x')).status, 415);
	const pre = await http(a, { method: 'OPTIONS', path: '/api/key', headers: { origin: null, 'access-control-request-method': 'POST' } });
	assert.ok([403, 405].includes(pre.status), `preflight ${pre.status}`);
	const pre2 = await http(a, { method: 'OPTIONS', path: '/api/key', headers: { 'access-control-request-method': 'POST' } });
	assert.equal(pre2.status, 405);
	assert.equal(pre2.headers['access-control-allow-origin'], undefined);
	assert.equal(pre.headers['access-control-allow-origin'], undefined);
	const ok = await http(a, { path: '/api/state', headers: { 'sec-fetch-site': 'same-origin', origin: `http://127.0.0.1:${a.port}` } });
	assert.equal(ok.status, 200);
	assert.equal(ok.headers['access-control-allow-origin'], undefined, 'no CORS header, ever');
	assert.equal((await http(a, { method: 'POST', path: '/api/providers', body: 'x'.repeat(70 * 1024) })).status, 413);
	assert.equal((await http(a, { method: 'POST', path: '/api/providers', body: '[1]' })).status, 400);
});

test('ui: the page and its files are served with a strict policy and no outside asset', async () => {
	const { a } = await app();
	for (const [path, type] of [['/', 'text/html'], ['/app.js', 'text/javascript'], ['/app.css', 'text/css'], ['/icon.svg', 'image/svg+xml']]) {
		const r = await http(a, { path });
		assert.equal(r.status, 200, path);
		assert.ok(r.headers['content-type'].startsWith(type), path);
		assert.match(r.headers['content-security-policy'], /default-src 'none'; script-src 'self'/);
		assert.equal(r.headers['x-frame-options'], 'DENY');
		assert.equal(r.headers['cache-control'], 'no-store');
		assert.ok(!/https?:\/\/(?!www\.w3\.org\/2000\/svg)/.test(r.text.replace(/https:\/\/(api|www)\.example\.com[^"' ]*/g, '')), `${path} loads nothing from outside`);
	}
	assert.equal((await http(a, { path: '/../package.json' })).status, 404);
	assert.equal((await http(a, { path: '/api/nope' })).status, 404);
	assert.equal((await http(a, { method: 'POST', path: '/' , body: {} })).status, 404);
});

test('ui: a browser started from a terminal gets a private launch file, never the token on its command line (review CL10-R1-2)', async () => {
	const { a } = await app();
	const f = a.launchFile();
	assert.ok(!f.includes(a.token), 'the path holds no token');
	assert.equal((statSync(f).mode & 0o777).toString(8), '600');
	assert.equal((statSync(dirname(f)).mode & 0o777).toString(8), '700');
	assert.ok(readFileSync(f, 'utf8').includes(`url=${a.url}`), 'it forwards to the tokened URL');
	await a.close();
	assert.ok(!existsSync(f), 'deleted when the server stops');
	assert.equal((await http(a, { path: '/open/' + '0'.repeat(32) }).catch(() => ({ status: 'closed' }))).status, 'closed');
});

// Review CL10-R1-1 (fresh adversarial reviewer): the key filter's mark answered "does this guess hold
// a piece of the key?" when an error repeated the caller's input (the provider name), hex form one
// byte at a time; the reviewer recovered a full key in 1582 requests. Now any input holding a piece
// of a key is refused and the session stops using keys.
test('review: a guess holding a piece of the key is refused and locks the session (no oracle), in the app and over MCP', async () => {
	const { a } = await app();
	await http(a, { method: 'POST', path: '/api/key', body: { provider: 'prices', key: KEY } });
	const hex = Buffer.from(KEY.slice(3, 8)).toString('hex');
	const miss = await http(a, { method: 'POST', path: '/api/key/remove', body: { provider: 'zz' + 'ab'.repeat(5) } });
	assert.equal(miss.status, 400);
	assert.doesNotMatch(miss.text, /zzab/, 'the name is not repeated');
	const hit = await http(a, { method: 'POST', path: '/api/key/remove', body: { provider: hex } });
	assert.equal(hit.status, 422);
	assert.equal(hit.json.locked, true);
	assert.doesNotMatch(hit.text, /key removed/, 'no filter mark');
	assert.equal((await http(a, { method: 'POST', path: '/api/preview', body: { provider: 'prices', path: '/simple/price', params: { ids: 'bitcoin' } } })).json.locked, true, 'locked afterwards');
	assert.equal((await http(a, { path: '/api/state' })).json.locked, true);
	const q = await app();
	await http(q.a, { method: 'POST', path: '/api/key', body: { provider: 'prices', key: KEY } });
	assert.equal((await http(q.a, { path: `/api/endpoints?provider=prices&search=${KEY.slice(4, 16)}` })).json.locked, true, 'in a query string too');
	// The MCP path: the same probe through a tool argument.
	const { createState, runTool, LOCKED } = await import('../src/mcp.mjs');
	const vault = memoryVault();
	await vault.set('prices', PROVIDER_ORIGIN, KEY);
	const dir = tmp('onyx-mcp-');
	registry(dir).add({ name: 'prices', baseUrl: PROVIDER_ORIGIN, auth: { in: 'header', name: 'x-api-key' } }, { addedBy: 'you' });
	const st = createState({ env: { ONYX_API_URL: intake.url }, fetch: routedFetch(provider.url), vault, dir, log: () => {} });
	await runTool(st, 'contrib_providers', {});
	const r1 = JSON.parse((await runTool(st, 'contrib_preview', { provider: 'nope' + 'cd'.repeat(4) })).content[0].text);
	assert.doesNotMatch(JSON.stringify(r1), /nopecd/);
	const r2 = JSON.parse((await runTool(st, 'contrib_preview', { provider: hex })).content[0].text);
	assert.equal(r2.error, LOCKED);
	const r3 = JSON.parse((await runTool(st, 'contrib_providers', {})).content[0].text);
	assert.equal(r3.error, LOCKED, 'every tool refuses afterwards');
	const r4 = JSON.parse((await runTool(st, 'contrib_preview', { provider: 'prices', path: '/x', 'unknown1234': 1 })).content[0].text);
	assert.equal(r4.ok, false);
});

test('ui: every route works end to end: provider, key, endpoints, preview, check, confirm, history, status, remove', async () => {
	const { a, dir, vault, logs } = await app({ withProviders: false });
	const st0 = await http(a, { path: '/api/state' });
	assert.equal(st0.json.providers.length, 0);
	assert.match(st0.json.network, /test network/);
	const add = await http(a, { method: 'POST', path: '/api/providers', body: { name: 'prices', baseUrl: PROVIDER_ORIGIN, authIn: 'header', authName: 'x-api-key', openapi: `${PROVIDER_ORIGIN}/openapi.json`, docsUrl: 'https://www.example.com/api-terms' } });
	assert.equal(add.status, 200, add.text);
	assert.equal(add.json.provider.keyStored, false);
	assert.equal(registry(dir).get('prices').addedBy, 'you');
	const dup = await http(a, { method: 'POST', path: '/api/providers', body: { name: 'prices', baseUrl: 'https://evil.example.net', authIn: 'header', authName: 'x-api-key' } });
	assert.equal(dup.status, 400);
	assert.equal(registry(dir).get('prices').baseUrl, PROVIDER_ORIGIN, 'an existing provider is not replaced');
	const q = await http(a, { method: 'POST', path: '/api/providers', body: { name: 'qp', baseUrl: `${PROVIDER_ORIGIN}/q`, authIn: 'query', authName: 'api_key' } });
	assert.equal(q.json.provider.keySentAs, 'query parameter api_key');
	const short = await http(a, { method: 'POST', path: '/api/key', body: { provider: 'prices', key: 'abc' } });
	assert.equal(short.status, 422);
	const set = await http(a, { method: 'POST', path: '/api/key', body: { provider: 'prices', key: KEY } });
	assert.equal(set.status, 200, set.text);
	assert.equal(set.json.stored, true);
	assert.equal(await vault.get('prices', PROVIDER_ORIGIN), KEY);
	assert.equal((await http(a, { method: 'POST', path: '/api/key', body: { provider: 'qp', key: OTHER_KEY } })).json.stored, true);
	const st1 = await http(a, { path: '/api/state' });
	assert.deepEqual(st1.json.providers.map((p) => [p.name, p.keyStored]), [['prices', true], ['qp', true]]);
	const eps = await http(a, { path: '/api/endpoints?provider=prices&search=price' });
	assert.deepEqual(eps.json.endpoints.map((e) => e.path), ['/simple/price']);
	const pv = await http(a, { method: 'POST', path: '/api/preview', body: { provider: 'prices', path: '/account' } });
	assert.equal(pv.status, 200);
	assert.equal(pv.json.keyRemovedFromAnswer, true);
	assert.equal(pv.json.answer.api_key_used, '[key removed]');
	const bad = await http(a, { method: 'POST', path: '/api/preview', body: { provider: 'prices', path: '/simple/price', params: { api_key: 'x' } } });
	assert.equal(bad.status, 422);
	const chk = await http(a, { method: 'POST', path: '/api/contribute', body: ARGS });
	assert.equal(chk.status, 200, chk.text);
	assert.equal(chk.json.recorded, false);
	assert.match(chk.json.checkId, /^chk_[0-9a-f]{16}$/);
	assert.equal(intake.seen.filter((r) => r.url === '/v1/submit').length, 0, 'a check records nothing');
	const before = intake.seen.length;
	const conf = await http(a, { method: 'POST', path: '/api/contribute', body: { checkId: chk.json.checkId, confirm: true } });
	assert.equal(conf.status, 200, conf.text);
	assert.match(conf.json.receipt.id, /^sub_[0-9a-f]{32}$/);
	const sent = intake.seen.slice(before);
	assert.equal(sent.length, 1);
	assert.deepEqual(JSON.parse(sent[0].body), chk.json.willSend, 'exactly the checked body');
	assert.equal((await http(a, { method: 'POST', path: '/api/contribute', body: { checkId: chk.json.checkId, confirm: true } })).status, 422, 'sent once');
	assert.equal((await http(a, { method: 'POST', path: '/api/contribute', body: { checkId: 'chk_0000000000000000', confirm: true } })).status, 422);
	const hist = await http(a, { path: '/api/contributions' });
	assert.equal(hist.json.contributions.length, 1);
	assert.equal(hist.json.contributions[0].id, conf.json.receipt.id);
	assert.equal(hist.json.contributions[0].provider, 'prices');
	assert.equal((statSync(join(dir, 'contributions.json')).mode & 0o777).toString(8), '600');
	const stat = await http(a, { path: `/api/contributions/status?id=${conf.json.receipt.id}` });
	assert.equal(stat.status, 200, stat.text);
	assert.equal(stat.json.status.id, conf.json.receipt.id);
	assert.equal((await http(a, { path: '/api/contributions/status?id=sub_nope' })).status, 422);
	const doc = await http(a, { path: '/api/doctor' });
	assert.equal(doc.status, 200);
	assert.ok(doc.json.checks.some((c) => c.id === 'onyx' && c.ok));
	assert.equal((await http(a, { method: 'POST', path: '/api/key/remove', body: { provider: 'qp' } })).json.removed, true);
	assert.equal(await vault.get('qp', PROVIDER_ORIGIN), null);
	assert.equal((await http(a, { method: 'POST', path: '/api/providers/remove', body: { name: 'prices' } })).json.removed, 'prices');
	assert.equal(await vault.get('prices', PROVIDER_ORIGIN), null, 'removing a provider removes its key');
	assert.equal(registry(dir).get('prices'), null);
	assert.ok(logs.some((l) => /POST \/api\/key 200/.test(l)));
	for (const f of readdirSync(dir)) noKeyIn(readFileSync(join(dir, f), 'utf8'), f);
});

test('ui: a key typed into the page is never sent back, not even in an error, and a key echoed by a provider is removed', async () => {
	const { a } = await app();
	// A refused key (spaces) and an unknown provider: the error never repeats the key.
	const odd = `${KEY} x`;
	const r1 = await http(a, { method: 'POST', path: '/api/key', body: { provider: 'prices', key: odd } });
	assert.equal(r1.status, 422);
	const r2 = await http(a, { method: 'POST', path: '/api/key', body: { provider: 'nobody', key: KEY } });
	assert.equal(r2.status, 400);
	// A malformed body with the key inside: refused without echo.
	const r3 = await http(a, { method: 'POST', path: '/api/key', body: `{"provider":"prices","key":"${KEY}"` });
	assert.equal(r3.status, 400);
	for (const r of [r1, r2, r3]) noKeyIn(r.text, 'an error');
	await http(a, { method: 'POST', path: '/api/key', body: { provider: 'prices', key: KEY } });
	const err = await http(a, { method: 'POST', path: '/api/preview', body: { provider: 'prices', path: '/error' } });
	assert.equal(err.status, 422);
	assert.match(err.text, /\[key removed\]/);
	const nf = await http(a, { method: 'POST', path: '/api/preview', body: { provider: 'prices', path: '/nothing-here', params: { x: KEY.slice(0, 14) } } });
	noKeyIn(nf.text, 'a 404 echo');
});

test('ui: the encrypted-file vault can be chosen in the app; the passphrase is never echoed', async () => {
	const { a, dir } = await app();
	const weak = await http(a, { method: 'POST', path: '/api/vault/file', body: { passphrase: 'short' } });
	assert.equal(weak.status, 400);
	const r = await http(a, { method: 'POST', path: '/api/vault/file', body: { passphrase: PASSPHRASE } });
	assert.equal(r.status, 200, r.text);
	assert.match(r.json.vault, /encrypted file/);
	await http(a, { method: 'POST', path: '/api/key', body: { provider: 'prices', key: KEY } });
	const raw = readFileSync(join(dir, 'vault.json'), 'utf8');
	noKeyIn(raw, 'the vault file');
	const pv = await http(a, { method: 'POST', path: '/api/preview', body: { provider: 'prices', path: '/simple/price', params: { ids: 'bitcoin' }, pointer: '/bitcoin/usd' } });
	assert.equal(pv.json.value, 67000, 'the key is read back from the file vault');
});

test('claude: the settings path per system', () => {
	assert.equal(claudeConfigPath({}, 'darwin', '/Users/u'), '/Users/u/Library/Application Support/Claude/claude_desktop_config.json');
	assert.equal(claudeConfigPath({ APPDATA: 'C:\\Users\\u\\AppData\\Roaming' }, 'win32', 'C:\\Users\\u').replace(/\\/g, '/'), 'C:/Users/u/AppData/Roaming/Claude/claude_desktop_config.json');
	assert.equal(claudeConfigPath({}, 'linux', '/home/u'), '/home/u/.config/Claude/claude_desktop_config.json');
	assert.equal(claudeConfigPath({ ONYX_CLAUDE_CONFIG: '/x/c.json' }, 'linux', '/home/u'), '/x/c.json');
	assert.deepEqual(mcpEntry({ execPath: '/opt/onyx-contrib', sea: true, env: { ONYX_CONTRIB_VAULT_PASSPHRASE: PASSPHRASE } }), { command: '/opt/onyx-contrib', args: ['mcp'] }, 'a passphrase is never written');
	assert.deepEqual(mcpEntry({ execPath: '/usr/bin/node', sea: false, cliPath: '/src/cli.mjs', env: { ONYX_API_URL: 'https://n.example.org' } }), { command: '/usr/bin/node', args: ['/src/cli.mjs', 'mcp'], env: { ONYX_API_URL: 'https://n.example.org' } });
});

test('claude: connect merges, never overwrites: other servers and settings kept, old file backed up, invalid JSON left alone', async () => {
	const { a, claude } = await app();
	// No file yet: created.
	const first = await http(a, { method: 'POST', path: '/api/claude/connect', body: {} });
	assert.equal(first.status, 200, first.text);
	assert.equal(first.json.changed, true);
	assert.equal(first.json.backup, null);
	assert.deepEqual(JSON.parse(readFileSync(claude, 'utf8')).mcpServers[SERVER_NAME].args, ['mcp']);
	// An existing file with other servers and settings.
	const other = { globalShortcut: 'Ctrl+Space', mcpServers: { files: { command: 'npx', args: ['-y', 'some-server'] }, [SERVER_NAME]: { command: '/old/path', args: ['mcp'] } } };
	writeFileSync(claude, JSON.stringify(other, null, 2));
	const second = await http(a, { method: 'POST', path: '/api/claude/connect', body: {} });
	assert.equal(second.json.changed, true);
	const now = JSON.parse(readFileSync(claude, 'utf8'));
	assert.equal(now.globalShortcut, 'Ctrl+Space');
	assert.deepEqual(now.mcpServers.files, other.mcpServers.files);
	assert.equal(now.mcpServers[SERVER_NAME].command, '/opt/onyx/onyx-contrib');
	assert.deepEqual(JSON.parse(readFileSync(second.json.backup, 'utf8')), other, 'the backup is the old file');
	// Unchanged: no new backup.
	const third = await http(a, { method: 'POST', path: '/api/claude/connect', body: {} });
	assert.equal(third.json.changed, false);
	assert.equal((await http(a, { path: '/api/state' })).json.claude.current, true);
	// Invalid JSON: refused, file untouched.
	writeFileSync(claude, '{ "mcpServers": ');
	const bad = await http(a, { method: 'POST', path: '/api/claude/connect', body: {} });
	assert.equal(bad.status, 400);
	assert.match(bad.json.error, /not valid JSON/);
	assert.equal(readFileSync(claude, 'utf8'), '{ "mcpServers": ');
	writeFileSync(claude, '[]');
	assert.throws(() => connectClaude({ path: claude, entry: { command: 'x', args: [] } }), /not a JSON object/);
	// Review CL10-R1-3: the mode is kept (the file may hold other servers' tokens) and a link is written through.
	writeFileSync(claude, JSON.stringify({ mcpServers: { other: { command: 'x', args: [], env: { TOKEN: 'secret' } } } }));
	chmodSync(claude, 0o600);
	const old = process.umask(0o002);
	try {
		const r = connectClaude({ path: claude, entry: { command: 'y', args: ['mcp'] } });
		assert.equal((statSync(claude).mode & 0o777).toString(8), '600');
		assert.equal((statSync(r.backup).mode & 0o777).toString(8), '600');
	} finally {
		process.umask(old);
	}
	const target = join(dirname(claude), 'real-config.json');
	writeFileSync(target, '{}');
	const link = join(dirname(claude), 'linked.json');
	symlinkSync(target, link);
	connectClaude({ path: link, entry: { command: 'y', args: ['mcp'] } });
	assert.ok(lstatSync(link).isSymbolicLink(), 'the link stays a link');
	assert.equal(JSON.parse(readFileSync(target, 'utf8')).mcpServers[SERVER_NAME].command, 'y');
	const fresh = join(tmp('onyx-claude-'), 'new', 'claude_desktop_config.json');
	connectClaude({ path: fresh, entry: { command: 'y', args: ['mcp'] } });
	assert.equal((statSync(fresh).mode & 0o777).toString(8), '600', 'a new file is 0600');
	writeFileSync(claude, '{"mcpServers": []}');
	assert.throws(() => connectClaude({ path: claude, entry: { command: 'x', args: [] } }), /not an object/);
});

test('claude: from an AppImage the program is copied to a fixed place first (the mount moves every launch)', async () => {
	const dir = tmp('onyx-ui-');
	const exe = join(tmp('onyx-mount-'), 'onyx-contrib');
	writeFileSync(exe, '#!/bin/sh\n');
	const claude = join(tmp('onyx-claude-'), 'claude_desktop_config.json');
	const a = await startUi({ env: { ONYX_API_URL: intake.url, ONYX_CLAUDE_CONFIG: claude, APPIMAGE: '/home/u/Onyx.AppImage' }, vault: memoryVault(), dir, log: () => {}, platform: 'linux', execPath: exe, sea: true });
	apps.push(a);
	const r = await http(a, { method: 'POST', path: '/api/claude/connect', body: {} });
	assert.equal(r.status, 200, r.text);
	const cmd = JSON.parse(readFileSync(claude, 'utf8')).mcpServers[SERVER_NAME].command;
	assert.equal(cmd, join(dir, 'bin', 'onyx-contrib'));
	assert.ok(existsSync(cmd));
});

test('doctor: plain words with one thing to do (missing secret-tool, no keyring, works)', async () => {
	assert.match(secretToolAdvice({ code: -1, stderr: 'secret-tool is not installed' }).fix, /Install libsecret-tools/);
	assert.match(secretToolAdvice({ code: -1, stderr: '' }).fix, /encrypted file/);
	assert.match(secretToolAdvice({ code: 1, stderr: 'Cannot autolaunch D-Bus without X11 $DISPLAY' }).say, /no keyring service/);
	assert.equal(secretToolAdvice({ code: 1, stderr: '' }), null, 'nothing found is fine');
	const run = async () => ({ code: -1, stdout: '', stderr: 'secret-tool is not installed' });
	const d = await doctor({ vault: memoryVault(), env: { ONYX_CLAUDE_CONFIG: '/nonexistent/c.json' }, platform: 'linux', run });
	assert.equal(d.ok, false);
	assert.match(d.checks.find((c) => c.id === 'vault').fix, /sudo apt install libsecret-tools/);
	assert.match(d.checks.find((c) => c.id === 'claude').say, /not found/);
	const good = await doctor({ vault: memoryVault(), env: { ONYX_CLAUDE_CONFIG: '/nonexistent/c.json' }, platform: 'darwin' });
	assert.equal(good.ok, true);
	assert.match(good.checks[0].say, /macOS login keychain/);
});

test('cli: `ui --desktop` prints only the tokened URL on stdout and stops when its input closes', async () => {
	const { main } = await import('../src/cli.mjs');
	const d = tmp('onyx-ui-cli-');
	const out = [];
	const stdin = new PassThrough();
	const exits = [];
	const realExit = process.exit;
	process.exit = (c) => exits.push(c);
	try {
		const code = await main(['ui', '--desktop'], { env: { ONYX_CONTRIB_HOME: d, ONYX_API_URL: intake.url, ONYX_CONTRIB_VAULT: 'file', ONYX_CONTRIB_VAULT_PASSPHRASE: PASSPHRASE }, stdin, out: (s) => out.push(s), err: () => {}, uiStarted: (x) => apps.push(x) });
		assert.equal(code, null);
		assert.equal(out.length, 1);
		assert.match(out[0], /^ONYX_UI_URL=http:\/\/127\.0\.0\.1:\d+\/#t=[0-9a-f]{64}$/);
		stdin.end();
		for (let i = 0; i < 100 && !exits.length; i++) await new Promise((r) => setTimeout(r, 10));
		assert.deepEqual(exits, [0]);
	} finally {
		process.exit = realExit;
	}
});

test('cli: doctor prints plain words and what to do', async () => {
	const { main } = await import('../src/cli.mjs');
	const d = tmp('onyx-doc-');
	const out = [];
	assert.equal(await main(['doctor'], { env: { ONYX_CONTRIB_HOME: d, ONYX_CONTRIB_VAULT: 'file', ONYX_CONTRIB_VAULT_PASSPHRASE: PASSPHRASE, ONYX_CLAUDE_CONFIG: '/nonexistent/c.json' }, out: (s) => out.push(s), err: (s) => out.push(s) }), 0);
	assert.match(out.join('\n'), /OK {3}Your keys go in an encrypted file/);
});

test('ui: no key, no passphrase in any response, header, log line or page file of this test file (CL2 scan)', () => {
	assert.ok(seen.length > 50);
	noKeyIn(JSON.stringify(seen), 'a response, a header or a log line');
	for (const f of readdirSync(new URL('../ui/', import.meta.url))) noKeyIn(readFileSync(new URL(`../ui/${f}`, import.meta.url), 'utf8'), f);
});
