// Static server for benchmarks: serves apps/web/build with gzip and the cache
// headers a production host would send (immutable hashed assets).
// Usage: node tests/serve.mjs [port] [build dir]
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { extname, join, normalize, resolve } from 'node:path';
import { gzipSync } from 'node:zlib';

const BUILD = new URL('../build/', import.meta.url).pathname;
const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.json': 'application/json', '.wasm': 'application/wasm', '.txt': 'text/plain' };
const cache = new Map();
export function serve(port = 0, root = BUILD) {
	const server = createServer(async (req, res) => {
		const path = normalize(decodeURIComponent(new URL(req.url, 'http://x').pathname)).replace(/^(\.\.[/\\])+/, '');
		const file = path === '/' ? 'index.html' : path;
		try {
			let body = cache.get(file);
			if (!body) {
				const raw = await readFile(join(root, file));
				body = { raw, gz: gzipSync(raw, { level: 9 }) };
				cache.set(file, body);
			}
			const gz = /\bgzip\b/.test(req.headers['accept-encoding'] ?? '');
			res.writeHead(200, {
				'content-type': types[extname(file)] ?? 'application/octet-stream',
				'cache-control': file.includes('/immutable/') ? 'public, max-age=31536000, immutable' : 'public, max-age=300',
				...(gz ? { 'content-encoding': 'gzip' } : {})
			});
			res.end(gz ? body.gz : body.raw);
		} catch {
			res.writeHead(404).end();
		}
	});
	return new Promise((r) => server.listen(port, '127.0.0.1', () => r(server)));
}
if (import.meta.url === `file://${process.argv[1]}`) {
	const s = await serve(Number(process.argv[2] ?? 4173), process.argv[3] ? resolve(process.argv[3]) : BUILD);
	console.log(`serving build on http://127.0.0.1:${s.address().port}/`);
}
