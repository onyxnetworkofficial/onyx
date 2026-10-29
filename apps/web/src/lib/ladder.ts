// NS5 (ADR-0048): a value's proof level and the rises that got it there, as the API's `ladder`
// field carries them. The page shows a ladder only after `checkLadder` accepts it: steps strictly
// stronger and no older than the one before, the top equal to the stated level, every name and
// record the level's and the lane's. A ladder that falls or states a level its steps do not reach
// is refused and shows no history (crates/onyx-api/src/ladder.rs is the client check it mirrors).

// A public build seals a level-2 ladder (scripts/build-web.sh, THREATS NS5-4): the value key and each
// step's evidence are unsalted functions of the value, which the paid API sells, so they are left out.
export type LadderStep = { level: number; name: string; evidence?: string; at: number; record: string };
// ADR-0048 amendment 2 (ROADMAP C64): a stronger answer of the same fact that states another value.
// It is shown next to the value and never changes its level; it carries the other value's hash only.
export type Contradiction = LadderStep & { value?: string };
export type Ladder = { value?: string; level: number; steps: LadderStep[]; contradictions?: Contradiction[]; sealed?: true };

export const LEVEL_SLUG: Record<number, string> = { 1: 'recomputed', 2: 're-observed', 3: 'source-proven', 4: 'claim', 5: 'track record' };
/** Where each step was made public: the chain record of its lane. */
export const RECORD_TEXT: Record<string, string> = {
	'OnyxRegistry.ShardAttested': 'shard level attested in the registry',
	'OnyxRegistry.ObservationRecorded': 'observation recorded in the registry',
	'OnyxSignedBook.ReportRecorded': 'signed report recorded in the book'
};

const HASH = /^0x[0-9a-f]{64}$/;

function only(o: Record<string, unknown>, keys: string[], what: string, out: string[]) {
	for (const k of Object.keys(o)) if (!keys.includes(k)) out.push(`${what}.${k}: unexpected field`);
}

/** Problems with a served ladder (empty: accepted). `stated` is the answer's own level. */
export function checkLadder(l: unknown, stated?: number): string[] {
	const out: string[] = [];
	if (!l || typeof l !== 'object') return ['the ladder is not an object'];
	const o = l as Record<string, unknown>;
	const sealed = o.sealed === true;
	only(o, sealed ? ['sealed', 'level', 'steps', 'contradictions'] : ['value', 'level', 'steps', 'contradictions'], 'ladder', out);
	if (!sealed && (typeof o.value !== 'string' || !HASH.test(o.value))) out.push('ladder.value is not a hash');
	const steps = Array.isArray(o.steps) ? (o.steps as unknown[]) : null;
	if (!steps || steps.length < 1 || steps.length > 5) return [...out, 'a ladder has one to five steps'];
	let before: LadderStep | null = null;
	steps.forEach((x, i) => {
		const what = `step ${i + 1}`;
		if (!x || typeof x !== 'object') {
			out.push(`${what} is not an object`);
			return;
		}
		const s = x as Record<string, unknown>;
		only(s, sealed ? ['level', 'name', 'at', 'record'] : ['level', 'name', 'evidence', 'at', 'record'], what, out);
		const level = s.level;
		if (typeof level !== 'number' || !Number.isInteger(level) || level < 1 || level > 5) {
			out.push(`${what}: not a level 1-5`);
			return;
		}
		if (s.name !== LEVEL_SLUG[level]) out.push(`${what}: the name is not level ${level}'s`);
		if (!sealed && (typeof s.evidence !== 'string' || !HASH.test(s.evidence))) out.push(`${what}: the evidence is not a hash`);
		if (typeof s.record !== 'string' || !(s.record in RECORD_TEXT)) out.push(`${what}: unknown chain record`);
		if (typeof s.at !== 'number' || !Number.isInteger(s.at) || s.at < 0) out.push(`${what}: not a time`);
		const step = s as unknown as LadderStep;
		if (before) {
			if (level >= before.level) out.push(`${what} is level ${level}, not stronger than level ${before.level}: a level never falls`);
			if (step.at < before.at) out.push(`${what} is older than the step before it`);
			if (step.record !== before.record) out.push(`${what} comes from another lane than this answer's`);
		}
		before = step;
	});
	const top = (steps[steps.length - 1] as LadderStep | undefined)?.level;
	if (o.level !== top) out.push(`the ladder says level ${String(o.level)}, its steps reach level ${String(top)}`);
	if (stated !== undefined && stated !== top) out.push(`the answer says level ${stated}, its ladder reaches level ${String(top)}`);
	if (o.contradictions !== undefined) {
		const cs = Array.isArray(o.contradictions) ? (o.contradictions as unknown[]) : null;
		if (!cs || cs.length > 5) return [...out, 'ladder.contradictions is not a list of at most five'];
		cs.forEach((x, i) => {
			const what = `contradiction ${i + 1}`;
			if (!x || typeof x !== 'object') {
				out.push(`${what} is not an object`);
				return;
			}
			const c = x as Record<string, unknown>;
			only(c, sealed ? ['level', 'name', 'at', 'record'] : ['level', 'name', 'evidence', 'at', 'record', 'value'], what, out);
			const level = c.level;
			if (typeof level !== 'number' || !Number.isInteger(level) || level < 1 || level > 5) {
				out.push(`${what}: not a level 1-5`);
				return;
			}
			// R-LAD2-3: only a stronger answer contradicts; it never moves this value's level.
			if (typeof top === 'number' && level >= top) out.push(`${what} is level ${level}, not stronger than this value's level ${top}`);
			if (c.name !== LEVEL_SLUG[level]) out.push(`${what}: the name is not level ${level}'s`);
			if (typeof c.record !== 'string' || !(c.record in RECORD_TEXT)) out.push(`${what}: unknown chain record`);
			if (typeof c.at !== 'number' || !Number.isInteger(c.at) || c.at < 0) out.push(`${what}: not a time`);
			if (!sealed && (typeof c.evidence !== 'string' || !HASH.test(c.evidence))) out.push(`${what}: the evidence is not a hash`);
			if (!sealed && (typeof c.value !== 'string' || !HASH.test(c.value))) out.push(`${what}: the value is not a hash`);
		});
	}
	return out;
}

const same = (a: LadderStep, b: LadderStep) => a.level === b.level && a.evidence === b.evidence && a.at === b.at && a.record === b.record;

/** THREATS NS5-9, NS5-10: problems with `next` as a later answer for the value of `prev`, a ladder this
 * reader accepted before (crates/onyx-core ladder.rs `check_extends`). Every step seen before must still be
 * there, or be superseded by a stronger step no later than it (a proof found late); the level never falls. */
export function checkExtends(prev: Ladder, next: Ladder): string[] {
	if (prev.value !== next.value) return ['the ladder is of another value'];
	const out: string[] = [];
	prev.steps.forEach((s, i) => {
		if (next.steps.some((t) => same(s, t))) return;
		const superseded = next.steps.some((t) => t.level < s.level && t.at <= s.at);
		const changed = next.steps.some((t) => t.level === s.level && !same(s, t) && t.at <= s.at);
		if (!superseded || changed) out.push(`step ${i + 1}, accepted on an earlier visit, was dropped or changed: a level never falls`);
	});
	if (next.level > prev.level) out.push(`the level fell from ${prev.level} to ${next.level} since an earlier visit`);
	return out;
}

const SEEN = 'onyx-ladder:';

/** The ladder this reader accepted before for `value`, if any (per-browser memory; empty in a private window). */
export function seenLadder(value: string): Ladder | null {
	try {
		const s = localStorage.getItem(SEEN + value);
		return s ? (JSON.parse(s) as Ladder) : null;
	} catch {
		return null;
	}
}

/** Keep `l` as the accepted ladder of its value (only a ladder with a value key; a sealed one has none). */
export function keepLadder(l: Ladder): void {
	if (!l.value) return;
	try {
		localStorage.setItem(SEEN + l.value, JSON.stringify(l));
	} catch {
		// Storage off: nothing is remembered, the next visit checks the shape only.
	}
}

export type LadderEntry = { id: string; metric: string; subject: string; round: string; ladder: Ladder };
export type LadderFile = { chainId: number; registry: string; checked: string; values: LadderEntry[] };

let ladders: Promise<LadderFile> | null = null;

/** The ladders of the Base Sepolia level-2 values (ROADMAP B6), as the client check accepted them. */
export function loadLadders(): Promise<LadderFile> {
	ladders ??= fetch('level2/ladders-base-sepolia.json').then((r) => {
		if (!r.ok) throw new Error(`level2/ladders-base-sepolia.json: HTTP ${r.status}`);
		return r.json() as Promise<LadderFile>;
	});
	return ladders;
}
