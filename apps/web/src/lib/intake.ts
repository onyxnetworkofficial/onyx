// Data submission intake in the browser (ROADMAP S1, ADR-0038): the same rules as the Rust crate
// `onyx-intake` (crates/onyx-intake), so the page can route and check a submission with no API.
// Both run fixtures/intake/vectors.json (apps/web/tests/s1.mjs here, crates/onyx-intake/tests/vectors.rs
// there), so the two cannot drift (THREATS SUB-7). Plain TypeScript with erasable syntax only: the test
// imports this file directly with Node's type stripping.

export type Kind = 'chain' | 'public_api' | 'signed' | 'own' | 'forecast' | 'keyed_api';
export type Rights = 'public_domain' | 'open_license' | 'own_data' | 'not_sure' | 'terms_forbid';
export type Changes = 'once' | 'daily' | 'live';
export type Answers = { what: Kind; noLogin: boolean; rights: Rights; licenseId?: string; namesPerson: boolean; changes: Changes };
export type KeyProof = { prover: string; real: boolean; responseKeccak256?: string; responseBytes?: number | bigint; proof?: string };
export type DataPayload = { format: 'csv' | 'json'; content: string; bytes: number; keccak256: string };
export type Submission = {
	answers: Answers;
	title: string;
	description: string;
	sourceName: string;
	sourceUrl: string;
	endpointUrl?: string;
	jsonPointer?: string;
	unit?: string;
	endpointTemplate?: string;
	seriesId?: string;
	keyProof?: KeyProof;
	dataPayload?: DataPayload;
};
export type Outcome = 'accept' | 'held' | 'refused';
export type Path = 'recipe_request' | 'auto_pull' | 'signed_source' | 'claim' | 'track_record' | 'keyed_proof';
export type Route = { outcome: Outcome; level: number | null; path: Path | null; reasons: string[]; status: string };

export const KINDS: Kind[] = ['chain', 'public_api', 'signed', 'own', 'forecast', 'keyed_api'];
export const RIGHTS: Rights[] = ['public_domain', 'open_license', 'own_data', 'not_sure', 'terms_forbid'];
export const CHANGES: Changes[] = ['once', 'daily', 'live'];

export const LEVEL_NAMES: Record<number, string> = { 1: 'Recomputed', 2: 'Re-observed', 3: 'Source-proven', 4: 'Claim (unverified)', 5: 'Track record' };

/** The sentence for each reason code (the Rust crate's `Reason::text`; pinned by the vectors). */
export const REASONS: Record<string, string> = {
	names_person: 'Onyx never publishes data about a person. Data may name companies, funds and protocols only.',
	terms_forbid: "The source's terms forbid redistribution, so Onyx cannot take it.",
	rights_review: 'The rights are not known yet. It is held for a rights review and never published until a reviewer clears it.',
	chain_recipe: 'Chain data can be recomputed by anyone. It needs a recipe: a small open program that reads pinned blocks. Your submission becomes a recipe request.',
	public_auto_pull:
		"Anyone can fetch it with no login, so it can reach level 2 once a reviewer clears its source's rights: bonded observers would then re-read it at the same time and must agree (built for the U.S. Treasury source after approval; other sources need their own adapter, ROADMAP S4).",
	live_window: 'It changes continuously, so observers must read it inside one short window and agree within a tolerance.',
	needs_login: 'It needs a login, key or payment, so observers cannot re-read it. It stays a claim until a way to prove the response exists (ROADMAP S2).',
	signed_key: "The source signs it. Once the source registers its signing key, anyone can check each value's signature.",
	own_claim: 'It is your own or private data. Onyx records what you say it means as a claim.',
	unverified: 'Labelled unverified: nobody has checked it. Buyers see it as level 4.',
	forecast_track: 'A forecast cannot be checked today. It is scored later against the real answer, and your track record builds up.',
	keyed_source:
		'It needs your own API key. Your key stays in your browser. A real prover (ROADMAP S2) would make the request from your side and send Onyx only a proof of the response; the stub in this build makes no request. A keyed source is held for a rights review of its terms and never published until cleared.',
	proof_not_real: 'The proof step is not built yet (ROADMAP S2). Today the page makes no request and sends a placeholder labelled not real; it proves nothing.',
	source_blocked: "Onyx's rights review blocks this source: its terms restrict redistribution. It is held and will not be published."
};

/** Why the server does not pull (the Rust crate's `NoFetch::text`). */
export const NO_FETCH: Record<string, string> = {
	no_endpoint: 'No endpoint was given, so there is nothing to pull.',
	not_while_refused_or_held: 'Nothing is pulled for a refused or held submission.',
	not_auto_pull: 'Only public data anyone can fetch without a login is pulled.',
	host_not_cleared: "Onyx pulls only from sources whose rights it has already reviewed. A reviewer reads this source's terms first; nothing is fetched until then.",
	port_not_allowed: 'Only the standard https port (443) is pulled.'
};

/** Hosts a server may pull from (rights-cleared, docs/ops/data-sources.md), sorted. */
export const CLEARED_HOSTS = ['api.fiscaldata.treasury.gov'];
/** Registrable domains the rights review blocks (FRED, N10), sorted: a submission naming one or any subdomain is held (S1-R1-3). */
export const BLOCKED_HOSTS = ['stlouisfed.org'];
/** Byte caps; responseBytes at most 2^53 - 1 (exact in every JSON reader); a typed key shorter than keyMin is refused (S1-R1-1). */
export const CAPS = { title: 120, description: 2000, sourceName: 120, url: 512, jsonPointer: 256, unit: 32, licenseId: 64, seriesId: 64, responseBytes: Number.MAX_SAFE_INTEGER, keyMin: 12, payloadBytes: 8192 };
export const KEY_PLACEHOLDER = '{api_key}';
export const STUB_PROVER = 'stub-s2';
/** Secret parameter names matched whole, and parts that make any name a secret (S1-R1-5; the Rust lists). */
export const SECRET_PARAMS = ['appid', 'auth', 'authorization', 'pwd', 'sig'];
export const SECRET_PARTS = ['credential', 'key', 'passw', 'secret', 'signature', 'token'];

const utf8 = (s: string) => new TextEncoder().encode(s).length;
const isAlnum = (c: string) => /^[A-Za-z0-9]$/.test(c);

export function classify(a: Answers): Route {
	const refusals: string[] = [];
	if (a.namesPerson) refusals.push('names_person');
	if (a.rights === 'terms_forbid') refusals.push('terms_forbid');
	if (refusals.length) return { outcome: 'refused', level: null, path: null, reasons: refusals, status: 'refused' };
	let level = 0;
	let path: Path = 'claim';
	let reasons: string[] = [];
	switch (a.what) {
		case 'chain':
			[level, path, reasons] = [1, 'recipe_request', ['chain_recipe']];
			break;
		case 'public_api':
			if (a.noLogin) [level, path, reasons] = [2, 'auto_pull', a.changes === 'live' ? ['public_auto_pull', 'live_window'] : ['public_auto_pull']];
			else [level, path, reasons] = [4, 'claim', ['needs_login', 'unverified']];
			break;
		case 'signed':
			[level, path, reasons] = [3, 'signed_source', ['signed_key']];
			break;
		case 'own':
			[level, path, reasons] = [4, 'claim', ['own_claim', 'unverified']];
			break;
		case 'forecast':
			[level, path, reasons] = [5, 'track_record', ['forecast_track']];
			break;
		case 'keyed_api':
			[level, path, reasons] = [4, 'keyed_proof', ['keyed_source', 'proof_not_real', 'unverified']];
			break;
	}
	let outcome: Outcome = 'accept';
	if (a.rights === 'not_sure') {
		reasons.unshift('rights_review');
		outcome = 'held';
	}
	if (a.what === 'keyed_api') outcome = 'held';
	return withStatus({ outcome, level, path, reasons, status: '' });
}

function withStatus(r: Route): Route {
	r.status = r.outcome === 'refused' ? 'refused' : r.outcome === 'accept' ? 'pending_review' : r.path === 'keyed_proof' ? 'held_keyed_source_review' : 'held_rights_review';
	return r;
}

export function isBlockedHost(host: string): boolean {
	return BLOCKED_HOSTS.some((b) => host === b || (host.endsWith(b) && host.slice(0, -b.length).endsWith('.')));
}

/** The route of a whole submission: the answers' route, held when any URL names a blocked host. */
export function routeFor(s: Submission): Route {
	const r = classify(s.answers);
	if (r.outcome === 'refused') return r;
	const urls = [s.sourceUrl || undefined, s.endpointUrl, s.endpointTemplate?.split(KEY_PLACEHOLDER).join('KEY')];
	const blocked = urls.some((u) => {
		if (u === undefined) return false;
		const p = parseHttpsUrl(u);
		return typeof p !== 'string' && isBlockedHost(p.host);
	});
	if (blocked) {
		r.outcome = 'held';
		r.reasons.unshift('source_blocked');
	}
	return withStatus(r);
}

export type HttpsUrl = { url: string; host: string; port: number | null };

/** An https URL with a plain host name (the Rust `HttpsUrl::parse`); a string says why not. */
export function parseHttpsUrl(s: string): HttpsUrl | string {
	if (utf8(s) > CAPS.url) return 'over 512 bytes';
	if (![...s].every((c) => c.charCodeAt(0) >= 0x21 && c.charCodeAt(0) <= 0x7e)) return 'only visible ASCII, no spaces';
	if ([...s].some((c) => '\\"<>^`{|}'.includes(c))) return 'characters a URL may not hold';
	if (!s.startsWith('https://')) return 'https only';
	const rest = s.slice(8);
	if (s.includes('#')) return 'no fragment';
	const m = rest.search(/[/?]/);
	const end = m < 0 ? rest.length : m;
	const authority = rest.slice(0, end);
	if (authority.includes('@')) return 'no user name or password in the URL';
	let host = authority;
	let port: number | null = null;
	const colon = authority.lastIndexOf(':');
	if (colon >= 0) {
		const p = authority.slice(colon + 1);
		host = authority.slice(0, colon);
		if (!/^[0-9]{1,5}$/.test(p)) return 'bad port';
		const n = Number(p);
		if (n < 1 || n > 65535) return 'bad port';
		port = n;
	}
	host = host.toLowerCase();
	if (host === '' || host.length > 253) return 'no host';
	if (host.startsWith('[') || /^(?:(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])\.){3}(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])$/.test(host)) return 'a host name, not an IP address';
	const labelsOk = host.split('.').every((l) => l.length > 0 && l.length <= 63 && !l.startsWith('-') && !l.endsWith('-') && /^[a-z0-9-]+$/.test(l));
	if (!labelsOk || !host.includes('.')) return 'a public host name (letters, digits, dots, hyphens)';
	const last = host.split('.').pop() ?? '';
	if (/^[0-9]+$/.test(last) || last.startsWith('0x')) return 'a host name, not an IP address';
	return { url: s, host, port };
}

/** Something shaped like an e-mail address, or a mailto: link (the Rust `has_email`). */
export function hasEmail(text: string): boolean {
	if (text.toLowerCase().includes('mailto:')) return true;
	const local = (c: string) => isAlnum(c) || '._%+-'.includes(c);
	const domain = (c: string) => isAlnum(c) || '.-'.includes(c);
	for (let i = 0; i < text.length; i++) {
		if (text[i] !== '@') continue;
		const before = i > 0 && local(text[i - 1]);
		let j = i + 1;
		while (j < text.length && domain(text[j])) j++;
		const after = text.slice(i + 1, j).replace(/[.-]+$/, '');
		const dot = after.lastIndexOf('.');
		const dotted = dot > 0 && after.length - dot - 1 >= 2 && /^[A-Za-z]+$/.test(after.slice(dot + 1));
		if (before && dotted) return true;
	}
	return false;
}

export function isJsonPointer(p: string): boolean {
	if (p === '') return true;
	if (!p.startsWith('/') || /[\u0000-\u001f\u007f-\u009f]/.test(p)) return false;
	for (let i = 0; i < p.length; i++) if (p[i] === '~' && p[i + 1] !== '0' && p[i + 1] !== '1') return false;
	return true;
}

/** A parameter name as a server reads it: percent-decoded, ASCII lower case, no array suffix. */
export function paramName(raw: string): string {
	let out = raw.replace(/%([0-9A-Fa-f]{2})/g, (_, h: string) => String.fromCharCode(parseInt(h, 16)));
	const b = out.indexOf('[');
	if (b >= 0) out = out.slice(0, b);
	return out.replace(/[A-Z]/g, (c) => c.toLowerCase());
}
export const isSecretName = (n: string) => SECRET_PARAMS.includes(n) || n.startsWith('x-amz-') || SECRET_PARTS.some((p) => n.includes(p));

/** Whether a URL's query gives a value to a secret parameter (pairs split on & and ;). */
export function hasSecretParam(u: string): boolean {
	const q = u.indexOf('?');
	if (q < 0) return false;
	return u
		.slice(q + 1)
		.split(/[&;]/)
		.some((pair) => {
			const eq = pair.indexOf('=');
			const [name, value] = eq < 0 ? [pair, ''] : [pair.slice(0, eq), pair.slice(eq + 1)];
			return value !== '' && isSecretName(paramName(name));
		});
}

export class IntakeError extends Error {
	code: string;
	constructor(code: string, message: string) {
		super(message);
		this.code = code;
	}
}

const fail = (code: string, message: string): never => {
	throw new IntakeError(code, message);
};

const ANSWER_KEYS = ['what', 'noLogin', 'rights', 'licenseId', 'namesPerson', 'changes'];
const SUB_KEYS = ['answers', 'title', 'description', 'sourceName', 'sourceUrl', 'endpointUrl', 'jsonPointer', 'unit', 'endpointTemplate', 'seriesId', 'keyProof', 'dataPayload'];
const PROOF_KEYS = ['prover', 'real', 'responseKeccak256', 'responseBytes', 'proof'];
const isObj = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);

function shape(v: unknown, keys: string[], required: string[]): Record<string, unknown> {
	if (!isObj(v)) fail('bad_json', 'not an object');
	const o = v as Record<string, unknown>;
	for (const k of Object.keys(o)) if (!keys.includes(k)) fail('bad_json', `unknown field ${k}`);
	for (const k of required) if (!(k in o) || o[k] === null) fail('bad_json', `missing field ${k}`);
	return o;
}
const optStr = (o: Record<string, unknown>, k: string): string | undefined => {
	const v = o[k];
	if (v === undefined || v === null) return undefined;
	if (typeof v !== 'string') fail('bad_json', `${k} must be a string`);
	return v as string;
};
const str = (o: Record<string, unknown>, k: string): string => {
	if (typeof o[k] !== 'string') fail('bad_json', `${k} must be a string`);
	return o[k] as string;
};
const bool = (o: Record<string, unknown>, k: string): boolean => {
	if (typeof o[k] !== 'boolean') fail('bad_json', `${k} must be true or false`);
	return o[k] as boolean;
};
function oneOf<T extends string>(o: Record<string, unknown>, k: string, all: T[]): T {
	if (!all.includes(o[k] as T)) fail('bad_json', `${k} is not one of the choices`);
	return o[k] as T;
}

/** The JSON shape (serde's `deny_unknown_fields` rules), then every check: an IntakeError with the vectors' code. */
export function parseSubmission(v: unknown): Submission {
 const o = shape(v, SUB_KEYS, ['answers', 'title', 'description', 'sourceName', 'sourceUrl']);
	const ao = shape(o.answers, ANSWER_KEYS, ['what', 'noLogin', 'rights', 'namesPerson', 'changes']);
	const answers: Answers = { what: oneOf(ao, 'what', KINDS), noLogin: bool(ao, 'noLogin'), rights: oneOf(ao, 'rights', RIGHTS), namesPerson: bool(ao, 'namesPerson'), changes: oneOf(ao, 'changes', CHANGES) };
	const lic = optStr(ao, 'licenseId');
	if (lic !== undefined) answers.licenseId = lic;
	const s: Submission = { answers, title: str(o, 'title'), description: str(o, 'description'), sourceName: str(o, 'sourceName'), sourceUrl: str(o, 'sourceUrl') };
	for (const k of ['endpointUrl', 'jsonPointer', 'unit', 'endpointTemplate', 'seriesId'] as const) {
		const x = optStr(o, k);
		if (x !== undefined) s[k] = x;
	}
	if (o.dataPayload !== undefined && o.dataPayload !== null) {
		const dp = shape(o.dataPayload, ['format', 'content', 'bytes', 'keccak256'], ['format', 'content', 'bytes', 'keccak256']);
		if (dp.format !== 'csv' && dp.format !== 'json') fail('bad_json', 'dataPayload format must be csv or json');
		const payloadBytes = dp.bytes;
		if (typeof payloadBytes !== 'number' || !Number.isInteger(payloadBytes)) fail('bad_json', 'dataPayload bytes must be a whole number');
		s.dataPayload = { format: dp.format as 'csv' | 'json', content: str(dp, 'content'), bytes: payloadBytes as number, keccak256: str(dp, 'keccak256') };
	}
	if (o.keyProof !== undefined && o.keyProof !== null) {
		const p = shape(o.keyProof, PROOF_KEYS, ['prover', 'real']);
		const kp: KeyProof = { prover: str(p, 'prover'), real: bool(p, 'real') };
		const h = optStr(p, 'responseKeccak256');
		if (h !== undefined) kp.responseKeccak256 = h;
		if (p.responseBytes !== undefined && p.responseBytes !== null) {
			// A whole number from 0 to 2^64 - 1, as serde reads a u64 (larger ones are refused by validate).
			const n = p.responseBytes;
			const ok = (typeof n === 'number' && Number.isInteger(n) && n >= 0) || (typeof n === 'bigint' && n >= 0n && n <= 18446744073709551615n);
			if (!ok) fail('bad_json', 'responseBytes must be a whole number');
			kp.responseBytes = n as number | bigint;
		}
		const pr = optStr(p, 'proof');
		if (pr !== undefined) kp.proof = pr;
		s.keyProof = kp;
	}
	validate(s);
	return s;
}

// Unicode White_Space and general category Cf, the Rust crate's lists (S1-R1-4).
const WHITE_SPACE: [number, number][] = [[0x09, 0x0d], [0x20, 0x20], [0x85, 0x85], [0xa0, 0xa0], [0x1680, 0x1680], [0x2000, 0x200a], [0x2028, 0x2029], [0x202f, 0x202f], [0x205f, 0x205f], [0x3000, 0x3000]];
const FORMAT: [number, number][] = [
	[0xad, 0xad], [0x600, 0x605], [0x61c, 0x61c], [0x6dd, 0x6dd], [0x70f, 0x70f], [0x890, 0x891], [0x8e2, 0x8e2], [0x180e, 0x180e], [0x200b, 0x200f], [0x202a, 0x202e], [0x2060, 0x2064],
	[0x2066, 0x206f], [0xfeff, 0xfeff], [0xfff9, 0xfffb], [0x110bd, 0x110bd], [0x110cd, 0x110cd], [0x13430, 0x1343f], [0x1bca0, 0x1bca3], [0x1d173, 0x1d17a], [0xe0001, 0xe0001], [0xe0020, 0xe007f]
];
const inRanges = (c: number, r: [number, number][]) => r.some(([a, b]) => c >= a && c <= b);
/** Nothing visible: only white space and format characters. */
export const isBlank = (s: string) => [...s].every((ch) => inRanges(ch.codePointAt(0)!, WHITE_SPACE) || inRanges(ch.codePointAt(0)!, FORMAT));

function text(field: string, s: string, max: number, lines: boolean) {
	if (isBlank(s)) fail('empty_field', `${field} is empty`);
	optText(field, s, max, lines);
}
function optText(field: string, s: string, max: number, lines: boolean) {
	if (utf8(s) > max) fail('field_too_long', `${field} is over ${max} bytes`);
	const ctl = lines ? /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f\ufeff]/ : /[\u0000-\u001f\u007f-\u009f\ufeff]/;
	if (ctl.test(s)) fail('control_characters', `${field} holds control characters`);
	if (hasEmail(s)) fail('personal_contact', `${field} holds an e-mail address; Onyx takes no personal contact details`);
}
function url(field: string, s: string) {
	const p = parseHttpsUrl(s);
	if (typeof p === 'string') fail('bad_url', `${field} must be an https URL with a host name: ${p}`);
}

export function validateDataPayload(p: DataPayload): void {
	const n = utf8(p.content);
	if (n === 0 || n > CAPS.payloadBytes) fail('payload_invalid', 'the data sample must be 1 to 8,192 UTF-8 bytes');
	if (p.bytes !== n || !/^0x[0-9a-f]{64}$/.test(p.keccak256)) fail('payload_invalid', 'the data sample byte count or digest is malformed');
	if (/[\u0000\ufeff]/.test(p.content)) fail('payload_invalid', 'control or invisible characters are not accepted');
	const lower = p.content.toLowerCase();
	if ((lower.includes('<') && lower.includes('>')) || ['<!doctype', '<?php', 'javascript:'].some((x) => lower.includes(x))) fail('payload_invalid', 'markup or executable content is not accepted');
	if (['-----begin ', 'bearer ', 'api_key', 'apikey', 'access_token', 'client_secret', 'authorization', 'password'].some((x) => lower.includes(x))) fail('payload_invalid', 'possible credential detected');
	if (hasEmail(p.content)) fail('payload_invalid', 'personal contact data is not accepted');
	const privateFields = ['email', 'phone', 'person', 'person_name', 'full_name', 'first_name', 'last_name', 'customer', 'user', 'address', 'ssn', 'social_security', 'birth_date', 'dob'];
	if (p.format === 'json') {
		if (hasDuplicateJsonKeys(p.content)) fail('payload_invalid', 'duplicate JSON object key');
		let value: unknown;
		try { value = JSON.parse(p.content); } catch { fail('payload_invalid', 'invalid JSON'); }
		const credentialMarkers = ['-----begin ', 'bearer ', 'api_key', 'apikey', 'access_token', 'client_secret', 'authorization', 'password'];
		const checkCredentials = (v: unknown): void => {
			if (Array.isArray(v)) for (const x of v) checkCredentials(x);
			else if (isObj(v)) for (const [k, x] of Object.entries(v)) {
				if (credentialMarkers.some((m) => k.toLowerCase().includes(m))) fail('payload_invalid', 'possible credential detected');
				checkCredentials(x);
			} else if (typeof v === 'string' && credentialMarkers.some((m) => v.toLowerCase().includes(m))) fail('payload_invalid', 'possible credential detected');
		};
		checkCredentials(value);
		const walk = (v: unknown): void => {
			if (Array.isArray(v)) for (const x of v) walk(x);
			else if (isObj(v)) for (const [k, x] of Object.entries(v)) {
				if (privateFields.includes(k.toLowerCase())) fail('payload_invalid', 'personal-record field detected');
				walk(x);
			}
		};
		walk(value);
	} else if (p.format === 'csv') {
		const rows = csvCells(p.content);
		if (!rows.length || !rows[0].length) fail('payload_invalid', 'CSV needs a header row');
		if (rows.some((r) => r.length !== rows[0].length)) fail('payload_invalid', 'CSV rows must have the same number of columns');
		const headers = rows[0].map((h) => h.trim().toLowerCase());
		if (headers.some((h) => h === '') || new Set(headers).size !== headers.length) fail('payload_invalid', 'CSV headers must be non-empty and unique');
		for (const cell of rows.flat()) {
			const v = cell.trimStart();
			if (/^[=+@]/.test(v) || (v.startsWith('-') && !Number.isFinite(Number(v)))) fail('payload_invalid', 'spreadsheet formulas are not accepted');
		}
		if (rows[0].some((k) => privateFields.includes(k.trim().toLowerCase()))) fail('payload_invalid', 'personal-record field detected');
	} else fail('payload_invalid', 'format must be csv or json');
}

function hasDuplicateJsonKeys(s: string): boolean {
	let i = 0;
	const ws = () => { while (/\s/.test(s[i] ?? '')) i++; };
	const string = (): string | null => {
		if (s[i] !== '"') return null;
		const start = i++;
		while (i < s.length) {
			if (s[i] === '\\') i += 2;
			else if (s[i++] === '"') {
				try { return JSON.parse(s.slice(start, i)); } catch { return null; }
			}
		}
		return null;
	};
	const value = (): boolean => {
		ws();
		if (s[i] === '{') {
			i++; ws(); const keys = new Set<string>();
			if (s[i] === '}') { i++; return false; }
			while (i < s.length) {
				ws(); const key = string(); if (key === null) return false;
				if (keys.has(key)) return true; keys.add(key); ws(); i++; // valid JSON already guarantees ':'
				if (value()) return true; ws();
				if (s[i] === '}') { i++; return false; } i++;
			}
		} else if (s[i] === '[') {
			i++; ws(); if (s[i] === ']') { i++; return false; }
			while (i < s.length) { if (value()) return true; ws(); if (s[i] === ']') { i++; return false; } i++; }
		} else if (s[i] === '"') { string(); }
		else { while (i < s.length && !/[\s,\]}]/.test(s[i])) i++; }
		return false;
	};
	return value();
}

function csvCells(s: string): string[][] {
	const rows: string[][] = [[]];
	let cell = '', quoted = false, afterQuote = false;
	for (let i = 0; i < s.length; i++) {
		const c = s[i];
		if (quoted) {
			if (c === '"' && s[i + 1] === '"') { cell += '"'; i++; }
			else if (c === '"') { quoted = false; afterQuote = true; }
			else cell += c;
			continue;
		}
		if (afterQuote) {
			if (c === ',') { rows.at(-1)!.push(cell); cell = ''; afterQuote = false; }
			else if (c === '\n') { rows.at(-1)!.push(cell); rows.push([]); cell = ''; afterQuote = false; }
			else if (c === '\r' && s[i + 1] === '\n') { i++; rows.at(-1)!.push(cell); rows.push([]); cell = ''; afterQuote = false; }
			else fail('payload_invalid', 'invalid character after a quoted CSV field');
			continue;
		}
		if (c === '"' && cell === '') quoted = true;
		else if (c === '"') fail('payload_invalid', 'quote in an unquoted CSV field');
		else if (c === ',') { rows.at(-1)!.push(cell); cell = ''; }
		else if (c === '\n') { rows.at(-1)!.push(cell); rows.push([]); cell = ''; }
		else if (c === '\r' && s[i + 1] === '\n') { i++; rows.at(-1)!.push(cell); rows.push([]); cell = ''; }
		else if (c === '\r') fail('payload_invalid', 'CSV line endings must be LF or CRLF');
		else cell += c;
	}
	if (quoted) fail('payload_invalid', 'unclosed CSV quote');
	if (cell !== '' || rows.at(-1)!.length) rows.at(-1)!.push(cell);
	else if (rows.length > 1) rows.pop();
	return rows;
}

/** Every check, in the Rust crate's order (the first broken rule wins). */
export function validate(s: Submission): void {
	const a = s.answers;
	if (a.rights === 'open_license') {
		if (a.licenseId === undefined) fail('license_id_missing', 'an open licence needs its id (for example CC-BY-4.0)');
		else if (!(a.licenseId.length > 0 && a.licenseId.length <= CAPS.licenseId && /^[A-Za-z0-9.+_:-]+$/.test(a.licenseId))) fail('license_id_bad', 'the licence id must be 1 to 64 characters from A-Z, a-z, 0-9 and . - + _ :');
	} else if (a.licenseId !== undefined) fail('license_id_unexpected', 'a licence id goes with the open-licence answer only');
	text('title', s.title, CAPS.title, false);
	text('description', s.description, CAPS.description, true);
	text('sourceName', s.sourceName, CAPS.sourceName, false);
	if (s.sourceUrl === '') {
		if (a.what !== 'own' || a.rights !== 'own_data') fail('source_url_required', 'a source URL is required unless this is your own data');
	} else url('sourceUrl', s.sourceUrl);
	if (s.dataPayload !== undefined) {
		if (a.what !== 'own' || a.rights !== 'own_data') fail('payload_route', 'a data file is only accepted for your own data when you have permission to share it');
		validateDataPayload(s.dataPayload);
	}
	if (s.endpointUrl !== undefined) url('endpointUrl', s.endpointUrl);
	if (s.endpointTemplate !== undefined) {
		const t = s.endpointTemplate;
		if (t.split(KEY_PLACEHOLDER).length - 1 !== 1) fail('bad_template', 'the endpoint template must hold {api_key} exactly once where the key goes');
		url('endpointTemplate', t.split(KEY_PLACEHOLDER).join('KEY'));
		if (hasSecretParam(t.split(KEY_PLACEHOLDER).join(''))) fail('secret_in_url', 'endpointTemplate carries a secret (a key, token or password); never put your key in a submission');
	}
	for (const [f, u] of [['sourceUrl', s.sourceUrl], ['endpointUrl', s.endpointUrl]] as const) {
		if (u !== undefined && hasSecretParam(u)) fail('secret_in_url', `${f} carries a secret (a key, token or password); never put your key in a submission`);
	}
	if (a.what !== 'keyed_api' && (s.endpointTemplate !== undefined || s.keyProof !== undefined)) fail('not_keyed', 'an endpoint template and a key proof go with a keyed source only');
	if (s.seriesId !== undefined && !(s.seriesId.length > 0 && s.seriesId.length <= CAPS.seriesId && /^[A-Za-z0-9._:-]+$/.test(s.seriesId))) fail('bad_series_id', 'the series id must be 1 to 64 characters from A-Z, a-z, 0-9 and . _ - :');
	if (s.keyProof !== undefined) {
		const p = s.keyProof;
		const hashOk = p.responseKeccak256 === undefined || /^0x[0-9a-f]{64}$/.test(p.responseKeccak256);
		const n = p.responseBytes;
		const bytesOk = n === undefined || (typeof n === 'number' ? Number.isSafeInteger(n) : n <= BigInt(CAPS.responseBytes));
		if (p.prover !== STUB_PROVER || !bytesOk || p.real || p.proof !== undefined || !hashOk || (p.responseBytes !== undefined) !== (p.responseKeccak256 !== undefined))
			fail('proof_not_supported', 'only the labelled stub prover (stub-s2, real: false) is taken until a real prover exists (S2)');
	}
	if (s.jsonPointer !== undefined) {
		if (s.endpointUrl === undefined && s.endpointTemplate === undefined) fail('pointer_without_endpoint', 'a JSON pointer needs an endpoint URL');
		if (utf8(s.jsonPointer) > CAPS.jsonPointer) fail('field_too_long', `jsonPointer is over ${CAPS.jsonPointer} bytes`);
		if (!isJsonPointer(s.jsonPointer)) fail('bad_json_pointer', "the JSON pointer must be empty or start with '/', with '~' only as ~0 or ~1");
	}
	if (s.unit !== undefined) optText('unit', s.unit, CAPS.unit, false);
}

/** The check-pull policy (the Rust `fetch_policy`): 'pull' or the no-fetch reason code. */
export function fetchPolicy(s: Submission, route: Route): string {
	if (route.outcome !== 'accept') return 'not_while_refused_or_held';
	if (s.endpointUrl === undefined) return 'no_endpoint';
	const u = parseHttpsUrl(s.endpointUrl);
	if (typeof u === 'string') return 'no_endpoint';
	if (route.path !== 'auto_pull') return 'not_auto_pull';
	if (!CLEARED_HOSTS.includes(u.host)) return 'host_not_cleared';
	if (u.port !== null && u.port !== 443) return 'port_not_allowed';
	return 'pull';
}

/** "Try an example": the U.S. Treasury walk-through (Fiscal Data, rights reviewed; pinned by the vectors). */
export const EXAMPLE: Submission = {
	answers: { what: 'public_api', noLogin: true, rights: 'open_license', namesPerson: false, changes: 'daily', licenseId: 'onyx:us-treasury-fiscal-data-open' },
	title: 'U.S. total public debt outstanding',
	description:
		"The U.S. Treasury's Debt to the Penny: total public debt outstanding in dollars and cents, one record per business day. The value is the record for the date in the endpoint's filter.",
	sourceName: 'U.S. Treasury Fiscal Data: Debt to the Penny',
	sourceUrl: 'https://fiscaldata.treasury.gov/datasets/debt-to-the-penny/debt-to-the-penny',
	endpointUrl: 'https://api.fiscaldata.treasury.gov/services/api/fiscal_service/v2/accounting/od/debt_to_penny?filter=record_date:eq:2026-10-08',
	jsonPointer: '/data/0/tot_pub_debt_out_amt',
	unit: 'USD'
};

/** The keyed walk-through: a FRED user with their own key (held; FRED is blocked, N10; pinned by the vectors). */
export const FRED_EXAMPLE: Submission = {
	answers: { what: 'keyed_api', noLogin: false, rights: 'not_sure', namesPerson: false, changes: 'daily' },
	title: 'U.S. consumer price index for all urban consumers (CPIAUCSL)',
	description: 'The monthly CPI-U index, seasonally adjusted, as FRED serves it. I would fetch it with my own FRED API key.',
	sourceName: 'FRED, Federal Reserve Bank of St. Louis',
	sourceUrl: 'https://fred.stlouisfed.org/series/CPIAUCSL',
	endpointTemplate: 'https://api.stlouisfed.org/fred/series/observations?series_id=CPIAUCSL&api_key={api_key}&file_type=json',
	seriesId: 'CPIAUCSL',
	jsonPointer: '/observations/0/value',
	unit: 'index 1982-1984=100',
	keyProof: { prover: 'stub-s2', real: false }
};

/** The stub prover (S1). It takes the key and the template, makes NO request, keeps nothing, and returns a
 *  proof object that says plainly it proves nothing. A real prover (S2) would run the request on the
 *  user's side and return a proof of the response; Onyx would still get only that object. `responseBody`
 *  is a body the user fetched themselves elsewhere, if they paste one: only its hash and length go out. */
export function stubProve(key: string, template: string, hash?: { keccak: string; bytes: number }): KeyProof {
	// The key is read only to say whether one was given; it is not used, stored or returned.
	void key;
	void template;
	const p: KeyProof = { prover: STUB_PROVER, real: false };
	if (hash) {
		p.responseKeccak256 = hash.keccak;
		p.responseBytes = hash.bytes;
	}
	return p;
}

/** The body that leaves the page (download or API): the submission's own fields, nothing else. When a key was
 *  typed (`secret`), it is trimmed and must be at least CAPS.keyMin long, and every text field of the
 *  submission is checked, in any case, for it (S1-R1-1): a match is refused. */
export function outgoing(s: Submission, secret?: string): string {
	const key = (secret ?? '').trim();
	if (key !== '') {
		if (key.length < CAPS.keyMin) fail('key_too_short', `That key is shorter than ${CAPS.keyMin} characters, too short to check the submission against. Check what you typed.`);
		const needle = key.toLowerCase();
		const texts: string[] = [];
		const walk = (v: unknown) => {
			if (typeof v === 'string') texts.push(v);
			else if (v && typeof v === 'object') Object.values(v).forEach(walk);
		};
		walk(s);
		if (texts.some((t) => t.toLowerCase().includes(needle))) fail('key_in_submission', 'Your API key appears in the details. Remove it: the key never leaves this page.');
	}
	return JSON.stringify(s, (_, v) => (typeof v === 'bigint' ? Number(v) : v), 2);
}

/** A strict JSON reader with serde_json's verdicts where they differ from JSON.parse (S1-R1-4): a duplicate key
 *  or trailing text is refused; integers stay exact (a BigInt past 2^53 - 1); a number with a fraction or an
 *  exponent becomes a JsonFraction, which no field takes. */
export class JsonFraction {
	text: string;
	constructor(text: string) {
		this.text = text;
	}
}
export function parseJsonStrict(src: string): unknown {
	let i = 0;
	const bad = (why: string): never => fail('bad_json', `not a submission: ${why} at ${i}`);
	const ws = () => {
		while (i < src.length && ' \t\n\r'.includes(src[i])) i++;
	};
	const value = (): unknown => {
		ws();
		const c = src[i];
		if (c === '{') {
			i++;
			const o: Record<string, unknown> = {};
			ws();
			if (src[i] === '}') return (i++, o);
			for (;;) {
				ws();
				if (src[i] !== '"') bad('expected a key');
				const k = string();
				if (Object.prototype.hasOwnProperty.call(o, k)) bad(`duplicate field ${k}`);
				ws();
				if (src[i++] !== ':') bad('expected :');
				o[k] = value();
				ws();
				if (src[i] === ',') {
					i++;
					continue;
				}
				if (src[i] === '}') return (i++, o);
				bad('expected , or }');
			}
		}
		if (c === '[') {
			i++;
			const a: unknown[] = [];
			ws();
			if (src[i] === ']') return (i++, a);
			for (;;) {
				a.push(value());
				ws();
				if (src[i] === ',') {
					i++;
					continue;
				}
				if (src[i] === ']') return (i++, a);
				bad('expected , or ]');
			}
		}
		if (c === '"') return string();
		for (const [w, v] of [['true', true], ['false', false], ['null', null]] as const) if (src.startsWith(w, i)) return ((i += w.length), v);
		const m = /^-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?/.exec(src.slice(i));
		if (!m || m[0] === '' || m[0] === '-') return bad('expected a value');
		i += m[0].length;
		if (m[2] || m[3]) return new JsonFraction(m[0]);
		const n = BigInt(m[0]);
		return n >= -BigInt(Number.MAX_SAFE_INTEGER) && n <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(n) : n;
	};
	const string = (): string => {
		i++;
		let out = '';
		for (;;) {
			if (i >= src.length) bad('unterminated string');
			const ch = src[i++];
			if (ch === '"') return out;
			if (ch.charCodeAt(0) < 0x20) bad('control character in a string');
			if (ch !== '\\') {
				out += ch;
				continue;
			}
			const e = src[i++];
			const map: Record<string, string> = { '"': '"', '\\': '\\', '/': '/', b: '\b', f: '\f', n: '\n', r: '\r', t: '\t' };
			if (e in map) out += map[e];
			else if (e === 'u') {
				const hex = (at: number) => (/^[0-9A-Fa-f]{4}$/.test(src.slice(at, at + 4)) ? parseInt(src.slice(at, at + 4), 16) : bad('bad \\u escape'));
				const u = hex(i);
				i += 4;
				if (u >= 0xd800 && u <= 0xdbff) {
					if (src.slice(i, i + 2) !== '\\u') bad('lone surrogate');
					const l = hex(i + 2);
					if (l < 0xdc00 || l > 0xdfff) bad('lone surrogate');
					i += 6;
					out += String.fromCharCode(u, l);
				} else if (u >= 0xdc00 && u <= 0xdfff) bad('lone surrogate');
				else out += String.fromCharCode(u);
			} else bad('bad escape');
		}
	};
	const v = value();
	ws();
	if (i !== src.length) bad('trailing characters');
	return v;
}

/** parseSubmission over raw JSON text, read with parseJsonStrict (what the API's serde reads). */
export const parseSubmissionText = (text: string): Submission => parseSubmission(parseJsonStrict(text));
