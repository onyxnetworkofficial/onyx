#!/usr/bin/env node
// Smoke test of an onyx-contrib program (CL10): the single-file build from scripts/desktop-build.sh,
// or the source (`node apps/contrib/src/cli.mjs`). Runs it with no Node on PATH, in a fresh settings
// folder with the encrypted-file vault (a fake passphrase), and checks: `version`; `doctor`; an MCP
// session (initialize, tools/list) on stdio; `ui --desktop` prints its tokened URL, serves the page
// and its files byte for byte as in apps/contrib/ui, answers /api/state with the token and refuses
// it without, and stops when its input closes. No outside network: Onyx's address is a closed
// loopback port.
//
//   node scripts/desktop-smoke.mjs <program> [args before the command, e.g. apps/contrib/src/cli.mjs]
import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { request } from 'node:http';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const [prog, ...pre] = process.argv.slice(2);
if (!prog) {
	process.stderr.write('usage: node scripts/desktop-smoke.mjs <program> [leading args]\n');
	process.exit(2);
}
const home = mkdtempSync(join(process.env.ONYX_SMOKE_TMP || join(homedir(), '.cache'), 'onyx-smoke-'));
const env = { ONYX_CONTRIB_HOME: home, ONYX_CONTRIB_VAULT: 'file', ONYX_CONTRIB_VAULT_PASSPHRASE: 'FAKE-smoke-pass-4417', ONYX_API_URL: 'http://127.0.0.1:9', ONYX_CLAUDE_CONFIG: join(home, 'claude.json'), PATH: process.platform === 'win32' ? process.env.SystemRoot + '\\System32' : '/nonexistent', ...(process.platform === 'win32' ? { SystemRoot: process.env.SystemRoot, APPDATA: home } : {}) };
// ONYX_SMOKE_PASS=A,B passes those variables through (to run the Windows program under Wine here:
// ONYX_SMOKE_PASS=HOME,WINEPREFIX,WINEDEBUG node scripts/desktop-smoke.mjs wine onyx-contrib-windows-x64.exe).
for (const k of (process.env.ONYX_SMOKE_PASS || '').split(',').filter(Boolean)) if (process.env[k] !== undefined) env[k] = process.env[k];
let failed = 0;
const ok = (cond, what) => {
	process.stdout.write(`${cond ? 'ok  ' : 'FAIL'} ${what}\n`);
	if (!cond) failed++;
};

function run(args, input = '') {
	return new Promise((done) => {
		const c = spawn(prog, [...pre, ...args], { env, stdio: ['pipe', 'pipe', 'pipe'] });
		let out = '';
		let err = '';
		c.stdout.on('data', (d) => (out += d));
		c.stderr.on('data', (d) => (err += d));
		c.on('error', (e) => done({ code: -1, out, err: String(e) }));
		c.on('close', (code) => done({ code, out, err }));
		c.stdin.end(input);
	});
}

function get(port, path, headers = {}) {
	return new Promise((done, fail) => {
		const r = request({ host: '127.0.0.1', port, path, headers: { host: `127.0.0.1:${port}`, ...headers } }, (res) => {
			const ch = [];
			res.on('data', (c) => ch.push(c));
			res.on('end', () => done({ status: res.statusCode, body: Buffer.concat(ch) }));
		});
		r.on('error', fail);
		r.end();
	});
}

try {
	const v = await run(['version']);
	ok(v.code === 0 && /^onyx-contrib \d+\.\d+\.\d+/.test(v.out), `version: ${v.out.trim() || v.err.trim()}`);
	const d = await run(['doctor']);
	ok(d.code === 0 && /encrypted file/.test(d.out), 'doctor runs and names the vault in plain words');
	const mcpIn = [
		{ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-06-18' } },
		{ jsonrpc: '2.0', id: 2, method: 'tools/list' },
	]
		.map((m) => JSON.stringify(m) + '\n')
		.join('');
	const m = await run(['mcp'], mcpIn);
	const replies = m.out.trim().split('\n').filter(Boolean).map((l) => JSON.parse(l));
	ok(replies[0]?.result?.serverInfo?.name === 'onyx-contrib' && replies[1]?.result?.tools?.length >= 6, `mcp: initialize and tools/list (${replies[1]?.result?.tools?.length ?? 0} tools)`);

	const ui = spawn(prog, [...pre, 'ui', '--desktop'], { env, stdio: ['pipe', 'pipe', 'pipe'] });
	const url = await new Promise((done) => {
		let buf = '';
		const t = setTimeout(() => done(null), 30_000);
		ui.stdout.on('data', (c) => {
			buf += c;
			const mm = /ONYX_UI_URL=(\S+)/.exec(buf);
			if (mm) {
				clearTimeout(t);
				done(mm[1]);
			}
		});
		ui.on('close', () => done(null));
	});
	ok(!!url && /^http:\/\/127\.0\.0\.1:\d+\/#t=[0-9a-f]{64}$/.test(url), 'ui --desktop prints a tokened 127.0.0.1 URL');
	if (url) {
		const u = new URL(url);
		const port = Number(u.port);
		const token = u.hash.slice(3);
		for (const [path, file] of [['/', 'index.html'], ['/app.js', 'app.js'], ['/app.css', 'app.css'], ['/icon.svg', 'icon.svg']]) {
			const r = await get(port, path);
			ok(r.status === 200 && r.body.equals(readFileSync(join(ROOT, 'apps/contrib/ui', file))), `ui serves ${path} as apps/contrib/ui/${file}`);
		}
		const s = await get(port, '/api/state', { 'x-onyx-token': token });
		const st = JSON.parse(s.body.toString('utf8'));
		ok(s.status === 200 && /test network/.test(st.network) && /encrypted file/.test(st.vault), 'ui /api/state answers with the token');
		ok((await get(port, '/api/state')).status === 401, 'ui /api/state refuses without the token');
		ok((await get(port, '/', { host: 'evil.example.com' })).status === 403, 'ui refuses another Host');
	}
	const closed = new Promise((done) => {
		const t = setTimeout(() => done(null), 15_000);
		ui.on('close', (code) => {
			clearTimeout(t);
			done(code);
		});
	});
	ui.stdin.end();
	const code = await closed;
	ok(code === 0, `ui stops when its input closes (exit ${code})`);
	if (code === null) ui.kill();
} finally {
	rmSync(home, { recursive: true, force: true });
}
process.stdout.write(failed ? `desktop-smoke: ${failed} FAILED\n` : 'desktop-smoke: all passed\n');
process.exit(failed ? 1 : 0);
