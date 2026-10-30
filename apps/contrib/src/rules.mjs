// The intake rules onyx-contrib needs before anything leaves this computer: byte caps, secret
// parameter names, the {api_key} placeholder, the stub prover's name, and the key guard of the
// website's `outgoing()` (apps/web/src/lib/intake.ts, R-SUB-9a). Copied, never raised: a test
// reads intake.ts and fails when a copy here differs (THREATS CL2-6). The node checks everything
// again; these copies only stop a bad body before it is sent.

/** Byte caps of the intake (intake.ts `CAPS`, ADR-0038 item 4). */
export const CAPS = { title: 120, description: 2000, sourceName: 120, url: 512, jsonPointer: 256, unit: 32, licenseId: 64, seriesId: 64, responseBytes: Number.MAX_SAFE_INTEGER, keyMin: 12, payloadBytes: 8192 };
export const KEY_PLACEHOLDER = '{api_key}';
export const STUB_PROVER = 'stub-s2';
export const SECRET_PARAMS = ['appid', 'auth', 'authorization', 'pwd', 'sig'];
export const SECRET_PARTS = ['credential', 'key', 'passw', 'secret', 'signature', 'token'];

export const utf8 = (s) => Buffer.byteLength(s, 'utf8');

/** A parameter name as a server reads it: percent-decoded, ASCII lower case, no array suffix. */
export function paramName(raw) {
	let out = raw.replace(/%([0-9A-Fa-f]{2})/g, (_, h) => String.fromCharCode(parseInt(h, 16)));
	const b = out.indexOf('[');
	if (b >= 0) out = out.slice(0, b);
	return out.replace(/[A-Z]/g, (c) => c.toLowerCase());
}
export const isSecretName = (n) => SECRET_PARAMS.includes(n) || n.startsWith('x-amz-') || SECRET_PARTS.some((p) => n.includes(p));

/** Whether a URL's query gives a value to a secret parameter (pairs split on & and ;). */
export function hasSecretParam(u) {
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

/** An https URL with a public host name (the intake's `parseHttpsUrl`, shortened): null when fine, else why not. */
export function httpsProblem(s) {
	if (typeof s !== 'string') return 'not a string';
	if (utf8(s) > CAPS.url) return `over ${CAPS.url} bytes`;
	if (![...s].every((c) => c.charCodeAt(0) >= 0x21 && c.charCodeAt(0) <= 0x7e)) return 'only visible ASCII, no spaces';
	if ([...s].some((c) => '\\"<>^`{|}'.includes(c))) return 'characters a URL may not hold';
	if (!s.startsWith('https://')) return 'https only';
	if (s.includes('#')) return 'no fragment';
	const rest = s.slice(8);
	const m = rest.search(/[/?]/);
	const authority = rest.slice(0, m < 0 ? rest.length : m);
	if (authority.includes('@')) return 'no user name or password in the URL';
	const host = authority.replace(/:[0-9]{1,5}$/, '').toLowerCase();
	if (!/^[a-z0-9-]+(\.[a-z0-9-]+)+$/.test(host) || /^[0-9.]+$/.test(host) || /\.(0x[0-9a-f]*|[0-9]+)$/.test(host)) return 'a public host name, not an IP address';
	return null;
}

/** Shortest piece of a key that counts as the key: a leak of 10 characters or more is caught (a cut answer, a half-echoed URL). */
export const KEY_WINDOW = 10;

/** The base64 characters that hold only the key's bits, with 0, 1 and 2 bytes before it (an echo of "user:key" too). */
function base64Forms(key) {
	const out = [];
	const kb = Buffer.from(key, 'utf8');
	for (let off = 0; off < 3; off++) {
		const b64 = Buffer.concat([Buffer.alloc(off), kb]).toString('base64').replace(/=+$/, '');
		const mid = b64.slice(Math.ceil((8 * off) / 6), Math.floor((8 * (off + kb.length)) / 6));
		out.push(mid, mid.replace(/\+/g, '-').replace(/\//g, '_'));
	}
	return out;
}

/**
 * The forms a key can take inside a request or an answer, lower case (every match ignores letter
 * case): as typed; URL-encoded as encodeURIComponent and as URLSearchParams write it; JSON-escaped,
 * also with "\/"; base64 and base64url at every alignment; hex.
 */
export function keyForms(key) {
	const json = JSON.stringify(key).slice(1, -1);
	const all = [key, encodeURIComponent(key), new URLSearchParams({ k: key }).toString().slice(2), json, json.replace(/\//g, '\\/'), Buffer.from(key, 'utf8').toString('hex'), ...base64Forms(key)];
	return [...new Set(all.map((f) => f.toLowerCase()))].filter((f) => f.length > 0);
}

/** Every piece of KEY_WINDOW characters of every form of every key (the whole form when shorter). */
function windows(keys) {
	const out = new Set();
	for (const key of keys) {
		if (typeof key !== 'string' || key === '') continue;
		for (const f of keyForms(key)) {
			if (f.length <= KEY_WINDOW) out.add(f);
			else for (let i = 0; i + KEY_WINDOW <= f.length; i++) out.add(f.slice(i, i + KEY_WINDOW));
		}
	}
	return [...out];
}

/** The [start, end) ranges of `text` that hold a piece of a key, merged. */
function keyRanges(text, keys) {
	const low = String(text).toLowerCase();
	const ranges = [];
	for (const w of windows(keys)) for (let i = low.indexOf(w); i !== -1; i = low.indexOf(w, i + 1)) ranges.push([i, i + w.length]);
	ranges.sort((a, b) => a[0] - b[0]);
	const merged = [];
	for (const r of ranges) {
		const m = merged.at(-1);
		if (m && r[0] <= m[1]) m[1] = Math.max(m[1], r[1]);
		else merged.push([...r]);
	}
	return merged;
}

const decoded = (s) => {
	try {
		return decodeURIComponent(s.replace(/\+/g, ' '));
	} catch {
		return s;
	}
};

/** Strings to search in a value: every string and object key, as is and percent-decoded. */
function texts(v, out = []) {
	const add = (s) => out.push(s, decoded(s));
	if (typeof v === 'string') add(v);
	else if (v && typeof v === 'object') for (const [k, x] of Object.entries(v)) (add(k), texts(x, out));
	return out;
}

/**
 * The key guard (intake.ts `outgoing()`, R-SUB-9a, made stricter): every key must be at least
 * CAPS.keyMin long, and no string in `body` (as is or percent-decoded) and no part of its JSON
 * text may hold KEY_WINDOW characters of it in any form or letter case. Null when the body may leave.
 */
export function keyGuard(body, keys) {
	for (const key of keys)
		if (typeof key !== 'string' || key.trim().length < CAPS.keyMin)
			return { code: 'key_too_short', error: `A stored key is shorter than ${CAPS.keyMin} characters, too short to check the outgoing body against. Nothing was sent.` };
	for (const t of [JSON.stringify(body), ...texts(body)])
		if (keyRanges(t, keys).length)
			return { code: 'key_in_submission', error: 'Your API key (or a piece of it) appears in what would be sent, for example because the provider echoed it in the answer. Nothing was sent: the key never leaves this computer.' };
	return null;
}

/** `text` with every piece of every key (KEY_WINDOW characters or more, any form, any letter case) replaced by "[key removed]". */
export function redact(text, keys) {
	const s = String(text);
	const ranges = keyRanges(s, keys);
	if (!ranges.length) return s;
	let out = '';
	let at = 0;
	for (const [a, b] of ranges) {
		out += s.slice(at, a) + '[key removed]';
		at = b;
	}
	return out + s.slice(at);
}

/** A parsed JSON value with the key removed from every string and object key (a string that holds it only percent-encoded is replaced whole). */
export function redactTree(v, keys) {
	const one = (s) => (keyRanges(decoded(s), keys).length && !keyRanges(s, keys).length ? '[key removed]' : redact(s, keys));
	if (typeof v === 'string') return one(v);
	if (Array.isArray(v)) return v.map((x) => redactTree(x, keys));
	if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, x]) => [one(k), redactTree(x, keys)]));
	return v;
}

/** RFC 6901: the value at `pointer` in `doc`, or `{ missing: true }`. */
export function atPointer(doc, pointer) {
	if (pointer === '') return { value: doc };
	if (typeof pointer !== 'string' || !pointer.startsWith('/')) return { missing: true };
	let v = doc;
	for (const raw of pointer.slice(1).split('/')) {
		const k = raw.replace(/~1/g, '/').replace(/~0/g, '~');
		if (Array.isArray(v) && /^(0|[1-9][0-9]*)$/.test(k) && Number(k) < v.length) v = v[Number(k)];
		else if (v && typeof v === 'object' && !Array.isArray(v) && Object.prototype.hasOwnProperty.call(v, k)) v = v[k];
		else return { missing: true };
	}
	return { value: v };
}

export function isJsonPointer(p) {
	if (p === '') return true;
	if (typeof p !== 'string' || !p.startsWith('/') || /[\u0000-\u001f\u007f-\u009f]/.test(p)) return false;
	for (let i = 0; i < p.length; i++) if (p[i] === '~' && p[i + 1] !== '0' && p[i + 1] !== '1') return false;
	return true;
}

/** Up to `max` JSON pointers to plain values (numbers, strings, booleans) in `doc`, breadth first. */
export function leafPointers(doc, max = 30) {
	const out = [];
	const queue = [['', doc, 0]];
	const esc = (k) => String(k).replace(/~/g, '~0').replace(/\//g, '~1');
	while (queue.length && out.length < max) {
		const [p, v, depth] = queue.shift();
		if (v !== null && typeof v === 'object') {
			if (depth >= 6) continue;
			const entries = Array.isArray(v) ? v.slice(0, 3).map((x, i) => [i, x]) : Object.entries(v).slice(0, 40);
			for (const [k, x] of entries) queue.push([`${p}/${esc(k)}`, x, depth + 1]);
		} else if (p !== '') out.push(p);
	}
	return out;
}
