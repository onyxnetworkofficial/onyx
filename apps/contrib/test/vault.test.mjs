// The key vault (CL2): each keychain backend gets the key on stdin only, never in a command line;
// the opt-in file vault encrypts it and binds it to its provider and origin. No real keychain is
// touched: the system tools are stand-in scripts that record what they were given.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { checkKey, fileVault, macVault, runTool, secretToolVault, vaultFromEnv } from '../src/vault.mjs';
import { KEY } from './fakes.mjs';

const ORIGIN = 'https://api.example.com';

/** A stand-in for a system tool: records argv and stdin, and acts as a tiny keychain in a folder. */
function fakeTool(d) {
	const path = join(d, 'fake-tool.mjs');
	writeFileSync(
		path,
		`#!/usr/bin/env node
import { appendFileSync, existsSync, readFileSync, writeFileSync } from 'node:fs';
const d = ${JSON.stringify(d)};
let stdin = '';
for await (const c of process.stdin) stdin += c;
const argv = process.argv.slice(2);
appendFileSync(d + '/argv.log', JSON.stringify(argv) + '\\n');
const store = existsSync(d + '/store.json') ? JSON.parse(readFileSync(d + '/store.json', 'utf8')) : {};
const save = () => writeFileSync(d + '/store.json', JSON.stringify(store));
const id = (a) => a.filter((x) => x !== '--label' && !x.startsWith('onyx-contrib key')).join('|');
if (argv[0] === 'store') { store[id(argv.slice(1))] = stdin; save(); }
else if (argv[0] === 'lookup') { const v = store[id(argv.slice(1))]; if (v === undefined) process.exit(1); process.stdout.write(v); }
else if (argv[0] === 'clear') { delete store[id(argv.slice(1))]; save(); }
else if (argv[0] === '-i') { const m = /-a "([^"]+)".* -w "([^"]+)"/.exec(stdin); store['mac|' + m[1]] = m[2]; save(); }
else if (argv[0] === 'find-generic-password') { const v = store['mac|' + argv[argv.indexOf('-a') + 1]]; if (v === undefined) process.exit(44); process.stdout.write(v + '\\n'); }
else if (argv[0] === 'delete-generic-password') { delete store['mac|' + argv[argv.indexOf('-a') + 1]]; save(); }
`,
	);
	chmodSync(path, 0o755);
	return path;
}

test('secret-tool: the key goes on stdin, never in argv; bound to name and origin', async () => {
	const d = mkdtempSync(join(tmpdir(), 'onyx-contrib-vault-'));
	try {
		const v = secretToolVault({ cmd: fakeTool(d) });
		await v.set('prices', ORIGIN, KEY);
		assert.equal(await v.get('prices', ORIGIN), KEY);
		assert.equal(await v.get('prices', 'https://evil.example.net'), null);
		assert.equal(await v.get('other', ORIGIN), null);
		const argv = readFileSync(join(d, 'argv.log'), 'utf8');
		assert.ok(!argv.includes(KEY), 'the key was in a command line');
		assert.match(argv, /"store","--label","onyx-contrib key for prices","service","onyx-contrib","name","prices","origin","https:\/\/api.example.com"/);
		await v.remove('prices', ORIGIN);
		assert.equal(await v.get('prices', ORIGIN), null);
	} finally {
		rmSync(d, { recursive: true, force: true });
	}
});

test('macOS security: the add command (with the key) is read from stdin, so argv holds no key', async () => {
	const d = mkdtempSync(join(tmpdir(), 'onyx-contrib-vault-'));
	try {
		const v = macVault({ cmd: fakeTool(d) });
		await v.set('prices', ORIGIN, KEY);
		assert.equal(await v.get('prices', ORIGIN), KEY);
		assert.equal(await v.get('prices', 'https://evil.example.net'), null);
		assert.ok(!readFileSync(join(d, 'argv.log'), 'utf8').includes(KEY), 'the key was in a command line');
		await assert.rejects(v.set('prices', ORIGIN, 'FAKE-key-with-"quote"'), /double quote/);
		await v.remove('prices', ORIGIN);
		assert.equal(await v.get('prices', ORIGIN), null);
	} finally {
		rmSync(d, { recursive: true, force: true });
	}
});

test('file vault (opt-in): encrypted, bound to name and origin, refuses a wrong passphrase', async () => {
	const d = mkdtempSync(join(tmpdir(), 'onyx-contrib-vault-'));
	try {
		assert.throws(() => fileVault({ dir: d, passphrase: 'short' }), /at least 12/);
		const v = fileVault({ dir: d, passphrase: 'FAKE-passphrase-for-tests' });
		await v.set('prices', ORIGIN, KEY);
		assert.equal(await v.get('prices', ORIGIN), KEY);
		assert.equal(await v.get('prices', 'https://evil.example.net'), null);
		const raw = readFileSync(join(d, 'vault.json'), 'utf8');
		assert.ok(!raw.includes(KEY) && !raw.includes(Buffer.from(KEY).toString('base64')) && !raw.includes(Buffer.from(KEY).toString('hex')));
		// An item moved to another provider's slot does not open (the slot is the GCM associated data).
		const j = JSON.parse(raw);
		j.items[`evil@${ORIGIN}`] = j.items[`prices@${ORIGIN}`];
		writeFileSync(join(d, 'vault.json'), JSON.stringify(j));
		await assert.rejects(v.get('evil', ORIGIN), /does not open/);
		await assert.rejects(fileVault({ dir: d, passphrase: 'FAKE-wrong-passphrase' }).get('prices', ORIGIN), /does not open/);
	} finally {
		rmSync(d, { recursive: true, force: true });
	}
});

test('vault choice: keychain by default per platform, the file only when asked; keys are checked', async () => {
	assert.match(vaultFromEnv({}, { platform: 'linux', dir: '/nonexistent' }).kind, /secret-tool/);
	assert.match(vaultFromEnv({}, { platform: 'darwin', dir: '/nonexistent' }).kind, /macOS/);
	assert.match(vaultFromEnv({}, { platform: 'win32', dir: '/nonexistent' }).kind, /DPAPI/);
	assert.throws(() => vaultFromEnv({ ONYX_CONTRIB_VAULT: 'file' }, { dir: '/nonexistent' }), /PASSPHRASE/);
	assert.throws(() => vaultFromEnv({ ONYX_CONTRIB_VAULT: 'plain' }, { dir: '/nonexistent' }), /keychain or file/);
	assert.match(checkKey('abc'), /shorter than 12/);
	assert.match(checkKey('FAKE key with spaces'), /printable ASCII/);
	assert.equal(checkKey(KEY), null);
	const r = await runTool('onyx-contrib-no-such-tool', []);
	assert.match(r.stderr, /not installed/);
	// A missing keychain tool is an error that says so, never a silent "no key".
	await assert.rejects(secretToolVault({ cmd: 'onyx-contrib-no-such-tool' }).get('prices', ORIGIN), /not installed.*ONYX_CONTRIB_VAULT=file/);
});
