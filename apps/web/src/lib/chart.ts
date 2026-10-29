// One chart style for the site (W6, docs/website-plan.md). Every chart (Data: blocks, flows,
// series, history, pools; Reserves; Home's compact pair) draws its axes, ticks and tooltip
// with these helpers and the `.chart` rules in app.css, so the numbers read the same way
// everywhere:
// - y ticks: BTC as `tickBtc` (grouped whole BTC from 100, one decimal from 1, two below,
//   true minus sign); counts and minutes as grouped integers (`int`);
// - x ticks: first, middle and last point (`edgeTicks`), block heights as "#971,234"
//   (`heightLabel`), with the UTC time or day above when the chart has times;
// - colours (app.css tokens only): bars above zero / in --accent, below zero / out --btc,
//   lines (running totals, balances) --line-net, categories --pool-1..6 and --pool-other.
import { int } from './format.ts';

/** Width of one IBM Plex Mono character at the 12 px axis size (0.6 em). */
const AXIS_CH = 7.2;
/** Tooltip width; the frame keeps it inside the chart. */
export const TIP_W = 220;

/** One BTC tick format for every chart. */
export function tickBtc(v: number): string {
	const a = Math.abs(v);
	const s = a === 0 ? '0' : a >= 100 ? int(Math.round(a)) : a >= 1 ? a.toFixed(1) : a.toFixed(2);
	return v < 0 && s !== '0' ? `−${s}` : s;
}

/** A block height as an axis tick or a tooltip head. */
export const heightLabel = (h: number) => `#${int(h)}`;

/** Left padding that fits the longest y label plus the 8 px gap to the plot; at least 54 px
 *  (six characters, "−1,500"), so stacked charts share one left edge. */
export const padFor = (labels: string[]) => Math.max(54, Math.ceil(Math.max(0, ...labels.map((l) => l.length)) * AXIS_CH) + 10);

/** W12 round 2: the right-hand gutter of a compact chart (Home's preview pair), wide enough for
 *  its scale labels (the top value and zero) set left-aligned after the plot. */
export const gutterFor = (labels: string[]) => Math.ceil(Math.max(0, ...labels.map((l) => l.length)) * AXIS_CH) + 8;

/** First, middle and last index (deduplicated) of `n` points. */
export const edgeTicks = (n: number) => (n <= 0 ? [] : [...new Set([0, Math.floor((n - 1) / 2), n - 1])]);

/** Text anchor of an x tick, so the end labels stay inside the plot. */
export const tickAnchor = (i: number, n: number) => (i === 0 && n > 1 ? 'start' : i === n - 1 && n > 1 ? 'end' : 'middle');

/** A bar from `y0` to `y`, rounded (3 px) only at the data end. Empty under half a pixel. */
export function barPath(x: number, y0: number, y: number, width: number): string {
	const h = Math.abs(y - y0);
	if (h < 0.5) return '';
	const r = Math.min(3, h, width / 2);
	const l = x - width / 2;
	const s = y < y0 ? 1 : -1;
	return `M${l},${y0}V${y + s * r}Q${l},${y} ${l + r},${y}H${l + width - r}Q${l + width},${y} ${l + width},${y + s * r}V${y0}Z`;
}

/** Bar width in a band: 62 % of it, between 2 and 24 px. */
export const barWidth = (band: number) => Math.max(2, Math.min(band * 0.62, 24));

/** The chart colours, as app.css tokens (W3: each >= 3:1 on every panel). */
export const COLOR = {
	/** Bars above zero, flows in. */
	in: 'var(--accent)',
	/** Bars below zero, flows out. */
	out: 'var(--btc)',
	/** Lines: running totals and balances. */
	line: 'var(--line-net)',
	/** Categories in a fixed order (pools, the fee split); `other` for the rest. */
	cat: (i: number) => `var(--pool-${i + 1})`,
	other: 'var(--pool-other)'
} as const;

/** One legend entry (ChartLegend): a bar square, a line dash or the unknown hatch. */
export type LegendItem = { label: string; kind?: 'bar' | 'line' | 'hatch'; color?: string };
/** Legends of the flow charts: in/out bars and the running line. */
export const FLOW_KEYS: LegendItem[] = [
	{ label: 'In', color: COLOR.in },
	{ label: 'Out', color: COLOR.out },
	{ label: 'Running net', kind: 'line' }
];
/** Reserves (W11): the balance line, blue for a week that ended higher (▲), amber for lower (▼). */
export const RESERVE_KEYS: LegendItem[] = [
	{ label: 'Balance at the end of each shard; ▲ the week ended higher', kind: 'line', color: COLOR.line },
	{ label: '▼ the week ended lower', kind: 'line', color: COLOR.out }
];
export const NET_KEYS: LegendItem[] = [
	{ label: 'Net in', color: COLOR.in },
	{ label: 'Net out', color: COLOR.out },
	{ label: 'Running total', kind: 'line' }
];

/** One point of an on-chain chart (ChainChart, ADR-0043): a day or a block. */
export type ChainPoint = {
	/** Drawing value, in the chart's unit (only the drawing uses it). */
	v: number;
	/** The exact value, as the tooltip and the table print it. */
	text: string;
	/** Upper x label (a date) and lower one (block heights). */
	top: string;
	sub: string;
	/** Tooltip head and the table's first column. */
	head: string;
};
