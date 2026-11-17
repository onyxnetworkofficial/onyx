/** Public receipt fields only (ADR-0039). No submission content or reviewer notes. */
export type ReceiptCheck = { level: 2; recordDate: string; roundId: string; result: string; value: string; registry: string };
/** `check` is present only once an approved submission was fed to the observers (ADR-0039 amendment 1). */
/** `reason` is present only on a receipt a reviewer put on hold or dropped: a code from a fixed list, never a reviewer's text (READINESS B2). */
export type Receipt = { id: string; status: string; reason?: string; check?: ReceiptCheck };
export const RECEIPT_STATUS: Record<string, string> = {
	pending_review: 'Waiting for review. Your data has not been verified or published.',
	held_rights_review: 'On hold while reuse permissions are checked. Your data is not published.',
	held_keyed_source_review: 'On hold: this source needs a supported proof method and a review of its terms. Your data is not published.',
	approved: 'Approved for checking. This is a review decision; verification and publication are separate steps.',
	rejected: 'This proposal will not proceed. You can submit a corrected proposal for a new review.',
	cleared: 'A reviewer read your proposal and cleared it for the next step. Your data has not been verified or published yet.',
	on_hold: 'A reviewer put your proposal on hold. Your data is not published.',
	dropped: 'A reviewer decided this proposal will not proceed. You can submit a corrected proposal for a new review.'
};
/** Why a reviewer put a proposal on hold or dropped it, in plain words. The server sends the code only. */
export const RECEIPT_REASON: Record<string, string> = {
	terms_forbid: "Reason: the source's terms do not allow its data to be shared again.",
	personal_data: 'Reason: the proposal holds or points to data about a person. Onyx takes data about organisations only.',
	not_entities: 'Reason: the data is about a person, not about an organisation, a fund or a protocol.',
	duplicate: 'Reason: the same data was already proposed or is already on Onyx.',
	unreachable_source: 'Reason: the reviewer could not open the source.',
	out_of_scope: 'Reason: this is not a kind of data Onyx takes at the moment.',
	spam: 'Reason: the proposal does not describe real data.',
	other: 'Reason: another reason, which this page cannot show.'
};
const WITH_REASON = ['on_hold', 'dropped'];
export function validReceiptId(id: string): boolean { return /^sub_[0-9a-f]{32}$/.test(id); }
const HASH = /^0x[0-9a-f]{64}$/;
const DATE = /^\d{4}-\d{2}-\d{2}$/;
const REGISTRY = /^(simulated|eip155:[1-9]\d{0,19})$/;
// PP11: the value the observers agreed on, as the feed's decimal text (no exponent, no leading zero).
const VALUE = /^-?(0|[1-9]\d*)(\.\d+)?$/;
function parseCheck(value: unknown, status: string): ReceiptCheck {
	const bad = new Error('The server did not return a valid receipt.');
	if (status !== 'approved' || !value || typeof value !== 'object' || Array.isArray(value)) throw bad;
	const c = value as Record<string, unknown>;
	if (Object.keys(c).sort().join() !== 'level,recordDate,registry,result,roundId,value') throw bad;
	if (c.level !== 2 || typeof c.recordDate !== 'string' || !DATE.test(c.recordDate) || typeof c.roundId !== 'string' || !HASH.test(c.roundId) || typeof c.result !== 'string' || !HASH.test(c.result) || typeof c.registry !== 'string' || !REGISTRY.test(c.registry) || typeof c.value !== 'string' || c.value.length > 80 || !VALUE.test(c.value)) throw bad;
	return { level: 2, recordDate: c.recordDate, roundId: c.roundId, result: c.result, value: c.value, registry: c.registry };
}
export function parseReceipt(value: unknown): Receipt {
	if (!value || typeof value !== 'object') throw new Error('The server did not return a valid receipt.');
	const b = value as Record<string, unknown>;
	if (typeof b.id !== 'string' || !validReceiptId(b.id) || typeof b.status !== 'string' || !Object.hasOwn(RECEIPT_STATUS, b.status)) throw new Error('The server did not return a valid receipt.');
	if (b.reason !== undefined) {
		// A reason only where a reviewer gives one, only a code this page has words for, and never beside observer evidence.
		if (typeof b.reason !== 'string' || !WITH_REASON.includes(b.status) || !Object.hasOwn(RECEIPT_REASON, b.reason) || b.check !== undefined) throw new Error('The server did not return a valid receipt.');
		return { id: b.id, status: b.status, reason: b.reason };
	}
	if (b.check === undefined) return { id: b.id, status: b.status };
	return { id: b.id, status: b.status, check: parseCheck(b.check, b.status) };
}
/** The observers' value with thousands separators, digit for digit (text only, never a float). */
export function groupedValue(v: string): string {
	const neg = v.startsWith('-');
	const [int, frac] = (neg ? v.slice(1) : v).split('.');
	const grouped = int.replace(/\B(?=(\d{3})+(?!\d))/g, ',');
	return (neg ? '-' : '') + grouped + (frac === undefined ? '' : '.' + frac);
}
/** What a fed receipt's evidence means, in the receipt's own words. */
export function checkSummary(c: ReceiptCheck): { title: string; meaning: string; limit: string; where: string } {
	return {
		title: 'Checked by observers (level 2, re-observed)',
		meaning: `Several observers fetched the source's own page for ${c.recordDate} in the same time window, sealed their answers, and agreed on one value. That value comes from the observers, not from your proposal.`,
		limit: 'This shows the source published this value. It does not prove the source itself is correct.',
		where: c.registry === 'simulated' ? 'Local rehearsal: this round was not posted on a chain.' : `Round registered on chain ${c.registry.slice('eip155:'.length)}.`
	};
}
