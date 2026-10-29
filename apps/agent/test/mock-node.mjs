#!/usr/bin/env node
// A stand-in Onyx node for scripts/agent-connect-test.sh (NL21): the proposal routes in the
// OpenAPI file's shapes, and the website's freshness file. It checks only what the toolkit sends
// (shape and size), records proposals in memory, and prints its URL once it listens.
import { createServer } from 'node:http';
import { createHash } from 'node:crypto';

const receipts = new Map();
const json = (res, status, body) => {
	res.writeHead(status, { 'content-type': 'application/json', 'cache-control': 'no-store' });
	res.end(JSON.stringify(body));
};
const server = createServer(async (req, res) => {
	const url = new URL(req.url, 'http://x');
	if (req.method === 'GET' && url.pathname === '/data/freshness.json')
		return json(res, 200, { version: 1, datasets: { 'usgs.earthquakes.m5_count': { asOf: '2026-07-19', what: 'day counted' } } });
	if (req.method === 'GET' && url.pathname === '/v1/health') return json(res, 200, { mock: true });
	const m = /^\/v1\/submit\/(sub_[0-9a-f]{32})$/.exec(url.pathname);
	if (req.method === 'GET' && m) return receipts.has(m[1]) ? json(res, 200, { id: m[1], status: 'pending_review' }) : json(res, 404, { error: { code: 'not_found', message: 'no such receipt' } });
	if (req.method === 'POST' && (url.pathname === '/v1/submit' || url.pathname === '/v1/submit/check')) {
		let body = '';
		for await (const c of req) {
			body += c;
			if (body.length > 64 * 1024) return json(res, 413, { error: { code: 'too_large', message: 'too large' } });
		}
		let p;
		try {
			p = JSON.parse(body);
		} catch {
			return json(res, 400, { error: { code: 'bad_json', message: 'not JSON' } });
		}
		const ok = ['title', 'description', 'sourceName', 'sourceUrl', 'answers'].every((k) => p[k] !== undefined) && /^https:\/\//.test(p.sourceUrl);
		const hash = '0x' + createHash('sha256').update(body).digest('hex');
		if (url.pathname === '/v1/submit/check') return json(res, 200, { valid: ok, hash, route: 'level2', fetch: { pull: false }, preview: null, fetchError: null });
		if (!ok) return json(res, 422, { error: { code: 'invalid', message: 'invalid proposal' } });
		const id = 'sub_' + hash.slice(2, 34);
		receipts.set(id, p);
		return json(res, 201, { id, status: 'pending_review', hash, route: 'level2' });
	}
	json(res, 404, { error: { code: 'not_found', message: 'not found' } });
});
server.listen(Number(process.env.PORT || 0), '127.0.0.1', () => console.log(`http://127.0.0.1:${server.address().port}`));
