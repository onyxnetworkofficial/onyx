// Onyx API client for agents and scripts (NS8, ADR-0047). Every operation comes from the
// OpenAPI file shipped with this toolkit (apps/agent/openapi.json, a byte-for-byte copy of
// docs/api/openapi.json kept by scripts/sync.mjs), never from the server, so a server cannot
// change what a call means. No dependencies; Node 20 or later.
//
// The client only fetches. It checks nothing about the data: pass an answer to checkAnswer
// (check.mjs), which runs the network's own Rust check.
import { readFileSync } from 'node:fs';

const SPEC_URL = new URL('../openapi.json', import.meta.url);
/** Largest answer read (an export of a large shard fits). */
export const MAX_BODY = 64 * 1024 * 1024;
const TIMEOUT_MS = 30_000;
/** Headers a caller may set: payment, prepaid voucher, contributor pass, cache check. */
const HEADER_PARAMS = new Set(['PAYMENT-SIGNATURE', 'ONYX-PREPAID-VOUCHER', 'ONYX-CONTRIBUTOR', 'If-None-Match']);

export function loadSpec(path = SPEC_URL) {
	return JSON.parse(readFileSync(path, 'utf8'));
}

/** HTTPS, or plain http only to this machine (as the server's own rule for its facilitator). */
export function checkBaseUrl(base) {
	let u;
	try {
		u = new URL(base);
	} catch {
		throw new Error(`base URL is not a URL: ${base}`);
	}
	const loopback = ['localhost', '127.0.0.1', '[::1]'].includes(u.hostname.toLowerCase());
	if (u.username || u.password || u.search || u.hash) throw new Error('base URL: no user, query or fragment');
	if (u.protocol === 'https:' || (u.protocol === 'http:' && loopback)) return u.origin;
	throw new Error(`base URL must be https (or http to this machine): ${base}`);
}

function deref(spec, p) {
	if (!p.$ref) return p;
	const parts = p.$ref.replace(/^#\//, '').split('/');
	return parts.reduce((o, k) => o[k], spec);
}

/** Every operation: id, method, path, summary, description, parameters, body, paid. */
export function operations(spec) {
	const out = [];
	for (const [path, item] of Object.entries(spec.paths)) {
		for (const method of ['get', 'post']) {
			const op = item[method];
			if (!op) continue;
			out.push({
				id: op.operationId,
				method: method.toUpperCase(),
				path,
				summary: op.summary ?? '',
				description: op.description ?? '',
				parameters: (op.parameters ?? []).map((p) => deref(spec, p)).map((p) => ({ ...p, schema: p.schema ? deref(spec, p.schema) : {} })),
				body: op.requestBody ? deref(spec, op.requestBody.content['application/json'].schema) : null,
				paid: op['x-onyx-paid'] === true,
			});
		}
	}
	return out;
}

function checkValue(p, v) {
	const s = p.schema;
	if (s.type === 'integer') {
		if (!Number.isSafeInteger(v) || v < (s.minimum ?? -Infinity)) throw new Error(`${p.name}: a whole number of at least ${s.minimum ?? 0} is required`);
		return String(v);
	}
	const t = typeof v === 'number' && Number.isSafeInteger(v) ? String(v) : v;
	if (typeof t !== 'string') throw new Error(`${p.name}: a string is required`);
	if (s.pattern && !new RegExp(s.pattern).test(t)) throw new Error(`${p.name}: does not match ${s.pattern}`);
	if (s.enum && !s.enum.includes(t)) throw new Error(`${p.name}: one of ${s.enum.join(', ')}`);
	return t;
}

/** The request for operation `op` with `args` (parameter names as in the OpenAPI file). */
export function buildRequest(base, op, args = {}) {
	const known = new Set(op.parameters.map((p) => p.name));
	if (op.body) known.add('body');
	for (const k of Object.keys(args)) if (!known.has(k)) throw new Error(`${op.id}: unknown argument ${k}`);
	let path = op.path;
	const query = new URLSearchParams();
	const headers = { accept: 'application/json' };
	for (const p of op.parameters) {
		const v = args[p.name];
		if (v === undefined || v === null) {
			if (p.required) throw new Error(`${op.id}: ${p.name} is required`);
			continue;
		}
		const s = checkValue(p, v);
		if (p.in === 'path') {
			// `.` and `..` survive encoding and a URL parser drops them, moving the request to
			// another route (review NS8-R1 finding 2).
			if (/^\.+$/.test(s)) throw new Error(`${p.name}: a path value cannot be only dots`);
			path = path.replace(`{${p.name}}`, encodeURIComponent(s));
		}
		else if (p.in === 'query') query.set(p.name, s);
		else if (p.in === 'header') {
			if (!HEADER_PARAMS.has(p.name)) throw new Error(`${op.id}: header ${p.name} is not settable`);
			headers[p.name] = s;
		}
	}
	const q = query.toString();
	const init = { method: op.method, headers };
	if (op.body) {
		if (args.body === undefined) throw new Error(`${op.id}: body is required`);
		headers['content-type'] = 'application/json';
		init.body = JSON.stringify(args.body);
	}
	return { url: `${base}${path}${q ? `?${q}` : ''}`, init };
}

async function readCapped(res) {
	const len = Number(res.headers.get('content-length') ?? 0);
	if (len > MAX_BODY) throw new Error(`answer larger than ${MAX_BODY} bytes`);
	const chunks = [];
	let n = 0;
	for await (const c of res.body ?? []) {
		n += c.length;
		if (n > MAX_BODY) throw new Error(`answer larger than ${MAX_BODY} bytes`);
		chunks.push(c);
	}
	return Buffer.concat(chunks).toString('utf8');
}

function decodeOffer(h) {
	if (!h) return null;
	try {
		return JSON.parse(Buffer.from(h, 'base64').toString('utf8'));
	} catch {
		return { undecoded: h };
	}
}

/**
 * A client for one Onyx node. `call(id, args)` returns { status, body, headers, paymentRequired }.
 * A 402 is a normal result: `paymentRequired` holds the decoded x402 offer; pay it with your own
 * wallet and call again with `PAYMENT-SIGNATURE` (or a prepaid `ONYX-PREPAID-VOUCHER`).
 */
export function createClient({ baseUrl, spec = loadSpec(), fetch: f = globalThis.fetch, userAgent = 'onyx-agent/0.1' } = {}) {
	const base = checkBaseUrl(baseUrl);
	const ops = new Map(operations(spec).map((o) => [o.id, o]));
	return {
		/** GET a JSON file from the same origin (the website's catalog files), size-capped. */
		async getJson(path, max = 1024 * 1024) {
			if (!/^\/[a-z0-9/_.-]+\.json$/.test(path)) throw new Error(`not a catalog path: ${path}`);
			const res = await f(`${base}${path}`, { headers: { accept: 'application/json', 'user-agent': userAgent }, redirect: 'error', signal: AbortSignal.timeout(TIMEOUT_MS) });
			if (res.status !== 200) {
				await res.body?.cancel();
				return null;
			}
			const len = Number(res.headers.get('content-length') ?? 0);
			if (len > max) throw new Error(`${path} larger than ${max} bytes`);
			const chunks = [];
			let n = 0;
			for await (const c of res.body ?? []) {
				n += c.length;
				if (n > max) throw new Error(`${path} larger than ${max} bytes`);
				chunks.push(c);
			}
			return JSON.parse(Buffer.concat(chunks).toString('utf8'));
		},
		base,
		operations: () => [...ops.values()],
		async call(id, args = {}) {
			const op = ops.get(id);
			if (!op) throw new Error(`unknown operation ${id}`);
			let { url, init } = buildRequest(base, op, args);
			init.headers['user-agent'] = userAgent;
			let res;
			// The API answers 308 to a non-canonical spelling: follow it on the same server only.
			for (let hop = 0; ; hop++) {
				res = await f(url, { ...init, redirect: 'manual', signal: AbortSignal.timeout(TIMEOUT_MS) });
				const loc = res.headers.get('location');
				if (res.status !== 308 || !loc || hop >= 2 || op.method !== 'GET') break;
				const next = new URL(loc, url);
				if (next.origin !== base) throw new Error(`redirect off the server refused: ${next.origin}`);
				await res.body?.cancel();
				url = next.href;
			}
			const text = await readCapped(res);
			let body;
			try {
				body = text ? JSON.parse(text) : null;
			} catch {
				body = { notJson: text.slice(0, 500) };
			}
			const keep = ['etag', 'cache-control', 'payment-response', 'onyx-prepaid-response', 'retry-after'];
			const headers = Object.fromEntries(keep.filter((k) => res.headers.has(k)).map((k) => [k, res.headers.get(k)]));
			return { status: res.status, body, headers, paymentRequired: res.status === 402 ? decodeOffer(res.headers.get('payment-required')) : null };
		},
	};
}
