// The demo export (adapters/source-btc/examples/export-demo.rs). It holds the
// claims to check; the checks themselves run in the Rust verifier.
export type Label = { address: string; entity: string; kind: string; confidence_ppm: number };
export type ProofJson = { index: number; leaf_count: number; siblings: string[] };
export type RunJson = {
	id: string;
	label_set: string;
	/** `null` in a public build: the recipe input holds the label rows (AUD-2-F1). */
	input: string | null;
	input_bytes: number;
	root: string;
	shard_id: string;
	fuel: number;
	wasmi_secs: number;
	rows: { entity: string; received_sats: string; sent_sats: string; net_sats: string; tx_count: number; leaf: string }[];
	proofs?: ProofJson[];
	labels_dropped?: number;
};
export type LiveBlock = {
	height: number;
	hash: string;
	header: string;
	txs: number;
	/** Header time (unix seconds), read from bytes 68..72 of the header. */
	time?: number;
	outputs?: number;
	output_sats?: string;
};
type LiveRow = { entity: string; received_sats: string; sent_sats: string; net_sats: string; tx_count: number; leaf: string };
export type Demo = {
	/** Set by a public build (`ONYX_PUBLIC=1 scripts/build-web.sh`): no label rows shipped (AUD-2-F1). */
	public?: boolean;
	/** Set by a rights-fallback build (`ONYX_RIGHTS=fallback`, ROADMAP L14): no label-derived
	 *  total shipped; run and live `rows` are empty and `spread`, `per_block`, `series` absent. */
	rights?: 'fallback';
	host_id: string;
	recipe_id: string;
	registry_root: string;
	snapshot: { chain: string; from_height: number; to_height: number; content_hash: string; source_bytes: number };
	blocks: { height: number; hash: string; txs: number; outputs: number; output_sats: string }[];
	/** The labels themselves travel inside the recipe input; the page decodes them from there. */
	label_set: { name: string; version: number; content_hash: string; count: number };
	runs: RunJson[];
	/** BIP-322 test vectors and the verdicts the Rust verifier gave them. */
	bip322_vectors?: { body: string; expect: string; got: string; variant: string }[];
	commitment: { round: number; miner: string; salt: string; binding_hash: string; commit_hash: string };
	spread?: { entity: string; values: { set: string; net_sats: string }[]; independent_recount: string };
	attestation: {
		statement: string;
		btc_address: string;
		address_sig: string;
		entity_key: string;
		entity_sig: string;
		content_hash: string;
		verdict: string;
		registry_domain: string;
	};
	live?: {
		from_height: number;
		to_height: number;
		node: string;
		content_hash: string;
		root: string;
		python_recount: string;
		blocks: LiveBlock[];
		rows: LiveRow[];
		/** The same recipe on each single block of the range. */
		/** `rows`: only companies active in the block; `root` and `leaf_count` cover the whole shard. */
		per_block?: { height: number; root: string; leaf_count?: number; rows: LiveRow[] }[];
		/** `btc-entity-flow-series` on the whole range: one row per company per touched block. */
		series?: {
			recipe_id: string;
			root: string;
			shard_id: string;
			leaf_count: number;
			fuel: number;
			python_recount: string;
			/** No labelled entity moved in the range: the recipe's NO_ACTIVITY leaf instead of rows (R-SERIES-1). */
			no_activity?: boolean;
			no_activity_leaf?: string | null;
			rows: { entity: string; height: number; net_sats: string; tx_count: number; leaf: string }[];
		};
	};
};

let cache: Promise<Demo> | null = null;
const inputs = new Map<string, Promise<Uint8Array>>();
let wasm: Promise<Uint8Array> | null = null;

export function loadDemo(): Promise<Demo> {
	cache ??= fetch('data/demo.json').then((r) => {
		if (!r.ok) throw new Error(`data/demo.json: HTTP ${r.status}`);
		return r.json() as Promise<Demo>;
	});
	return cache;
}

let historyIndex: Promise<string> | null = null;
const historyDays = new Map<string, Promise<string>>();

function text(path: string): Promise<string> {
	return fetch(path).then((r) => {
		if (!r.ok) throw new Error(`${path}: HTTP ${r.status}`);
		return r.text();
	});
}

/** The one-week history index (examples/export-history.rs), as text: the
 *  verifier parses and checks it; the page draws only what checks return. */
export function loadHistoryIndex(): Promise<string> {
	historyIndex ??= text('history/week/index.json');
	return historyIndex;
}

/** One day of the history's leaves (`<YYYY-MM-DD>.json`), loaded when the
 *  page checks that day. */
export function loadHistoryDay(file: string): Promise<string> {
	if (!/^\d{4}-\d{2}-\d{2}\.json$/.test(file)) return Promise.reject(new Error(`bad day file ${file}`));
	let p = historyDays.get(file);
	if (!p) {
		p = text(`history/week/${file}`);
		historyDays.set(file, p);
	}
	return p;
}

let pools: Promise<string> | null = null;

/** Blocks per mining pool over the one-week history, one shard per UTC day
 *  (examples/export-pools.rs), as text: the verifier checks it against the pins. */
let reserves: Promise<string> | null = null;

/** The one-week reserve history (examples/export-reserves.rs, ADR-0034), as text. */
export function loadReserves(): Promise<string> {
	reserves ??= text('history/reserves-week.json');
	return reserves;
}

/** Label set v3, the issuers' own rosters (ROADMAP P2a, examples/export-rosters.rs): every
 *  entity's source and grade, and its daily flows over the fixture blocks. Private preview only:
 *  a rights-fallback build ships no such file (ADR-0035). */
export type RosterEntity = {
	entity: string;
	kind: string;
	source: string;
	source_url: string;
	snapshots: number;
	first_issued: string;
	last_issued: string;
	last_height: number;
	grade: string;
	memberships: number;
	signed_memberships: number;
	addresses: number;
	open_at_publication: number;
	in_fixture_range: number;
	balance: string;
};
export type RosterRow = { entity: string; received_sats: string; sent_sats: string; tx_count: number; addresses_at_end: number; balance_sats: string | null };
export type Rosters = {
	label_set: { name: string; version: number; published: number; content_hash: string };
	range: { from: number; to: number; scan_height: number };
	entities: RosterEntity[];
	days: { date: string; from: number; to: number; rows: RosterRow[] }[];
	v2_cross_checks: { entity: string; height: number; balance_sats: string }[];
	shards: unknown[];
};
let rosters: Promise<Rosters> | null = null;
export function loadRosters(): Promise<Rosters> {
	rosters ??= text('history/rosters-week.json').then((t) => JSON.parse(t) as Rosters);
	return rosters;
}

export function loadPools(): Promise<string> {
	pools ??= text('history/pools-week.json');
	return pools;
}

let chain: Promise<string> | null = null;
let chainWasm: Promise<Uint8Array> | null = null;
let chainInput: Promise<Uint8Array> | null = null;

/** The on-chain window (examples/export-chain-stats.rs, ADR-0043): one `btc-chain-stats`
 *  leaf per block, as text; the verifier checks it against the pins. Public chain data:
 *  shipped in every build. */
export function loadChainStats(): Promise<string> {
	chain ??= text('onchain/btc-chain-stats.json');
	return chain;
}

/** The `btc-chain-stats` recipe (copied from recipes/out by scripts/build-web.sh). */
export function loadChainRecipe(): Promise<Uint8Array> {
	chainWasm ??= fetch('data/chain-stats.wasm').then(async (r) => {
		if (!r.ok) throw new Error(`data/chain-stats.wasm: HTTP ${r.status}`);
		return new Uint8Array(await r.arrayBuffer());
	});
	return chainWasm;
}

/** The full recipe input of the window's smallest block, base64 text. */
export function loadChainInput(): Promise<Uint8Array> {
	chainInput ??= text('onchain/rerun-input.b64.txt').then((t) => decodeBase64(t.trim()));
	return chainInput;
}

let level2: Promise<string> | null = null;

/** The recorded level-2 round on U.S. Treasury Fiscal Data
 *  (adapters/source-fiscaldata, examples/level2-round.rs), as text. */
export function loadLevel2(): Promise<string> {
	level2 ??= text('level2/debt-to-penny.json');
	return level2;
}

/** The daily level-2 sources (NS3) and their recorded round summaries. */
export const DAILY_SOURCES = {
	usgs: 'level2/usgs-quakes.json',
	ncei: 'level2/ncei-tmax.json'
} as const;
export type DailySource = keyof typeof DAILY_SOURCES;
const daily: Partial<Record<DailySource, Promise<string>>> = {};

/** One daily source's recorded level-2 round (adapters/source-usgs, adapters/source-ncei,
 *  examples/level2-round.rs), as text. */
export function loadDaily(source: DailySource): Promise<string> {
	daily[source] ??= text(DAILY_SOURCES[source]);
	return daily[source];
}

let secHoldings: Promise<string> | null = null;

/** The recorded level-2 rounds on SEC EDGAR filings
 *  (adapters/source-edgar, examples/level2-round.rs), as text. */
export function loadSecHoldings(): Promise<string> {
	secHoldings ??= text('level2/sec-holdings.json');
	return secHoldings;
}

let etfHoldings: Promise<string> | null = null;

/** Spot bitcoin ETF holdings from SEC filings (ROADMAP P2c; adapters/source-edgar,
 *  examples/etf-holdings.rs), as text. */
export function loadEtfHoldings(): Promise<string> {
	etfHoldings ??= text('level2/etf-holdings.json');
	return etfHoldings;
}

export function loadRecipe(): Promise<Uint8Array> {
	wasm ??= fetch('data/recipe.wasm').then(async (r) => new Uint8Array(await r.arrayBuffer()));
	return wasm;
}

type B64 = { fromBase64?: (s: string) => Uint8Array };

/** Base64 to bytes. Native `Uint8Array.fromBase64` where the browser has it
 *  (a 2.4 MB input took ~560 ms on a slow phone with the manual path). */
function decodeBase64(t: string): Uint8Array {
	const native = (Uint8Array as unknown as B64).fromBase64;
	if (native) return native(t);
	const bin = atob(t);
	const out = new Uint8Array(bin.length);
	for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
	return out;
}

/** Recipe input for a run, stored as base64 text (some hosts refuse .bin files). */
export function loadInput(path: string | null): Promise<Uint8Array> {
	if (path === null) return Promise.reject(new Error('no recipe input: the public build ships no label rows'));
	let p = inputs.get(path);
	if (!p) {
		p = fetch(path)
			.then((r) => r.text())
			.then((t) => decodeBase64(t.trim()));
		inputs.set(path, p);
	}
	return p;
}

/** Run `f` once the browser is idle (after first paint), so the verifier
 *  (1.8 MiB gzip) and the inputs never delay what the page shows first. */
export function whenIdle(f: () => void): void {
	const idle = (globalThis as { requestIdleCallback?: (cb: () => void, o?: { timeout: number }) => void }).requestIdleCallback;
	const go = () => (idle ? idle(f, { timeout: 1500 }) : setTimeout(f, 200));
	// Wait for the fonts too: text repaints when they arrive, and that repaint
	// is the page's largest paint, so heavy downloads start after it.
	const fonts = (globalThis as { document?: Document }).document?.fonts;
	if (fonts) fonts.ready.then(go, go);
	else go();
}

/** Run `f` once, the first time one of `els` is on screen (W4): a page that shows checked
 *  values only further down loads the verifier (1.8 MiB) and the inputs when the reader gets
 *  there, or when they press a button that needs them, never with the first paint. Without
 *  IntersectionObserver, when the browser is idle. Returns a function that stops waiting. */
export function whenVisible(els: (Element | undefined | null)[], f: () => void): () => void {
	const targets = els.filter((e): e is Element => !!e);
	const IO = (globalThis as { IntersectionObserver?: typeof IntersectionObserver }).IntersectionObserver;
	if (!IO || !targets.length) {
		whenIdle(f);
		return () => {};
	}
	const io = new IO((entries) => {
		if (!entries.some((e) => e.isIntersecting)) return;
		io.disconnect();
		f();
	});
	for (const t of targets) io.observe(t);
	return () => io.disconnect();
}

let ethChain: Promise<string> | null = null;
let ethChainWasm: Promise<Uint8Array> | null = null;
let ethChainInput: Promise<Uint8Array> | null = null;

/** The Ethereum window (adapters/source-eth/examples/export-eth-chain-stats.rs, ADR-0044): one
 *  `eth-chain-stats` leaf per block, as text; the verifier checks it against the pins. Public
 *  chain data: shipped in every build. */
export function loadEthChainStats(): Promise<string> {
	ethChain ??= text('onchain/eth-chain-stats.json');
	return ethChain;
}

/** The `eth-chain-stats` recipe (copied from recipes/out by scripts/build-web.sh). */
export function loadEthChainRecipe(): Promise<Uint8Array> {
	ethChainWasm ??= fetch('data/eth-chain-stats.wasm').then(async (r) => {
		if (!r.ok) throw new Error(`data/eth-chain-stats.wasm: HTTP ${r.status}`);
		return new Uint8Array(await r.arrayBuffer());
	});
	return ethChainWasm;
}

/** The full recipe input of the window's rerun block, base64 text. */
export function loadEthChainInput(): Promise<Uint8Array> {
	ethChainInput ??= text('onchain/eth-rerun-input.b64.txt').then((t) => decodeBase64(t.trim()));
	return ethChainInput;
}
