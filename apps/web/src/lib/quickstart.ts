// The API page's "Get data in 3 commands" box and its check commands (newcomer finding A14).
// Pure (no Svelte, no DOM), so `node --test tests/quickstart.test.mjs` imports it as is.
//
// Review NEWC-R1 H1: the check commands must work with the onyx-check a reader can download
// today (RELEASE in release.ts). Since v0.4.0-testnet (READINESS A21) that one reads a newest-first
// page with `--order desc` and proves the page starts at the dataset's newest row, so the first
// example checks the same newest-first read as the third read command. The second example stays
// oldest first with whole-number bounds: the form every released onyx-check reads.

/** The dataset the examples read: one the hosted test node lists, with data up to this month
 * (READINESS A20: `btc.mvrv` ended 2026-08-28; this one ran to 2026-11-13 when checked on
 * 2026-11-16, and holds every day of CHECK_RANGE). */
export const EXAMPLE = { publisher: 'onchain-archive', dataset: 'wbtc.mvrv' };
/** OnyxSignedBook on Base Sepolia (deploy/early-record/contracts.json). */
export const BOOK = '0x04bb31d80c8ce9df02c8a03486b5812ec6e5ccc5';
/** The oldest-first check example's key range: 2026-01-01 to 2026-01-31, as the days' first seconds (UTC). */
export const CHECK_RANGE = { from: 1767225600, to: 1769817600 };

const rows = (origin: string) => `${origin}/v1/bulk/${EXAMPLE.publisher}/${EXAMPLE.dataset}/rows`;

/** The three read commands against `origin`. */
export function quickCommands(origin: string): { what: string; cmd: string }[] {
	return [
		{ what: 'Find a dataset by words in its name', cmd: `curl "${origin}/v1/bulk?q=${EXAMPLE.dataset}&limit=5"` },
		{ what: 'Read what it is: columns, license, rows, signer', cmd: `curl ${origin}/v1/bulk/${EXAMPLE.publisher}/${EXAMPLE.dataset}` },
		{ what: 'Read rows, newest first, each with its proof', cmd: `curl "${rows(origin)}?order=desc&limit=3"` }
	];
}

const check = (args: string) =>
	`onyx-check --bulk rows.json --publisher ${EXAMPLE.publisher} --dataset ${EXAMPLE.dataset} ${args} --rpc https://sepolia.base.org --chain-id 84532 --book ${BOOK}`;

/** Save the newest rows and check them: two lines for the released onyx-check (v0.4.0-testnet or
 * later). With `--order desc` and no `--to`, the check also proves the page starts at the newest row. */
export function checkCommands(origin: string): string {
	return `curl -s "${rows(origin)}?order=desc&limit=5" > rows.json\n` + check('--order desc');
}

/** Save a range, oldest first, and check it: two lines every released onyx-check accepts. */
export function checkRangeCommands(origin: string): string {
	const { from, to } = CHECK_RANGE;
	return `curl -s "${rows(origin)}?from=${from}&to=${to}&limit=31" > rows.json\n` + check(`--from ${from} --to ${to}`);
}
