// The five proof levels in one wording (newcomer test 2026-11-18: level 4 read "a claim nobody
// has checked" on Add data and "Bonded claim: submitter stakes" on How it works), and ONE status
// vocabulary for the whole site (newcomer test 3, 2026-11-18: "Live", "Built" and "recorded"
// disagreed between How it works, Add data and Browse data). Add data, How it works and the data
// map all print this table; no page writes a status word of its own.
// Pure (no Svelte, no DOM), so `node --test tests/catalog.test.mjs` imports it as is.

/** The four states anything on the site can be in. */
export type Status = 'live' | 'recorded' | 'planned' | 'off';

/** The word shown for each state and what it means. */
export const STATUS: Record<Status, { label: string; means: string }> = {
	live: { label: 'Live now', means: 'Served by the hosted test node today.' },
	recorded: { label: 'Recorded', means: 'A past run, shown from files this site ships, with its date.' },
	planned: { label: 'Planned', means: 'Not built, or not running anywhere yet.' },
	off: { label: 'Switched off', means: 'Built, and turned off or held back today.' }
};

export const STATUSES = Object.keys(STATUS) as Status[];

export type Level = { level: number; name: string; means: string; status: Status; note: string };

/** Each level's state today and one line on what that covers. */
export const LEVELS: Level[] = [
	{ level: 1, name: 'Recomputed', means: 'Anyone reruns the recipe on public inputs.', status: 'recorded', note: 'blocks rerun in your browser' },
	{
		level: 2,
		name: 'Re-observed',
		means: 'Observers drawn at random from a bonded pool fetch the same source and must agree.',
		status: 'live',
		note: 'the hosted node, one operator; this site’s own rounds are recorded'
	},
	{
		level: 3,
		name: 'Source-proven',
		means: 'The source signs it, or a TLS proof shows the web response.',
		status: 'live',
		note: 'publisher-signed datasets; no signed data feed connected yet'
	},
	{ level: 4, name: 'Claim', means: 'A claim nobody has checked, labelled unverified.', status: 'recorded', note: 'unsigned lists; bond planned' },
	{ level: 5, name: 'Track record', means: 'A forecast, scored when the truth arrives later.', status: 'recorded', note: 'one example, none scored yet' }
];

/** A level's state as one line: "Live now: the hosted node serves …". */
export const levelMark = (l: Pick<Level, 'status' | 'note'>): string => `${STATUS[l.status].label}: ${l.note}`;

/** Payments (x402) are built and switched off on the hosted test node: every price on the site is
 * a planned price, and everything is free to read today. One sentence for every page that
 * shows a price. */
export const PAYMENTS: { status: Status; note: string } = {
	status: 'off',
	note: 'Payments are switched off on the hosted test node: everything is free to read today. Every price on this page is planned, or was paid in test tokens in a recorded run.'
};

/** "Planned: 0.25¢ an answer": a price nobody pays today. */
export const planned = (price: string): string => `Planned: ${price}`;
