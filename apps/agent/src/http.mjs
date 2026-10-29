#!/usr/bin/env node
// Onyx MCP server over HTTP (agent connect, NL21): the same tools as src/mcp.mjs, reachable at
// one URL, so an AI client connects with one line. MCP "Streamable HTTP" transport (spec
// 2025-06-18): POST one JSON-RPC message, get one JSON answer; no server-sent stream (GET is 405).
// Sessions by `Mcp-Session-Id`. Read-only by default; it holds no key and pays for nothing.
//
// Settings (environment), besides src/mcp.mjs's ONYX_API_URL, ONYX_CHECK_* and ONYX_PAY_*:
//   ONYX_MCP_LISTEN          address to listen on; default 127.0.0.1:8090 (put a TLS front before it)
//   ONYX_MCP_PATH            the endpoint; default /mcp
//   ONYX_MCP_ORIGINS         browser origins allowed (comma list); a request with any other Origin
//                            is refused (DNS rebinding); requests without Origin are allowed
//   ONYX_MCP_TRUSTED_PROXY   the front's address (e.g. 127.0.0.1): only then is X-Forwarded-For read
//   ONYX_MCP_ALLOW_PROPOSE=1 offer onyx_propose_dataset (a write); off by default here
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { realpathSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { createClient, loadSpec } from './client.mjs';
import { answerStore, buildTools, handle, limiter, PROTOCOLS } from './mcp.mjs';

/** Largest request body. */
export const MAX_BODY = 256 * 1024;
/** Sessions kept at once; the one idle longest is dropped first. */
export const MAX_SESSIONS = 1000;
/** A session idle this long is forgotten (the client starts a new one). */
export const SESSION_IDLE_MS = 30 * 60 * 1000;
/** Requests per client address: a bucket of BURST refilled at RATE per minute (THREATS NL21-4). */
export const RATE = 60;
export const BURST = 30;
/** Tool calls running at once on the whole server, and per session. */
export const MAX_RUNNING = 16;
export const MAX_RUNNING_SESSION = 2;
/** Proposals per client address per hour (THREATS NL21-2). */
export const PROPOSALS_PER_HOUR = 5;

function listenOf(s = '127.0.0.1:8090') {
	const i = s.lastIndexOf(':');
	const host = s.slice(0, i).replace(/^\[|\]$/g, '');
	const port = Number(s.slice(i + 1));
	if (!host || !Number.isInteger(port) || port < 0 || port > 65535) throw new Error(`ONYX_MCP_LISTEN: host:port, not ${s}`);
	return { host, port };
}

/** The client's address: the socket's, or the last X-Forwarded-For hop when the socket is the trusted front. */
export function clientAddress(req, trusted) {
	const peer = (req.socket.remoteAddress ?? '').replace(/^::ffff:/, '');
	if (!trusted || peer !== trusted) return peer;
	const hops = String(req.headers['x-forwarded-for'] ?? '').split(',').map((s) => s.trim()).filter(Boolean);
	return hops.length ? hops[hops.length - 1] : peer;
}

/** The rate-limit key of an address: an IPv6 address counts by its /64 (one site, many addresses). */
export function rateKey(addr) {
	if (!addr.includes(':')) return addr;
	const [head, tail = ''] = addr.split('::');
	const h = head ? head.split(':') : [];
	const tl = tail ? tail.split(':') : [];
	const full = [...h, ...Array(Math.max(0, 8 - h.length - tl.length)).fill('0'), ...tl];
	return full.slice(0, 4).map((x) => x.toLowerCase().replace(/^0+(?=.)/, '')).join(':') + '::/64';
}

/** Token buckets per key; `take(key)` is false when the bucket is empty. */
export function rateLimiter({ rate = RATE, burst = BURST, now = Date.now, max = 10_000 } = {}) {
	const b = new Map();
	return {
		take(key) {
			const t = now();
			let e = b.get(key);
			if (!e) {
				e = { tokens: burst, at: t };
				if (b.size >= max) b.delete(b.keys().next().value);
			}
			e.tokens = Math.min(burst, e.tokens + ((t - e.at) / 60_000) * rate);
			e.at = t;
			b.delete(key);
			b.set(key, e);
			if (e.tokens < 1) return false;
			e.tokens -= 1;
			return true;
		},
	};
}

/** The HTTP server and its state. `fetch` and `now` are for tests. */
export function createHttpServer({ env = process.env, fetch, now = Date.now } = {}) {
	const spec = loadSpec();
	const write = env.ONYX_MCP_ALLOW_PROPOSE === '1';
	// The raw submit tool is never offered remotely: onyx_propose_dataset checks first.
	const tools = buildTools(spec, { ...env, ONYX_MCP_ALLOW_SUBMIT: '0' }, { write });
	const client = createClient({ baseUrl: env.ONYX_API_URL || 'http://127.0.0.1:8080', spec, fetch, userAgent: env.ONYX_USER_AGENT || 'onyx-agent/0.1' });
	const path = env.ONYX_MCP_PATH || '/mcp';
	const origins = new Set((env.ONYX_MCP_ORIGINS ?? '').split(',').map((s) => s.trim()).filter(Boolean));
	const trusted = env.ONYX_MCP_TRUSTED_PROXY || '';
	const limit = limiter(MAX_RUNNING);
	const rate = rateLimiter({ now });
	const proposals = new Map();
	const sessions = new Map();

	const sessionOf = (id) => {
		const s = sessions.get(id);
		if (!s) return null;
		if (now() - s.seen > SESSION_IDLE_MS) {
			sessions.delete(id);
			return null;
		}
		s.seen = now();
		sessions.delete(id);
		sessions.set(id, s);
		return s;
	};
	const newSession = (addr) => {
		while (sessions.size >= MAX_SESSIONS) sessions.delete(sessions.keys().next().value);
		const id = randomUUID();
		const own = limiter(MAX_RUNNING_SESSION);
		// One proposal counter per client address, not per session, so new sessions do not reset it.
		let p = proposals.get(addr);
		if (!p || now() - p.since > 3_600_000) {
			p = { sent: 0, since: now() };
			proposals.set(addr, p);
			if (proposals.size > 10_000) proposals.delete(proposals.keys().next().value);
		}
		const state = { tools, client, env, write, remote: true, answers: answerStore(), limit: (task) => own(() => limit(task)), proposals: p, seen: now() };
		sessions.set(id, state);
		return { id, state };
	};

	const send = (res, status, body, headers = {}) => {
		const h = { 'cache-control': 'no-store', 'x-content-type-options': 'nosniff', ...headers };
		if (body === undefined) {
			res.writeHead(status, h);
			return res.end();
		}
		res.writeHead(status, { 'content-type': 'application/json', ...h });
		res.end(JSON.stringify(body));
	};
	const rpcError = (res, status, code, message, headers) => send(res, status, { jsonrpc: '2.0', id: null, error: { code, message } }, headers);

	const server = createServer(async (req, res) => {
		try {
			const url = new URL(req.url ?? '/', 'http://x');
			if (url.pathname !== path) return send(res, 404, { error: 'not found' });
			const origin = req.headers.origin;
			if (origin !== undefined && !origins.has(origin)) return send(res, 403, { error: 'origin not allowed' });
			const addr = rateKey(clientAddress(req, trusted));
			if (!rate.take(addr)) return rpcError(res, 429, -32000, 'too many requests', { 'retry-after': '10' });
			if (req.method === 'GET') return send(res, 405, undefined, { allow: 'POST, DELETE' });
			const sid = req.headers['mcp-session-id'];
			if (req.method === 'DELETE') {
				if (typeof sid === 'string' && sessions.delete(sid)) return send(res, 204);
				return send(res, 404, { error: 'no such session' });
			}
			if (req.method !== 'POST') return send(res, 405, undefined, { allow: 'POST, DELETE' });
			const pv = req.headers['mcp-protocol-version'];
			if (pv !== undefined && !PROTOCOLS.includes(pv)) return rpcError(res, 400, -32600, `unsupported MCP-Protocol-Version ${pv}`);
			if (!/^application\/json\b/i.test(req.headers['content-type'] ?? '')) return rpcError(res, 415, -32600, 'content-type must be application/json');
			const chunks = [];
			let n = 0;
			for await (const c of req) {
				n += c.length;
				if (n > MAX_BODY) return rpcError(res, 413, -32600, 'message too large');
				chunks.push(c);
			}
			let msg;
			try {
				msg = JSON.parse(Buffer.concat(chunks).toString('utf8'));
			} catch {
				return rpcError(res, 400, -32700, 'parse error');
			}
			if (Array.isArray(msg)) return rpcError(res, 400, -32600, 'batches are not supported');
			let state;
			const headers = {};
			if (msg?.method === 'initialize') {
				const s = newSession(addr);
				state = s.state;
				headers['mcp-session-id'] = s.id;
			} else {
				if (typeof sid !== 'string') return rpcError(res, 400, -32600, 'Mcp-Session-Id is required (send initialize first)');
				state = sessionOf(sid);
				if (!state) return rpcError(res, 404, -32600, 'session expired or unknown: initialize again');
			}
			const out = await handle(state, msg);
			// A notification or a response from the client: accepted, no body.
			if (out === null) return send(res, 202, undefined, headers);
			return send(res, 200, out, headers);
		} catch (e) {
			if (!res.headersSent) rpcError(res, 500, -32603, 'internal error');
			else res.destroy();
		}
	});
	server.requestTimeout = 60_000;
	server.headersTimeout = 10_000;
	return { server, sessions, path, write };
}

// Run when started directly, also through a symlink (an installed `onyx-mcp-http` bin).
const self = fileURLToPath(import.meta.url);
let started = '';
try {
	started = process.argv[1] ? realpathSync(process.argv[1]) : '';
} catch {}
if (started === self) {
	const { host, port } = listenOf(process.env.ONYX_MCP_LISTEN);
	const { server, path, write } = createHttpServer();
	server.listen(port, host, () => console.error(`onyx MCP over HTTP on http://${host}:${server.address().port}${path} (${write ? 'proposals on' : 'read-only'})`));
}
