// The Datasets page (ROADMAP S22): the node's bulk datasets, searched and paged by the node
// (`GET /v1/bulk?q=&limit=&after=`). Every value shown comes from the answer through the checks
// below: an item that does not match the shape is dropped, never printed.
import { group } from './format.ts';
import { ASSETS, anyGroups, anyValue } from './glossary.ts';

export type Dataset = {
	publisher: string;
	dataset: string;
	version: number;
	rows: number;
	bytes: number;
	shards: number;
	stored: number;
	keyMin: string;
	keyMax: string;
	license: string;
	/** When the publisher made this version (unix seconds); null when not given. */
	created: number | null;
	/** The UTC day of the newest key ("2026-11-13"), as the node reads it; null when the key is
	 * not a time (or the node does not say): such a dataset is never called stale. */
	newest: string | null;
	/** Whole days from the newest key to the node's clock; null with `newest`. */
	ageDays: number | null;
	/** The node says the newest key is dated in the future (review FRESH-R1 M1): no newest day is
	 * shown, and the dataset is never called fresh or stale. */
	future: boolean;
	/** The manifest root of this version (0x and 64 hex digits), or '' when not given: a download
	 * of many pages pins it, so every page is of one version. */
	root: string;
};

export type Page = { items: Dataset[]; total: number; next: string | null };

/** Rows one page asks for (the node allows up to 500). */
export const PAGE = 50;
/** Days after which a dataset counts as not updated (READINESS A20): one rule for the mark and
 * the node's `fresh` filter. */
export const STALE_DAYS = 30;
/** Characters of a search at most (the node's MAX_SEARCH_CHARS). */
export const MAX_SEARCH = 64;

const ID = /^[a-z0-9][a-z0-9._:-]{0,127}$/;
const INT = /^-?[0-9]{1,39}$/;
const LICENSE = /^[A-Za-z0-9.+-]{1,40}$/;

/** A search as the node takes it: lowercase, only letters, digits, spaces and . - _ /. */
export function cleanSearch(s: string): string {
	return s
		.toLowerCase()
		.replace(/[^a-z0-9 ./_-]+/g, ' ')
		.replace(/\s+/g, ' ')
		.trim()
		.slice(0, MAX_SEARCH);
}

/** The list URL for a search and the `next` cursor of the page before: what was typed goes as the
 * node's whole-word `any` search, each word with the ids that mean it (#lib/glossary.ts).
 * `freshOnly` asks the node for the datasets with data from the last STALE_DAYS days only. */
export function listUrl(q: string, after: string | null = null, limit = PAGE, freshOnly = false): string {
	return anyUrl(q ? anyValue(anyGroups(q)) : '', after, limit, freshOnly);
}

/** The list URL for a ready `any` value (id words as the node takes them; '' lists everything). */
export function anyUrl(any: string, after: string | null = null, limit = PAGE, freshOnly = false): string {
	const p = new URLSearchParams();
	if (any) p.set('any', any);
	if (freshOnly) p.set('fresh', `${STALE_DAYS}d`);
	p.set('limit', String(limit));
	if (after) p.set('after', after);
	return `/v1/bulk?${p}`;
}

const nat = (v: unknown): number | null => (typeof v === 'number' && Number.isSafeInteger(v) && v >= 0 ? v : null);
const DAY_TEXT = /^[0-9]{4}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])$/;

/** The node's `newest` and `newest_age_days`, both or neither; `newest_in_future` wins over both
 * (a node that sends a day and the future mark together is not believed about the day). */
function freshness(o: Record<string, unknown>): { newest: string | null; ageDays: number | null; future: boolean } {
	if (o.newest_in_future === true) return { newest: null, ageDays: null, future: true };
	const age = nat(o.newest_age_days);
	if (typeof o.newest !== 'string' || !DAY_TEXT.test(o.newest) || age === null) return { newest: null, ageDays: null, future: false };
	return { newest: o.newest, ageDays: age, future: false };
}

/** The last day a dataset has data for when that is over STALE_DAYS days back, else null (also
 * for a dataset whose key is not a time: nothing says how old it is). */
export function staleSince(d: Pick<Dataset, 'newest' | 'ageDays'>): string | null {
	return d.newest !== null && d.ageDays !== null && d.ageDays > STALE_DAYS ? d.newest : null;
}

/** One list item, or null when any field is out of shape. */
export function item(v: unknown): Dataset | null {
	if (!v || typeof v !== 'object') return null;
	const o = v as Record<string, unknown>;
	const [version, rows, bytes, shards, stored] = [o.version, o.rows, o.bytes, o.shards, o.stored_shards].map(nat);
	const { publisher, dataset, key_min, key_max, license } = o;
	if (typeof publisher !== 'string' || !ID.test(publisher) || typeof dataset !== 'string' || !ID.test(dataset)) return null;
	if (typeof key_min !== 'string' || !INT.test(key_min) || typeof key_max !== 'string' || !INT.test(key_max)) return null;
	if (version === null || rows === null || bytes === null || shards === null || stored === null) return null;
	return {
		publisher,
		dataset,
		version,
		rows,
		bytes,
		shards,
		stored,
		keyMin: key_min,
		keyMax: key_max,
		license: typeof license === 'string' && LICENSE.test(license) ? license : '',
		created: nat(o.created_unix),
		...freshness(o),
		root: typeof o.root === 'string' && /^0x[0-9a-f]{64}$/.test(o.root) ? o.root : ''
	};
}

/** A list answer: the items in shape, the match count and the next cursor. */
export function page(v: unknown): Page | null {
	if (!v || typeof v !== 'object') return null;
	const o = v as Record<string, unknown>;
	if (!Array.isArray(o.datasets)) return null;
	const items = o.datasets.map(item).filter((x): x is Dataset => x !== null);
	const total = nat(o.total) ?? items.length;
	const next = typeof o.next === 'string' && /^[a-z0-9._:-]+\/[a-z0-9._:-]+$/.test(o.next) ? o.next : null;
	return { items, total, next };
}

const DAY = 86_400;
// 2009-01-03 to 2100-01-01: keys outside are not read as days.
const FIRST_DAY = 1_230_940_800;
const LAST_DAY = 4_102_444_800;

/** True when both keys are whole UTC days in a plausible span (the key is a day's unix time). */
export function dayKeys(a: string, b: string): boolean {
	return [a, b].every((k) => {
		const n = Number(k);
		return Number.isSafeInteger(n) && n % DAY === 0 && n >= FIRST_DAY && n <= LAST_DAY;
	});
}

const ISO = (t: number) => new Date(t * 1000).toISOString().slice(0, 10);

/** The key range: "2010-07-17 to 2026-08-28" for day keys, else "1,000 to 2,000". */
export function keyRange(a: string, b: string): string {
	if (dayKeys(a, b)) return a === b ? ISO(Number(a)) : `${ISO(Number(a))} to ${ISO(Number(b))}`;
	const f = (k: string) => group(BigInt(k));
	return a === b ? f(a) : `${f(a)} to ${f(b)}`;
}

/** A unix time as its UTC day, "2026-11-04"; empty when out of range. */
export const isoDay = (t: number | null): string => (t !== null && t > 0 && t <= LAST_DAY ? ISO(t) : '');

/** A cell's key as a date for day keys, else grouped digits. */
export function keyText(k: string, days: boolean): string {
	if (!INT.test(k)) return '';
	return days ? ISO(Number(k)) : group(BigInt(k));
}

/** Bytes in KB, MB or GB (1,000-based, one decimal from MB up). */
export function size(n: number): string {
	if (n < 1_000) return `${n} B`;
	if (n < 1_000_000) return `${Math.round(n / 1_000)} KB`;
	if (n < 1_000_000_000) return `${(n / 1_000_000).toFixed(1)} MB`;
	return `${(n / 1_000_000_000).toFixed(1)} GB`;
}

/** The asset part of an id like "btc.sth-net-position" ("btc"), for grouping by eye. */
export const head = (dataset: string) => dataset.split('.')[0];

/** A decimal without the zeros its scale pads it with ("2959.1500" -> "2959.15", "3.000" -> "3"). */
export function trimDec(v: string): string {
	return /^-?[0-9]+\.[0-9]+$/.test(v) ? v.replace(/0+$/, '').replace(/\.$/, '') : v;
}

export type Sample = { columns: string[]; rows: string[][]; from: string; to: string; book: string; nextPage: number | null };

/** A rows answer as a table: the schema's column names and each row's values as text. */
export function sample(v: unknown): Sample | null {
	if (!v || typeof v !== 'object') return null;
	const o = v as Record<string, unknown>;
	const m = o.manifest as Record<string, unknown> | undefined;
	const cols = (m?.schema as Record<string, unknown> | undefined)?.columns;
	if (!Array.isArray(cols) || !Array.isArray(o.rows)) return null;
	const columns = cols.map((c) => (c && typeof c === 'object' && typeof (c as { name?: unknown }).name === 'string' ? (c as { name: string }).name : '')).filter((n) => /^[a-z_][a-z0-9_]{0,62}$/.test(n));
	if (columns.length !== cols.length || columns.length === 0) return null;
	const rows: string[][] = [];
	const keys: string[] = [];
	for (const r of o.rows) {
		const vals = r && typeof r === 'object' ? (r as { values?: unknown }).values : null;
		const key = r && typeof r === 'object' ? (r as { key?: unknown }).key : null;
		if (!vals || typeof vals !== 'object' || typeof key !== 'string' || !INT.test(key)) continue;
		keys.push(key);
		rows.push(
			columns.map((c) => {
				const x = (vals as Record<string, unknown>)[c];
				return typeof x === 'string' ? trimDec(x).slice(0, 80) : typeof x === 'number' ? String(x) : '';
			})
		);
	}
	if (!rows.length) return null;
	const a = o.anchor as Record<string, unknown> | undefined;
	const book = typeof a?.book === 'string' && /^0x[0-9a-fA-F]{40}$/.test(a.book) ? a.book : '';
	return { columns, rows, from: keys[0], to: keys[keys.length - 1], book, nextPage: nat(o.next_page) };
}

/** The rows URL for the newest `n` rows of a dataset (the last page of `n`). */
export function newestUrl(d: Dataset, n = 10): string {
	const last = Math.max(0, Math.ceil(d.rows / n) - 1);
	return `/v1/bulk/${d.publisher}/${d.dataset}/rows?limit=${n}&page=${last}`;
}

const PAIR = /^[a-z0-9][a-z0-9._:-]{0,127}\/[a-z0-9][a-z0-9._:-]{0,127}$/;

/** The search and the open dataset a page link carries ("#/datasets?q=btc&d=pub/ds"). */
export function fromLink(hash: string): { q: string; d: string | null } {
	const i = hash.indexOf('?');
	const p = new URLSearchParams(i < 0 ? '' : hash.slice(i + 1));
	const d = p.get('d');
	return { q: cleanSearch(p.get('q') ?? ''), d: d && PAIR.test(d) ? d : null };
}

/** The page link for a search and an open dataset. */
export function toLink(q: string, d: string | null = null): string {
	const p = new URLSearchParams();
	if (q) p.set('q', q);
	if (d) p.set('d', d);
	const s = p.toString();
	return `#/datasets${s ? '?' + s : ''}`;
}

/** The node's own answer about one dataset (`GET /v1/bulk/{publisher}/{dataset}`): 200 while it
 * is held, 410 `bulk_dataset_removed` once its publisher removed it. */
export const infoUrl = (pair: string): string => `/v1/bulk/${pair}`;

/** Where a removed dataset went: the last id of its successor chain and whether that id is live. */
export type Moved = { to: string | null; live: boolean };

/** A removed dataset's successor, from the node's 410 (R-BULK-17): `to` is the chain's last id
 * under the same publisher, `live` only when the node says the chain ends live at another id.
 * Null for any other answer and for a removed dataset without a `successor` (nothing to say).
 * This is the node's word, not a proof: the proofs are at `…/successor` (`onyx-check --successor`). */
export function moved(status: number, v: unknown, pair: string): Moved | null {
	if (status !== 410 || !v || typeof v !== 'object') return null;
	const o = v as { error?: { code?: unknown }; successor?: unknown };
	if (o.error?.code !== 'bulk_dataset_removed' || !o.successor || typeof o.successor !== 'object') return null;
	const s = o.successor as Record<string, unknown>;
	const [publisher, dataset] = pair.split('/');
	const final = typeof s.final === 'string' && ID.test(s.final) && s.final !== dataset ? `${publisher}/${s.final}` : null;
	return { to: final, live: s.status === 'live' && final !== null };
}

/** The node's whole dataset count (`total` of an unfiltered list of one), or null. */
export const countUrl = (): string => listUrl('', null, 1);

/** Days a chart covers at most (one row a day: about 90 rows, under the node's 100 a page). */
export const CHART_DAYS = 90;

/** The rows URL a dataset opens with: for day keys the newest CHART_DAYS days (a chart and the
 * newest rows from one answer), else the newest 10 rows. */
export function detailUrl(d: Dataset): string {
	if (!dayKeys(d.keyMin, d.keyMax)) return newestUrl(d);
	const to = BigInt(d.keyMax);
	const from = to - BigInt((CHART_DAYS - 1) * DAY);
	const lo = BigInt(d.keyMin);
	return `/v1/bulk/${d.publisher}/${d.dataset}/rows?from=${from > lo ? from : lo}&to=${to}&limit=100`;
}

const DEC = /^-?[0-9]+(\.[0-9]+)?$/;

/** The columns a chart can draw: every value a plain number, at least two of them, never in a
 * table of series (one row per series and day: one line would mix them). */
export function chartColumns(s: Sample): number[] {
	if (s.columns.includes('series')) return [];
	return s.columns
		.map((_, i) => i)
		.filter((i) => i > 0)
		.filter((i) => {
			const vals = s.rows.map((r) => r[i]).filter((v) => v !== '');
			return vals.length >= 2 && vals.every((v) => DEC.test(v));
		});
}

/** One column as chart points: the exact value as text, a number only for drawing. */
export function chartPoints(s: Sample, col: number, days: boolean): { v: number; text: string; head: string }[] {
	return s.rows
		.filter((r) => r[col] !== '' && DEC.test(r[col]))
		.map((r) => ({ v: Number(r[col]), text: r[col], head: keyText(r[0], days) }));
}

// ---- Order (newcomer test, 2026-11-18): what a search and the opening list show first. ----

/** Datasets a list call may ask for at once (the node's MAX_LIST_PAGE). */
export const LIST_MAX = 500;
/** Matches of one search that are loaded and ranked (4 calls at most). */
export const SEARCH_CAP = 2000;
/** The assets the opening list shows first, in this order. */
export const MAJORS = ['btc', 'eth'];
/** The publisher of the network's own test records. */
export const TEST_PUBLISHER = 'early-record';

/** Data from the last STALE_DAYS days, as the node's `fresh` filter counts it (a key that is no
 * time, or a day in the future, is not fresh). */
export const isFresh = (d: Pick<Dataset, 'ageDays'>): boolean => d.ageDays !== null && d.ageDays <= STALE_DAYS;

/** A test or internal set: its id starts with "testnet", or the test publisher made it. */
export const isTestSet = (d: Pick<Dataset, 'publisher' | 'dataset'>): boolean => d.publisher === TEST_PUBLISHER || d.dataset.startsWith('testnet');

const majorRank = (d: Pick<Dataset, 'dataset'>): number => {
	const i = MAJORS.indexOf(head(d.dataset));
	return i < 0 ? MAJORS.length : i;
};
const idText = (d: Pick<Dataset, 'publisher' | 'dataset'>): string => `${d.publisher}/${d.dataset}`;
const byId = (a: Pick<Dataset, 'publisher' | 'dataset'>, b: Pick<Dataset, 'publisher' | 'dataset'>): number => (idText(a) < idText(b) ? -1 : idText(a) > idText(b) ? 1 : 0);

type Ordered = Pick<Dataset, 'publisher' | 'dataset' | 'ageDays'>;
/** 0: updated in the last STALE_DAYS days; 1: not; 2: test data (always last). */
const orderGroup = (d: Ordered): number => (isTestSet(d) ? 2 : isFresh(d) ? 0 : 1);

/** The opening order (no search): datasets updated in the last STALE_DAYS days first, then the
 * rest, test data last; in each group the majors first (btc, eth), then by id. */
export function defaultOrder(a: Ordered, b: Ordered): number {
	return orderGroup(a) - orderGroup(b) || (isTestSet(a) ? 0 : majorRank(a) - majorRank(b)) || byId(a, b);
}

/** One step of the opening list: a node list (`any`, `fresh`) and which of its items this step
 * shows. The steps in a row give `defaultOrder` without loading the whole list first. */
export type Segment = { any: string; fresh: boolean; keep: (d: Dataset) => boolean };

/** The steps of the opening list. `freshOnly`: only the datasets updated in the last STALE_DAYS days. */
export function defaultSegments(freshOnly: boolean): Segment[] {
	const part = (fresh: boolean): Segment[] => [
		...MAJORS.map((m) => ({ any: m, fresh, keep: (d: Dataset) => !isTestSet(d) && head(d.dataset) === m && isFresh(d) === fresh })),
		{ any: '', fresh, keep: (d: Dataset) => !isTestSet(d) && !MAJORS.includes(head(d.dataset)) && isFresh(d) === fresh }
	];
	const tests: Segment = { any: `testnet*,${TEST_PUBLISHER}`, fresh: freshOnly, keep: (d) => isTestSet(d) && (!freshOnly || isFresh(d)) };
	return freshOnly ? [...part(true), tests] : [...part(true), ...part(false), tests];
}

/** What a search names: the asset ids it means (an id as typed, or exactly an asset's name:
 * "bitcoin" is btc, never wbtc or bch) and its other words. */
export function searchAssets(typed: string): { assets: string[]; rest: string[] } {
	const assets: string[] = [];
	const rest: string[] = [];
	for (const token of cleanSearch(typed).split(' ').filter(Boolean)) {
		// An id with a dash ("usdt-trx") is one asset.
		const parts = Object.hasOwn(ASSETS, token) ? [token] : token.split(/[^a-z0-9]+/).filter(Boolean);
		for (const w of parts) {
			const named = Object.entries(ASSETS).filter(([, a]) => a.name.toLowerCase() === w).map(([id]) => id);
			if (Object.hasOwn(ASSETS, w)) assets.push(w);
			else if (named.length) assets.push(...named);
			else rest.push(w);
		}
	}
	return { assets: [...new Set(assets)], rest };
}

/** The words after the first dot of an id ("btc.mvrv-z-score" -> mvrv, z, score). */
const metricWords = (dataset: string): string[] => {
	const i = dataset.indexOf('.');
	return (i < 0 ? '' : dataset.slice(i + 1)).split(/[^a-z0-9]+/).filter(Boolean);
};

/** The matches of a search, best first (the node and the AI connector use the same rule):
 * 1. a dataset whose asset (the id before the first dot) IS a searched asset comes before one
 *    that only holds the word somewhere: a stale dataset of the asset still comes before a fresh
 *    one of another asset;
 * 2. then a dataset whose metric name is exactly the other words searched ("mvrv": btc.mvrv
 *    before btc.sth-mvrv);
 * 3. then data from the last STALE_DAYS days, the shorter name, the id. */
export function rankSearch<T extends Ordered>(items: T[], typed: string): T[] {
	const { assets, rest } = searchAssets(typed);
	const want = rest.join(' ');
	const key = (d: T): number[] => {
		const words = metricWords(d.dataset);
		return [assets.length && assets.includes(head(d.dataset)) ? 0 : 1, want && words.join(' ') === want ? 0 : 1, isFresh(d) ? 0 : 1, words.length];
	};
	return items
		.map((d) => ({ d, k: key(d) }))
		.sort((a, b) => a.k[0] - b.k[0] || a.k[1] - b.k[1] || a.k[2] - b.k[2] || a.k[3] - b.k[3] || byId(a.d, b.d))
		.map((x) => x.d);
}

/** How many datasets have data from the last STALE_DAYS days and how many do not, from the
 * node's two counts (all, and its `fresh` filter). Null when a count is missing or they disagree. */
export function freshCounts(all: number | null, fresh: number | null): { all: number; fresh: number; stale: number } | null {
	if (all === null || fresh === null || !Number.isSafeInteger(all) || !Number.isSafeInteger(fresh) || all <= 0 || fresh < 0 || fresh > all) return null;
	return { all, fresh, stale: all - fresh };
}

/** The same count over a loaded list (the matches of a search). */
export const freshIn = (items: Pick<Dataset, 'ageDays'>[]): number => items.filter(isFresh).length;

/** The node's count of datasets with data from the last STALE_DAYS days (`total` of a filtered list of one). */
export const freshCountUrl = (): string => listUrl('', null, 1, true);

// ---- The opened card: headline column, newest rows first, CSV. ----

const flat = (s: string): string => s.replace(/[^a-z0-9]+/g, '');

/** The column a dataset's chart opens with: the one named like the metric ("mvrv" for btc.mvrv,
 * "mvrv_zscore" for btc.mvrv-z-score, "price_usd" for btc.price), else the first column a chart
 * can draw; -1 when there is none. */
export function headlineColumn(s: Sample, dataset: string): number {
	const cols = chartColumns(s);
	if (!cols.length) return -1;
	const words = metricWords(dataset);
	const name = (i: number) => s.columns[i];
	const shortest = (c: number[]) => (c.length ? c.reduce((a, b) => (name(b).length < name(a).length ? b : a)) : -1);
	if (words.length) {
		const metric = words.join('_');
		// The whole metric name, written with or without separators.
		const exact = cols.find((i) => name(i) === metric) ?? cols.find((i) => flat(name(i)) === flat(metric));
		if (exact !== undefined) return exact;
		// Its last words ("entity-adjusted-mvrv": mvrv), the longest ending first.
		for (let from = 1; from < words.length; from++) {
			const tail = flat(words.slice(from).join('_'));
			const hit = cols.find((i) => flat(name(i)) === tail);
			if (hit !== undefined) return hit;
		}
		// The metric with a unit or a qualifier ("price_usd", "sth_net_position_change").
		const starts = shortest(cols.filter((i) => name(i).startsWith(`${metric}_`)));
		if (starts >= 0) return starts;
		const holds = shortest(cols.filter((i) => `_${name(i)}_`.includes(`_${metric}_`)));
		if (holds >= 0) return holds;
	}
	return cols[0];
}

/** The newest `n` rows of a sample, newest first (a sample's rows come oldest first). */
export const newestFirst = (rows: string[][], n = 10): string[][] => rows.slice(-n).reverse();

/** One CSV cell: quoted when it holds a comma, a quote or a line break; a text that a
 * spreadsheet would run as a formula gets a leading apostrophe (a plain number stays as it is). */
function csvCell(v: string): string {
	const safe = /^[=+@\t\r]/.test(v) || (v.startsWith('-') && !DEC.test(v)) ? `'${v}` : v;
	return /[",\r\n]/.test(safe) ? `"${safe.replace(/"/g, '""')}"` : safe;
}

/** Rows as a CSV file's text: the column names, then one line per row (CRLF, RFC 4180). With
 * `days`, the key column is written as a UTC date ("2026-08-28"), as the table shows it. */
export function csv(columns: string[], rows: string[][], days = false): string {
	const line = (cells: string[]) => cells.map(csvCell).join(',');
	return [line(columns), ...rows.map((r) => line(r.map((v, i) => (i === 0 && days ? keyText(v, true) || v : v))))].join('\r\n') + '\r\n';
}

/** Rows a history call asks for (the node's MAX_PAGE_ROWS). */
export const HISTORY_PAGE = 1000;
/** Rows a full-history download stops at (50 calls). */
export const HISTORY_CAP = 50_000;

/** One page of a dataset's whole history, oldest first: `page` from 0, then each answer's
 * `next_page` until it is null. `root` pins the version, so every page is of the same one. */
export function historyUrl(d: Pick<Dataset, 'publisher' | 'dataset' | 'root'>, page: number): string {
	return `/v1/bulk/${d.publisher}/${d.dataset}/rows?limit=${HISTORY_PAGE}&page=${page}${d.root ? `&root=${d.root}` : ''}`;
}

/** The name a download is saved under ("onchain-archive_btc.mvrv.csv"). */
export const csvName = (d: Pick<Dataset, 'publisher' | 'dataset'>): string => `${d.publisher}_${d.dataset}.csv`.replace(/[^a-z0-9._-]/g, '_');

/** Five lines of Python (requests and pandas) that read a dataset's newest rows. */
export function pythonSnippet(origin: string, d: Pick<Dataset, 'publisher' | 'dataset'>): string {
	return [
		'import requests, pandas as pd',
		`url = "${origin}/v1/bulk/${d.publisher}/${d.dataset}/rows"`,
		`rows = requests.get(url, params={"order": "desc", "limit": ${HISTORY_PAGE}}).json()["rows"]`,
		'df = pd.DataFrame([r["values"] for r in rows])',
		'print(df.head(10))'
	].join('\n');
}
