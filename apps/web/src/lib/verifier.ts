// Calls into the Rust verifier worker. Every check on this site goes through
// here: none is written in TypeScript (ADR-0020).
type Pending = { resolve: (v: unknown) => void; reject: (e: Error) => void };

let worker: Worker | null = null;
let seq = 0;
const pending = new Map<number, Pending>();

function get(): Worker {
	if (!worker) {
		worker = new Worker(new URL('./verifier.worker.ts', import.meta.url), { type: 'module' });
		worker.onmessage = (e: MessageEvent<{ id: number; ok: boolean; value?: unknown; error?: string }>) => {
			const p = pending.get(e.data.id);
			if (!p) return;
			pending.delete(e.data.id);
			if (e.data.ok) p.resolve(e.data.value);
			else p.reject(new Error(e.data.error ?? 'verifier error'));
		};
		// If the worker cannot start, fail every waiting call instead of spinning forever.
		worker.onerror = (e) => {
			for (const p of pending.values()) p.reject(new Error(`verifier failed to start: ${e.message}`));
			pending.clear();
			worker = null;
		};
	}
	return worker;
}

function call<T>(fn: string, ...args: unknown[]): Promise<T> {
	const id = ++seq;
	return new Promise<T>((resolve, reject) => {
		pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
		// Plain copies: reactive proxies cannot cross to a worker.
		get().postMessage({ id, fn, args: args.map((a) => (Array.isArray(a) ? [...a] : a)) });
	});
}

export type FlowRow = {
	leaf: string;
	entity: string;
	from_height: number;
	to_height: number;
	received_sats: string;
	sent_sats: string;
	net_sats: string;
	tx_count: number;
};
export type FlowValues = { entity: string; received_sats: string; sent_sats: string; net_sats: string; tx_count: number };
/** Per-block rows checked against the range rows under the pinned root (verify/src/lib.rs). */
export type BlockSums = {
	ok: boolean;
	root: string;
	root_ok: boolean;
	entities: FlowValues[];
	blocks: { height: number; rows: FlowValues[] }[];
	problems: string[];
};
/** The per-block series checked against the range rows under both pinned roots (verify/src/lib.rs). */
export type SeriesSums = {
	ok: boolean;
	/** The series is the NO_ACTIVITY leaf: an all-zero series (R-SERIES-1). */
	no_activity: boolean;
	range_root: string;
	range_root_ok: boolean;
	series_root: string;
	series_root_ok: boolean;
	entities: {
		entity: string;
		total: Omit<FlowValues, 'entity'>;
		points: ({ height: number } & Omit<FlowValues, 'entity'>)[];
	}[];
	problems: string[];
};
/** A six-block bucket of one day: per-entity sums (claimed by the index until that day's leaves are checked). */
export type HistoryBucket = { from: number; to: number; entities: FlowValues[] };
/** The one-week history index, checked against the pins (verify/src/lib.rs `check_history_index`). */
export type HistoryIndexCheck = {
	ok: boolean;
	commitment: string;
	commitment_ok: boolean;
	from: number;
	to: number;
	tip: string;
	blocks: { height: number; time: number | null; hash: string }[];
	shards: { from: number; to: number; series_root: string; balance_root: string; no_activity: boolean }[];
	days: {
		date: string;
		from: number;
		to: number;
		file: string;
		shards: number;
		rows: number;
		first_time: number;
		last_time: number;
		buckets: HistoryBucket[];
	}[];
	problems: string[];
};
/** One day's leaves checked against the index (verify/src/lib.rs `check_history_day`). */
export type HistoryDayCheck = {
	ok: boolean;
	date: string;
	from: number;
	to: number;
	rows: number;
	buckets_ok: boolean;
	/** The day's six-block buckets recomputed from its checked rows. */
	buckets: HistoryBucket[];
	entities: { entity: string; points: ({ height: number } & Omit<FlowValues, 'entity'>)[] }[];
	problems: string[];
};
/** The one-week pool history, checked (verify/src/lib.rs `check_pools`): rows come from the checked leaves. */
export type PoolsCheck = {
	ok: boolean;
	commitment_ok: boolean;
	days: { date: string; from: number; to: number; ok: boolean; rows: { pool: string; blocks: number }[] }[];
	totals: Record<string, number>;
	problems: string[];
};
/** The one-week reserve history, checked (verify/src/lib.rs `check_reserves`): balances come from the checked chain. */
export type ReservesCheck = {
	ok: boolean;
	commitment_ok: boolean;
	/** ROADMAP D3: every company's base is the one derived from the pinned scan and the shards' flows. */
	derived_base_ok: boolean;
	base_height: number;
	scan_height: number;
	shards: number;
	week_shards: number;
	entities: { entity: string; points: { height: number; balance_sats: string }[] }[];
	scan_totals: { entity: string; balance_sats: string }[];
	problems: string[];
};
/** The recorded level-2 round, checked (verify/src/lib.rs `check_level2_round`). */
/** Every field the card shows comes from here (checked or pinned), never from the summary JSON (L2SRC-R2-F3). */
export type Level2Check = {
	ok: boolean;
	/** Shipped sealed (a public build, ATT-R2-F1): no value, only signed commits; the paid API sells the value. */
	sealed: boolean;
	value: string;
	commits_checked: number;
	page_hash: string;
	result_hash: string;
	record_date: string;
	subject: string;
	window_start: number;
	window_end: number;
	observers: number;
	/** Bonded pool the observers were drawn from at random (L2-F2). */
	pool_size: number;
	/** The fact's draws (the recorded announcement's draw count) and how many ended without a value (SEC-R2-F1). */
	draws: number;
	without_value: number;
	agreed: number;
	source_name: string;
	terms_url: string;
	terms_quote: string;
	attribution: string;
	/** ROADMAP C11: a round of a draw with a reveal key, its sealed posts checked (verify/src/sealed.rs); null for the recorded rounds (no reveal key). */
	sealed_reveals: { posts: number; dismissed: number; reveal_log_complete: string; attempt_matches: string | null } | null;
	problems: string[];
};
/** Spot bitcoin ETF holdings from SEC filings (static/level2/etf-holdings.json), recomputed by the verifier (verify/src/lib.rs `check_etf_holdings`). */
export type EtfHolding = { period_end: string; held_sats: string; held_btc: string; as_filed: string; accn: string; form: string; filed: string; filing_url: string; rows: number };
export type EtfFlow = { from: string; to: string; days: number; from_sats: string; to_sats: string; change_sats: string; change_btc: string; from_accn: string; to_accn: string };
export type EtfFund = {
	key: string;
	ticker: string;
	name: string;
	fund_name: string;
	sponsor: string;
	cik: string;
	fact: string;
	unit: string;
	url: string;
	metric: string;
	observers: number;
	holdings: EtfHolding[];
	refused: { period_end: string; reason: string }[];
	flows: EtfFlow[];
};
export type EtfNotCovered = { ticker: string; name: string; cik: string | null; reason: string };
export type EtfSummary = {
	level: number;
	status: string;
	source: { name: string; terms_url: string; terms_quote: string; attribution: string; user_agent: string };
	window_start: number;
	rerun: string;
	flows_note: string;
	not_covered_note: string;
	funds: EtfFund[];
	us_not_covered: EtfNotCovered[];
	outside_us: EtfNotCovered[];
};
export type EtfCheck = { ok: boolean; problems: string[]; summary: EtfSummary | null };
/** One company's recorded SEC EDGAR round, checked (verify/src/lib.rs `check_edgar_rounds`). */
export type EdgarCompanyCheck = {
	ok: boolean;
	key: string;
	name: string;
	cik: string;
	unit: string;
	unit_meaning: string;
	filings_url: string;
	/** Shipped sealed (a public build, ATT-R2-F1). */
	sealed: boolean;
	value: string;
	period_end: string;
	window_start: number;
	window_end: number;
	observers: number;
	/** Bonded pool the observers were drawn from at random (L2-F2). */
	pool_size: number;
	/** The fact's draws (the recorded announcement's draw count) and how many ended without a value (SEC-R2-F1). */
	draws: number;
	without_value: number;
	agreed: number;
	result_hash: string;
	problems: string[];
};
/** The recorded SEC EDGAR rounds, checked. The card shows only these fields. */
export type EdgarCheck = {
	ok: boolean;
	source_name: string;
	terms_url: string;
	terms_quote: string;
	attribution: string;
	companies: EdgarCompanyCheck[];
	problems: string[];
};
/** One level-5 track record, checked (verify/src/track_view.rs): every field comes from the checked rows. */
export type TrackView = {
	source: string;
	metric: string;
	example: boolean;
	established: boolean;
	agreed: number;
	resolved: number;
	/** Only when established (proof-ladder.md "Display"). */
	rate: string | null;
	agreed_first: number;
	/** Only when established. */
	mean_brier: string | null;
	disagreed: number;
	equivocated: number;
	late: number;
	void: number;
	contested: number;
	self_resolved: number;
	unresolved: number;
	distinct_subjects: number;
	min_resolved: number;
	min_subjects: number;
	metric_sources: number;
	metric_resolved_facts: number;
	statements: number;
	history_checked: boolean;
	close_after_secs: number | null;
	/** A period rule (C9): the fact is the period ending at its date, closing `lead_secs` before the period starts. */
	period_secs: number | null;
	lead_secs: number | null;
	/** C9 skill against repeating the last value known at each close (persistence). */
	baseline_entries: number;
	baseline_source_agreed: number;
	baseline_agreed: number;
	/** Only when established: the source's rate minus persistence's, e.g. "+12.5 points". */
	skill: string | null;
	tolerance: string | null;
	resolver_levels: number[];
	intake: boolean;
	history: { subject: string; as_of: number; outcome: string; first: boolean; confidence_ppm: number; committed_at: number }[];
	/** C20: the commitment this browser saw last and asked with (`since`), when it did. */
	since?: string;
};
/** The site's example track records, checked against the pinned commitment (ROADMAP C6). */
export type TrackExampleCheck = { ok: boolean; commitment: string; as_of: number; records: TrackView[]; problems: string[]; unknown?: boolean };
/** The example's logs rerun in the browser (ROADMAP C20): `compute` again, every commitment extending the one before. */
export type TrackLogCheck = {
	ok: boolean;
	commitment: string;
	links: number | null;
	statements: number | null;
	answers: number | null;
	recomputed: boolean;
	intake: string | null;
	/** C23: the registry root the served registry was checked to (null when the check failed). */
	registry_root: string | null;
	problems: string[];
};
/** A level-3 answer checked by the verifier (verify/src/signed_view.rs, ROADMAP C61): every field the card shows. */
export type SignedCheck = {
	ok: boolean;
	problems: string[];
	/** The recorded rehearsal (chain answers recorded and pinned) or the reader's own RPC. */
	rehearsal: boolean;
	chain_reads: 'recorded' | 'yours';
	recorded_unix?: number;
	clock_stand_in_secs?: number;
	source: string;
	metric: string;
	subject_kind: string;
	subject: string;
	/** Digits (a fixed-point value with its decimals), never a float. */
	value: string;
	observed_unix: number;
	sequence: number;
	key: string;
	signer: string;
	fact: string;
	report_hash: string;
	value_hash: string;
	accepted_at: number;
	servable_at: number;
	block: number;
	block_hash: string;
	block_time: number;
	chain_id: number;
	book: string;
	code_hash: string;
};
export type Run = { outcome: string; fuel: string; recipe_id: string; root: string; rows: FlowRow[] };
/** Sums of the on-chain window's checked rows for one UTC day or the whole window
 *  (verify/src/chain.rs): amounts in satoshis as strings, shares in ppm. */
export type ChainSums = {
	blocks: number;
	first_time: number;
	last_time: number;
	txs: number;
	inputs: number;
	outputs: number;
	mean_size_bytes: string;
	fullness_ppm: number;
	mean_interval_secs: string;
	subsidy_sats: string;
	fees_sats: string;
	issued_sats: string;
	transfer_sats: string;
	segwit_ppm: number;
	taproot_inputs_ppm: number;
	taproot_outputs_ppm: number;
	coin_blocks_destroyed: string;
};
/** The on-chain window checked against its pins (verify/src/chain.rs `check_chain_stats`):
 *  every figure the page draws is in here, computed from the checked leaves. */
export type ChainCheck = {
	ok: boolean;
	commitment: string;
	commitment_ok: boolean;
	recipe_id: string;
	from: number;
	to: number;
	window: ChainSums;
	days: (ChainSums & { date: string; from: number; to: number; ok: boolean })[];
	blocks: {
		height: number;
		hash: string;
		time: number;
		interval_secs: number;
		txs: number;
		weight_wu: number;
		fees_sats: string;
		fee_rate_median_msat_vb: number;
		fee_rate_avg_msat_vb: number;
		active_scripts: number;
	}[];
	problems: string[];
};
/** One block's full input rerun here on the canonical engine (verify/src/chain.rs `rerun_chain_block`). */
export type ChainRerun = {
	ok: boolean;
	outcome: string;
	fuel: string;
	recipe_id: string;
	height: number;
	input_bytes: number;
	txs_in_input: number;
	root: string;
	row: { height: number; txs: number; fees_sats: string; fee_rate_median_msat_vb: number; active_scripts: number } | null;
	problems: string[];
};
export type Vector = { body: string; variant: string; expect: string; got: string };

export type Pinned = {
	recipe_id: string;
	input_keccak: string;
	root: string;
	/** Content hash of test label set v2 (AUD-2-F1: the public build checks it, it ships no rows). */
	label_set: string;
	live: { from: number; to: number; tip: string; root: string; series_root: string; series_recipe_id: string };
	history: { from: number; to: number; tip: string; commitment: string };
	chain: { recipe_id: string; from: number; to: number; tip: string; commitment: string; rerun_height: number };
	min_bits: number;
};
export type Inspected = {
	input_keccak: string;
	source_hash: string;
	chain: string;
	blocks: { height: number; hash: string; txs: number }[];
	label_set: {
		name: string;
		version: number;
		content_hash: string;
		labels: { address: string; entity: string; kind: string; confidence_ppm: number }[];
	};
};

/** One salted label row's check (verify/src/salted_view.rs). */
export type SaltedCheck = { ok: boolean; error: string | null; index: number | null; address: string | null; entity: string | null; from: number | null; to: number | null };

export const verifier = {
	pinned: async () => JSON.parse(await call<string>('pinned_json')) as Pinned,
	inspectInput: async (input: Uint8Array) => JSON.parse(await call<string>('inspect_input', input)) as Inspected,
	decodeLeaf: async (leaf: string) => JSON.parse(await call<string>('decode_flow_leaf', leaf)) as FlowRow,
	hostId: () => call<string>('host_id'),
	pinnedMinBits: () => call<number>('pinned_min_bits'),
	keccak: (b: Uint8Array) => call<string>('keccak', b),
	sourceHash: (input: Uint8Array) => call<string>('source_hash', input),
	runRecipe: async (wasm: Uint8Array, input: Uint8Array) =>
		JSON.parse(await call<string>('run_recipe', wasm, input)) as Run,
	merkleRoot: (rows: string[]) => call<string>('merkle_root', rows),
	verifyProof: (root: string, row: string, index: number, count: number, siblings: string[]) =>
		call<boolean>('verify_proof', root, row, index, count, siblings),
	recoverEip191: (msg: string, sig: string) => call<string>('recover_eip191', msg, sig),
	demoEntityKey: () => call<string>('demo_entity_key_address'),
	verifyAttestation: (statement: string, addressSig: string, key: string, sig: string, from: number, to: number) =>
		call<string>('verify_demo_attestation', statement, addressSig, key, sig, from, to),
	bip322Vectors: async () => JSON.parse(await call<string>('run_bip322_vectors')) as Vector[],
	blockSums: async (rangeRows: string[], blocks: { height: number; rows: string[] }[]) =>
		JSON.parse(await call<string>('check_block_sums', rangeRows, JSON.stringify(blocks))) as BlockSums,
	seriesSums: async (rangeRows: string[], seriesRows: string[]) =>
		JSON.parse(await call<string>('check_series', rangeRows, seriesRows)) as SeriesSums,
	historyIndex: async (index: string) => JSON.parse(await call<string>('check_history_index', index)) as HistoryIndexCheck,
	/** Check one day file for the slot `date` it was loaded for. */
	historyDay: async (index: string, date: string, day: string) =>
		JSON.parse(await call<string>('check_history_day', index, date, day)) as HistoryDayCheck,
	pools: async (doc: string) => JSON.parse(await call<string>('check_pools', doc)) as PoolsCheck,
	/** ROADMAP D30 (PP3): a bought salted label row, against the pin compiled into the verifier. */
	saltedPin: () => call<string>('salted_test_pin'),
	saltedAnswer: async (answer: string, pin: string) => JSON.parse(await call<string>('check_salted_answer', answer, pin)) as SaltedCheck,
	saltedGuess: (row: string, sibling: string) => call<boolean>('salted_guess_confirms', row, sibling),
	chainStats: async (doc: string) => JSON.parse(await call<string>('check_chain_stats', doc)) as ChainCheck,
	chainRerun: async (wasm: Uint8Array, input: Uint8Array, doc: string) =>
		JSON.parse(await call<string>('rerun_chain_block', wasm, input, doc)) as ChainRerun,
	reserves: async (doc: string) => JSON.parse(await call<string>('check_reserves', doc)) as ReservesCheck,
	level2Round: async (summary: string) => JSON.parse(await call<string>('check_level2_round', summary)) as Level2Check,
	/** A daily source's recorded round (NS3), the same checks with its own pins (verify/src/lib.rs `check_daily_round`). */
	dailyRound: async (source: 'usgs' | 'ncei', summary: string) => JSON.parse(await call<string>('check_daily_round', source, summary)) as Level2Check,
	edgarRounds: async (summary: string) => JSON.parse(await call<string>('check_edgar_rounds', summary)) as EdgarCheck,
	etfHoldings: async (doc: string) => JSON.parse(await call<string>('check_etf_holdings', doc)) as EtfCheck,
	checkHeader: async (header: string) =>
		JSON.parse(await call<string>('check_header', header)) as { hash: string; parent: string },
	brierTerm: (pPpm: number, hit: boolean, k: number) => call<string>('brier_term', pPpm, hit, k),
	trackExample: async (doc: string) => JSON.parse(await call<string>('check_track_example', doc)) as TrackExampleCheck,
	/** NL4: one record served by the node (`GET /v1/track/{source}/{metric}`), checked as served (no commitment pinned). */
	trackRecord: async (body: string, source: string, metric: string) =>
		JSON.parse(await call<string>('check_track_record', body, source, metric, 0n, '')) as TrackView,
	/** C20: the example asked with the commitments this browser saw last (`seen`: {source: {commitment, as_of}}). */
	trackExampleSince: async (doc: string, seen: string) =>
		JSON.parse(await call<string>('check_track_example_since', doc, seen)) as TrackExampleCheck,
	/** C20/C23: rerun the example's logs with the served registry (checked to the commitment's registry root first);
	 * `known` is the commitment this browser saw last, or ''. */
	trackExampleLog: async (log: string, known: string, registry: string) =>
		JSON.parse(await call<string>('check_track_example_log', log, known, registry)) as TrackLogCheck,
	/** C61: the recorded level-3 rehearsal, checked against its pinned chain answers. */
	signedRehearsal: async (doc: string) => JSON.parse(await call<string>('check_signed_rehearsal', doc)) as SignedCheck,
	/** C61: the same record with a forged value (hashes recomputed, signature kept). */
	signedForgedValue: (doc: string) => call<string>('signed_forged_value', doc),
	/** C61: the book, fact and calldata to read at the reader's finalized block. */
	signedLiveCalls: async (body: string) =>
		JSON.parse(await call<string>('signed_live_calls', body)) as { to: string; settleable: string; factView: string; chainId: number },
	/** C61: the body against the reader's own RPC reads, at the reader's clock. */
	signedLive: async (body: string, reads: string, now: number) =>
		JSON.parse(await call<string>('check_signed_live', body, reads, BigInt(now))) as SignedCheck,
	/** NS11: the reads of the live markets (verify/src/markets_view.rs). */
	marketsCalls: async (config: string) =>
		JSON.parse(await call<string>('markets_calls', config)) as { chainId: number; calls: { key: string; to: string; data: string }[] },
	/** NS11: the live markets decoded and checked from the reader's own RPC reads, at the reader's clock. */
	marketsLive: async (config: string, reads: string, now: number) =>
		JSON.parse(await call<string>('markets_live', config, reads, BigInt(now))) as MarketsLive,
	/** ADR-0044: the Ethereum window checked against its pins (verify/src/eth_chain.rs). */
	ethChainStats: async (doc: string) => JSON.parse(await call<string>('check_eth_chain_stats', doc)) as EthChainCheck,
	/** ADR-0044: one block's full input rerun here on the canonical engine. */
	ethChainRerun: async (wasm: Uint8Array, input: Uint8Array, doc: string) =>
		JSON.parse(await call<string>('rerun_eth_chain_block', wasm, input, doc)) as EthChainRerun
};

/** Sums of the Ethereum window's checked rows (verify/src/eth_chain.rs): wei and gas as decimal
 *  strings (they pass 2^53), fullness in ppm. */
export type EthChainSums = {
	blocks: number;
	first_time: number;
	last_time: number;
	txs: number;
	failed_txs: number;
	contract_creations: number;
	gas_used: string;
	fullness_ppm: number;
	mean_base_fee_wei: string;
	min_base_fee_wei: string;
	max_base_fee_wei: string;
	burned_wei: string;
	priority_fees_wei: string;
	blob_txs: number;
	blob_gas_used: string;
	blob_fee_wei: string;
	value_wei: string;
};
/** One checked `eth-chain-stats` row, as the verifier returns it. */
export type EthChainBlock = {
	number: number;
	hash: string;
	time: number;
	interval_secs: number;
	txs: number;
	failed_txs: number;
	contract_creations: number;
	gas_used: number;
	gas_limit: number;
	fullness_ppm: number;
	base_fee_wei: string;
	burned_wei: string;
	priority_fees_wei: string;
	blob_txs: number;
	blob_gas_used: number;
	blob_base_fee_wei: string;
	blob_fee_wei: string;
	value_wei: string;
};
/** The Ethereum window checked against its pins (`check_eth_chain_stats`): every figure the page
 *  draws is in here, computed from the checked leaves. */
export type EthChainCheck = {
	ok: boolean;
	commitment: string;
	commitment_ok: boolean;
	recipe_id: string;
	from: number;
	to: number;
	tip: string;
	rerun_number: number;
	window: EthChainSums;
	days: (EthChainSums & { date: string; from: number; to: number; ok: boolean })[];
	/** Bars: consecutive runs of `group_blocks` checked rows. */
	group_blocks: number;
	groups: (EthChainSums & { from: number; to: number })[];
	blocks: EthChainBlock[];
	problems: string[];
};
/** One block's full input rerun here (`rerun_eth_chain_block`). */
export type EthChainRerun = {
	ok: boolean;
	outcome: string;
	fuel: string;
	recipe_id: string;
	number: number;
	input_bytes: number;
	txs_in_input: number;
	root: string;
	row: { number: number; txs: number; gas_used: number; burned_wei: string; priority_fees_wei: string; blob_txs: number } | null;
	problems: string[];
};

/** One live market as the verifier read it (verify/src/markets_view.rs); amounts are decimal strings. */
export type LiveMarket = {
	id: number;
	ok: boolean;
	problems: string[];
	state?: 'open' | 'resolved' | 'void';
	trading?: boolean;
	tradingClose?: number;
	observableAt?: number;
	resolveBy?: number;
	buckets?: { low: string | null; high: string | null; pool: string; chancePpm: string; bettors: number }[];
	total?: string;
	bettors?: number;
	value?: string | null;
	level?: number | null;
	acceptedAt?: number | null;
	winner?: number | null;
};
/** One test insurance product as the verifier read it (NL18); amounts are decimal strings. */
export type LiveProduct = {
	id: number;
	ok: boolean;
	problems: string[];
	state?: 'open' | 'paid' | 'expired' | 'void';
	buying?: boolean;
	cmp?: 'atleast' | 'atmost';
	threshold?: string;
	payout?: string;
	premium?: string;
	sold?: number;
	maxPolicies?: number;
	buyBy?: number;
	observableAt?: number;
	resolveBy?: number;
	value?: string | null;
	level?: number | null;
};
export type LiveInsurance = { ok: boolean; problems: string[]; count?: number; pool?: string; reserved?: string; free?: string; products: LiveProduct[]; markets?: LiveMarket[] };
export type MarketsLive = { ok: boolean; problems: string[]; count?: number; markets: LiveMarket[]; insurance?: LiveInsurance | null };
