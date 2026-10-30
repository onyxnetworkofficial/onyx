#!/usr/bin/env node
// A stand-in for the Rust `level2-keyed` tool in onyx-contrib's tests (no Rust build needed).
// `seat`: prints a seat for the manifest (fake signature). `observer`: records its argv, its
// environment and what it got on stdin into $FAKE_KEYED_RECORD, then prints lines that ECHO the
// key (a leaky observer), so the test can check onyx-contrib filters them.
import { readFileSync, writeFileSync } from 'node:fs';
import { keccak256 } from '../src/keccak.mjs';

const args = process.argv.slice(2);
const flag = (n) => {
	const i = args.indexOf(n);
	return i >= 0 ? args[i + 1] : undefined;
};
if (args[0] === 'seat') {
	const m = JSON.parse(readFileSync(flag('--manifest'), 'utf8'));
	process.stdout.write(
		JSON.stringify({ observer: '0x' + '11'.repeat(20), source: m.id, operator: flag('--operator') ?? null, terms_hash: keccak256(m.terms_quote), joined_at: 1_793_000_000, sig: '0x' + '22'.repeat(65) }) + '\n',
	);
	process.exit(0);
}
if (args[0] === 'observer') {
	const chunks = [];
	for await (const c of process.stdin) chunks.push(c);
	const stdin = Buffer.concat(chunks).toString('utf8');
	writeFileSync(process.env.FAKE_KEYED_RECORD, JSON.stringify({ argv: process.argv, env: process.env, stdin }));
	const key = stdin.trim();
	process.stdout.write(`observer: fetched with ${key}\n`);
	process.stderr.write(`observer: error from https://keyed.example.org/v1?api_key=${encodeURIComponent(key)}\nobserver: done`);
	process.exit(0);
}
process.exit(2);
