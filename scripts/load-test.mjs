#!/usr/bin/env node
// Load test of an onyx-node query API on THIS machine (north-star loop NL20, launch checklist
// LC-22, ROADMAP L5). Node 20 or later, no dependencies.
//
// Usage: node scripts/load-test.mjs --url http://127.0.0.1:18480 --obs <obs.json>
//          [--levels 1,8,32,64] [--seconds 10] [--clients 1|N] [--json out.json]
//
// It refuses any host but 127.0.0.1, ::1 or localhost: never point it at a server you do not
// run on this machine (the live server is tested by the local node with the same binary and
// export; docs/ops/benchmarks.md "API load test").
//
// --obs is a JSON list of [metric, subject] pairs the node serves (free level-2 values).
// Each worker loops over a fixed mix for --seconds at each concurrency level:
//   health   GET /v1/health
//   value    GET /v1/observations/{metric}?subject=...      (a served day)
//   miss     GET /v1/observations/{metric}?subject=...      (a day with no round: 404)
//   nopath   GET /v1/nope                                   (404)
//   bad      GET /v1/observations/{metric}?subject=%00...    (malformed subject)
//   long     GET /v1/observations/{metric}?subject=<4 KiB>  (oversized query)
//   badbody  POST /v1/submit/check with a broken JSON body  (400)
// --clients N spreads requests over N client addresses with X-Forwarded-For (the node must
// trust 127.0.0.1 as its proxy, as behind Caddy); --clients 1 is one client, which meets the
// per-client rate limits (429) by design.
import http from 'node:http';
import { performance } from 'node:perf_hooks';
import { readFileSync, writeFileSync } from 'node:fs';

const args = Object.fromEntries(
	process.argv.slice(2).reduce((acc, a, i, all) => {
		if (a.startsWith('--')) acc.push([a.slice(2), all[i + 1] && !all[i + 1].startsWith('--') ? all[i + 1] : '1']);
		return acc;
	}, [])
);
const base = new URL(args.url || 'http://127.0.0.1:18480');
if (!['127.0.0.1', '[::1]', 'localhost'].includes(base.hostname)) {
	console.error(`refused: ${base.hostname} is not this machine (load-test only a node you run here)`);
	process.exit(2);
}
if (!args.obs) {
	console.error('usage: node scripts/load-test.mjs --url http://127.0.0.1:PORT --obs obs.json [--levels 1,8,32,64] [--seconds 10] [--clients N]');
	process.exit(2);
}
const obs = JSON.parse(readFileSync(args.obs, 'utf8'));
const levels = (args.levels || '1,8,32,64').split(',').map(Number);
const seconds = Number(args.seconds || 10);
const clients = Math.max(1, Number(args.clients || 1));
const agent = new http.Agent({ keepAlive: true, maxSockets: 1024 });

const kinds = [];
for (const [m, s] of obs) kinds.push(['value', 'GET', `/v1/observations/${m}?subject=${s}`]);
const [m0, s0] = obs[0];
const prefix = s0.slice(0, s0.lastIndexOf(':') + 1);
kinds.push(['health', 'GET', '/v1/health']);
kinds.push(['miss', 'GET', `/v1/observations/${m0}?subject=${prefix}2001-01-01`]);
kinds.push(['nopath', 'GET', '/v1/nope']);
kinds.push(['bad', 'GET', `/v1/observations/${m0}?subject=%00%ff%fe`]);
kinds.push(['long', 'GET', `/v1/observations/${m0}?subject=${'x'.repeat(4096)}`]);
kinds.push(['badbody', 'POST', '/v1/submit/check', '{"source": ']);

function once([kind, method, path, body], client) {
	return new Promise((resolve) => {
		const t0 = performance.now();
		const headers = { 'user-agent': 'onyx-dev' };
		if (clients > 1) headers['x-forwarded-for'] = `198.18.${(client >> 8) & 255}.${client & 255}`;
		if (body) {
			headers['content-type'] = 'application/json';
			headers['content-length'] = Buffer.byteLength(body);
		}
		const req = http.request({ host: base.hostname.replace(/^\[|\]$/g, ''), port: base.port, path, method, headers, agent }, (res) => {
			res.resume();
			res.on('end', () => resolve({ kind, status: res.statusCode, ms: performance.now() - t0 }));
		});
		req.setTimeout(10_000, () => req.destroy(new Error('timeout')));
		req.on('error', (e) => resolve({ kind, status: e.message === 'timeout' ? 'timeout' : 'error', ms: performance.now() - t0 }));
		if (body) req.write(body);
		req.end();
	});
}

const pct = (sorted, p) => (sorted.length ? sorted[Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length))] : NaN);
const results = [];
for (const level of levels) {
	const stop = performance.now() + seconds * 1000;
	const all = [];
	let n = 0;
	const worker = async (w) => {
		while (performance.now() < stop) {
			const k = kinds[n++ % kinds.length];
			all.push(await once(k, (w * 7919 + n) % clients));
		}
	};
	const t0 = performance.now();
	await Promise.all(Array.from({ length: level }, (_, w) => worker(w)));
	const wall = (performance.now() - t0) / 1000;
	const ms = all.map((r) => r.ms).sort((a, b) => a - b);
	const status = {};
	for (const r of all) status[r.status] = (status[r.status] || 0) + 1;
	const byKind = {};
	for (const r of all) (byKind[r.kind] ||= {})[r.status] = ((byKind[r.kind] ||= {})[r.status] || 0) + 1;
	const row = {
		concurrency: level,
		clients,
		requests: all.length,
		rps: Math.round(all.length / wall),
		p50: +pct(ms, 50).toFixed(2),
		p95: +pct(ms, 95).toFixed(2),
		p99: +pct(ms, 99).toFixed(2),
		max: +ms[ms.length - 1].toFixed(2),
		errors: (status.error || 0) + (status.timeout || 0) + Object.entries(status).filter(([s]) => /^5/.test(s)).reduce((a, [, c]) => a + c, 0),
		status,
		byKind
	};
	results.push(row);
	console.log(`c=${level} clients=${clients} n=${row.requests} rps=${row.rps} p50=${row.p50}ms p95=${row.p95}ms p99=${row.p99}ms max=${row.max}ms errors=${row.errors} status=${JSON.stringify(status)}`);
}
if (args.json) writeFileSync(args.json, JSON.stringify(results, null, 2));
agent.destroy();
process.exit(results.some((r) => r.errors > 0) ? 1 : 0);
