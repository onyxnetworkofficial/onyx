// The doctor in plain words (CL10): is the key vault usable on this computer, can Onyx be
// reached, is Claude Desktop there. Each check says what was found and, when something is wrong,
// the one thing to do about it. No key is read: the vault check looks up a name that never exists.
import { existsSync } from 'node:fs';
import { claudeConfigPath } from './claude.mjs';
import { runTool, SERVICE } from './vault.mjs';

const FILE_VAULT = 'Or keep keys in an encrypted file instead: choose "Use an encrypted file" in the app, or set ONYX_CONTRIB_VAULT=file and ONYX_CONTRIB_VAULT_PASSPHRASE (12 characters or more) before starting onyx-contrib.';

/** Why a secret-tool lookup failed, in words a person can act on (null when the keyring answered). */
export function secretToolAdvice(r) {
	if (r.code === -1) return { say: 'The Linux keychain tool (secret-tool) is not installed.', fix: `Install libsecret-tools: open a terminal and run "sudo apt install libsecret-tools" (Debian, Ubuntu, Mint) or "sudo dnf install libsecret" (Fedora). ${FILE_VAULT}` };
	const e = (r.stderr || '').toLowerCase();
	if (r.code !== 0 && /(d-bus|dbus|autolaunch|org\.freedesktop\.secrets|secret service|no such interface|not provided)/.test(e))
		return { say: 'secret-tool is installed, but no keyring service is running (no GNOME Keyring or KWallet answered).', fix: `Log in to a desktop session with GNOME Keyring or KWallet (install gnome-keyring if you use another desktop). ${FILE_VAULT}` };
	return null;
}

/**
 * Run every check. `vault` is the vault in use; `run` runs a tool (tests pass a stand-in);
 * `client` is the Onyx client (getHealth). Returns { ok, checks: [{ id, ok, say, fix? }] }.
 */
export async function doctor({ vault, env = process.env, platform = process.platform, run = runTool, client, dir }) {
	const checks = [];
	// 1. The vault.
	const fileMode = (env.ONYX_CONTRIB_VAULT || 'keychain') === 'file' || /encrypted file/.test(vault?.kind ?? '');
	if (!vault) checks.push({ id: 'vault', ok: false, say: 'No key vault could be opened.', fix: FILE_VAULT });
	else if (!fileMode && platform === 'linux') {
		const r = await run('secret-tool', ['lookup', 'service', SERVICE, 'name', 'doctor', 'origin', 'https://doctor.invalid']);
		const bad = secretToolAdvice(r);
		checks.push(bad ? { id: 'vault', ok: false, ...bad } : { id: 'vault', ok: true, say: 'Your keys go in the Linux keychain (GNOME Keyring or KWallet), which answered.' });
	} else {
		try {
			await vault.get('doctor', 'https://doctor.invalid');
			checks.push({ id: 'vault', ok: true, say: fileMode ? 'Your keys go in an encrypted file in your settings folder; it opens with your passphrase.' : platform === 'darwin' ? 'Your keys go in the macOS login keychain, which answered.' : platform === 'win32' ? 'Your keys are protected by Windows for your user account (DPAPI), which answered.' : `Your keys go in: ${vault.kind}.` });
		} catch (e) {
			checks.push({ id: 'vault', ok: false, say: `The key vault did not answer: ${String(e?.message ?? e).slice(0, 200)}`, fix: platform === 'darwin' ? `Unlock your login keychain (Keychain Access). ${FILE_VAULT}` : FILE_VAULT });
		}
	}
	// 2. Onyx.
	if (client) {
		try {
			const r = await client.call('getHealth');
			checks.push(r.status === 200 ? { id: 'onyx', ok: true, say: `Onyx (${client.base}) answers. This is the test network: no real money.` } : { id: 'onyx', ok: false, say: `Onyx (${client.base}) answered ${r.status}.`, fix: 'Try again in a few minutes; if it stays, the test network may be down for maintenance.' });
		} catch {
			checks.push({ id: 'onyx', ok: false, say: `Onyx (${client.base}) could not be reached.`, fix: 'Check your internet connection. Your keys and providers stay on this computer either way.' });
		}
	}
	// 3. Claude Desktop (optional).
	const cfg = claudeConfigPath(env, platform);
	const there = existsSync(cfg) || existsSync(cfg.replace(/claude_desktop_config\.json$/, ''));
	checks.push(there ? { id: 'claude', ok: true, say: 'Claude Desktop is installed on this computer; "Connect to Claude Desktop" adds Onyx to it.' } : { id: 'claude', ok: true, optional: true, say: 'Claude Desktop was not found. You do not need it: this app does everything on its own. To use Onyx from Claude, install Claude Desktop, then press "Connect to Claude Desktop".' });
	if (dir) checks.push({ id: 'settings', ok: true, say: `Your providers and past contributions are kept in ${dir} (never a key in plain text).` });
	return { ok: checks.every((c) => c.ok), checks };
}
