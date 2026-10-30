// Fetch a provider's endpoint on this computer with the stored key, show the value, and propose
// it to Onyx as a keyed source. What reaches Onyx is only what its intake already takes
// (docs/spec/submissions.md, R-SUB-4/5): the endpoint as a template with {api_key} where the key
// goes, the JSON pointer, the sample value as a line of the description, and the stub prover's
// digest and length of the answer. Never the key, never a header.
//
// Why the sample is a description line and not a `dataPayload`: the intake takes a data sample
// only for the contributor's own data (`what: own`, `rights: own_data`, R-SUB-12) and refuses it
// on a keyed source (`payload_route`). A provider's data is not the contributor's own.
import { randomBytes } from 'node:crypto';
import { cleanDeep, MAX_PROPOSALS, proposalStatus } from '../../agent/src/connect.mjs';
import { keccak256 } from './keccak.mjs';
import { atPointer, CAPS, hasSecretParam, httpsProblem, isJsonPointer, isSecretName, KEY_PLACEHOLDER, keyGuard, leafPointers, paramName, redact, redactTree, STUB_PROVER, utf8 } from './rules.mjs';
import { parseBase } from './providers.mjs';

/** Largest provider answer read on this computer. */
export const MAX_ANSWER = 1024 * 1024;
/** Characters of the sample value written into the description. */
export const SAMPLE_CHARS = 600;
/** Checked proposals kept for confirmation. */
export const KEEP_CHECKS = 16;
const TIMEOUT_MS = 20_000;
const PATH_RE = /^\/[A-Za-z0-9._~!$&'()*+,;=:@%{}/-]*$/;

/** The endpoint's public URL (no key) for `path` and `params`, checked to stay on the provider's origin. */
export function endpointUrl(p, path, params = {}) {
	if (typeof path !== 'string' || !PATH_RE.test(path) || path.startsWith('//') || path.split('/').some((s) => s === '..' || s === '.')) throw new Error('path: a path such as /v3/simple/price, from the endpoint list');
	if (params === null || typeof params !== 'object' || Array.isArray(params)) throw new Error('params: an object of names and values');
	const left = { ...params };
	let filled = path.replace(/\{([A-Za-z0-9_.-]{1,64})\}/g, (_, name) => {
		const v = left[name];
		if (v === undefined) throw new Error(`params: the path needs ${name}`);
		delete left[name];
		const s = String(v);
		if (s === '' || /^\.+$/.test(s)) throw new Error(`params.${name}: a value`);
		return encodeURIComponent(s);
	});
	if (/[{}]/.test(filled)) throw new Error('path: unbalanced { }');
	const { origin, base } = parseBase(p.baseUrl);
	const u = new URL(base + filled);
	for (const [k, v] of Object.entries(left)) {
		if (!/^[A-Za-z0-9_.\-[\]]{1,64}$/.test(k)) throw new Error(`params: ${k} is not a parameter name`);
		const n = paramName(k);
		if ((p.auth.in === 'query' && n === p.auth.name.toLowerCase()) || isSecretName(n)) throw new Error(`params: ${k} looks like a key parameter. Your key is added on this computer; never pass it as a parameter.`);
		if (!['string', 'number', 'boolean'].includes(typeof v) || String(v).length > 200) throw new Error(`params.${k}: a short string, number or boolean`);
		u.searchParams.append(k, String(v));
	}
	// `%2e%2e` survives the check above and a URL parser resolves it: the result must stay under the base path.
	const basePath = new URL(base).pathname.replace(/\/$/, '');
	if (u.origin !== origin || !u.pathname.startsWith(basePath + '/')) throw new Error('path: the endpoint must stay on the provider\'s own host, under its base URL');
	return u.href;
}

/** Read a body, at most `max` bytes. */
async function readCapped(res, max) {
	const len = Number(res.headers.get('content-length') ?? 0);
	if (len > max) throw new Error(`the answer is larger than ${max} bytes`);
	const chunks = [];
	let n = 0;
	for await (const c of res.body ?? []) {
		n += c.length;
		if (n > max) throw new Error(`the answer is larger than ${max} bytes`);
		chunks.push(c);
	}
	return Buffer.concat(chunks);
}

/**
 * Call `path` on provider `p` with `key`, on this computer. The key goes only to the provider's
 * origin: no redirect is followed. Every error text has the key removed. Returns the public URL
 * (no key), the raw bytes and the parsed JSON.
 */
export async function callProvider(p, key, path, params, { fetch = globalThis.fetch } = {}) {
	const publicUrl = endpointUrl(p, path, params);
	const u = new URL(publicUrl);
	const headers = { accept: 'application/json', 'user-agent': 'onyx-contrib/0.1' };
	if (p.auth.in === 'query') u.searchParams.append(p.auth.name, key);
	else headers[p.auth.name] = (p.auth.prefix ?? '') + key;
	try {
		const res = await fetch(u.href, { method: 'GET', headers, redirect: 'manual', signal: AbortSignal.timeout(TIMEOUT_MS) });
		if (res.status >= 300 && res.status < 400) {
			await res.body?.cancel();
			throw new Error(`the provider answered with a redirect (${res.status}); onyx-contrib follows none, so your key goes only to ${u.origin}`);
		}
		const raw = await readCapped(res, MAX_ANSWER);
		if (res.status !== 200) throw new Error(`the provider answered ${res.status}: ${redact(raw.toString('utf8'), [key]).slice(0, 300)}`);
		let json;
		try {
			json = JSON.parse(raw.toString('utf8'));
		} catch {
			throw new Error('the provider\'s answer is not JSON');
		}
		return { publicUrl, raw, json };
	} catch (e) {
		const msg = e?.name === 'TimeoutError' ? `the provider did not answer within ${TIMEOUT_MS / 1000} s` : String(e?.message ?? e);
		throw new Error(redact(msg, [key]));
	}
}

/** What the assistant sees of an answer: the value at `pointer` (or a sketch), with the key removed. */
export function previewOf(p, call, pointer, key) {
	// The parsed answer is redacted (every string and object key, whatever JSON escapes the provider
	// used) before anything is cut, listed or shown, so a provider that echoes the key shows none of it.
	const safe = redactTree(call.json, [key]);
	const out = { provider: p.name, endpoint: call.publicUrl, bytes: call.raw.length, keyRemovedFromAnswer: JSON.stringify(safe) !== JSON.stringify(call.json) };
	if (pointer !== undefined) {
		if (!isJsonPointer(pointer)) throw new Error('pointer: empty or starting with /, as in /bitcoin/usd');
		const at = atPointer(safe, pointer);
		if (at.missing) out.missing = `nothing at ${pointer}`;
		else out.value = cleanDeep(at.value);
		out.pointer = pointer;
	} else out.answer = cleanDeep(safe);
	out.pointers = leafPointers(safe);
	return out;
}

/** The proposal body for Onyx (docs/spec/submissions.md), from a call made on this computer. */
export function buildProposal(p, call, a) {
	const fail = (m) => {
		throw new Error(m);
	};
	const need = (k, max) => {
		if (typeof a[k] !== 'string' || !a[k].trim()) fail(`${k}: required`);
		if (utf8(a[k]) > max) fail(`${k}: at most ${max} bytes (the intake's cap)`);
		return a[k];
	};
	const title = need('title', CAPS.title);
	const description = need('description', CAPS.description - 400);
	if (!['public_domain', 'open_license', 'not_sure', 'terms_forbid'].includes(a.rights)) fail('rights: public_domain, open_license, not_sure or terms_forbid (read the provider\'s terms)');
	if (!['once', 'daily', 'live'].includes(a.changes)) fail('changes: once, daily or live');
	if (typeof a.namesPerson !== 'boolean') fail('namesPerson: true if the data names private people');
	if (!isJsonPointer(a.pointer) || utf8(a.pointer) > CAPS.jsonPointer) fail(`pointer: a JSON pointer of at most ${CAPS.jsonPointer} bytes`);
	const at = atPointer(call.json, a.pointer);
	if (at.missing) fail(`pointer: nothing at ${a.pointer} in the answer`);
	const answers = { what: 'keyed_api', noLogin: false, rights: a.rights, namesPerson: a.namesPerson, changes: a.changes };
	if (a.rights === 'open_license') {
		if (typeof a.licenseId !== 'string' || !/^[A-Za-z0-9.+_:-]{1,64}$/.test(a.licenseId)) fail('licenseId: the licence id, for example CC-BY-4.0');
		answers.licenseId = a.licenseId;
	} else if (a.licenseId !== undefined) fail('licenseId goes with rights open_license only');
	const sourceUrl = a.sourceUrl ?? p.docsUrl ?? `${new URL(p.baseUrl).origin}/`;
	const why = httpsProblem(sourceUrl);
	if (why || hasSecretParam(sourceUrl)) fail(`sourceUrl: ${why ?? 'it carries a secret parameter'}`);
	const sourceName = a.sourceName ?? p.sourceName ?? p.name;
	if (typeof sourceName !== 'string' || utf8(sourceName) > CAPS.sourceName) fail(`sourceName: at most ${CAPS.sourceName} bytes`);

	let sample = JSON.stringify(at.value) ?? 'null';
	if (sample.length > SAMPLE_CHARS) sample = sample.slice(0, SAMPLE_CHARS - 1) + '…';
	const how = p.auth.in === 'query' ? `the key goes in the ${p.auth.name} query parameter, where the template says ${KEY_PLACEHOLDER}` : `the key goes in the ${p.auth.name} header${p.auth.prefix ? ` after "${p.auth.prefix.trim()}"` : ''}`;
	const line = `\n\nRead on the contributor's computer with onyx-contrib at ${new Date().toISOString().slice(0, 19)}Z (${how}; the key stayed there): ${a.pointer} = ${sample}`;
	const body = { answers, title, description: description + line, sourceName, sourceUrl };
	if (utf8(body.description) > CAPS.description) fail(`description: at most ${CAPS.description} bytes with the sample line; shorten it`);
	if (p.auth.in === 'query') {
		const sep = call.publicUrl.includes('?') ? '&' : '?';
		body.endpointTemplate = `${call.publicUrl}${sep}${encodeURIComponent(p.auth.name)}=${KEY_PLACEHOLDER}`;
		const filled = body.endpointTemplate.split(KEY_PLACEHOLDER).join('KEY');
		const w = httpsProblem(filled);
		if (w) fail(`the endpoint: ${w}`);
		if (hasSecretParam(body.endpointTemplate.split(KEY_PLACEHOLDER).join(''))) fail('the endpoint carries another secret parameter');
	} else {
		body.endpointUrl = call.publicUrl;
		const w = httpsProblem(call.publicUrl);
		if (w) fail(`the endpoint: ${w}`);
		if (hasSecretParam(call.publicUrl)) fail('the endpoint carries a secret parameter');
	}
	body.jsonPointer = a.pointer;
	if (a.unit !== undefined) {
		if (typeof a.unit !== 'string' || !a.unit.trim() || utf8(a.unit) > CAPS.unit) fail(`unit: at most ${CAPS.unit} bytes`);
		body.unit = a.unit;
	}
	if (a.seriesId !== undefined) {
		if (typeof a.seriesId !== 'string' || !/^[A-Za-z0-9._:-]{1,64}$/.test(a.seriesId)) fail('seriesId: 1 to 64 characters from A-Z, a-z, 0-9 and . _ - :');
		body.seriesId = a.seriesId;
	}
	// The labelled stub (R-SUB-5): the digest and length of the answer read here; it proves nothing.
	body.keyProof = { prover: STUB_PROVER, real: false, responseKeccak256: keccak256(call.raw), responseBytes: call.raw.length };
	return body;
}

const NOT_OFFERED = 'This Onyx node does not take proposals (it answers 404 or 405 on /v1/submit).';

/**
 * The contribute tool. Without `confirm`: fetch here, build the body, refuse it if it holds the key
 * in any form, ask the node's check (records nothing) and keep the exact body under a checkId.
 * With `confirm: true` and that checkId: send exactly the kept body (no new fetch), at most
 * MAX_PROPOSALS per session.
 */
export async function contribute(ctx, a) {
	const { client, session } = ctx;
	session.checks ??= new Map();
	session.sent ??= 0;
	if (a.confirm === true) {
		const c = typeof a.checkId === 'string' ? session.checks.get(a.checkId) : undefined;
		if (!c) return { ok: false, error: 'confirm needs the checkId of a check in this session. Call contrib_contribute without confirm first, show your user the result, then confirm.' };
		if (c.sent) return { ok: false, error: `That check was sent already (receipt ${c.receipt}).` };
		const guard = keyGuard(c.body, [await ctx.keyFor(c.provider), ...(ctx.keys ?? [])]);
		if (guard) return { ok: false, ...guard };
		if (session.sent >= MAX_PROPOSALS) return { ok: false, error: `This session already sent ${MAX_PROPOSALS} contributions. Start a new one later.` };
		const r = await client.call('submitProposal', { body: c.body });
		if (r.status === 404 || r.status === 405) return { ok: false, error: NOT_OFFERED };
		if (r.status !== 201) return { ok: false, status: r.status, error: cleanDeep(r.body?.error ?? r.body) };
		const receipt = cleanDeep(r.body);
		if (!/^sub_[0-9a-f]{32}$/.test(receipt?.id ?? '')) return { ok: false, error: 'The node answered without a valid receipt id.' };
		session.sent++;
		c.sent = true;
		c.receipt = receipt.id;
		return { ok: true, recorded: true, receipt, next: `Follow it with contrib_status {"id":"${receipt.id}"}. It is held for a review of the provider's terms before anything is published.` };
	}
	if (a.checkId !== undefined) return { ok: false, error: 'checkId goes with confirm: true only.' };
	const p = ctx.provider(a.provider);
	const key = await ctx.keyFor(p.name);
	const call = await callProvider(p, key, a.path, a.params ?? {}, { fetch: ctx.fetch });
	const body = buildProposal(p, call, a);
	const guard = keyGuard(body, [key, ...(ctx.keys ?? [])]);
	if (guard) return { ok: false, ...guard };
	const check = await client.call('checkSubmission', { body });
	if (check.status === 404 || check.status === 405) return { ok: false, error: NOT_OFFERED };
	if (check.status !== 200) return { ok: false, status: check.status, error: cleanDeep(check.body?.error ?? check.body), sentForCheck: body };
	const checked = cleanDeep(check.body);
	if (!check.body?.valid) return { ok: false, recorded: false, check: checked, next: 'Fix what the check names and try again.' };
	const checkId = 'chk_' + randomBytes(8).toString('hex');
	session.checks.set(checkId, { body, provider: p.name });
	while (session.checks.size > KEEP_CHECKS) session.checks.delete(session.checks.keys().next().value);
	return {
		ok: true,
		recorded: false,
		checkId,
		check: checked,
		willSend: body,
		next: 'The node checked it and recorded nothing. Show your user `willSend` (it holds no key). Only if they agree, call contrib_contribute with {"checkId": "' + checkId + '", "confirm": true}.',
	};
}

export const status = (client, id) => proposalStatus(client, id);
