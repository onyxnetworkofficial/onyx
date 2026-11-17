// The publish page's status box (ADR-0066 item 9): the status list `publishers.json`, which
// scripts/check-publishers.py writes from the public record deploy/publishers/. The list is read
// in its exact shape only (the shape the public-build guard enforces, adapters/source-btc/tests/
// public_build.rs): any other field, state or value refuses the whole file, so nothing an outside
// application could carry is ever shown. A state is printed exactly as the list has it, with one
// plain sentence that says no more than the state does.
// Pure (no Svelte, no DOM), so `node --test tests/publishers.test.mjs` imports it as is.

export type Publisher = {
	publisher: string;
	state: string;
	date: string;
	/** Null for a publisher registered before the record began: the list carries none for it. */
	origin: string | null;
	signers: string[];
	registered: { tx: string; block: number }[];
	reason: string | null;
};
export type Application = { application: string; state: string; date: string; reason: string | null; blocksSignerUntil: number | null };
export type StatusList = { chainId: number | null; book: string | null; publishers: Publisher[]; applications: Application[] };

/** A publisher's states, in the list's own words, each with what it says and nothing more. */
export const PUBLISHER_STATES: Record<string, string> = {
	approved: 'Approved by the reviewer, and the registration was found in the signed book on the test chain.',
	'approved (record)': 'Approved in the public record. The check that reads the registration from the test chain has not run for it yet.',
	'registered-before-record': 'Registered before this record began.',
	paused: 'Paused: the node keeps serving its data and takes no new data from it.',
	retired: 'Retired: it publishes nothing new.'
};

/** An application's states while it is not approved. */
export const APPLICATION_STATES: Record<string, string> = {
	received: 'Received. The reviewer has not decided yet.',
	refused: 'Refused by the reviewer.',
	withdrawn: 'Withdrawn by the key that signed it.',
	expired: 'Expired before a decision.'
};

/** The reason codes of a decision, in plain words. */
export const REASONS: Record<string, string> = {
	'name-not-neutral': 'The publisher id or the origin names a person, a company, a product or the network.',
	'name-taken': 'The publisher id is taken, or reads like a registered one.',
	'origin-unreachable': 'The files could not be read from the origin.',
	'origin-overlap': 'The origin overlaps a registered origin.',
	'sample-failed': 'The first dataset did not pass its check.',
	'rights-unclear': 'It is not clear that anyone may share the data.',
	'personal-data': 'The data is about individual people.',
	'not-hostable': 'The data cannot be hosted.',
	duplicate: 'The same application was decided before.',
	'no-capacity': 'The node has no room for another publisher right now.',
	'no-cosignature': 'A further key needs the signature of a key that is already registered.',
	malformed: 'The application file is not in its form.',
	'bad-signature': 'The signature does not fit the application.',
	'wrong-network': 'The application is for another network.',
	expired: 'The application was past its expiry.',
	'bad-time': 'The times in the application do not fit the rules.',
	'keys-document': 'The keys document does not fit the application.',
	'signer-blocked': 'The key is blocked after an earlier refusal.'
};

/** The state of a publisher the book held before the record began. The list gives its id, state,
 * date, keys and registration only: no origin (review S4-R1 M2), and the page shows id and state. */
export const BEFORE_RECORD = 'registered-before-record';

/** The reviewer's target: a decision within this many days (ADR-0066 item 8). */
export const DECIDE_DAYS = 5;
/** The fewest hex digits of an application's hash a lookup takes. */
export const MIN_PREFIX = 8;

const ID = /^[a-z0-9_-]+(?:[.:][a-z0-9_-]+)*$/;
const LABEL = '[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?';
const ORIGIN = new RegExp(`^${LABEL}(?:\\.${LABEL})+(?:/[a-z0-9_-]+){0,4}$`);
const DATE = /^\d{4}-\d{2}-\d{2}$/;
const hex = (bytes: number) => new RegExp(`^0x[0-9a-f]{${2 * bytes}}$`);
const ADDRESS = hex(20);
const HASH = hex(32);

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);
const str = (v: unknown, re: RegExp, max = 300): v is string => typeof v === 'string' && v.length <= max && re.test(v);
const whole = (v: unknown): v is number => typeof v === 'number' && Number.isSafeInteger(v) && v >= 0;
/** Every one of `required`, and besides them at most `optional`. */
const fields = (o: Record<string, unknown>, required: string[], optional: string[]) =>
	required.every((k) => k in o) && Object.keys(o).every((k) => required.includes(k) || optional.includes(k));

function publisher(v: unknown): Publisher | null {
	if (!isObject(v) || !fields(v, ['publisher', 'state', 'date', 'signers', 'registered'], ['origin', 'reason'])) return null;
	const { publisher: id, state, date, signers, registered } = v;
	const reason = v.reason ?? null;
	if (!str(id, ID, 128) || !str(date, DATE)) return null;
	if (typeof state !== 'string' || !Object.hasOwn(PUBLISHER_STATES, state)) return null;
	// An origin on every publisher but one from before the record, which must carry none.
	const origin = state === BEFORE_RECORD ? null : v.origin;
	if (state === BEFORE_RECORD ? 'origin' in v : !str(origin, ORIGIN)) return null;
	if (reason !== null && (typeof reason !== 'string' || !Object.hasOwn(REASONS, reason))) return null;
	if (!Array.isArray(signers) || !signers.every((s) => str(s, ADDRESS))) return null;
	if (!Array.isArray(registered)) return null;
	const reg: { tx: string; block: number }[] = [];
	for (const r of registered) {
		if (!isObject(r) || !fields(r, ['tx', 'block'], []) || !str(r.tx, HASH) || !whole(r.block)) return null;
		reg.push({ tx: r.tx, block: r.block });
	}
	return { publisher: id, state, date, origin: origin as string | null, signers: signers as string[], registered: reg, reason };
}

function application(v: unknown): Application | null {
	if (!isObject(v) || !fields(v, ['application', 'state', 'date'], ['reason', 'blocks_signer_until'])) return null;
	const { application: hash, state, date } = v;
	const reason = v.reason ?? null;
	const until = v.blocks_signer_until ?? null;
	if (!str(hash, HASH) || !str(date, DATE)) return null;
	if (typeof state !== 'string' || !Object.hasOwn(APPLICATION_STATES, state)) return null;
	if (reason !== null && (typeof reason !== 'string' || !Object.hasOwn(REASONS, reason))) return null;
	if (until !== null && !whole(until)) return null;
	return { application: hash, state, date, reason, blocksSignerUntil: until };
}

/** Whether `values` holds one value twice. */
const repeats = (values: string[]) => new Set(values).size !== values.length;

/** The status list in its exact shape, or null: one field, state or value out of shape refuses
 * the whole file (a list that is partly unreadable must not look complete). So does a value that
 * must stand once and stands twice: a publisher id, an origin, a key address, a registration's
 * transaction, an application's hash (the record has one id per origin and per key). */
export function parseStatus(raw: unknown): StatusList | null {
	if (!isObject(raw) || !fields(raw, ['version', 'publishers', 'applications'], ['chain_id', 'book'])) return null;
	if (raw.version !== 1 || !Array.isArray(raw.publishers) || !Array.isArray(raw.applications)) return null;
	const chainId = raw.chain_id ?? null;
	const book = raw.book ?? null;
	if (chainId !== null && !whole(chainId)) return null;
	if (book !== null && !str(book, ADDRESS)) return null;
	const publishers: Publisher[] = [];
	for (const p of raw.publishers) {
		const row = publisher(p);
		if (!row) return null;
		publishers.push(row);
	}
	const applications: Application[] = [];
	for (const a of raw.applications) {
		const row = application(a);
		if (!row) return null;
		applications.push(row);
	}
	const once = [
		publishers.map((p) => p.publisher),
		publishers.flatMap((p) => (p.origin === null ? [] : [p.origin])),
		publishers.flatMap((p) => p.signers),
		publishers.flatMap((p) => p.registered.map((r) => r.tx)),
		applications.map((a) => a.application)
	];
	if (once.some(repeats)) return null;
	return { chainId, book, publishers, applications };
}

/** The same, from the file's text. */
export function parseStatusText(text: string): StatusList | null {
	try {
		return parseStatus(JSON.parse(text));
	} catch {
		return null;
	}
}

export type Lookup =
	| { kind: 'empty' }
	/** Neither a publisher id nor at least MIN_PREFIX hex digits of a hash. */
	| { kind: 'invalid' }
	| { kind: 'none' }
	| { kind: 'found'; publishers: Publisher[]; applications: Application[] };

/** Look `query` up in `list`: a publisher by its exact id, an application that is not approved by
 * the first digits of its hash (with or without `0x`). Nothing is sent anywhere. */
export function lookup(list: StatusList, query: string): Lookup {
	const q = query.trim().toLowerCase();
	if (!q) return { kind: 'empty' };
	const digits = q.startsWith('0x') ? q.slice(2) : q;
	const asHash = /^[0-9a-f]+$/.test(digits) && digits.length >= MIN_PREFIX && digits.length <= 64;
	const asId = q.length <= 128 && ID.test(q);
	if (!asHash && !asId) return { kind: 'invalid' };
	const publishers = asId ? list.publishers.filter((p) => p.publisher === q) : [];
	const applications = asHash ? list.applications.filter((a) => a.application.slice(2).startsWith(digits)) : [];
	return publishers.length + applications.length ? { kind: 'found', publishers, applications } : { kind: 'none' };
}

/** Unix seconds as "2026-11-24 12:05 UTC". */
export function utcMinute(unix: number): string {
	const d = new Date(unix * 1000);
	return Number.isNaN(d.getTime()) ? String(unix) : `${d.toISOString().slice(0, 16).replace('T', ' ')} UTC`;
}

/** What an applicant does next, for an application that is not approved. */
export function nextStep(a: Application): string {
	switch (a.state) {
		case 'received':
			return `The reviewer decides within ${DECIDE_DAYS} days of the day it was received.`;
		case 'refused':
			return a.blocksSignerUntil === null
				? 'To apply again: fix the reason named above, then write a new application.'
				: `To apply again: fix the reason named above, then apply with a new key. The key of this application is blocked until ${utcMinute(a.blocksSignerUntil)}; after that time it can apply again too.`;
		case 'expired':
			return 'To apply again: run the application step again with --reapply, the same key and the same sample folder, put the new file on your origin, then open a new issue with the new hash.';
		case 'withdrawn':
			return 'This application is closed. To apply again, write a new application.';
		default:
			return '';
	}
}
