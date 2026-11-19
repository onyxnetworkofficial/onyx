// Plain-English names for bulk dataset ids (the All datasets page, ROADMAP S22). An id is
// "<asset>.<metric words>", like "btc.sth-net-position". This file gives the asset's name, a title
// built from the metric's words ("Short-term holder net position") and, for standard terms only,
// a one-line meaning. An abbreviation without a settled public meaning stays as written (upper
// case): a guessed meaning would be worse than none. Search uses the same tables to turn a word a
// reader types ("bitcoin", "holders", "destroyed") into the id words that mean it.

type Asset = { name: string; kind?: 'stablecoin' | 'gold' | 'meme' | 'wrapped' };

/** The assets of the bulk datasets, by id. */
export const ASSETS: Record<string, Asset> = {
	'1inch': { name: '1inch' },
	aave: { name: 'Aave' },
	ada: { name: 'Cardano' },
	ape: { name: 'ApeCoin' },
	axs: { name: 'Axie Infinity' },
	bat: { name: 'Basic Attention Token' },
	bch: { name: 'Bitcoin Cash' },
	bnb: { name: 'BNB' },
	bonk: { name: 'Bonk', kind: 'meme' },
	btc: { name: 'Bitcoin' },
	'btt-trx': { name: 'BitTorrent on Tron' },
	chz: { name: 'Chiliz' },
	comp: { name: 'Compound' },
	crv: { name: 'Curve' },
	dai: { name: 'Dai', kind: 'stablecoin' },
	doge: { name: 'Dogecoin', kind: 'meme' },
	ens: { name: 'Ethereum Name Service' },
	eth: { name: 'Ethereum' },
	fet: { name: 'Fetch.ai' },
	gala: { name: 'Gala' },
	grt: { name: 'The Graph' },
	hype: { name: 'Hyperliquid' },
	imx: { name: 'Immutable' },
	'jst-trx': { name: 'JUST on Tron' },
	jto: { name: 'Jito' },
	jup: { name: 'Jupiter' },
	ldo: { name: 'Lido DAO' },
	leo: { name: 'LEO' },
	link: { name: 'Chainlink' },
	lrc: { name: 'Loopring' },
	ltc: { name: 'Litecoin' },
	mana: { name: 'Decentraland' },
	mkr: { name: 'Maker' },
	mnt: { name: 'Mantle' },
	ondo: { name: 'Ondo' },
	paxg: { name: 'PAX Gold', kind: 'gold' },
	pengu: { name: 'Pudgy Penguins', kind: 'meme' },
	pepe: { name: 'Pepe', kind: 'meme' },
	pump: { name: 'Pump.fun' },
	pyth: { name: 'Pyth Network' },
	pyusd: { name: 'PayPal USD', kind: 'stablecoin' },
	ray: { name: 'Raydium' },
	render: { name: 'Render' },
	sand: { name: 'The Sandbox' },
	shib: { name: 'Shiba Inu', kind: 'meme' },
	snx: { name: 'Synthetix' },
	sol: { name: 'Solana' },
	'sun-trx': { name: 'SUN on Tron' },
	trump: { name: 'Official Trump', kind: 'meme' },
	trx: { name: 'Tron' },
	'tusd-trx': { name: 'TrueUSD on Tron', kind: 'stablecoin' },
	uni: { name: 'Uniswap' },
	usd1: { name: 'USD1', kind: 'stablecoin' },
	usdc: { name: 'USD Coin', kind: 'stablecoin' },
	'usdc-sol': { name: 'USD Coin on Solana', kind: 'stablecoin' },
	'usdc-trx': { name: 'USD Coin on Tron', kind: 'stablecoin' },
	'usdd-trx': { name: 'USDD on Tron', kind: 'stablecoin' },
	usde: { name: 'Ethena USDe', kind: 'stablecoin' },
	usdg: { name: 'Global Dollar', kind: 'stablecoin' },
	usds: { name: 'USDS', kind: 'stablecoin' },
	usdt: { name: 'Tether', kind: 'stablecoin' },
	'usdt-sol': { name: 'Tether on Solana', kind: 'stablecoin' },
	'usdt-trx': { name: 'Tether on Tron', kind: 'stablecoin' },
	usdy: { name: 'Ondo US Dollar Yield', kind: 'stablecoin' },
	usyc: { name: 'USYC', kind: 'stablecoin' },
	wbtc: { name: 'Wrapped Bitcoin', kind: 'wrapped' },
	weth: { name: 'Wrapped Ether', kind: 'wrapped' },
	wif: { name: 'dogwifhat', kind: 'meme' },
	'win-trx': { name: 'WINkLink on Tron' },
	wld: { name: 'Worldcoin' },
	wlfi: { name: 'World Liberty Financial' },
	xaut: { name: 'Tether Gold', kind: 'gold' },
	xrp: { name: 'XRP' }
};

/** Kinds a reader may search for ("stablecoin", "gold", "meme", "wrapped"). */
const KINDS: Record<string, string[]> = { stablecoin: ['stablecoins', 'stable'], gold: [], meme: ['memecoin', 'memecoins'], wrapped: [] };

// Durations in ids (7d, 1w, 2y, 1yr, 200w).
const DURATION = /^(\d+)(d|w|m|y|yr)$/;
const UNIT: Record<string, [string, string]> = { d: ['day', 'days'], w: ['week', 'weeks'], m: ['month', 'months'], y: ['year', 'years'], yr: ['year', 'years'] };

/** How a word of an id reads in a title; a word not here reads as itself. */
export const WORDS: Record<string, string> = {
	lth: 'long-term holder',
	sth: 'short-term holder',
	pl: 'profit/loss',
	pnl: 'profit/loss',
	rpl: 'realized profit/loss',
	plr: 'profit/loss ratio',
	cb: 'cost basis',
	mom: 'momentum',
	comp: 'composite',
	cum: 'cumulative',
	pct: 'percent',
	addr: 'address',
	tx: 'transaction',
	txs: 'transactions',
	utxo: 'UTXO',
	utxos: 'UTXOs',
	evm: 'EVM',
	noncirc: 'non-circulating',
	val: 'value',
	norm: 'normalized',
	ext: 'extended',
	alt: 'alternative',
	sd: 'standard deviation',
	ma: 'moving average',
	sma: 'moving average',
	sma30: '30-day average',
	sma90: '90-day average',
	sma365: '365-day average',
	vol30: '30-day volatility',
	sharpe30: '30-day Sharpe',
	sharpe90: '90-day Sharpe',
	z: 'Z-score',
	zscore: 'Z-score',
	rcap: 'realized cap',
	cdd: 'coin days destroyed',
	vdd: 'value days destroyed',
	cyd: 'coin years destroyed',
	hodl: 'HODL',
	hodler: 'HODLer',
	hodled: 'HODLed',
	sopr: 'SOPR',
	asopr: 'adjusted SOPR',
	mvrv: 'MVRV',
	mvrvz: 'MVRV Z-score',
	nupl: 'NUPL',
	nvt: 'NVT',
	rvt: 'RVT',
	rhodl: 'RHODL',
	cvdd: 'CVDD',
	slrv: 'SLRV',
	sopd: 'SOPD',
	urpd: 'URPD',
	asol: 'ASOL',
	msol: 'MSOL',
	etf: 'ETF',
	mweb: 'MWEB',
	btcfi: 'BTCfi',
	gini: 'Gini',
	onchain: 'on-chain',
	op: 'OP',
	ohlc: 'OHLC'
};

/** Abbreviations kept as written, in capitals (no settled public meaning to spell out). */
const ABBREVIATIONS = new Set(['a2vr', 'acdd', 'mctc', 'pvr', 'rla', 'rpa', 'rps', 'rup', 'rul', 'se', 'sec', 'sh', 'cbd', 'spl', 'nvt', 'rvt']);

/** One-line meanings of standard terms, by their id words (longest first when matched). */
export const MEANINGS: Record<string, string> = {
	lth: 'Long-term holders: coins that have not moved for a long time (the usual cut is about 155 days).',
	sth: 'Short-term holders: coins that moved recently (the usual cut is about 155 days).',
	sopr: 'SOPR (spent output profit ratio): the price coins moved at divided by the price they last moved at before; above 1 means they moved at a profit on average.',
	mvrv: 'MVRV: market value divided by realized value; above 1 means holders are in profit on average.',
	nupl: 'NUPL (net unrealized profit/loss): paper profit minus paper loss, as a share of market value.',
	'realized-cap': 'Realized cap: every coin valued at the price it last moved, not at today’s price.',
	'realized-price': 'Realized price: realized cap divided by supply, the average price coins last moved at.',
	'cost-basis': 'Cost basis: the price holders paid for their coins.',
	cdd: 'Coin days destroyed: coins moved times the days they had been still; it jumps when old coins move.',
	vdd: 'Value days destroyed: coin days destroyed weighted by price.',
	cvdd: 'CVDD (cumulative value-days destroyed): a long-run price-floor model built from value days destroyed.',
	thermocap: 'Thermocap: the total value of all coins ever issued, at the price on the day they were issued.',
	'investor-cap': 'Investor cap: realized cap minus thermocap, the part paid by buyers rather than by issuance.',
	'hodl-waves': 'HODL waves: supply split into age bands (how long coins have been held).',
	'reserve-risk': 'Reserve risk: price measured against how firmly long-term holders hold; low values mean firm holders and a low price.',
	nvt: 'NVT: market value divided by the value moved on chain, like a price-to-earnings ratio.',
	liveliness: 'Liveliness: coin days destroyed divided by coin days created; it rises when old coins move.',
	vaultedness: 'Vaultedness: one minus liveliness, how much of the supply stays still.',
	coinblocks: 'Coin blocks: coins times blocks held; created while coins sit still, destroyed when they move.',
	dormancy: 'Dormancy: the average number of days coins had been still, per coin moved.',
	gini: 'Gini coefficient: how unequal holdings are (0: all equal; 1: one holder has everything).',
	'stock-to-flow': 'Stock to flow: the supply that exists divided by the new supply each year.',
	'z-score': 'Z-score: how far the value is from its average, in standard deviations.',
	zscore: 'Z-score: how far the value is from its average, in standard deviations.',
	momentum: 'Momentum: how fast the value is changing.',
	heatmap: 'Heatmap: values over time made to be drawn as colours, often split into bands (by price or by age).',
	'by-age': 'By age: split by how long coins have been held.',
	'age-bands': 'Age bands: supply split by how long coins have been held.',
	'entity-adjusted': 'Entity-adjusted: addresses are grouped into owners first, so moves between one owner’s own addresses are left out.',
	exchange: 'Exchange: measured on addresses the publisher labels as exchanges.',
	'balance-distribution': 'Balance distribution: addresses grouped by how much they hold.',
	'in-profit': 'In profit: coins worth more today than the price they last moved at.',
	'in-loss': 'In loss: coins worth less today than the price they last moved at.',
	'realized-profit-loss': 'Realized profit/loss: gains or losses locked in when coins move, against the price they last moved at.',
	'realized-pl': 'Realized profit/loss: gains or losses locked in when coins move, against the price they last moved at.',
	unrealized: 'Unrealized profit/loss: paper gains or losses on coins that have not moved.',
	'active-addresses': 'Active addresses: addresses that sent or received coins in a day.',
	'hash-rate': 'Hash rate: the computing power that secures the chain.',
	'mining-difficulty': 'Difficulty: how hard it is to find a block.',
	'market-cap': 'Market cap: price times circulating supply.',
	'circulating-supply': 'Circulating supply: the coins available on the market.',
	'transfer-volume': 'Transfer volume: the value moved on chain.',
	revived: 'Revived supply: old coins that moved again after a long time still.',
	sopd: 'SOPD (spent output profit distribution): the profit or loss of moved coins, by price.',
	urpd: 'URPD (UTXO realized price distribution): how much supply last moved at each price.',
	slrv: 'SLRV: realized value of coins moved within a day, divided by that of coins held 6 to 12 months.',
	rhodl: 'RHODL ratio: realized value of coins held 1 week against those held 1 to 2 years; high values mark heated markets.',
	asol: 'ASOL: the average lifespan of the coins moved each day.',
	msol: 'MSOL: the median lifespan of the coins moved each day.',
	'accumulation-trend-score': 'Accumulation trend score: whether large holders are adding (near 1) or selling (near 0).',
	staking: 'Staking flows: coins moving into and out of staking.',
	gas: 'Gas: what transactions pay for computation on the chain.',
	'op-return': 'OP_RETURN: small pieces of data written into transactions.',
	taproot: 'Taproot: a Bitcoin upgrade for scripts; how much it is used.',
	mweb: 'MWEB: Litecoin’s private extension block (MimbleWimble).',
	etf: 'ETF: coins held by exchange-traded funds.',
	'never-moved': 'Never moved: coins that have not moved since they were created.',
	'free-float': 'Free float: the supply that can trade.',
	issuance: 'Issuance: new coins created.',
	'block-height': 'Block height: the number of blocks on the chain.'
};

/** Meanings of whole metrics, where one word alone would mislead ("price" in "price-floor-levels"). */
const WHOLE: Record<string, string> = {
	price: 'The daily price in US dollars.',
	'spot-price': 'The daily price in US dollars.',
	// Newcomer test 3 (2026-11-18), checked against the live columns of every chain's copy: active
	// addresses, transaction count, average transaction value, and on some chains transfer volume
	// or coins burned. None has a gas column, so the word "gas" in the id is not explained here.
	'gas-analytics': 'Network activity by day: active addresses, transactions and the value they moved; on some chains also the coins burned. Despite its id, it has no gas column.',
	// Its columns are percentiles (p25 to p95), not one coefficient.
	gini: 'Named after the Gini coefficient (how unequal holdings are), but its columns are percentiles (p25 to p95), not one coefficient.'
};

/** Titles of whole metrics whose id words would mislead ("gas-analytics" holds no gas figure). The
 * id itself never changes: the card prints it under the title. */
const WHOLE_TITLE: Record<string, string> = {
	'gas-analytics': 'Network activity'
};

/** What the unit hint at the end of a column name means, in the order shown. */
const UNIT_HINTS: [RegExp, string, string][] = [
	[/_usd$/, '_usd', 'in US dollars'],
	[/_native$/, '_native', 'in the asset’s own coin or token, not in dollars'],
	[/_count$/, '_count', 'a count (a whole number)'],
	[/_(pct|percent)$/, '_pct', 'a percentage (50 means 50%)']
];

/** The legend lines for the unit hints the columns end in ("_usd: in US dollars"); empty when no
 * column carries one. */
export function unitLegend(columns: string[]): string[] {
	return UNIT_HINTS.filter(([re]) => columns.some((c) => re.test(c))).map(([, hint, means]) => `${hint}: ${means}`);
}

/** Words a reader may type for a concept, by the id words that carry it. */
const SEARCH: Record<string, string[]> = {
	lth: ['holder', 'holders', 'long', 'long-term', 'hodler'],
	sth: ['holder', 'holders', 'short', 'short-term'],
	hodler: ['holder', 'holders'],
	holder: ['holders'],
	profit: ['gain', 'gains'],
	pl: ['profit', 'loss', 'gain', 'gains', 'pnl'],
	pnl: ['profit', 'loss'],
	rpl: ['profit', 'loss', 'realized'],
	sopr: ['profit', 'spent'],
	nupl: ['profit', 'loss', 'unrealized'],
	sopd: ['profit', 'loss', 'spent'],
	mvrv: ['valuation', 'value'],
	nvt: ['valuation', 'value'],
	'realized-price': ['valuation'],
	thermocap: ['valuation', 'miner', 'issuance'],
	'balance-distribution': ['whale', 'whales', 'wealth', 'rich'],
	'wealth-concentration': ['whale', 'whales', 'rich'],
	whale: ['whales'],
	gini: ['whale', 'whales', 'inequality', 'concentration'],
	exchange: ['exchanges', 'cex'],
	miner: ['miners', 'mining'],
	mining: ['miner', 'miners'],
	'hash-rate': ['miner', 'miners', 'mining', 'hashrate'],
	'block-rewards': ['miner', 'miners', 'reward', 'rewards'],
	fees: ['fee', 'cost'],
	fee: ['fees'],
	gas: ['fee', 'fees'],
	cdd: ['age', 'old', 'destroyed', 'dormant'],
	revived: ['old', 'dormant', 'awakening'],
	dormancy: ['dormant', 'age', 'old'],
	'hodl-waves': ['age', 'old'],
	'age-bands': ['age'],
	'by-age': ['age'],
	supply: ['coins', 'issuance'],
	issuance: ['inflation', 'new', 'minted'],
	addresses: ['wallet', 'wallets', 'users'],
	addr: ['address', 'addresses', 'wallet', 'wallets'],
	price: ['usd', 'dollar'],
	'cost-basis': ['cost', 'paid', 'entry'],
	staking: ['stake', 'staked'],
	etf: ['fund', 'funds']
};

const cap = (s: string) => (s ? s[0].toUpperCase() + s.slice(1) : s);

/** The asset id and metric words of a dataset id ("btt-trx.price" -> "btt-trx", ["price"]). */
export function split(dataset: string): { asset: string; words: string[] } {
	const i = dataset.indexOf('.');
	const asset = i < 0 ? dataset : dataset.slice(0, i);
	const words = (i < 0 ? '' : dataset.slice(i + 1)).split(/[^a-z0-9]+/).filter(Boolean);
	return { asset, words };
}

/** The asset's name, or its id in capitals. */
export const assetName = (asset: string): string => ASSETS[asset]?.name ?? asset.toUpperCase();

function duration(w: string): string | null {
	const m = DURATION.exec(w);
	if (!m) return null;
	const [one, many] = UNIT[m[2]];
	return `${m[1]} ${m[1] === '1' ? one : many}`;
}

/** A metric's words as a title ("sth-net-position" -> "Short-term holder net position"). */
export function metricTitle(words: string[]): string {
	const whole = WHOLE_TITLE[words.join('-')];
	if (whole) return whole;
	const out: string[] = [];
	for (let i = 0; i < words.length; i++) {
		const w = words[i];
		const next = words[i + 1];
		// "sth lth" and "lth sth": both holder groups.
		if ((w === 'sth' && next === 'lth') || (w === 'lth' && next === 'sth')) {
			out.push('short- and long-term holder');
			i++;
			continue;
		}
		const d = duration(w);
		const dn = next ? duration(next) : null;
		if (d && dn) {
			out.push(`${d} to ${dn}`);
			i++;
			continue;
		}
		if (w === 'entity' && next === 'adjusted') {
			out.push('entity-adjusted');
			i++;
			continue;
		}
		// "z-score" is one term ("mvrv-z-score": MVRV Z-score, not "Z-score score").
		if (w === 'z' && next === 'score') {
			out.push('Z-score');
			i++;
			continue;
		}
		if (w === 'op' && next === 'return') {
			out.push('OP_RETURN');
			i++;
			continue;
		}
		if (d) out.push(d);
		else if (WORDS[w]) out.push(WORDS[w]);
		// An abbreviation with no settled public meaning stays as written.
		else if (ABBREVIATIONS.has(w) || /^q\d{1,2}$/.test(w)) out.push(w.toUpperCase());
		else out.push(w);
	}
	return cap(out.join(' '));
}

/** "Bitcoin · Short-term holder net position". */
export function datasetTitle(dataset: string): string {
	const { asset, words } = split(dataset);
	return words.length ? `${assetName(asset)} · ${metricTitle(words)}` : assetName(asset);
}

const MEANING_KEYS = Object.keys(MEANINGS).sort((a, b) => b.split('-').length - a.split('-').length || b.length - a.length);

/** The meanings of the standard terms in a metric, at most `max`, longest terms first. */
export function meanings(words: string[], max = 2): string[] {
	const whole = WHOLE[words.join('-')];
	if (whole) return [whole];
	const used = new Array(words.length).fill(false);
	const found: { at: number; text: string }[] = [];
	for (const k of MEANING_KEYS) {
		const kw = k.split('-');
		for (let i = 0; i + kw.length <= words.length; i++) {
			if (kw.every((w, j) => words[i + j] === w) && !used.slice(i, i + kw.length).some(Boolean)) {
				for (let j = 0; j < kw.length; j++) used[i + j] = true;
				if (!found.some((f) => f.text === MEANINGS[k])) found.push({ at: i, text: MEANINGS[k] });
				break;
			}
		}
	}
	return found
		.sort((a, b) => a.at - b.at)
		.slice(0, max)
		.map((f) => f.text);
}

/** At most this many id terms for one typed word (the node takes 200 in all). */
const PER_WORD = 24;

/** The `any` groups for what a reader typed: one group per word (at most 8), each the word as a
 * word start plus the asset ids, kinds and id words that mean it. Empty when nothing was typed. */
export function anyGroups(typed: string): string[][] {
	const words = typed.toLowerCase().split(/[^a-z0-9]+/).filter(Boolean).slice(0, 8);
	return words.map((w) => {
		const terms = new Set<string>([`${w}*`]);
		const starts = (s: string) => s.toLowerCase().split(/[^a-z0-9]+/).some((x) => x.startsWith(w));
		if (w.length >= 3) {
			// Asset names from 4 letters ("eth" alone would also pick Ethena and ENS). A word that is
			// exactly an asset's name means that asset only ("bitcoin": not Bitcoin Cash).
			const exact = Object.entries(ASSETS).filter(([, a]) => a.name.toLowerCase() === w);
			for (const [id, a] of exact.length ? exact : Object.entries(ASSETS)) {
				if ((w.length >= 4 && starts(a.name)) || (a.kind && (a.kind.startsWith(w) || KINDS[a.kind].some((k) => k.startsWith(w))))) terms.add(id);
			}
			for (const [id, shown] of Object.entries(WORDS)) if (starts(shown)) terms.add(id);
			for (const [id, said] of Object.entries(SEARCH)) if (said.some((s) => s.startsWith(w))) terms.add(id);
		}
		return [...terms].slice(0, PER_WORD);
	});
}

/** The `any` query value for the groups ("btc*,btc;sth*,sth,lth"). */
export const anyValue = (groups: string[][]): string => groups.map((g) => g.join(',')).join(';');

// ---- "Did you mean" (newcomer test 3, 2026-11-18: "bitcon" and "etherium gas price" found nothing). ----

/** Plain id words that are in many dataset ids and in none of the tables above. */
const COMMON = ['active', 'addresses', 'analytics', 'balance', 'bands', 'basis', 'blocks', 'burn', 'cap', 'change', 'circulating', 'concentration', 'cost', 'count', 'daily', 'difficulty', 'distribution', 'fees', 'flow', 'flows', 'holder', 'holders', 'loss', 'market', 'median', 'net', 'network', 'position', 'price', 'profit', 'ratio', 'realized', 'reserves', 'revenue', 'rewards', 'risk', 'supply', 'transaction', 'transfer', 'unrealized', 'velocity', 'volatility', 'volume', 'wealth'];

/** Every word the search knows, each once: asset ids and the words of asset names first (a typo
 * is most often an asset), then metric words. Lower case, letters and digits only. */
export const KNOWN_WORDS: string[] = (() => {
	const parts = (x: string) => x.toLowerCase().split(/[^a-z0-9]+/).filter((w) => w.length >= 3);
	const out = new Set<string>();
	for (const [id, a] of Object.entries(ASSETS)) for (const w of [...parts(id), ...parts(a.name)]) out.add(w);
	for (const k of Object.keys(KINDS)) out.add(k);
	for (const [k, v] of Object.entries(SEARCH)) for (const w of [...parts(k), ...v.flatMap(parts)]) out.add(w);
	for (const k of [...Object.keys(WORDS), ...Object.keys(MEANINGS)]) for (const w of parts(k)) out.add(w);
	for (const w of COMMON) out.add(w);
	return [...out];
})();

/** Edits between two words: a letter added, dropped or changed, or two neighbours swapped
 * (optimal string alignment). Stops counting above `max` (returns max + 1). */
export function editDistance(a: string, b: string, max = 2): number {
	if (Math.abs(a.length - b.length) > max) return max + 1;
	let prev2: number[] = [];
	let prev = Array.from({ length: b.length + 1 }, (_, j) => j);
	for (let i = 1; i <= a.length; i++) {
		const cur = [i];
		for (let j = 1; j <= b.length; j++) {
			const cost = a[i - 1] === b[j - 1] ? 0 : 1;
			let v = Math.min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + cost);
			if (i > 1 && j > 1 && a[i - 1] === b[j - 2] && a[i - 2] === b[j - 1]) v = Math.min(v, prev2[j - 2] + 1);
			cur.push(v);
		}
		prev2 = prev;
		prev = cur;
	}
	return Math.min(prev[b.length], max + 1);
}

/** The known word nearest to a typed one: the word itself when it is known (or starts a known
 * word, as the node's search matches word starts), else the closest within 1 edit (words of up
 * to 5 letters) or 2 (longer), the first in KNOWN_WORDS on a tie; null when nothing is close.
 * Words under 4 letters and durations ("30d") are never corrected. */
export function nearestWord(w: string): string | null {
	if (w.length < 4 || DURATION.test(w) || /^[0-9]+$/.test(w)) return w;
	if (KNOWN_WORDS.some((k) => k.startsWith(w))) return w;
	const max = w.length <= 5 ? 1 : 2;
	let best: string | null = null;
	let bestD = max + 1;
	for (const k of KNOWN_WORDS) {
		if (k.length < 4) continue;
		const d = editDistance(w, k, max);
		if (d < bestD) [best, bestD] = [k, d];
	}
	return best;
}

/** Searches to offer when one found nothing, best first, at most 4: the search with each
 * misspelt word replaced by the nearest known one ("bitcon" -> "bitcoin"), then that search with
 * words dropped from the end ("ethereum gas price" -> "ethereum gas" -> "ethereum"). A word with
 * no near known word is dropped. Never the typed search itself; empty when nothing is left. */
export function suggestSearches(typed: string): string[] {
	const words = typed.toLowerCase().split(/[^a-z0-9]+/).filter(Boolean).slice(0, 8);
	const typedText = words.join(' ');
	const fixed = words.map(nearestWord).filter((w): w is string => w !== null);
	const out: string[] = [];
	for (let n = fixed.length; n >= 1 && out.length < 4; n--) {
		const text = fixed.slice(0, n).join(' ');
		if (text !== typedText && !out.includes(text)) out.push(text);
	}
	return out;
}
