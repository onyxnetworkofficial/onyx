// Test stand-ins on 127.0.0.1, no network: an API provider that wants a key, and an Onyx intake
// that records every request it gets. The intake runs the website's own rules
// (apps/web/src/lib/intake.ts) when this Node can import TypeScript, so a body it accepts is one
// the real intake accepts too.
import { createServer } from 'node:http';
import { createHash } from 'node:crypto';

/** Obviously fake keys: never real ones. */
export const KEY = 'FAKE-test-key-0123456789abcdef';
export const OTHER_KEY = 'FAKE-other-key-9876543210fedcba';
export const PROVIDER_ORIGIN = 'https://api.example.com';

let intakeRules;
try {
	intakeRules = await import(new URL('../../web/src/lib/intake.ts', import.meta.url).href);
} catch {
	intakeRules = null;
}
/** The website's intake rules, or null when this Node cannot strip TypeScript types. */
export const INTAKE = intakeRules;

const json = (res, status, body, headers = {}) => {
	res.writeHead(status, { 'content-type': 'application/json', ...headers });
	res.end(typeof body === 'string' ? body : JSON.stringify(body));
};
const listen = (server) => new Promise((r) => server.listen(0, '127.0.0.1', () => r(`http://127.0.0.1:${server.address().port}`)));

export const SPEC = {
	openapi: '3.0.3',
	info: { title: 'Example prices', version: '1' },
	components: {
		parameters: { ids: { name: 'ids', in: 'query', required: true, schema: { type: 'string', example: 'bitcoin' }, description: 'Coin ids, comma separated.' } },
		securitySchemes: { k: { type: 'apiKey', in: 'header', name: 'x-api-key' } },
	},
	paths: {
		'/simple/price': { get: { summary: 'Price of coins', parameters: [{ $ref: '#/components/parameters/ids' }, { name: 'x-api-key', in: 'header', schema: { type: 'string' } }] } },
		'/coins/{id}': { get: { summary: 'One coin', parameters: [{ name: 'id', in: 'path', required: true, schema: { type: 'string' } }, { name: 'api_key', in: 'query' }] } },
		'/account': { get: { summary: 'Your account (echoes the key)' } },
		'/orders': { post: { summary: 'Place an order (not a GET)' } },
	},
};

/** The provider: answers only with the right key (header x-api-key, or query api_key on /q/...). */
export async function fakeProvider() {
	const seen = [];
	const server = createServer((req, res) => {
		const url = new URL(req.url, 'http://x');
		seen.push({ method: req.method, url: req.url, headers: { ...req.headers } });
		if (url.pathname === '/openapi.json') return json(res, 200, SPEC);
		if (url.pathname === '/openapi-moved') return json(res, 302, {}, { location: 'https://10.0.0.1/openapi.json' });
		const viaQuery = url.pathname.startsWith('/q/');
		const key = viaQuery ? url.searchParams.get('api_key') : req.headers['x-api-key'];
		if (key !== KEY) return json(res, 401, { error: 'bad key' });
		const path = viaQuery ? url.pathname.slice(2) : url.pathname;
		if (path === '/simple/price') return json(res, 200, { bitcoin: { usd: 67000, last_updated_at: 1761700000 } });
		if (path === '/coins/bitcoin') return json(res, 200, { id: 'bitcoin', market_data: { current_price: { usd: 67000 } } });
		if (path === '/account') return json(res, 200, { plan: 'demo', api_key_used: KEY, usage: 3 });
		if (path === '/long') return json(res, 200, { text: 'x'.repeat(5000) });
		if (path === '/big') return json(res, 200, { blob: 'y'.repeat(2 * 1024 * 1024) });
		if (path === '/moved') return json(res, 302, {}, { location: 'https://elsewhere.example.net/steal' });
		if (path === '/error') return json(res, 500, { error: `internal error for key ${KEY}` });
		// Like many APIs, the 404 echoes the request URL (with a query key in it).
		json(res, 404, { error: 'not found', requested: req.url });
	});
	const url = await listen(server);
	return { url, seen, close: () => server.close() };
}

/** The Onyx intake: /v1/submit/check, /v1/submit and /v1/submit/{id}, recording every raw request. */
export async function fakeIntake() {
	const seen = [];
	const receipts = new Map();
	const server = createServer(async (req, res) => {
		let body = '';
		for await (const c of req) body += c;
		seen.push({ method: req.method, url: req.url, headers: { ...req.headers }, body });
		const url = new URL(req.url, 'http://x');
		const m = /^\/v1\/submit\/(sub_[0-9a-f]{32})$/.exec(url.pathname);
		if (req.method === 'GET' && m) return receipts.has(m[1]) ? json(res, 200, { id: m[1], status: receipts.get(m[1]) }) : json(res, 404, { error: { code: 'not_found', message: 'no such receipt' } });
		if (req.method !== 'POST' || !['/v1/submit', '/v1/submit/check'].includes(url.pathname)) return json(res, 404, { error: { code: 'not_found', message: 'not found' } });
		if (body.length > 64 * 1024) return json(res, 413, { error: { code: 'body_too_large', message: 'too large' } });
		let route;
		if (INTAKE) {
			try {
				const s = INTAKE.parseSubmissionText(body);
				route = INTAKE.routeFor(s);
			} catch (e) {
				return json(res, 400, { error: { code: e.code ?? 'bad_json', message: e.message } });
			}
		} else {
			const p = JSON.parse(body);
			route = { outcome: 'held', level: 4, path: 'keyed_proof', reasons: ['keyed_source'], status: p.answers?.what === 'keyed_api' ? 'held_keyed_source_review' : 'pending_review' };
		}
		const hash = '0x' + createHash('sha256').update(body).digest('hex');
		if (url.pathname === '/v1/submit/check') return json(res, 200, { valid: route.outcome !== 'refused', hash, route, fetch: { pull: false, reason: 'not_while_refused_or_held' }, preview: null, fetchError: null });
		if (route.outcome === 'refused') return json(res, 422, { error: { code: 'refused', message: 'refused' }, route });
		const id = 'sub_' + createHash('sha256').update(body + receipts.size).digest('hex').slice(0, 32);
		receipts.set(id, route.status);
		return json(res, 201, { id, status: route.status, hash, route });
	});
	const url = await listen(server);
	return { url, seen, close: () => server.close() };
}

/** A fetch that sends https://api.example.com/... to the local provider; everything else as is. */
export function routedFetch(providerUrl) {
	const calls = [];
	const f = (url, init = {}) => {
		const s = String(url);
		calls.push({ url: s, headers: { ...(init.headers ?? {}) } });
		return fetch(s.startsWith(PROVIDER_ORIGIN) ? providerUrl + s.slice(PROVIDER_ORIGIN.length) : s, init);
	};
	f.calls = calls;
	return f;
}
