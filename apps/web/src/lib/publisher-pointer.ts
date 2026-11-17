// The publish page's pointer form (ADR-0066 item 7): the two values an applicant sends, the
// node's answer read in its exact shape or not at all, and the page's own words for every outcome.
// The form shows no text a server sent: a receipt's values pass their patterns first, and an error
// is shown by its status and a code from the list below, each with words written here.
// Pure (no Svelte, no DOM), so `node --test tests/publishers.test.mjs` imports it as is.

/** The route, relative to the site's own origin: the form never asks another host. */
export const POINTER_PATH = 'v1/publishers/pointer';
/** How long a pointer waits, and how long its receipt answers after that. */
export const POINTER_DAYS = 6;
export const RECEIPT_DAYS = 12;

const LABEL = '[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?';
const HOST = new RegExp(`^${LABEL}(?:\\.${LABEL})+$`);
const PART = /^[a-z0-9](?:[a-z0-9_-]{0,61}[a-z0-9])?$/;
const HASH = /^0x[0-9a-f]{64}$/;
const ID = /^ptr_[0-9a-f]{32}$/;

/** Why `origin` is not an origin, in plain words; `null` when it is one. The same grammar as the
 * tools and the node (a lowercase host, up to four path parts): the node decides, this only saves
 * a round trip. */
export function originProblem(origin: string): string | null {
	if (origin === '') return 'Type your origin, for example data.example.org/river-levels.';
	if (origin.length > 253) return 'An origin is at most 253 characters.';
	if (origin.includes('://')) return 'Leave out https:// in front: type the host, or the host and a path.';
	if (origin.includes('@')) return 'An origin is a host and a path. It holds no @ and no mail address.';
	const [host, ...parts] = origin.split('/');
	const labels = host.split('.');
	if (!HOST.test(host) || /^\d+$/.test(labels[labels.length - 1])) return 'The host must be a lowercase name with a dot, such as data.example.org: no address, no port, no capital letter.';
	if (labels.some((l) => l.startsWith('xn--'))) return 'A host with an xn-- part is not taken: it can read like another name.';
	if (parts.length > 4) return 'An origin has at most four path parts.';
	if (!parts.every((p) => PART.test(p))) return 'A path part is lowercase letters, digits, - and _, with no / at the end.';
	return null;
}

/** Why `hash` is not an application's hash; `null` when it is one. */
export function hashProblem(hash: string): string | null {
	if (hash === '') return 'Paste the hash of your application file: the doctor prints it.';
	return HASH.test(hash) ? null : 'The hash is 0x and 64 digits (0 to 9 and a to f, lowercase).';
}

/** The request body: exactly the two values. */
export function pointerBody(origin: string, application: string): string {
	return JSON.stringify({ origin, application });
}

export type PointerReceipt = { id: string; status: 'received' | 'expired' | 'dropped'; expiresUnix: number; reason: 'pointer_lifetime_over' | 'application_hash_mismatch' | null };

/** The node's receipt in its exact shape, or `null`: nothing of an answer out of shape is shown. */
export function parsePointerReceipt(v: unknown): PointerReceipt | null {
	if (!v || typeof v !== 'object' || Array.isArray(v)) return null;
	const b = v as Record<string, unknown>;
	const keys = Object.keys(b).sort().join();
	if (keys !== 'decision,expiresUnix,id,status' && keys !== 'decision,expiresUnix,id,reason,status') return null;
	if (typeof b.id !== 'string' || !ID.test(b.id) || b.decision !== 'see_status_list') return null;
	if (typeof b.expiresUnix !== 'number' || !Number.isSafeInteger(b.expiresUnix) || b.expiresUnix < 0) return null;
	if (b.status === 'received') return 'reason' in b ? null : { id: b.id, status: 'received', expiresUnix: b.expiresUnix, reason: null };
	if (b.status === 'expired') return b.reason === 'pointer_lifetime_over' ? { id: b.id, status: 'expired', expiresUnix: b.expiresUnix, reason: 'pointer_lifetime_over' } : null;
	// Dropped by the reviewer's tool: the file at the origin is not the one this pointer names.
	if (b.status === 'dropped') return b.reason === 'application_hash_mismatch' ? { id: b.id, status: 'dropped', expiresUnix: b.expiresUnix, reason: 'application_hash_mismatch' } : null;
	return null;
}

/** `2026-11-23 00:00 UTC` for a unix time. */
export function utcMinute(unix: number): string {
	const d = new Date(unix * 1000);
	return Number.isNaN(d.getTime()) ? 'an unknown time' : d.toISOString().slice(0, 16).replace('T', ' ') + ' UTC';
}

/** What a receipt means, in the page's words. */
export function receiptWords(r: PointerReceipt): string {
	if (r.status === 'dropped')
		return 'Dropped: the review found that the file at this origin does not have this hash, so this pointer no longer waits. If the origin is yours, run the doctor again and send the hash it prints.';
	return r.status === 'received'
		? `Received. The reviewer's tools will fetch your application from your origin. This pointer waits until ${utcMinute(r.expiresUnix)}.`
		: `This pointer is older than ${POINTER_DAYS} days and no longer waits. If no decision is in the status list, write a new application and send its pointer.`;
}

/** The page's words for a refusal, by the node's code. A code that is not here gets `otherWords`. */
export const REFUSALS: Record<string, string> = {
	bad_pointer: 'The node did not take the two values as sent. Reload the page and try again.',
	bad_origin: 'The node does not take this origin: it must be a lowercase host, or a host and up to four path parts.',
	bad_application_hash: 'The node does not take this hash: it must be 0x and 64 lowercase hex digits.',
	origin_shared_host: 'This origin is on a shared host, where someone else could take the name later. Use a host you control.',
	pointer_origin_busy: 'This origin’s host already has three pointers waiting with other hashes. Anyone can point at any origin, so they may not be yours. A place is free again when the reviewer drops a pointer whose hash is not your file’s, or when one expires. You do not have to wait: file your application as a public issue (the second way below).',
	cross_site_refused: 'The node took this request for one from another site and refused it. Nothing was stored. Open this page at the site’s own address and send again, or file your application as a public issue (the second way below).',
	json_required: 'The node did not take the two values as sent. Reload the page and try again.',
	body_too_large: 'The node did not take the two values as sent. Reload the page and try again.',
	rate_limited: 'Too many pointers were sent from your network in the last minute. Wait a minute and send again.',
	daily_quota: 'Today’s number of pointers is used up, for your network or for the whole node. Try again after 00:00 UTC, or file your application as a public issue (the second way below).',
	pointer_full: 'The node’s list of pointers is full. Try again later, or file your application as a public issue (the second way below).',
	pointer_busy: 'The node is busy. Wait a few seconds and send again.',
	pointer_unavailable: 'The node cannot store pointers at the moment. Try again later, or file your application as a public issue (the second way below).'
};
/** The route is not there (404, 403, 405): this site's node does not take pointers. */
export const OFF_WORDS = 'This site’s node does not take pointers: the way without an account is switched off here, or this is a copy of the site without a node. Nothing was stored. File your application as a public issue instead (the second way below).';
export const otherWords = (status: number) =>
	`The node answered with an error (HTTP ${status}) and this page cannot tell whether your pointer was stored. Sending the same two values again is safe: the same pointer is stored once. Or file your application as a public issue (the second way below).`;
export const UNREACHED_WORDS = 'The node could not be reached, so this page cannot tell whether your pointer was stored. Sending the same two values again is safe: the same pointer is stored once. Or file your application as a public issue (the second way below).';
export const BAD_ANSWER_WORDS = 'The node’s answer is not a receipt this page can read, so none is shown. Sending the same two values again is safe: the same pointer is stored once.';

export type PointerOutcome = { kind: 'receipt'; receipt: PointerReceipt; again: boolean } | { kind: 'off' | 'refused' | 'failed'; words: string };

/** What the form shows for an answer of the node: `status` and the parsed JSON body (or `null`
 * when the body is not JSON). No text of the body is passed on, only values that fit a pattern. */
export function pointerOutcome(status: number, body: unknown): PointerOutcome {
	if (status === 200 || status === 201) {
		const receipt = parsePointerReceipt(body);
		return receipt ? { kind: 'receipt', receipt, again: status === 200 } : { kind: 'failed', words: BAD_ANSWER_WORDS };
	}
	const code = body && typeof body === 'object' ? (body as { error?: { code?: unknown } }).error?.code : undefined;
	// The node's own refusal of a request it takes for another site's: not "the route is off".
	if (status === 403 && code === 'cross_site_refused') return { kind: 'refused', words: REFUSALS.cross_site_refused };
	if (status === 404 || status === 403 || status === 405) return { kind: 'off', words: OFF_WORDS };
	if (typeof code === 'string' && Object.hasOwn(REFUSALS, code)) return { kind: 'refused', words: REFUSALS[code] };
	return { kind: 'failed', words: otherWords(status) };
}
