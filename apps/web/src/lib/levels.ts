// The five proof levels in one wording (newcomer test 2026-11-18: level 4 read "a claim nobody
// has checked" on Add data and "Bonded claim: submitter stakes" on How it works). Add data and
// How it works both print this table; `mark` says what is built and what is only planned.
// Pure (no Svelte, no DOM), so `node --test tests/catalog.test.mjs` imports it as is.

export type Level = { level: number; name: string; means: string; mark: string };

export const LEVELS: Level[] = [
	{ level: 1, name: 'Recomputed', means: 'Anyone reruns the recipe on public inputs.', mark: 'Built' },
	{ level: 2, name: 'Re-observed', means: 'Observers drawn at random from a bonded pool fetch the same source and must agree.', mark: 'Built' },
	{ level: 3, name: 'Source-proven', means: 'The source signs it, or a TLS proof shows the web response.', mark: 'Built' },
	{ level: 4, name: 'Claim', means: 'A claim nobody has checked, labelled unverified.', mark: 'Built; the bond (staking on the claim) is planned' },
	{ level: 5, name: 'Track record', means: 'A forecast, scored when the truth arrives later.', mark: 'Built' }
];
