// Keyed rounds (contributor loop CL4, ADR-0054): a key holder takes a seat in a keyed source's
// pool and observes its rounds with its OWN provider key, read from this computer's vault only.
//
// What leaves this computer: the seat (observer address, source, declared operator, the hash of
// the terms shown, the time, a signature: never anything about the key) and the observer's
// ordinary commit and reveal (the value, the window start and a blinded page hash). The key
// itself goes only to the observer process, on its standard input (never a command line, an
// environment variable or a file), and from there only to the provider's own host. Every line
// the observer prints passes the key filter before it is shown.
//
// The signing and the round are the Rust tool `level2-keyed` (crates/onyx-level2/examples), the
// same code the coordinator runs: ONYX_CONTRIB_KEYED_BIN names its binary.
import { spawn } from 'node:child_process';
import { chmodSync, existsSync, mkdirSync, readdirSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { keccak256 } from './keccak.mjs';
import { redact } from './rules.mjs';
import { runTool } from './vault.mjs';

/** Lines of an observer's output kept for its status. */
export const KEEP_LINES = 40;

/** The parts of a keyed manifest this client needs (the Rust tool checks every rule). */
export function readKeyedManifest(path) {
	const text = readFileSync(path, 'utf8');
	if (text.length > 1 << 20) throw new Error(`${path}: larger than 1 MiB`);
	const m = JSON.parse(text);
	const k = m?.descriptor?.access?.keyed;
	if (!k || typeof k !== 'object') throw new Error(`${path}: not a keyed manifest (its access is not keyed)`);
	if (!['query', 'header'].includes(k.place) || typeof k.name !== 'string' || !['raw', 'bearer'].includes(k.scheme)) throw new Error(`${path}: descriptor.access.keyed is malformed`);
	const url = new URL(String(m?.template?.url_template ?? '').replace('{date}', '2000-01-01').replace('{api_key}', 'KEY'));
	if (url.protocol !== 'https:') throw new Error(`${path}: the template is not https`);
	if (typeof m.terms_quote !== 'string' || typeof m.terms_url !== 'string' || typeof m.id !== 'string') throw new Error(`${path}: id, terms_url and terms_quote are required`);
	return {
		path: resolve(path),
		id: m.id,
		metric: m.metric,
		name: m.name,
		origin: url.origin,
		access: { place: k.place, name: k.name, scheme: k.scheme },
		termsUrl: m.terms_url,
		termsQuote: m.terms_quote,
		termsHash: keccak256(m.terms_quote),
	};
}

/**
 * The provider must send the key exactly where the manifest says, to the manifest's host: the
 * vault's key is stored for that provider AND its origin, so a manifest for another host never
 * gets it (THREATS CL4-2), and a key is never placed where its provider did not say.
 */
export function checkProviderFits(p, m) {
	const origin = new URL(p.baseUrl).origin;
	if (origin !== m.origin) throw new Error(`the manifest fetches ${m.origin}, but your key for ${p.name} is stored for ${origin}: refused (a key goes only to its own host)`);
	if (m.access.place === 'query') {
		if (p.auth.in !== 'query' || p.auth.name !== m.access.name) throw new Error(`the manifest puts the key in the ${m.access.name} query parameter; ${p.name} sends it ${p.auth.in === 'query' ? `as ${p.auth.name}` : `in the ${p.auth.name} header`}: refused`);
	} else {
		const bearer = (p.auth.prefix ?? '').trim().toLowerCase() === 'bearer';
		if (p.auth.in !== 'header' || p.auth.name.toLowerCase() !== m.access.name || bearer !== (m.access.scheme === 'bearer')) throw new Error(`the manifest puts the key in the ${m.access.name} header (${m.access.scheme}); ${p.name} sends it otherwise: refused`);
	}
}

/** The level2-keyed binary (ONYX_CONTRIB_KEYED_BIN). */
export function keyedBin(env = process.env) {
	const b = env.ONYX_CONTRIB_KEYED_BIN;
	if (!b) throw new Error('ONYX_CONTRIB_KEYED_BIN is not set: it names the level2-keyed binary (cargo build -p onyx-level2 --example level2-keyed; README section 7)');
	if (!existsSync(b)) throw new Error(`ONYX_CONTRIB_KEYED_BIN: ${b} does not exist`);
	return b;
}

const seatDir = (dir) => join(dir, 'keyed');
export const seatPath = (dir, sourceId) => join(seatDir(dir), `${sourceId}.seat.json`);

/** What a holder is shown before joining: the terms, their hash, and what the seat holds. */
export function joinPreview(p, m, { operator } = {}) {
	return {
		source: m.id,
		metric: m.metric,
		provider: p.name,
		keySentTo: `${m.origin}, ${m.access.place === 'query' ? `as the ${m.access.name} query parameter` : `in the ${m.access.name} header`}, by your own computer only`,
		terms: { url: m.termsUrl, quote: m.termsQuote, hash: m.termsHash },
		seatHolds: ['your observer address', 'the source id', `operator: ${operator ?? 'none'}`, 'the terms hash above', 'the time you join', 'your signature'],
		seatNeverHolds: 'your API key, any hash or fingerprint of it, or anything about your account',
		note: 'What Onyx does so your key is not linked to your address: your observer fetches at a private random second, every seat fetches every round whether drawn or not, commits are posted at the window end, and your reveal shows only the value, the window start and a blinded page hash. What it cannot stop: the provider sees which of its keys fetch at round times, and a provider that answers your key differently on purpose (or refuses it) can find your address from your public reveal, at the cost of a small slash to you. Declaring your operator limits you to one drawn seat per round; not declaring it is not detected.',
	};
}

/** Sign the seat with the observer key (level2-keyed seat), check it and store it (0600). */
export async function joinSeat({ dir, p, m, repo, observerKey, operator, bin, run = runTool }) {
	checkProviderFits(p, m);
	const args = ['seat', '--manifest', m.path, '--repo', resolve(repo), '--key-file', resolve(observerKey)];
	if (operator !== undefined) {
		if (!/^[a-z0-9-]+(\.[a-z0-9-]+)*$/.test(operator) || operator.length > 64) throw new Error('operator: dot-separated a-z, 0-9 and -, at most 64 characters');
		args.push('--operator', operator);
	}
	const r = await run(bin, args);
	if (r.code !== 0) throw new Error(`level2-keyed seat failed: ${r.stderr.trim().slice(0, 300)}`);
	const seat = JSON.parse(r.stdout);
	if (seat.source !== m.id || seat.terms_hash !== m.termsHash) throw new Error('the signed seat is not for this manifest and these terms');
	const keys = Object.keys(seat).sort().join(',');
	if (keys !== 'joined_at,observer,operator,sig,source,terms_hash') throw new Error(`unexpected seat fields: ${keys}`);
	mkdirSync(seatDir(dir), { recursive: true, mode: 0o700 });
	const path = seatPath(dir, m.id);
	const tmp = `${path}.${process.pid}.tmp`;
	writeFileSync(tmp, JSON.stringify(seat, null, 1) + '\n', { mode: 0o600 });
	renameSync(tmp, path);
	chmodSync(path, 0o600);
	return { seat, path };
}

/** The seats this computer holds. */
export function listSeats(dir) {
	const d = seatDir(dir);
	if (!existsSync(d)) return [];
	return readdirSync(d)
		.filter((f) => f.endsWith('.seat.json'))
		.sort()
		.map((f) => JSON.parse(readFileSync(join(d, f), 'utf8')));
}

/** Flags the holder may not pass to the observer: the key comes from the vault only. */
const REFUSED = /^--(api-key|key-stdin)/;

/**
 * Run one keyed observer: `level2-keyed observer` with the key on its standard input. Every
 * line it prints is passed through the key filter before `out`/`err` see it. Resolves to its
 * exit code.
 */
export function observe({ p, m, key, repo, bin, args = [], env = process.env, out = (s) => process.stdout.write(s + '\n'), err = (s) => process.stderr.write(s + '\n'), spawnFn = spawn }) {
	checkProviderFits(p, m);
	for (const a of args) if (REFUSED.test(a)) throw new Error(`${a}: the key comes from your vault only`);
	const argv = ['observer', '--manifest', m.path, '--repo', resolve(repo), '--api-key-stdin', ...args];
	return new Promise((done) => {
		let child;
		try {
			child = spawnFn(bin, argv, { stdio: ['pipe', 'pipe', 'pipe'], shell: false, windowsHide: true, env });
		} catch (e) {
			err(redact(`observer: ${e.message}`, [key]));
			return done(1);
		}
		const pipe = (stream, sink) => {
			let buf = '';
			stream.setEncoding('utf8');
			stream.on('data', (c) => {
				buf += c;
				let i;
				while ((i = buf.indexOf('\n')) >= 0) {
					sink(redact(buf.slice(0, i), [key]));
					buf = buf.slice(i + 1);
				}
			});
			stream.on('end', () => {
				if (buf) sink(redact(buf, [key]));
				buf = '';
			});
		};
		pipe(child.stdout, out);
		pipe(child.stderr, err);
		child.on('error', (e) => err(redact(`observer: ${e.message}`, [key])));
		child.on('close', (code) => done(code ?? 1));
		child.stdin.on('error', () => {});
		child.stdin.end(key + '\n');
	});
}
