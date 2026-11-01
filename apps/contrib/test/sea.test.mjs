// The single-file program's bundle (CL10, scripts/sea-bundle.mjs): the bundle runs the same
// program (scripts/desktop-smoke.mjs against it, with no Node on PATH for the child), embeds the
// app's files byte for byte, and the bundler refuses module syntax it does not understand, so a
// new construct fails the build instead of the program. Also the Windows signature cutter.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = fileURLToPath(new URL('../../../', import.meta.url));
const { bundle, transform } = await import(new URL('../../../scripts/sea-bundle.mjs', import.meta.url).href);
const { unsign } = await import(new URL('../../../scripts/pe-unsign.mjs', import.meta.url).href);

test('sea bundle: one CommonJS file that runs version, doctor, MCP and the app like the source', () => {
	const d = mkdtempSync(join(tmpdir(), 'onyx-sea-'));
	try {
		const f = join(d, 'onyx-contrib.cjs');
		writeFileSync(f, bundle());
		const r = spawnSync(process.execPath, [join(ROOT, 'scripts/desktop-smoke.mjs'), process.execPath, f], { encoding: 'utf8', timeout: 120_000, env: { ...process.env, ONYX_SMOKE_TMP: d } });
		assert.equal(r.status, 0, r.stdout + r.stderr);
		assert.match(r.stdout, /desktop-smoke: all passed/);
		assert.doesNotMatch(r.stdout, /FAIL/);
	} finally {
		rmSync(d, { recursive: true, force: true });
	}
});

test('sea bundle: module syntax it does not read is refused, never shipped', () => {
	const ok = transform('apps/contrib/src/x.mjs', "import { a, b as c } from './rules.mjs';\nimport * as fs from 'node:fs';\nexport const X = 1;\nexport function f() { return import.meta.url; }\n");
	assert.deepEqual(ok.deps, ['apps/contrib/src/rules.mjs']);
	assert.deepEqual(ok.names, ['X', 'f']);
	assert.match(ok.body, /const \{ a, b: c \} = __load\("apps\/contrib\/src\/rules\.mjs"\);/);
	for (const [src, why] of [
		["export default function f() {}\n", /export form/],
		["import x from './rules.mjs';\n", /import clause/],
		["import { a } from 'some-package';\n", /from a package/],
		["import './side.mjs';\n", /import statement/],
		["export * from './rules.mjs';\n", /export form/],
		["export { a } from './rules.mjs';\n", /export form/],
		["const m = await import('./rules.mjs');\n", /dynamic import/],
		["const x = import.meta.dirname;\n", /import\.meta other/],
		["const x = await Promise.resolve(1);\n", /does not compile/],
	])
		assert.throws(() => transform('apps/contrib/src/x.mjs', src), why, src);
});

test('pe-unsign: cuts a trailing certificate table and zeroes its entry; refuses other shapes', () => {
	// A minimal PE32+ layout: MZ, e_lfanew, PE\0\0, COFF header, optional header with 16 directories.
	const pe = 0x40;
	const opt = pe + 24;
	const body = 0x200;
	const cert = Buffer.from('CERTIFICATE-TABLE');
	const size = cert.length + ((8 - (cert.length % 8)) % 8);
	const b = Buffer.alloc(body + size);
	b.writeUInt16LE(0x5a4d, 0);
	b.writeUInt32LE(pe, 0x3c);
	b.writeUInt32LE(0x00004550, pe);
	b.writeUInt16LE(0x20b, opt);
	b.writeUInt32LE(0x1234, opt + 64);
	b.writeUInt32LE(16, opt + 108);
	b.writeUInt32LE(body, opt + 112 + 32);
	b.writeUInt32LE(size, opt + 112 + 36);
	cert.copy(b, body);
	const out = unsign(b);
	assert.equal(out.length, body);
	assert.equal(out.readUInt32LE(opt + 112 + 32), 0);
	assert.equal(out.readUInt32LE(opt + 112 + 36), 0);
	assert.ok(!out.includes(cert));
	assert.equal(unsign(out).length, body, 'unsigned stays as it is');
	const mid = Buffer.from(b);
	mid.writeUInt32LE(0x100, opt + 112 + 32);
	assert.throws(() => unsign(mid), /not at the end/);
	assert.throws(() => unsign(Buffer.from('not a program at all, not a program at all, not a program at all')), /not a PE file/);
	const pe32 = Buffer.from(b);
	pe32.writeUInt16LE(0x10b, opt);
	assert.throws(() => unsign(pe32), /PE32\+/);
});
