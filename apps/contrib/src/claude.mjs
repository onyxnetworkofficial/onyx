// "Connect to Claude Desktop" (CL10): add onyx-contrib's MCP server to Claude Desktop's settings
// file. The file is merged, never overwritten: every other server and setting stays as it was,
// the old file is copied to a dated backup first, and a file that is not valid JSON is left alone.
// No key and no passphrase is ever written there.
import { randomBytes } from 'node:crypto';
import { chmodSync, copyFileSync, existsSync, mkdirSync, readFileSync, realpathSync, renameSync, statSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';

/** The server's name in Claude Desktop's `mcpServers`. */
export const SERVER_NAME = 'onyx-contrib';

/** Claude Desktop's settings file for this system (ONYX_CLAUDE_CONFIG overrides it, for tests and odd installs). */
export function claudeConfigPath(env = process.env, platform = process.platform, home = homedir()) {
	if (env.ONYX_CLAUDE_CONFIG) return env.ONYX_CLAUDE_CONFIG;
	if (platform === 'darwin') return join(home, 'Library', 'Application Support', 'Claude', 'claude_desktop_config.json');
	if (platform === 'win32') return join(env.APPDATA || join(home, 'AppData', 'Roaming'), 'Claude', 'claude_desktop_config.json');
	return join(env.XDG_CONFIG_HOME || join(home, '.config'), 'Claude', 'claude_desktop_config.json');
}

/**
 * The command Claude Desktop runs: this single-file program with `mcp`, or Node with cli.mjs when
 * run from the source. Settings that change where keys and providers live go along; a passphrase never.
 */
export function mcpEntry({ execPath = process.execPath, sea = false, cliPath, env = process.env }) {
	const entry = sea ? { command: execPath, args: ['mcp'] } : { command: execPath, args: [cliPath, 'mcp'] };
	const keep = {};
	for (const k of ['ONYX_API_URL', 'ONYX_CONTRIB_HOME']) if (env[k]) keep[k] = env[k];
	if (Object.keys(keep).length) entry.env = keep;
	return entry;
}

const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);

/** Whether Claude Desktop's file holds our server, and whether it is the same command. */
export function claudeStatus(path, entry) {
	if (!existsSync(path)) return { path, found: false, connected: false };
	try {
		const cfg = JSON.parse(readFileSync(path, 'utf8'));
		const cur = cfg?.mcpServers?.[SERVER_NAME];
		return { path, found: true, connected: !!cur, current: !!cur && same(cur, entry) };
	} catch {
		return { path, found: true, connected: false, invalid: true };
	}
}

/**
 * Add (or update) our entry in Claude Desktop's file. Returns { path, changed, backup }.
 * Refuses a file that is not a JSON object, and an `mcpServers` that is not an object.
 */
export function connectClaude({ path, entry, now = new Date() }) {
	let cfg = {};
	let had = false;
	if (existsSync(path)) {
		had = true;
		const text = readFileSync(path, 'utf8');
		try {
			cfg = text.trim() === '' ? {} : JSON.parse(text);
		} catch {
			throw new Error(`Claude Desktop's settings file (${path}) is not valid JSON, so it was left as it is. Fix it in Claude Desktop (Settings, Developer, Edit Config), then try again.`);
		}
		if (!cfg || typeof cfg !== 'object' || Array.isArray(cfg)) throw new Error(`Claude Desktop's settings file (${path}) is not a JSON object; it was left as it is.`);
	}
	if (cfg.mcpServers !== undefined && (!cfg.mcpServers || typeof cfg.mcpServers !== 'object' || Array.isArray(cfg.mcpServers))) throw new Error(`"mcpServers" in ${path} is not an object; it was left as it is.`);
	if (cfg.mcpServers && same(cfg.mcpServers[SERVER_NAME], entry)) return { path, changed: false, backup: null };
	let backup = null;
	if (had) {
		backup = `${path}.onyx-backup-${now.toISOString().replace(/[:.]/g, '-')}`;
		copyFileSync(path, backup);
		chmodSync(backup, statSync(realpathSync(path)).mode & 0o777);
	}
	const next = { ...cfg, mcpServers: { ...(cfg.mcpServers ?? {}), [SERVER_NAME]: entry } };
	// Review CL10-R1-3: the file often holds other servers' tokens, so its mode is kept (0600 when
	// new, whatever the umask), and a settings file that is a link is written where it points.
	const real = had ? realpathSync(path) : path;
	const mode = had ? statSync(real).mode & 0o777 : 0o600;
	mkdirSync(dirname(real), { recursive: true });
	const tmp = `${real}.${process.pid}.${randomBytes(4).toString('hex')}.tmp`;
	writeFileSync(tmp, JSON.stringify(next, null, 2) + '\n', { mode, flag: 'wx' });
	chmodSync(tmp, mode);
	renameSync(tmp, real);
	return { path, changed: true, backup };
}
