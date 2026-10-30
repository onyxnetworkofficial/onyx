// The provider registry and endpoint discovery. A provider is a name, a base URL, how its key is
// sent (a header, optionally with a prefix such as "Bearer ", or a query parameter) and optionally
// an OpenAPI file (JSON; YAML is not read). The registry is a plain JSON file holding no key.
import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { clean } from '../../agent/src/connect.mjs';
import { httpsProblem, isSecretName, paramName } from './rules.mjs';

/** Largest OpenAPI file read. */
export const MAX_SPEC = 5 * 1024 * 1024;
/** Endpoints listed per page. */
export const PAGE = 40;
const NAME_RE = /^[a-z0-9][a-z0-9_-]{0,39}$/;
const HEADER_RE = /^[A-Za-z0-9-]{1,64}$/;
const QUERY_RE = /^[A-Za-z0-9_.-]{1,64}$/;
const PREFIX_RE = /^[A-Za-z][A-Za-z0-9-]{0,30} $/;
const REFUSED_HEADERS = new Set(['host', 'content-length', 'content-type', 'cookie', 'connection', 'transfer-encoding', 'accept', 'user-agent']);

/** Where onyx-contrib keeps its files (ONYX_CONTRIB_HOME, else the platform's config folder). */
export function configDir(env = process.env, platform = process.platform) {
	if (env.ONYX_CONTRIB_HOME) return env.ONYX_CONTRIB_HOME;
	if (platform === 'win32') return join(env.APPDATA || join(homedir(), 'AppData', 'Roaming'), 'onyx-contrib');
	return join(env.XDG_CONFIG_HOME || join(homedir(), '.config'), 'onyx-contrib');
}

/** A provider base URL: https with a public host name and an optional path, nothing else. */
export function parseBase(s) {
	const why = httpsProblem(s);
	if (why) throw new Error(`baseUrl: ${why}`);
	const u = new URL(s);
	if (u.search || u.hash || u.username || u.password) throw new Error('baseUrl: no query, fragment or user name');
	return { origin: u.origin, base: u.origin + u.pathname.replace(/\/+$/, '') };
}

/** A checked provider record, or an Error. Fields: name, baseUrl, auth {in, name, prefix?}, openapi?, docsUrl?, sourceName?. */
export function checkProvider(p, { allowFile = false } = {}) {
	if (!p || typeof p !== 'object') throw new Error('a provider is an object');
	const known = ['name', 'baseUrl', 'auth', 'openapi', 'docsUrl', 'sourceName'];
	for (const k of Object.keys(p)) if (!known.includes(k)) throw new Error(`unknown field ${k} (a key is never given here: run \`onyx-contrib key set <name>\` in a terminal)`);
	if (typeof p.name !== 'string' || !NAME_RE.test(p.name)) throw new Error('name: 1 to 40 characters a-z, 0-9, _ and -, starting with a letter or digit');
	const { base } = parseBase(p.baseUrl);
	const a = p.auth;
	if (!a || typeof a !== 'object') throw new Error('auth: {"in":"header","name":"x-api-key"} or {"in":"query","name":"api_key"}');
	for (const k of Object.keys(a)) if (!['in', 'name', 'prefix'].includes(k)) throw new Error(`auth: unknown field ${k}`);
	const auth = { in: a.in, name: a.name };
	if (a.in === 'header') {
		if (typeof a.name !== 'string' || !HEADER_RE.test(a.name) || REFUSED_HEADERS.has(a.name.toLowerCase())) throw new Error('auth.name: a header name such as x-api-key or Authorization');
		if (a.prefix !== undefined) {
			if (typeof a.prefix !== 'string' || !PREFIX_RE.test(a.prefix)) throw new Error('auth.prefix: one word and a space, such as "Bearer "');
			auth.prefix = a.prefix;
		}
	} else if (a.in === 'query') {
		if (typeof a.name !== 'string' || !QUERY_RE.test(a.name)) throw new Error('auth.name: a query parameter name such as api_key');
		if (a.prefix !== undefined) throw new Error('auth.prefix goes with a header only');
	} else throw new Error('auth.in: header or query');
	const out = { name: p.name, baseUrl: base, auth };
	if (p.openapi !== undefined) {
		if (typeof p.openapi !== 'string') throw new Error('openapi: an https URL');
		if (p.openapi.startsWith('https://')) {
			const why = httpsProblem(p.openapi);
			if (why) throw new Error(`openapi: ${why}`);
		} else if (!allowFile) throw new Error('openapi: an https URL (a local file is added in the terminal: onyx-contrib provider add … --openapi ./file.json)');
		else if (!existsSync(p.openapi)) throw new Error(`openapi: no file ${p.openapi}`);
		out.openapi = p.openapi;
	}
	if (p.docsUrl !== undefined) {
		const why = httpsProblem(p.docsUrl);
		if (why) throw new Error(`docsUrl: ${why}`);
		out.docsUrl = p.docsUrl;
	}
	if (p.sourceName !== undefined) {
		if (typeof p.sourceName !== 'string' || !p.sourceName.trim() || Buffer.byteLength(p.sourceName) > 120 || /[\u0000-\u001f\u007f-\u009f]/.test(p.sourceName)) throw new Error('sourceName: 1 to 120 bytes, one line');
		out.sourceName = p.sourceName;
	}
	return out;
}

/** The registry file (<config>/providers.json): `{ version: 1, providers: { name: record } }`. */
export function registry(dir) {
	const path = join(dir, 'providers.json');
	const load = () => {
		if (!existsSync(path)) return { version: 1, providers: {} };
		const v = JSON.parse(readFileSync(path, 'utf8'));
		if (v?.version !== 1 || typeof v.providers !== 'object') throw new Error(`${path} is not an onyx-contrib registry`);
		return v;
	};
	const save = (v) => {
		mkdirSync(dir, { recursive: true, mode: 0o700 });
		const tmp = `${path}.${process.pid}.tmp`;
		writeFileSync(tmp, JSON.stringify(v, null, 1) + '\n', { mode: 0o600 });
		renameSync(tmp, path);
	};
	return {
		path,
		list: () => Object.values(load().providers),
		get: (name) => (Object.prototype.hasOwnProperty.call(load().providers, name) ? load().providers[name] : null),
		/** Add a provider. An existing name is replaced only with `replace` (the terminal), never by the assistant. */
		add(p, { addedBy, replace = false, allowFile = false }) {
			const rec = checkProvider(p, { allowFile });
			const v = load();
			if (v.providers[rec.name] && !replace) throw new Error(`A provider named ${rec.name} exists already. Only you can change it, in a terminal: onyx-contrib provider add ${rec.name} … --replace`);
			v.providers[rec.name] = { ...rec, addedBy, addedAt: new Date().toISOString().slice(0, 10) };
			save(v);
			return v.providers[rec.name];
		},
		remove(name) {
			const v = load();
			const had = !!v.providers[name];
			delete v.providers[name];
			save(v);
			return had;
		},
	};
}

/** Read the provider's OpenAPI file: an https URL fetched without the key, or (terminal only) a local file. */
export async function loadOpenApi(p, { fetch = globalThis.fetch } = {}) {
	if (!p.openapi) throw new Error(`Provider ${p.name} has no OpenAPI file. Add one in a terminal: onyx-contrib provider add ${p.name} … --openapi <https URL or file> --replace`);
	let text;
	if (p.openapi.startsWith('https://')) {
		// No key goes with this request: an OpenAPI file is public, and its host may differ from the API's.
		// Redirects are followed by hand, at most 3, each hop checked like the first URL (https, a host name, no IP literal).
		let url = p.openapi;
		let res;
		for (let hop = 0; ; hop++) {
			res = await fetch(url, { headers: { accept: 'application/json', 'user-agent': 'onyx-contrib/0.1' }, redirect: 'manual', signal: AbortSignal.timeout(20_000) });
			const loc = res.headers.get('location');
			if (res.status < 300 || res.status >= 400 || !loc) break;
			await res.body?.cancel();
			if (hop >= 3) throw new Error('the OpenAPI file redirects too often');
			url = new URL(loc, url).href;
			const why = httpsProblem(url);
			if (why) throw new Error(`the OpenAPI file redirects to a refused URL: ${why}`);
		}
		if (res.status !== 200) throw new Error(`the OpenAPI file answered ${res.status}`);
		const chunks = [];
		let n = 0;
		for await (const c of res.body ?? []) {
			n += c.length;
			if (n > MAX_SPEC) throw new Error(`the OpenAPI file is larger than ${MAX_SPEC} bytes`);
			chunks.push(c);
		}
		text = Buffer.concat(chunks).toString('utf8');
	} else {
		text = readFileSync(p.openapi, 'utf8');
		if (text.length > MAX_SPEC) throw new Error(`the OpenAPI file is larger than ${MAX_SPEC} bytes`);
	}
	try {
		return JSON.parse(text);
	} catch {
		throw new Error('the OpenAPI file is not JSON (onyx-contrib v0 reads JSON only; convert YAML once, for example with an online converter or `yq -o=json`)');
	}
}

function deref(spec, v, depth = 0) {
	if (!v || typeof v !== 'object' || typeof v.$ref !== 'string' || depth > 5) return v;
	if (!v.$ref.startsWith('#/')) return {};
	const t = v.$ref
		.slice(2)
		.split('/')
		.reduce((o, k) => (o && typeof o === 'object' ? o[k.replace(/~1/g, '/').replace(/~0/g, '~')] : undefined), spec);
	return deref(spec, t, depth + 1) ?? {};
}

/** True when a parameter carries the key: the provider's own key parameter or header, or a secret name. */
export function isKeyParam(p, prm) {
	const n = paramName(String(prm.name ?? ''));
	if (prm.in === p.auth.in && n === p.auth.name.toLowerCase()) return true;
	return (prm.in === 'query' || prm.in === 'header') && isSecretName(n);
}

/**
 * The GET endpoints of `spec` (OpenAPI 3 or Swagger 2, JSON), with their parameters, for the user
 * to pick from. Texts are cleaned and cut (they are the provider's data, never instructions). The
 * key's own parameter is left out; `search` filters on path and summary.
 */
export function listEndpoints(p, spec, { search = '', offset = 0, limit = PAGE } = {}) {
	if (!spec || typeof spec !== 'object' || typeof spec.paths !== 'object') throw new Error('the OpenAPI file has no paths');
	const all = [];
	const q = clean(search, 100).toLowerCase();
	for (const [path, item0] of Object.entries(spec.paths)) {
		const item = deref(spec, item0);
		const op = item?.get;
		if (!op || typeof op !== 'object' || typeof path !== 'string' || !path.startsWith('/')) continue;
		const summary = clean(op.summary || op.operationId || '', 160);
		if (q && !`${path} ${summary} ${clean(op.description ?? '', 300)}`.toLowerCase().includes(q)) continue;
		const params = [];
		let keyParam = false;
		const seen = new Set();
		for (const raw of [...(op.parameters ?? []), ...(item.parameters ?? [])]) {
			const prm = deref(spec, raw);
			if (!prm || typeof prm.name !== 'string' || !['path', 'query', 'header'].includes(prm.in)) continue;
			if (seen.has(`${prm.in}:${prm.name}`)) continue;
			seen.add(`${prm.in}:${prm.name}`);
			if (isKeyParam(p, prm)) {
				keyParam = true;
				continue;
			}
			if (prm.in === 'header') continue;
			const schema = deref(spec, prm.schema) ?? {};
			const e = { name: clean(prm.name, 64), in: prm.in, required: prm.in === 'path' || prm.required === true, type: clean(schema.type ?? prm.type ?? 'string', 20) };
			if (prm.description) e.description = clean(prm.description, 160);
			const en = schema.enum ?? prm.enum;
			if (Array.isArray(en)) e.enum = en.slice(0, 12).map((x) => clean(String(x), 40));
			if (schema.example !== undefined || prm.example !== undefined) e.example = clean(String(prm.example ?? schema.example), 60);
			params.push(e);
		}
		const e = { path: clean(path, 300), summary, params };
		if (keyParam) e.keyParam = 'left out: your key is added on this computer';
		all.push(e);
	}
	all.sort((a, b) => a.path.localeCompare(b.path));
	const start = Math.max(0, Math.floor(Number(offset) || 0));
	const n = Math.min(PAGE, Math.max(1, Math.floor(Number(limit) || PAGE)));
	return { total: all.length, offset: start, endpoints: all.slice(start, start + n), more: start + n < all.length };
}

/** A provider as shown to the assistant: never a key, only whether one is stored. */
export function shown(p, hasKey) {
	const auth = p.auth.in === 'header' ? `header ${p.auth.name}${p.auth.prefix ? ` (prefix "${p.auth.prefix.trim()}")` : ''}` : `query parameter ${p.auth.name}`;
	return { name: p.name, baseUrl: p.baseUrl, keySentAs: auth, keyStored: hasKey, openapi: p.openapi ?? null, docsUrl: p.docsUrl ?? null, addedBy: p.addedBy };
}
