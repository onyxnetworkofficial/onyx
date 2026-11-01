// `onyx-contrib ui` (CL10, ADR-0061): the contributor app as a small local web server for one
// person on this computer. The desktop app (apps/desktop) starts it and opens its window on it;
// from a terminal it opens in the browser.
//
// Who may call it (THREATS CL10):
//   - it listens on 127.0.0.1 only, on a random port;
//   - every /api request carries the per-launch secret token (x-onyx-token), 32 random bytes,
//     compared in constant time; the page gets it from the URL fragment (#t=…), which the
//     browser never sends to any server, and drops it from the address bar; a browser started
//     from a terminal opens a private file that forwards there (its command line holds no token);
//   - Host must be 127.0.0.1:<port> or localhost:<port> (DNS rebinding);
//   - Origin, when sent, must be this server's own; a request that changes anything must send it;
//     Sec-Fetch-Site, when sent, must be same-origin or none; no CORS answer is ever given;
//   - writes are JSON only (a cross-site form cannot send application/json without a preflight).
// Keys: a key typed into the page is sent once, stored in the vault and never shown again. Every
// response body and log line passes the key filter for every key and passphrase this server has
// seen (the CL2 filter: any 10-character piece, in any encoding), and a request whose input holds
// such a piece is refused and locks the session (review CL10-R1-1: no key-guessing oracle).
import { createServer } from 'node:http';
import { randomBytes, timingSafeEqual } from 'node:crypto';
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { MAX_PROPOSALS } from '../../agent/src/connect.mjs';
import { claudeConfigPath, claudeStatus, connectClaude, mcpEntry } from './claude.mjs';
import { doctor } from './doctor.mjs';
import { history } from './history.mjs';
import { createState, LOCKED, runTool, VERSION } from './mcp.mjs';
import { configDir, shown } from './providers.mjs';
import { holdsKey, KEY_WINDOW, redact } from './rules.mjs';
import { checkKey, fileVault, vaultFromEnv } from './vault.mjs';

/** Largest request body (a provider form or a key). */
export const MAX_BODY = 64 * 1024;
/** A launch file (see launchFile) is deleted after this long. */
export const LAUNCH_TTL_MS = 60_000;

const STATIC = {
	'/': ['index.html', 'text/html; charset=utf-8'],
	'/app.js': ['app.js', 'text/javascript; charset=utf-8'],
	'/app.css': ['app.css', 'text/css; charset=utf-8'],
	'/icon.svg': ['icon.svg', 'image/svg+xml'],
};
const CSP = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
const HEADERS = { 'cache-control': 'no-store', 'x-content-type-options': 'nosniff', 'x-frame-options': 'DENY', 'referrer-policy': 'no-referrer', 'content-security-policy': CSP, 'cross-origin-opener-policy': 'same-origin', 'cross-origin-resource-policy': 'same-origin' };

/** The app's static files (apps/contrib/ui), read once. */
function loadStatic() {
	const out = new Map();
	for (const [route, [file, type]] of Object.entries(STATIC)) out.set(route, { body: readFileSync(new URL(`../ui/${file}`, import.meta.url)), type });
	return out;
}

const eq = (a, b) => {
	const x = Buffer.from(String(a));
	const y = Buffer.from(String(b));
	return x.length === y.length && timingSafeEqual(x, y);
};

class Refused extends Error {
	constructor(status, message) {
		super(message);
		this.status = status;
	}
}

/**
 * Start the app server. Options: env, fetch, vault, dir, log, platform, execPath, sea (running as
 * the single-file program), cliPath (cli.mjs, when run with Node), port (0: random), run (runs the
 * doctor's keychain probe; tests pass a stand-in).
 * Returns { url, port, token, launchFile(), close(), state }.
 */
export async function startUi({ env = process.env, fetch = globalThis.fetch, vault, dir, log = (l) => process.stderr.write(l + '\n'), platform = process.platform, execPath = process.execPath, sea = false, cliPath, port = 0, run } = {}) {
	dir ??= configDir(env, platform);
	if (!vault) {
		try {
			vault = vaultFromEnv(env, { dir, platform });
		} catch (e) {
			// The app still opens, says why in its health check, and offers the encrypted file.
			const why = String(e?.message ?? e);
			const no = async () => {
				throw new Error(why);
			};
			vault = { kind: 'none (see the health check)', get: no, set: no, remove: no };
		}
	}
	const state = createState({ env, fetch, vault, dir, log });
	const token = randomBytes(32).toString('hex');
	const launchDirs = [];
	const files = loadStatic();
	// Keys typed into the page and the file vault's passphrase, filtered from every answer and log
	// line along with every key read from the vault (state.keys). Kept apart from state.keys, which
	// the outgoing guard also checks: a refused short key must not block every later contribution.
	const typed = new Set();
	const secrets = { [Symbol.iterator]: () => [...state.keys, ...typed][Symbol.iterator]() };
	state.secrets = secrets;
	const remember = (s) => {
		if (typeof s === 'string' && s.length >= KEY_WINDOW) typed.add(s);
	};
	const say = (l) => state.log(redact(l, secrets));
	let realPort = 0;

	const send = (res, status, body, type = 'application/json; charset=utf-8', extra = {}) => {
		const text = type.startsWith('application/json') ? redact(JSON.stringify(body), secrets) : body;
		res.writeHead(status, { ...HEADERS, 'content-type': type, ...extra });
		res.end(text);
	};

	/** Host, Origin, Sec-Fetch-Site: only this page, on this computer, may call. */
	function gate(req) {
		const host = req.headers.host ?? '';
		if (host !== `127.0.0.1:${realPort}` && host !== `localhost:${realPort}`) throw new Refused(403, 'wrong host');
		const site = req.headers['sec-fetch-site'];
		if (site !== undefined && site !== 'same-origin' && site !== 'none') throw new Refused(403, 'cross-site request refused');
		const origin = req.headers.origin;
		if (origin !== undefined && origin !== `http://${host}`) throw new Refused(403, 'other origin refused');
		if (req.method !== 'GET' && req.method !== 'HEAD' && origin === undefined) throw new Refused(403, 'a write needs its Origin');
	}

	async function readBody(req) {
		if (!/^application\/json(\s*;|$)/i.test(req.headers['content-type'] ?? '')) throw new Refused(415, 'send JSON (application/json)');
		const chunks = [];
		let n = 0;
		for await (const c of req) {
			n += c.length;
			if (n > MAX_BODY) throw new Refused(413, `more than ${MAX_BODY} bytes`);
			chunks.push(c);
		}
		try {
			const v = JSON.parse(Buffer.concat(chunks).toString('utf8') || '{}');
			if (!v || typeof v !== 'object' || Array.isArray(v)) throw new Error();
			return v;
		} catch {
			throw new Refused(400, 'the request is not a JSON object');
		}
	}

	/** A tool of the MCP server, its result parsed back (already filtered for keys). */
	async function tool(name, args) {
		const r = await runTool(state, name, args);
		const v = JSON.parse(r.content[0].text);
		delete v.note;
		delete v.next;
		return v;
	}

	const claudePath = () => claudeConfigPath(env, platform);
	/** The command Claude Desktop will run. An AppImage runs from a new folder each launch, so the program is copied to a fixed place first. */
	function entryFor(write) {
		let exe = execPath;
		if (sea && env.APPIMAGE) {
			exe = join(state.dir, 'bin', 'onyx-contrib');
			if (write) {
				mkdirSync(join(state.dir, 'bin'), { recursive: true, mode: 0o700 });
				copyFileSync(execPath, exe);
				chmodSync(exe, 0o755);
			}
		}
		return mcpEntry({ execPath: exe, sea, cliPath, env });
	}

	async function providersView() {
		const out = [];
		for (const p of state.registry.list()) {
			const k = await state.vault.get(p.name, new URL(p.baseUrl).origin).catch(() => null);
			if (k) state.keys.add(k);
			out.push(shown(p, !!k));
		}
		return out;
	}

	async function api(req, url) {
		const tok = req.headers['x-onyx-token'];
		if (typeof tok !== 'string' || !eq(tok, token)) throw new Refused(401, 'this app was opened without its launch link: open it again from the Onyx app or `onyx-contrib ui`');
		const r = `${req.method} ${url.pathname}`;
		const q = (k) => url.searchParams.get(k) ?? undefined;
		// Read a write's body once, then the probe check (review CL10-R1-1): input that holds a piece
		// of a key this session knows is refused and locks the session (see runTool), so the key
		// filter's marks can never answer "is this a piece of the key?". Only the key field of
		// POST /api/key and the passphrase are exempt: they are the secrets themselves.
		const posted = req.method === 'POST' ? await readBody(req) : {};
		const probe = r === 'POST /api/key' ? { provider: posted.provider } : r === 'POST /api/vault/file' ? {} : posted;
		if (r !== 'GET /api/state' && r !== 'GET /api/doctor') {
			if (state.locked) return { ok: false, locked: true, error: LOCKED };
			if (holdsKey({ path: url.pathname, query: [...url.searchParams.entries()], body: probe }, secrets)) {
				state.locked = true;
				say('onyx-contrib ui: a request held a piece of a key; session locked');
				return { ok: false, locked: true, error: LOCKED };
			}
		}
		const body = async () => posted;
		switch (r) {
			case 'GET /api/state':
				return { ok: true, locked: !!state.locked, version: VERSION, network: 'test network (Base Sepolia): no real money', onyx: state.client.base, vault: state.vault.kind, providers: await providersView(), claude: claudeStatus(claudePath(), entryFor(false)), sent: state.session.sent ?? 0, perSession: MAX_PROPOSALS };
			case 'GET /api/doctor': {
				const d = await doctor({ vault: state.vault, env, platform, client: state.client, dir: state.dir, ...(run ? { run } : {}) });
				return { ...d, healthy: d.ok, ok: true };
			}
			case 'POST /api/providers': {
				const a = await body();
				const rec = { name: a.name, baseUrl: a.baseUrl, auth: a.authIn === 'query' ? { in: 'query', name: a.authName } : { in: 'header', name: a.authName, ...(a.bearer === true ? { prefix: 'Bearer ' } : {}) } };
				for (const k of ['openapi', 'docsUrl', 'sourceName']) if (typeof a[k] === 'string' && a[k].trim()) rec[k] = a[k].trim();
				const p = state.registry.add(rec, { addedBy: 'you' });
				return { ok: true, provider: shown(p, false) };
			}
			case 'POST /api/providers/remove': {
				const a = await body();
				const p = state.provider(a.name);
				await state.vault.remove(p.name, new URL(p.baseUrl).origin);
				state.registry.remove(p.name);
				return { ok: true, removed: p.name };
			}
			case 'POST /api/key': {
				const a = await body();
				const key = a.key;
				remember(key);
				const p = state.provider(a.provider);
				const why = checkKey(key);
				if (why) return { ok: false, error: why };
				const origin = new URL(p.baseUrl).origin;
				await state.vault.set(p.name, origin, key);
				if ((await state.vault.get(p.name, origin)) !== key) return { ok: false, error: 'the keychain did not keep the key (it reads back differently); nothing usable was stored' };
				say(`onyx-contrib ui: key stored for ${p.name}`);
				return { ok: true, stored: true, provider: p.name, sentTo: `${origin}, ${p.auth.in === 'header' ? `in the ${p.auth.name} header` : `as the ${p.auth.name} query parameter`}`, vault: state.vault.kind };
			}
			case 'POST /api/key/remove': {
				const a = await body();
				const p = state.provider(a.provider);
				await state.vault.remove(p.name, new URL(p.baseUrl).origin);
				return { ok: true, removed: true, provider: p.name };
			}
			case 'GET /api/endpoints':
				return tool('contrib_endpoints', { provider: q('provider'), ...(q('search') ? { search: q('search') } : {}), ...(q('offset') ? { offset: Number(q('offset')) } : {}) });
			case 'POST /api/preview': {
				const a = await body();
				return tool('contrib_preview', pick(a, ['provider', 'path', 'params', 'pointer']));
			}
			case 'POST /api/contribute': {
				const a = await body();
				if (a.confirm === true) return tool('contrib_contribute', { checkId: a.checkId, confirm: true });
				return tool('contrib_contribute', pick(a, ['provider', 'path', 'params', 'pointer', 'title', 'description', 'rights', 'licenseId', 'namesPerson', 'changes', 'unit', 'seriesId', 'sourceUrl', 'sourceName']));
			}
			case 'GET /api/contributions':
				return { ok: true, contributions: history(state.dir).list() };
			case 'GET /api/contributions/status':
				return tool('contrib_status', { id: q('id') });
			case 'POST /api/claude/connect': {
				await body();
				const done = connectClaude({ path: claudePath(), entry: entryFor(true) });
				const fileNote = /encrypted file/.test(state.vault.kind) ? ' Your keys are in an encrypted file: Claude Desktop needs ONYX_CONTRIB_VAULT=file and your passphrase in its settings to read them, and this app does not write a passphrase there.' : '';
				return { ok: true, ...done, next: `Quit Claude Desktop completely and start it again; Onyx's tools then show in its tools menu.${fileNote}` };
			}
			case 'POST /api/vault/file': {
				const a = await body();
				remember(a.passphrase);
				const v = fileVault({ dir: state.dir, passphrase: a.passphrase });
				await v.get('doctor', 'https://doctor.invalid');
				state.vault = v;
				return { ok: true, vault: v.kind };
			}
			default:
				throw new Refused(404, 'no such route');
		}
	}

	const server = createServer(async (req, res) => {
		const url = new URL(req.url ?? '/', 'http://127.0.0.1');
		let status = 200;
		try {
			gate(req);
			if (req.method === 'OPTIONS') throw new Refused(405, 'no cross-origin requests');
			if (url.pathname.startsWith('/api/')) {
				const v = await api(req, url);
				status = v.ok === false ? 422 : 200;
				return send(res, status, v);
			}
			const f = files.get(url.pathname);
			if (!f || (req.method !== 'GET' && req.method !== 'HEAD')) throw new Refused(404, 'not found');
			res.writeHead(200, { ...HEADERS, 'content-type': f.type });
			return res.end(req.method === 'HEAD' ? undefined : f.body);
		} catch (e) {
			status = e instanceof Refused ? e.status : 400;
			return send(res, status, { ok: false, error: String(e?.message ?? e) });
		} finally {
			say(`onyx-contrib ui: ${req.method} ${url.pathname} ${status}`);
		}
	});
	server.headersTimeout = 20_000;
	server.requestTimeout = 60_000;
	await new Promise((ok, fail) => {
		server.once('error', fail);
		server.listen(port, '127.0.0.1', ok);
	});
	realPort = server.address().port;
	const url = `http://127.0.0.1:${realPort}/#t=${token}`;
	return {
		url,
		port: realPort,
		token,
		state,
		/**
		 * A page for the browser to open (review CL10-R1-2): a 0600 file in a fresh 0700 folder that
		 * forwards to the tokened URL, so a browser's command line (readable by other users) holds a
		 * file name, never the token. Deleted after a minute and when the server stops.
		 */
		launchFile() {
			const d = mkdtempSync(join(tmpdir(), 'onyx-app-'));
			const f = join(d, 'open-onyx.html');
			const esc = url.replace(/&/g, '&amp;').replace(/"/g, '&quot;');
			writeFileSync(f, `<!doctype html><meta charset="utf-8"><meta name="referrer" content="no-referrer"><meta http-equiv="refresh" content="0;url=${esc}"><title>Onyx</title><p><a href="${esc}">Open the Onyx app</a></p>\n`, { mode: 0o600, flag: 'wx' });
			launchDirs.push(d);
			setTimeout(() => rmSync(d, { recursive: true, force: true }), LAUNCH_TTL_MS).unref();
			return f;
		},
		close: () =>
			new Promise((r) => {
				for (const d of launchDirs) rmSync(d, { recursive: true, force: true });
				server.close(() => r());
			}),
	};
}

function pick(a, keys) {
	const out = {};
	for (const k of keys) if (a[k] !== undefined && a[k] !== '') out[k] = a[k];
	return out;
}
