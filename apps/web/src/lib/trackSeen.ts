// ROADMAP C20 item 4: the browser remembers the last track commitment it saw per record and asks
// with `since`, so a server that later serves an older commitment, or one that does not chain back
// to it, is refused (THREATS TR-R2-5, TR-S-14). Per viewer only: every read and write is wrapped,
// and a blocked or empty store is a first visit (no `since`), never an error.
const PREFIX = 'onyx-track-seen-v1:';
const HASH = /^0x[0-9a-f]{64}$/;

/** A commitment this browser saw for a record, and its `as_of` (the floor next time). */
export type Seen = { commitment: string; as_of: number };

const key = (source: string, metric: string) => `${PREFIX}${source}/${metric}`;

/** The commitment this browser saw last for (source, metric), if any and well formed. */
export function seenOf(source: string, metric: string): Seen | null {
	try {
		const raw = localStorage.getItem(key(source, metric));
		if (!raw) return null;
		const v = JSON.parse(raw) as Partial<Seen>;
		if (typeof v.commitment === 'string' && HASH.test(v.commitment) && Number.isSafeInteger(v.as_of) && (v.as_of as number) >= 0) {
			return { commitment: v.commitment, as_of: v.as_of as number };
		}
	} catch {
		// Storage blocked or a value this page did not write: a first visit.
	}
	return null;
}

/** Remember `s` for (source, metric) unless this browser already saw a later commitment. */
export function remember(source: string, metric: string, s: Seen): void {
	const old = seenOf(source, metric);
	if (old && old.as_of > s.as_of) return;
	try {
		localStorage.setItem(key(source, metric), JSON.stringify(s));
	} catch {
		// Not remembered: the next visit checks without `since`.
	}
}

/** Forget what this browser saw for (source, metric). */
export function forget(source: string, metric: string): void {
	try {
		localStorage.removeItem(key(source, metric));
	} catch {
		// Nothing stored, or storage blocked.
	}
}

/** `GET /v1/track/{source}/{metric}`, with `?since=` the commitment this browser saw last. */
export function recordUrl(base: string, source: string, metric: string, seen: Seen | null): string {
	const path = `${base.replace(/\/$/, '')}/v1/track/${encodeURIComponent(source)}/${encodeURIComponent(metric)}`;
	return seen ? `${path}?since=${seen.commitment}` : path;
}
