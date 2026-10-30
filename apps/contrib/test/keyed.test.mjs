// onyx-contrib keyed rounds (CL4, ADR-0054) without a network and without the Rust build: a
// stand-in for `level2-keyed` (test/fake-keyed-bin.mjs). The key goes to the observer on stdin
// only (never argv, the environment or a file of ours), the observer's output is filtered, the
// seat holds nothing about the key, and a provider whose host or key place differs from the
// manifest gets no key at all (THREATS CL4-1, CL4-2, CL4-6, CL4-9).
import { test, after } from 'node:test';
import assert from 'node:assert/strict';
import { chmodSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { main } from '../src/cli.mjs';
import { checkProviderFits, observe, readKeyedManifest } from '../src/keyed.mjs';
import { createState, runTool } from '../src/mcp.mjs';
import { registry } from '../src/providers.mjs';
import { memoryVault } from '../src/vault.mjs';

const here = fileURLToPath(new URL('.', import.meta.url));
const REPO = resolve(here, '../../..');
const DEMO = join(REPO, 'fixtures/keyed-demo');
const MANIFEST = join(DEMO, 'sources/demo.keyed.widget_index/manifest.json');
/** A FAKE key, with characters a query must encode. */
const KEY = 'FAKE-cl4-node-key+0123/456789';
const ORIGIN = 'https://keyed.example.org';

const dirs = [];
after(() => {
	for (const d of dirs) rmSync(d, { recursive: true, force: true });
});

function setup({ auth = { in: 'query', name: 'api_key' }, baseUrl = `${ORIGIN}/v1` } = {}) {
	const dir = mkdtempSync(join(tmpdir(), 'onyx-contrib-keyed-'));
	dirs.push(dir);
	// The stand-in imports ../src/keccak.mjs relative to itself, so it runs from test/.
	const runBin = join(here, 'fake-keyed-bin.mjs');
	chmodSync(runBin, 0o755);
	registry(dir).add({ name: 'widgets', baseUrl, auth }, { addedBy: 'you' });
	const vault = memoryVault();
	const record = join(dir, 'record.json');
	const env = { ...process.env, ONYX_CONTRIB_HOME: dir, ONYX_CONTRIB_KEYED_BIN: runBin, ONYX_CONTRIB_REPO: DEMO, FAKE_KEYED_RECORD: record };
	return { dir, vault, env, record, bin: runBin };
}

const cli = async (argv, { env, vault }) => {
	const out = [];
	const err = [];
	const code = await main(argv, { env, vault, stdin: { isTTY: false }, out: (s) => out.push(s), err: (s) => err.push(s) });
	return { code, out: out.join('\n'), err: err.join('\n') };
};

/** Every file under `dir`, as text. */
function allText(dir) {
	let s = '';
	for (const f of readdirSync(dir, { recursive: true })) {
		const p = join(dir, String(f));
		if (statSync(p).isFile() && !p.endsWith('record.json')) s += readFileSync(p, 'utf8');
	}
	return s;
}

test('keyed: the manifest says where the key goes and the terms hash is the quote\'s Keccak-256', () => {
	const m = readKeyedManifest(MANIFEST);
	assert.equal(m.origin, ORIGIN);
	assert.deepEqual(m.access, { place: 'query', name: 'api_key', scheme: 'raw' });
	assert.match(m.termsHash, /^0x[0-9a-f]{64}$/);
	assert.throws(() => readKeyedManifest(join(REPO, 'sources/us.treasury.tga_total_deposits/manifest.json')), /not a keyed manifest/);
});

test('keyed: a provider for another host or another key place never gets the key (CL4-2)', async () => {
	const m = readKeyedManifest(MANIFEST);
	const cases = [
		{ baseUrl: 'https://other.example.org/v1', auth: { in: 'query', name: 'api_key' }, re: /stored for https:\/\/other/ },
		{ baseUrl: `${ORIGIN}/v1`, auth: { in: 'query', name: 'key' }, re: /query parameter/ },
		{ baseUrl: `${ORIGIN}/v1`, auth: { in: 'header', name: 'x-api-key' }, re: /query parameter/ },
	];
	for (const c of cases) {
		const s = setup(c);
		const p = registry(s.dir).get('widgets');
		assert.throws(() => checkProviderFits(p, m), c.re);
		await s.vault.set('widgets', new URL(c.baseUrl).origin, KEY);
		const r = await cli(['keyed', 'observe', 'widgets', '--manifest', MANIFEST, '--repo', DEMO, '--', '--key-file', 'x.key'], s);
		assert.equal(r.code, 1);
		assert.ok(!r.out.includes(KEY) && !r.err.includes(KEY));
		assert.throws(() => readFileSync(s.record), /ENOENT/, 'the observer was never started');
	}
});

test('keyed: join shows the terms, needs --yes without a terminal, and the seat holds nothing about the key (CL4-6, CL4-9)', async () => {
	const s = setup();
	await s.vault.set('widgets', ORIGIN, KEY);
	const args = ['keyed', 'join', 'widgets', '--manifest', MANIFEST, '--repo', DEMO, '--observer-key', join(s.dir, 'obs.key'), '--operator', 'acme.data'];
	const refused = await cli(args, s);
	assert.equal(refused.code, 1);
	assert.match(refused.err, /Synthetic demo data, CC0/);
	assert.match(refused.err, /--yes/);
	const ok = await cli([...args, '--yes'], s);
	assert.equal(ok.code, 0, ok.err);
	const seat = JSON.parse(ok.out.split('\n')[0]);
	assert.deepEqual(Object.keys(seat).sort(), ['joined_at', 'observer', 'operator', 'sig', 'source', 'terms_hash']);
	assert.equal(seat.operator, 'acme.data');
	assert.equal(seat.terms_hash, readKeyedManifest(MANIFEST).termsHash);
	const stored = join(s.dir, 'keyed', 'demo.keyed.widget_index.seat.json');
	assert.equal(statSync(stored).mode & 0o777, 0o600);
	assert.ok(!allText(s.dir).includes(KEY), 'no file of ours holds the key');
	assert.ok(!ok.out.includes(KEY) && !ok.err.includes(KEY));
	const listed = await cli(['keyed', 'seats'], s);
	assert.equal(JSON.parse(listed.out).source, 'demo.keyed.widget_index');
	// No key stored: no seat (it could never observe).
	const t = setup();
	const none = await cli([...args, '--yes'], t);
	assert.equal(none.code, 1);
	assert.match(none.err, /no key stored/);
});

test('keyed: observe hands the key on stdin only and filters every line the observer prints (CL4-1)', async () => {
	const s = setup();
	await s.vault.set('widgets', ORIGIN, KEY);
	const r = await cli(['keyed', 'observe', 'widgets', '--manifest', MANIFEST, '--repo', DEMO, '--', '--key-file', 'obs.key', '--state', join(s.dir, 'st')], s);
	assert.equal(r.code, 0, r.err);
	const rec = JSON.parse(readFileSync(s.record, 'utf8'));
	assert.equal(rec.stdin, KEY + '\n');
	assert.ok(!rec.argv.join(' ').includes(KEY), 'never on the command line');
	assert.ok(!JSON.stringify(rec.env).includes(KEY), 'never in the environment');
	assert.ok(rec.argv.includes('--api-key-stdin'));
	for (const [i, line] of [r.out, r.err].entries()) {
		assert.ok(!line.includes(KEY), `stream ${i}`);
		assert.ok(!line.includes(encodeURIComponent(KEY)), `stream ${i} (encoded)`);
		assert.ok(!line.includes(KEY.slice(0, 12)), `stream ${i} (a piece)`);
	}
	assert.match(r.out, /\[key removed\]/);
	assert.match(r.err, /observer: done/);
	// The holder cannot pass a key flag of its own.
	assert.throws(() => observe({ p: registry(s.dir).get('widgets'), m: readKeyedManifest(MANIFEST), key: KEY, repo: DEMO, bin: s.bin, args: ['--api-key-stdin'] }), /from your vault only/);
});

test('keyed MCP: join checks first, signs only on confirm; observe runs in the background; no result holds the key', async () => {
	const s = setup();
	await s.vault.set('widgets', ORIGIN, KEY);
	const state = createState({ env: s.env, vault: s.vault, dir: s.dir, log: () => {} });
	const call = async (name, args) => {
		const r = await runTool(state, name, args);
		assert.ok(!r.content[0].text.includes(KEY), name);
		return JSON.parse(r.content[0].text);
	};
	const first = await call('contrib_keyed_join', { provider: 'widgets', manifest: MANIFEST, signerFile: join(s.dir, 'obs.key') });
	assert.equal(first.ok, true);
	assert.equal(first.joined, false);
	assert.match(first.terms.quote, /CC0/);
	assert.throws(() => readFileSync(join(s.dir, 'keyed', 'demo.keyed.widget_index.seat.json')), /ENOENT/, 'nothing stored before confirm');
	const bad = await call('contrib_keyed_join', { provider: 'widgets', manifest: MANIFEST, signerFile: '/etc/passwd' });
	assert.equal(bad.ok, false);
	// Review CL4-R1-9: the assistant cannot point the terms check at another table.
	const forged = await call('contrib_keyed_join', { provider: 'widgets', manifest: MANIFEST, signerFile: join(s.dir, 'obs.key'), repo: s.dir });
	assert.equal(forged.ok, false);
	assert.match(forged.error, /unknown argument repo/);
	const done = await call('contrib_keyed_join', { checkId: first.checkId, confirm: true });
	assert.equal(done.joined, true, JSON.stringify(done));
	const again = await call('contrib_keyed_join', { checkId: first.checkId, confirm: true });
	assert.equal(again.ok, false);
	const started = await call('contrib_keyed_observe', { provider: 'widgets', manifest: MANIFEST, signerFile: join(s.dir, 'obs.key'), announcement: join(s.dir, 'round.json') });
	assert.equal(started.started, true);
	let st;
	// A condition with a cap far above any run (CLAUDE.md: no fixed sleeps before a check).
	for (let i = 0; i < 2000; i++) {
		st = await call('contrib_keyed_status', { runId: started.runId });
		if (st.runs[0].state === 'ended') break;
		await new Promise((r) => setTimeout(r, 25));
	}
	assert.equal(st.runs[0].state, 'ended');
	assert.equal(st.runs[0].exitCode, 0);
	assert.ok(st.runs[0].lastLines.some((l) => l.includes('[key removed]')));
	assert.equal(st.seats.length, 1);
	const rec = JSON.parse(readFileSync(s.record, 'utf8'));
	assert.equal(rec.stdin, KEY + '\n');
	assert.ok(!rec.argv.join(' ').includes(KEY));
});
