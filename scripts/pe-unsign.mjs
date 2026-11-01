#!/usr/bin/env node
// Remove the Authenticode signature from a Windows program (PE32+), in place: what
// `signtool remove /s` does, on any system (CL10, scripts/desktop-build.sh). Node's official
// node.exe is signed; writing the onyx-contrib bundle into it would leave a signature that no longer
// matches, which Windows treats worse than none. The certificate table is the last thing in the
// file: it is cut off and its directory entry zeroed. Refuses anything that is not that shape.
//
//   node scripts/pe-unsign.mjs <file.exe>
import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';

/** The file's bytes without its certificate table (the same bytes when there is none). */
export function unsign(buf) {
	const b = Buffer.from(buf);
	if (b.length < 0x40 || b.readUInt16LE(0) !== 0x5a4d) throw new Error('not a PE file (no MZ header)');
	const pe = b.readUInt32LE(0x3c);
	if (pe + 24 > b.length || b.readUInt32LE(pe) !== 0x00004550) throw new Error('not a PE file (no PE signature)');
	const opt = pe + 24;
	const magic = b.readUInt16LE(opt);
	if (magic !== 0x20b) throw new Error('not a 64-bit PE (PE32+) file');
	const nDirs = b.readUInt32LE(opt + 108);
	if (nDirs < 5) return b;
	const sec = opt + 112 + 4 * 8; // IMAGE_DIRECTORY_ENTRY_SECURITY
	const at = b.readUInt32LE(sec);
	const size = b.readUInt32LE(sec + 4);
	if (at === 0 && size === 0) return b;
	if (at + size !== b.length && at + size + ((8 - (size % 8)) % 8) !== b.length) throw new Error('the certificate table is not at the end of the file; refusing to cut');
	const out = b.subarray(0, at);
	out.writeUInt32LE(0, sec);
	out.writeUInt32LE(0, sec + 4);
	out.writeUInt32LE(0, opt + 64); // CheckSum: not used for programs; zero is valid
	return out;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
	const f = process.argv[2];
	if (!f) {
		process.stderr.write('usage: node scripts/pe-unsign.mjs <file.exe>\n');
		process.exit(2);
	}
	const before = readFileSync(f);
	const after = unsign(before);
	writeFileSync(f, after);
	process.stdout.write(`pe-unsign: ${f}: ${before.length - after.length} bytes of signature removed\n`);
}
