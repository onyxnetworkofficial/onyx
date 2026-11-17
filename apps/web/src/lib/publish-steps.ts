// The publish page's steps (ADR-0066 item 2): the applicant's ten steps, numbered and titled as the
// guide docs/ops/bulk-publish.md numbers and titles them, each with the guide's exact command
// (tests/publishers.test.mjs holds every command line and every `onyx-publish` subcommand against
// the guide's text and the program's own usage, so the page and the tools cannot drift apart).
// Pure (no Svelte, no DOM), so `node --test tests/publishers.test.mjs` imports it as is.
import { BASE, RELEASE, REPO, programs } from './release.ts';
import { BOOK, HOSTED_NODE } from './quickstart.ts';

/** Base Sepolia, the test network's chain. */
export const CHAIN_ID = 84532;
export const RPC = 'https://sepolia.base.org';
export { BOOK, HOSTED_NODE };
/** The status list of the hosted site, to save for `doctor --publishers`. */
export const STATUS_URL = `${HOSTED_NODE}/publishers.json`;

/** The publisher tools' file in the release the site links (release.ts). */
export const TOOLS_FILE = programs().find((p) => p.id === 'publish')!.files[0];
export const TOOLS_URL = BASE + TOOLS_FILE;
/** Unpack the download and put its programs (`onyx-publish/bin/`) on the PATH of this terminal:
 * every command below is a bare `onyx-publish ...`. */
export const UNPACK = [`tar -xzf ${TOOLS_FILE}`, 'export PATH="$PWD/onyx-publish/bin:$PATH"'];
/** The other way to the tools: build them from the source of `release` (the same code as the
 * download, never an unreleased state), then put `target/release/` on the PATH. */
export const buildFromSource = (release: string = RELEASE) => [
	`git clone --branch ${release} ${REPO} && cd onyx`,
	'cargo build --release -p onyx-publish -p onyx-check',
	'export PATH="$PWD/target/release:$PATH"'
];

/** The second way to file an application: a public issue on the source repository. The first is
 * the page's form, which sends the same two values to the site's node (publisher-pointer.ts). */
export const ISSUES = `${REPO}/issues`;
export const ISSUE_TITLE = 'Publisher application: <publisher id>';
/** The issue's two lines (.github/ISSUE_TEMPLATE/publisher-application.md). */
export const ISSUE_LINES = ['origin: <your origin, for example data.example.org/river-levels>', 'application keccak: <0x and 64 hex digits>'];

/** Made-up examples (they name no real publisher). */
export const EXAMPLE_ID = 'river-levels.open';
export const EXAMPLE_ORIGIN = 'data.example.org/river-levels';

export type Step = {
	title: string;
	/** How long it takes. */
	time: string;
	/** Whether what the step makes leaves the applicant's computer. */
	leaves: 'Never leaves your computer' | 'Becomes public' | 'Nothing is sent';
	what: string;
	/** Each entry is one block: a command, or two lines that belong together. */
	commands: string[];
	/** A warning that belongs to this step. */
	warn?: string;
};

/** The schema example of the guide's step 3. */
export const SCHEMA_EXAMPLE = `{"columns": [
  {"name": "t", "kind": "int"},
  {"name": "metric", "kind": "text"},
  {"name": "value", "kind": "decimal", "scale": 8, "nullable": true}
], "key": "t"}`;

/** The ten steps, numbered and titled as the guide's, against `node` (the node's address). */
export function steps(node: string = HOSTED_NODE): Step[] {
	const chain = `--rpc ${RPC} --chain-id ${CHAIN_ID} --book ${BOOK}`;
	return [
		{
			title: 'Make two keys',
			time: '2 minutes',
			leaves: 'Never leaves your computer',
			what: 'A working key and a spare. Each command prints only the key’s address and also writes it next to the key (publisher.key.address, spare.key.address: public, safe to keep). Move spare.key to a USB stick or another offline place now, and leave spare.key.address in the folder: the doctor counts your spare by it.',
			commands: ['onyx-publish keygen --out publisher.key', 'onyx-publish keygen --out spare.key'],
			warn: 'If you lose your only key, your publisher id is lost: nobody can prove that a new key is yours. Never send a key file to anyone.'
		},
		{
			title: 'Make your keys document',
			time: '1 minute',
			leaves: 'Becomes public',
			what: 'A small file that says which key signs for your id. The folder site/ mirrors your origin: what is in it goes online, nothing else.',
			commands: ['onyx-publish keys-doc --key publisher.key --publisher <publisher id> --out site/.well-known/onyx-source-keys.json']
		},
		{
			title: 'Prepare a small first dataset',
			time: 'depends on you',
			leaves: 'Nothing is sent',
			what: 'A real dataset, small: one CSV file with a header line, at most 8 MiB, sorted by its key column (usually a time in unix seconds). The reviewer reads its rows. It becomes version 1 of that dataset; you add the rest later as version 2. Write a schema.json that names every column, for example:',
			commands: [SCHEMA_EXAMPLE]
		},
		{
			title: 'Build and sign it',
			time: '1 minute',
			leaves: 'Becomes public',
			what: 'The build checks every row and stops at the first bad one with its line number. Pass the licence your data is under to --license, as its short standard name (an SPDX id such as CC0-1.0 or CC-BY-4.0). Then sign what was built with your key.',
			commands: [
				'onyx-publish build --schema schema.json --input data.csv --dataset <dataset id> --publisher <publisher id> --license CC0-1.0 --out site/sample',
				'onyx-publish sign --key publisher.key --dir site/sample'
			]
		},
		{
			title: 'Write your application',
			time: '1 minute',
			leaves: 'Becomes public',
			what: 'Do this within the hour after the build. The file holds your id, origin and key address, one sentence about the data, the sample’s fingerprint and the limits you ask for, signed with your key. It expires after 6 days.',
			commands: [`onyx-publish apply --key publisher.key --publisher <publisher id> --origin <origin> --data "<one sentence about the data>" --chain-id ${CHAIN_ID} --book ${BOOK}`],
			warn: 'From here on, never build version 1 of this dataset again: a second version 1 would suspend your key.'
		},
		{
			title: 'Put site/ on your origin; run the doctor',
			time: '10 minutes',
			leaves: 'Nothing is sent',
			what: 'Copy the content of site/ to your origin (HTTPS, no redirect). The doctor reads the files back, compares them with yours and prints one line per step: ok, missing or problem, each with the next command. When nothing is missing it prints the two lines for the next step. If you made your spare key somewhere else, add --spare-offline.',
			commands: ['onyx-publish doctor --live']
		},
		{
			title: 'Point the reviewer at your origin',
			time: '2 minutes',
			leaves: 'Becomes public',
			what: 'Send two values: your origin and the hash of your application file, as the doctor printed them. You never send the application itself; the review fetches it from your origin. There are two ways, and one is enough.',
			commands: [ISSUE_LINES.join('\n')],
			warn: 'The form needs no account, and the node keeps no address of yours. The issue is public and shows the account that opens it. Your origin is public either way.'
		},
		{
			title: 'Wait for the decision',
			time: 'up to 5 days',
			leaves: 'Nothing is sent',
			what: 'The decision is approved, or refused with a reason. It is written to the status list at the foot of this page, where you look it up by your application’s hash; an application filed as an issue is also answered there. To read the list in your terminal, save it and let the doctor print your line.',
			commands: [`curl -s -o publishers.json ${STATUS_URL}\nonyx-publish doctor --publishers publishers.json`]
		},
		{
			title: 'Upload your first dataset',
			time: 'a wait of 20 to 40 minutes, then about 10 minutes',
			leaves: 'Becomes public',
			what: 'The reviewer’s run records your sample’s signed report with your registration, so you need no test ETH for it. The node takes your upload once the test chain has made that final; the command waits and says so.',
			commands: [`onyx-publish upload --dir site/sample --node ${node} --wait-secs 3600`]
		},
		{
			title: 'Check it like a buyer would',
			time: '2 minutes',
			leaves: 'Nothing is sent',
			what: 'Read rows back from the node and prove each one against your signature and the test chain. It prints "ok": true when every row is proven.',
			commands: [
				`curl -s "${node}/v1/bulk/<publisher id>/<dataset id>/rows?from=<a key>&to=<a later key>&limit=100" > rows.json\n` +
					`onyx-check --bulk rows.json --publisher <publisher id> --dataset <dataset id> --from <a key> --to <a later key> ${chain}`
			]
		}
	];
}

/** Every `onyx-publish` subcommand the page's commands name (the steps and the tools). */
export function subcommands(): string[] {
	const lines = steps().flatMap((s) => s.commands.flatMap((c) => c.split('\n')));
	return [...new Set(lines.flatMap((l) => (/^onyx-publish ([a-z-]+)/.exec(l) ?? []).slice(1)))];
}

/** The words a newcomer may not know, each in one sentence. */
export const WORDS: { word: string; means: string }[] = [
	{ word: 'Signed book', means: 'The contract on the test chain that records which key signs for which publisher id. Only the reviewer’s key can add one.' },
	{ word: 'Hash', means: 'A short fingerprint of a file, written as 0x and 64 digits (Keccak-256). Change one byte of the file and the hash changes.' },
	{ word: 'SPDX id', means: 'The short standard name of a licence, such as CC0-1.0 or CC-BY-4.0.' },
	{ word: 'Test ETH', means: 'What a transaction on the test chain is paid with. It is free from a faucet and worth nothing.' }
];
