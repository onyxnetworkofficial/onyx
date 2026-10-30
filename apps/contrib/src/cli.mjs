#!/usr/bin/env node
// onyx-contrib: the terminal side of the contributor client (CL2). Keys are set here, and only here,
// from a hidden prompt or from standard input; they go straight into the system keychain.
//
//   onyx-contrib provider add <name> --base-url <https URL> (--header <name> [--prefix "Bearer "] | --query <name>)
//                [--openapi <https URL or file.json>] [--docs <https URL>] [--source-name <text>] [--replace]
//   onyx-contrib provider list | provider remove <name>
//   onyx-contrib key set <name>      hidden prompt (or the key on stdin: `onyx-contrib key set x < keyfile`)
//   onyx-contrib key remove <name>
//   onyx-contrib endpoints <name> [search words]
//   onyx-contrib preview <name> <path> [--param k=v]... [--pointer /a/b]
//   onyx-contrib keyed join <name> --manifest FILE [--repo DIR] --observer-key FILE [--operator ID] [--yes]
//   onyx-contrib keyed observe <name> --manifest FILE [--repo DIR] -- <level2-keyed observer flags>
//   onyx-contrib keyed seats         keyed seats this computer holds (CL4; README section 7)
//   onyx-contrib mcp                 the MCP server on stdio (what Claude Desktop or Claude Code start)
//   onyx-contrib doctor              which vault this machine uses, and whether it works
import { realpathSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { callProvider, previewOf } from './contrib.mjs';
import { checkProviderFits, joinPreview, joinSeat, keyedBin, listSeats, observe, readKeyedManifest } from './keyed.mjs';
import { serve } from './mcp.mjs';
import { configDir, listEndpoints, loadOpenApi, registry, shown } from './providers.mjs';
import { redact } from './rules.mjs';
import { checkKey, vaultFromEnv } from './vault.mjs';

/** Read a line from the terminal without echoing it. */
export function hiddenPrompt(question, { input = process.stdin, output = process.stderr } = {}) {
	return new Promise((resolve, reject) => {
		output.write(question);
		input.setRawMode(true);
		input.resume();
		input.setEncoding('utf8');
		let s = '';
		const done = (err) => {
			input.setRawMode(false);
			input.pause();
			input.removeListener('data', onData);
			output.write('\n');
			err ? reject(err) : resolve(s);
		};
		const onData = (chunk) => {
			for (const ch of chunk) {
				if (ch === '\r' || ch === '\n') return done();
				if (ch === '\u0003') return done(new Error('cancelled'));
				if (ch === '\u007f' || ch === '\b') s = s.slice(0, -1);
				else if (ch >= ' ') s += ch;
			}
		};
		input.on('data', onData);
	});
}

async function readStdin(input = process.stdin) {
	const chunks = [];
	let n = 0;
	for await (const c of input) {
		n += c.length;
		if (n > 8192) throw new Error('more than 8 KiB on stdin; give the key alone');
		chunks.push(c);
	}
	return Buffer.concat(chunks).toString('utf8').replace(/\r?\n$/, '');
}

function flags(args) {
	const out = { _: [], param: [] };
	for (let i = 0; i < args.length; i++) {
		const a = args[i];
		if (!a.startsWith('--')) {
			out._.push(a);
			continue;
		}
		const k = a.slice(2);
		if (k === 'replace' || k === 'yes') out[k] = true;
		else if (i + 1 >= args.length) throw new Error(`${a} needs a value`);
		else if (k === 'param') out.param.push(args[++i]);
		else out[k] = args[++i];
	}
	return out;
}

async function confirm(question, { input = process.stdin, output = process.stderr } = {}) {
	output.write(question + ' [y/N] ');
	const { createInterface } = await import('node:readline');
	const rl = createInterface({ input, output });
	const answer = await new Promise((r) => rl.once('line', r));
	rl.close();
	return /^y(es)?$/i.test(answer.trim());
}

/** Run the CLI with `argv` (no node and script name). Returns the exit code. */
export async function main(argv, { env = process.env, stdin = process.stdin, out = (s) => process.stdout.write(s + '\n'), err = (s) => process.stderr.write(s + '\n'), vault, fetch = globalThis.fetch } = {}) {
	const dir = configDir(env);
	const reg = registry(dir);
	const v = () => vault ?? vaultFromEnv(env, { dir });
	const [cmd, sub, ...rest] = argv;
	const usage = () => {
		err('usage: onyx-contrib provider add|list|remove, key set|remove <name>, endpoints <name>, preview <name> <path>, keyed join|observe|seats, mcp, doctor (README)');
		return 2;
	};
	try {
		if (cmd === 'mcp') {
			serve({ env, fetch });
			return null;
		}
		if (cmd === 'doctor') {
			const vv = v();
			out(`config folder: ${dir}`);
			out(`vault: ${vv.kind}`);
			try {
				await vv.get('doctor', 'https://doctor.invalid');
				out('vault check: works (a lookup ran; no key was read)');
			} catch (e) {
				out(`vault check: FAILED: ${e.message}`);
			}
			out(`Onyx API: ${env.ONYX_API_URL || 'https://onyx.cryptagraph.com'}`);
			return 0;
		}
		if (cmd === 'provider' && sub === 'add') {
			const f = flags(rest);
			const name = f._[0];
			if (!name || !f['base-url'] || !!f.header === !!f.query) return usage();
			const auth = f.header ? { in: 'header', name: f.header, ...(f.prefix ? { prefix: f.prefix.endsWith(' ') ? f.prefix : f.prefix + ' ' } : {}) } : { in: 'query', name: f.query };
			const rec = { name, baseUrl: f['base-url'], auth };
			if (f.openapi) rec.openapi = f.openapi;
			if (f.docs) rec.docsUrl = f.docs;
			if (f['source-name']) rec.sourceName = f['source-name'];
			const old = reg.get(name);
			const p = reg.add(rec, { addedBy: 'you', replace: !!f.replace, allowFile: true });
			if (old && new URL(old.baseUrl).origin !== new URL(p.baseUrl).origin) err(`Note: the host changed, so the key stored for ${old.baseUrl} no longer applies. Run: onyx-contrib key set ${name}`);
			out(`Added ${p.name}: ${p.baseUrl}, key sent as ${shown(p, false).keySentAs}. Next: onyx-contrib key set ${p.name}`);
			return 0;
		}
		if (cmd === 'provider' && sub === 'list') {
			const vv = v();
			for (const p of reg.list()) {
				const has = !!(await vv.get(p.name, new URL(p.baseUrl).origin).catch(() => null));
				out(`${p.name}  ${p.baseUrl}  key ${has ? 'stored' : 'NOT stored'}  (added by ${p.addedBy})`);
			}
			return 0;
		}
		if (cmd === 'provider' && sub === 'remove') {
			const p = reg.get(rest[0]);
			if (!p) throw new Error(`no provider ${rest[0]}`);
			await v().remove(p.name, new URL(p.baseUrl).origin);
			reg.remove(p.name);
			out(`Removed ${p.name} and its stored key.`);
			return 0;
		}
		if (cmd === 'key' && (sub === 'set' || sub === 'remove')) {
			const f = flags(rest);
			const p = reg.get(f._[0] ?? '');
			if (!p) throw new Error(`no provider ${f._[0] ?? ''}: add it first (onyx-contrib provider add …)`);
			const origin = new URL(p.baseUrl).origin;
			if (sub === 'remove') {
				await v().remove(p.name, origin);
				out(`Removed the key for ${p.name}.`);
				return 0;
			}
			const tty = !!stdin.isTTY;
			err(`The key for ${p.name} will be sent only to ${origin}, ${p.auth.in === 'header' ? `in the ${p.auth.name} header` : `as the ${p.auth.name} query parameter`}.`);
			if (p.addedBy === 'assistant') err('This provider was added by an AI assistant. Check that host is really the API your key belongs to.');
			if (tty && !f.yes && !(await confirm('Continue?', { input: stdin }))) return 1;
			// Without a terminal there is no question to answer: a provider the assistant added then needs --yes.
			if (!tty && p.addedBy === 'assistant' && !f.yes) throw new Error(`${p.name} was added by an AI assistant: check its host, then run again with --yes`);
			const key = tty ? await hiddenPrompt(`Paste the key for ${p.name} (hidden): `, { input: stdin }) : await readStdin(stdin);
			const why = checkKey(key);
			if (why) throw new Error(why);
			const vault = v();
			await vault.set(p.name, origin, key);
			// Read it back: some keychain tools (macOS `security -i`) exit 0 when a command inside failed.
			if ((await vault.get(p.name, origin)) !== key) throw new Error('the keychain did not keep the key (it reads back differently); nothing was stored that onyx-contrib can use');
			out(`Stored the key for ${p.name} in the ${v().kind}. It is not shown again.`);
			return 0;
		}
		if (cmd === 'endpoints') {
			const p = reg.get(sub ?? '');
			if (!p) throw new Error(`no provider ${sub ?? ''}`);
			const r = listEndpoints(p, await loadOpenApi(p, { fetch }), { search: rest.join(' ') });
			for (const e of r.endpoints) out(`${e.path}  ${e.summary}  ${e.params.map((x) => `${x.name}${x.required ? '*' : ''}`).join(' ')}`);
			out(`${r.endpoints.length} of ${r.total} shown`);
			return 0;
		}
		if (cmd === 'preview') {
			const p = reg.get(sub ?? '');
			if (!p) throw new Error(`no provider ${sub ?? ''}`);
			const f = flags(rest);
			const params = Object.fromEntries(f.param.map((kv) => [kv.slice(0, kv.indexOf('=')), kv.slice(kv.indexOf('=') + 1)]));
			const key = await v().get(p.name, new URL(p.baseUrl).origin);
			if (!key) throw new Error(`no key stored for ${p.name}: onyx-contrib key set ${p.name}`);
			try {
				const call = await callProvider(p, key, f._[0], params, { fetch });
				out(redact(JSON.stringify(previewOf(p, call, f.pointer, key), null, 1), [key]));
			} catch (e) {
				throw new Error(redact(e.message, [key]));
			}
			return 0;
		}
		if (cmd === 'keyed') return await keyedCmd({ sub, rest, reg, v, dir, env, stdin, out, err, usage });
		return usage();
	} catch (e) {
		err(`onyx-contrib: ${e.message}`);
		return 1;
	}
}

/**
 * CL4 (ADR-0054): `keyed join` takes a seat in a keyed source's pool after showing its terms;
 * `keyed observe` runs one keyed observer with the key from the vault (on its stdin only);
 * `keyed seats` lists the seats this computer holds.
 */
async function keyedCmd({ sub, rest, reg, v, dir, env, stdin, out, err, usage }) {
	const cut = rest.indexOf('--');
	const own = cut < 0 ? rest : rest.slice(0, cut);
	const pass = cut < 0 ? [] : rest.slice(cut + 1);
	if (sub === 'seats') {
		for (const s of listSeats(dir)) out(JSON.stringify(s));
		return 0;
	}
	const f = flags(own);
	const p = reg.get(f._[0] ?? '');
	if (!p || !f.manifest) return usage();
	const m = readKeyedManifest(f.manifest);
	const repo = f.repo ?? '.';
	checkProviderFits(p, m);
	const origin = new URL(p.baseUrl).origin;
	if (sub === 'join') {
		if (!f['observer-key']) return usage();
		const key = await v().get(p.name, origin);
		if (!key) throw new Error(`no key stored for ${p.name}: onyx-contrib key set ${p.name} (a seat without a key could never observe)`);
		const preview = joinPreview(p, m, { operator: f.operator });
		err(redact(JSON.stringify(preview, null, 1), [key]));
		const tty = !!stdin.isTTY;
		if (!f.yes) {
			if (!tty) throw new Error('read the terms above, then run again with --yes to accept them and take the seat');
			if (!(await confirm('Do the provider\'s terms allow this, and do you accept them?', { input: stdin }))) return 1;
		}
		const { seat, path } = await joinSeat({ dir, p, m, repo, observerKey: f['observer-key'], operator: f.operator, bin: keyedBin(env) });
		out(redact(JSON.stringify(seat), [key]));
		out(`Seat stored in ${path}. Send this seat (it holds no key) to the round's coordinator.`);
		return 0;
	}
	if (sub === 'observe') {
		const key = await v().get(p.name, origin);
		if (!key) throw new Error(`no key stored for ${p.name}: onyx-contrib key set ${p.name}`);
		return await observe({ p, m, key, repo, bin: keyedBin(env), args: pass, env, out, err });
	}
	return usage();
}

const self = fileURLToPath(import.meta.url);
let started = '';
try {
	started = process.argv[1] ? realpathSync(process.argv[1]) : '';
} catch {}
if (started === self)
	main(process.argv.slice(2)).then((code) => {
		if (code !== null) process.exitCode = code;
	});
