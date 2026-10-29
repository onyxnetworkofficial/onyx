/** The datasets this site shows, dataset first (PRODUCT-PLAN.md U5, pre-public loop PP4). Each
 * entry names what the page section below it proves; nothing here is a number, so nothing here
 * can disagree with a checked value. `section` is that section's data-testid. */
export type Dataset = {
	id: string;
	title: string;
	what: string;
	level: 1 | 2 | 3 | 4 | 5;
	source: string;
	unit: string;
	rights: string;
	/** The query API route that served it (listed on the API page), and where it was served. */
	api?: string;
	apiWhere?: string;
	/** Derived from the company label sets: held back until their rights are cleared. */
	labelDerived?: boolean;
	section?: string;
	page?: string;
	/** No date on its card: the data file holds block heights (its page shows them). */
	noDate?: boolean;
};

/** One card's "Data as of" (PP11): scripts/gen-freshness.py reads it from the shipped data files. */
export type Freshness = Record<string, { asOf: string; what: string }>;

/** `2026-10-08` as `8 Oct 2026`, from the text alone (no time zone can move the day). */
export function dayText(iso: string): string {
	const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(iso);
	const months = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
	if (!m || +m[2] < 1 || +m[2] > 12) return iso;
	return `${+m[3]} ${months[+m[2] - 1]} ${m[1]}`;
}

/** The freshness file's entries in their exact shape; anything else is dropped. */
export function parseFreshness(v: unknown): Freshness {
	const out: Freshness = {};
	const ds = v && typeof v === 'object' && (v as { version?: unknown }).version === 1 ? (v as { datasets?: unknown }).datasets : null;
	if (!ds || typeof ds !== 'object') return out;
	for (const [id, e] of Object.entries(ds as Record<string, unknown>)) {
		const r = e as { asOf?: unknown; what?: unknown };
		if (typeof r?.asOf === 'string' && /^\d{4}-\d{2}-\d{2}$/.test(r.asOf) && typeof r.what === 'string' && r.what.length <= 40) out[id] = { asOf: r.asOf, what: r.what };
	}
	return out;
}

export const LEVEL_NAME: Record<number, string> = { 1: 'Recomputed', 2: 'Re-observed', 3: 'Source-proven', 4: 'Claim', 5: 'Track record' };

export const DATASETS: Dataset[] = [
	{ id: 'btc.blocks', title: 'Bitcoin blocks', what: 'Transactions and outputs of every block in the window, with each block header checked.', level: 1, source: 'Bitcoin mainnet blocks', unit: 'transactions, BTC', rights: 'Public chain data', section: 'blocks' },
	{ id: 'btc.chain-stats', title: 'Bitcoin chain statistics', what: 'Fees, fee rates, size, supply and script use per block, from the blocks alone.', level: 1, source: 'Bitcoin mainnet blocks', unit: 'satoshis, sat/vB, counts', rights: 'Public chain data', section: 'onchain' },
	{ id: 'btc.entity-flow', title: 'Company flows', what: 'What each exchange or fund sent and received per block.', level: 1, source: 'Bitcoin blocks and the company label set', unit: 'BTC per block', rights: 'Label set rights under review', api: '/v1/metrics/btc.entity-flow', apiWhere: 'local and Base Sepolia rehearsals', labelDerived: true, section: 'flows' },
	{ id: 'btc.pools', title: 'Who mined the blocks', what: 'Blocks per mining pool, from coinbase payouts and tags.', level: 4, source: 'Bitcoin blocks and the bitcoin-data/mining-pools list', unit: 'blocks per pool', rights: 'Pool list under the MIT licence', section: 'pools' },
	{ id: 'eth.chain-stats', title: 'Ethereum chain statistics', what: 'Gas, base fee and transactions per block, each block checked against its hash.', level: 1, source: 'Ethereum mainnet blocks', unit: 'gas, gwei, counts', rights: 'Public chain data', section: 'eth-onchain' },
	{ id: 'us.treasury.total_public_debt_outstanding', title: 'U.S. total public debt', what: 'Treasury "Debt to the Penny", read by observers drawn at random who seal their answers.', level: 2, source: 'U.S. Treasury Fiscal Data', unit: 'USD', rights: 'U.S. government open data', api: '/v1/observations/us.treasury.total_public_debt_outstanding', apiWhere: 'local rehearsal', section: 'level2' },
	{ id: 'usgs.earthquakes.m5_count', title: 'Earthquakes per day', what: 'How many earthquakes of magnitude 5.0 or more the USGS catalog lists worldwide on a day, read 100 days later.', level: 2, source: 'USGS earthquake catalog', unit: 'earthquakes', rights: 'Facts from a U.S. public-domain catalog', section: 'daily' },
	{ id: 'noaa.ghcnd.usw00003017.tmax', title: 'Daily high temperature, Denver', what: 'The highest temperature NOAA recorded at Denver International Airport on a day, read 30 days later.', level: 2, source: 'NOAA NCEI GHCN-Daily', unit: '°C', rights: 'U.S. government work', section: 'daily' },
	{ id: 'sec.crypto-units', title: 'Crypto-asset units in SEC filings', what: 'The crypto-asset units a company tagged in its SEC filing for a period.', level: 2, source: 'SEC EDGAR (data.sec.gov)', unit: 'units', rights: 'Public U.S. filings', section: 'sec' },
	{ id: 'sec.etf-btc', title: 'Spot bitcoin ETF holdings', what: 'The bitcoin each U.S. spot fund tagged in a 10-Q or 10-K.', level: 2, source: 'SEC EDGAR (data.sec.gov)', unit: 'BTC', rights: 'Public U.S. filings', section: 'etf' },
	{ id: 'btc.entity-reserve', title: 'Company reserves', what: 'Each company\'s bitcoin at its own published addresses, cross-checked block by block.', level: 4, source: 'Issuer address lists and Bitcoin blocks', unit: 'BTC', rights: 'Label set rights under review', api: '/v1/metrics/btc.entity-reserve', apiWhere: 'local and Base Sepolia rehearsals', labelDerived: true, page: '#/reserves', noDate: true }
];
