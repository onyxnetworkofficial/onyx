// The key vault: third-party API keys in the operating system's own keychain, never in a file of
// ours, never in a command line, never in a log, a tool result or a request to Onyx.
//
// Each key is stored under the provider's name AND the origin it may be sent to
// (https://host[:port]), so a provider that is later pointed at another host finds no key
// (THREATS CL2-3). Backends, chosen by ONYX_CONTRIB_VAULT:
//   keychain (default)  Linux: `secret-tool` (libsecret: GNOME Keyring, KWallet's Secret Service);
//                       macOS: `security` (the login keychain); Windows: PowerShell with DPAPI
//                       (CurrentUser scope), the protected blob in %APPDATA%\onyx-contrib\keys.
//                       The key reaches each tool on its standard input only.
//   file (opt-in)       AES-256-GCM in <config>/vault.json, the key derived with scrypt from
//                       ONYX_CONTRIB_VAULT_PASSPHRASE. For machines without a keychain service
//                       (a server, some WSL setups). It protects a copied vault file, not a machine
//                       where the passphrase is readable too (it sits in your MCP client's settings).
import { spawn } from 'node:child_process';
import { createCipheriv, createDecipheriv, createHash, randomBytes, scryptSync } from 'node:crypto';
import { chmodSync, existsSync, mkdirSync, readFileSync, renameSync, unlinkSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

export const SERVICE = 'onyx-contrib';

/** Run `cmd args` with `input` on stdin and no shell. The key only ever travels on stdin. */
export function runTool(cmd, args, input = '') {
	return new Promise((resolve) => {
		let child;
		try {
			child = spawn(cmd, args, { stdio: ['pipe', 'pipe', 'pipe'], shell: false, windowsHide: true });
		} catch (e) {
			return resolve({ code: -1, stdout: '', stderr: String(e.message) });
		}
		const out = [];
		const err = [];
		child.stdout.on('data', (c) => out.push(c));
		child.stderr.on('data', (c) => err.push(c));
		child.on('error', (e) => resolve({ code: -1, stdout: '', stderr: e.code === 'ENOENT' ? `${cmd} is not installed` : String(e.message) }));
		child.on('close', (code) => resolve({ code, stdout: Buffer.concat(out).toString('utf8'), stderr: Buffer.concat(err).toString('utf8').slice(0, 300) }));
		child.stdin.on('error', () => {});
		child.stdin.end(input);
	});
}

/** A key must be printable ASCII without spaces (API keys are), at least 12 characters (intake keyMin). */
export function checkKey(key) {
	if (typeof key !== 'string' || key.length === 0) return 'no key given';
	if (!/^[\x21-\x7e]+$/.test(key)) return 'a key is printable ASCII without spaces or line breaks';
	if (key.length < 12) return 'that key is shorter than 12 characters, too short for onyx-contrib to check outgoing data against it';
	if (key.length > 4096) return 'that key is longer than 4096 characters';
	return null;
}

const itemId = (name, origin) => `${name}@${origin}`;
/** A missing keychain tool is an error, not "no key" (exit -1 is our spawn failure). */
const missing = (r) => {
	if (r.code === -1) throw new Error(`${r.stderr}. Install it (README) or use ONYX_CONTRIB_VAULT=file.`);
};

/** Linux: libsecret's secret-tool. */
export function secretToolVault({ run = runTool, cmd = 'secret-tool' } = {}) {
	const attrs = (name, origin) => ['service', SERVICE, 'name', name, 'origin', origin];
	return {
		kind: 'keychain (secret-tool, libsecret)',
		async set(name, origin, key) {
			const r = await run(cmd, ['store', '--label', `onyx-contrib key for ${name}`, ...attrs(name, origin)], key);
			if (r.code !== 0) throw new Error(`secret-tool could not store the key (${r.stderr.trim() || `exit ${r.code}`}). Is a keyring (GNOME Keyring, KWallet) running? Or use ONYX_CONTRIB_VAULT=file (README).`);
		},
		async get(name, origin) {
			const r = await run(cmd, ['lookup', ...attrs(name, origin)]);
			missing(r);
			if (r.code !== 0 || r.stdout === '') return null;
			return r.stdout.replace(/\r?\n$/, '');
		},
		async remove(name, origin) {
			await run(cmd, ['clear', ...attrs(name, origin)]);
		},
	};
}

/** macOS: `security`. The add command is read from stdin (`security -i`), so the key is in no argv. */
export function macVault({ run = runTool, cmd = 'security' } = {}) {
	return {
		kind: 'keychain (macOS security)',
		async set(name, origin, key) {
			if (/["\\]/.test(key)) throw new Error('On macOS a key with a double quote or a backslash cannot be stored by onyx-contrib v0.');
			const line = `add-generic-password -U -s "${SERVICE}" -a "${itemId(name, origin)}" -l "onyx-contrib key for ${name}" -w "${key}"\n`;
			const r = await run(cmd, ['-i'], line);
			if (r.code !== 0) throw new Error(`security could not store the key (exit ${r.code}).`);
		},
		async get(name, origin) {
			const r = await run(cmd, ['find-generic-password', '-s', SERVICE, '-a', itemId(name, origin), '-w']);
			missing(r);
			if (r.code !== 0 || r.stdout === '') return null;
			return r.stdout.replace(/\r?\n$/, '');
		},
		async remove(name, origin) {
			await run(cmd, ['delete-generic-password', '-s', SERVICE, '-a', itemId(name, origin)]);
		},
	};
}

/** Windows: DPAPI (CurrentUser) through PowerShell; the protected blob is a file in the profile. */
export function dpapiVault({ dir, run = runTool, cmd = 'powershell.exe' } = {}) {
	const file = (name, origin) => join(dir, 'keys', createHash('sha256').update(itemId(name, origin)).digest('hex').slice(0, 32) + '.dpapi');
	const ps = (script) => ['-NoProfile', '-NonInteractive', '-Command', `Add-Type -AssemblyName System.Security; ${script}`];
	return {
		kind: 'keychain (Windows DPAPI)',
		async set(name, origin, key) {
			const r = await run(cmd, ps("$k=[Console]::In.ReadToEnd(); $p=[Security.Cryptography.ProtectedData]::Protect([Text.Encoding]::UTF8.GetBytes($k),$null,'CurrentUser'); [Console]::Out.Write([Convert]::ToBase64String($p))"), key);
			if (r.code !== 0 || !/^[A-Za-z0-9+/=]+$/.test(r.stdout.trim())) throw new Error(`PowerShell could not protect the key (exit ${r.code}).`);
			mkdirSync(join(dir, 'keys'), { recursive: true });
			writeFileSync(file(name, origin), r.stdout.trim() + '\n', { mode: 0o600 });
		},
		async get(name, origin) {
			const f = file(name, origin);
			if (!existsSync(f)) return null;
			const r = await run(cmd, ps("$b=[Convert]::FromBase64String([Console]::In.ReadToEnd().Trim()); $u=[Security.Cryptography.ProtectedData]::Unprotect($b,$null,'CurrentUser'); [Console]::Out.Write([Text.Encoding]::UTF8.GetString($u))"), readFileSync(f, 'utf8'));
			missing(r);
			return r.code === 0 && r.stdout !== '' ? r.stdout : null;
		},
		async remove(name, origin) {
			const f = file(name, origin);
			if (existsSync(f)) unlinkSync(f);
		},
	};
}

/** Opt-in: an AES-256-GCM file, the key from scrypt(passphrase). The item id is the GCM associated data. */
export function fileVault({ dir, passphrase }) {
	if (typeof passphrase !== 'string' || passphrase.length < 12) throw new Error('ONYX_CONTRIB_VAULT=file needs ONYX_CONTRIB_VAULT_PASSPHRASE of at least 12 characters.');
	const path = join(dir, 'vault.json');
	let derived = null;
	const load = () => (existsSync(path) ? JSON.parse(readFileSync(path, 'utf8')) : { version: 1, salt: randomBytes(16).toString('hex'), items: {} });
	const keyOf = (v) => {
		if (!derived || derived.salt !== v.salt) derived = { salt: v.salt, key: scryptSync(passphrase, Buffer.from(v.salt, 'hex'), 32, { N: 1 << 15, r: 8, p: 1, maxmem: 64 * 1024 * 1024 }) };
		return derived.key;
	};
	const save = (v) => {
		mkdirSync(dir, { recursive: true, mode: 0o700 });
		const tmp = `${path}.${process.pid}.tmp`;
		writeFileSync(tmp, JSON.stringify(v, null, 1) + '\n', { mode: 0o600 });
		renameSync(tmp, path);
		chmodSync(path, 0o600);
	};
	return {
		kind: 'encrypted file (opt-in, ONYX_CONTRIB_VAULT=file)',
		async set(name, origin, key) {
			const v = load();
			const iv = randomBytes(12);
			const c = createCipheriv('aes-256-gcm', keyOf(v), iv);
			c.setAAD(Buffer.from(itemId(name, origin)));
			const ct = Buffer.concat([c.update(key, 'utf8'), c.final()]);
			v.items[itemId(name, origin)] = { iv: iv.toString('hex'), tag: c.getAuthTag().toString('hex'), ct: ct.toString('hex') };
			save(v);
		},
		async get(name, origin) {
			const v = load();
			const it = v.items[itemId(name, origin)];
			if (!it) return null;
			try {
				const d = createDecipheriv('aes-256-gcm', keyOf(v), Buffer.from(it.iv, 'hex'));
				d.setAAD(Buffer.from(itemId(name, origin)));
				d.setAuthTag(Buffer.from(it.tag, 'hex'));
				return Buffer.concat([d.update(Buffer.from(it.ct, 'hex')), d.final()]).toString('utf8');
			} catch {
				throw new Error('The vault file does not open with this passphrase (or it was changed).');
			}
		},
		async remove(name, origin) {
			const v = load();
			if (v.items[itemId(name, origin)]) {
				delete v.items[itemId(name, origin)];
				save(v);
			}
		},
	};
}

/** In memory, for tests only (never chosen from the environment). */
export function memoryVault() {
	const m = new Map();
	return {
		kind: 'memory (tests)',
		async set(name, origin, key) {
			m.set(itemId(name, origin), key);
		},
		async get(name, origin) {
			return m.get(itemId(name, origin)) ?? null;
		},
		async remove(name, origin) {
			m.delete(itemId(name, origin));
		},
	};
}

/** The vault this machine uses (ONYX_CONTRIB_VAULT: keychain by default, or file). */
export function vaultFromEnv(env = process.env, { dir, platform = process.platform, run } = {}) {
	const mode = env.ONYX_CONTRIB_VAULT || 'keychain';
	if (mode === 'file') return fileVault({ dir, passphrase: env.ONYX_CONTRIB_VAULT_PASSPHRASE });
	if (mode !== 'keychain') throw new Error(`ONYX_CONTRIB_VAULT: keychain or file, not ${mode}`);
	if (platform === 'linux') return secretToolVault({ run });
	if (platform === 'darwin') return macVault({ run });
	if (platform === 'win32') return dpapiVault({ dir, run });
	throw new Error(`No keychain support on ${platform}; use ONYX_CONTRIB_VAULT=file (README).`);
}
