// The Datasets page (ROADMAP S22): the node's bulk datasets, searched and paged by the node
// (`GET /v1/bulk?q=&limit=&after=`). Every value shown comes from the answer through the checks
// below: an item that does not match the shape is dropped, never printed.
import { group } from './format.ts';
import { anyGroups, anyValue } from './glossary.ts';

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
};

export type Page = { items: Dataset[]; total: number; next: string | null };

/** Rows one page asks for (the node allows up to 500). */
export const PAGE = 50;
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
 * node's whole-word `any` search, each word with the ids that mean it (#lib/glossary.ts). */
export function listUrl(q: string, after: string | null = null, limit = PAGE): string {
	const p = new URLSearchParams();
	if (q) p.set('any', anyValue(anyGroups(q)));
	p.set('limit', String(limit));
	if (after) p.set('after', after);
	return `/v1/bulk?${p}`;
}

const nat = (v: unknown): number | null => (typeof v === 'number' && Number.isSafeInteger(v) && v >= 0 ? v : null);

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
		created: nat(o.created_unix)
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

export type Sample = { columns: string[]; rows: string[][]; from: string; to: string; book: string };

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
	return { columns, rows, from: keys[0], to: keys[keys.length - 1], book };
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
