// The Data page's on-chain section (ADR-0043): chart points from the verifier's checked
// `btc-chain-stats` window. Every figure is a field of `ChainCheck`, which the Rust verifier
// computed from the leaves it checked against the pinned commitment; this file only turns
// those integers into labels (string math for exact values, numbers only for drawing).
import { btc, group, int } from './format.ts';
import { heightLabel, tickBtc, type ChainPoint } from './chart.ts';
import type { ChainCheck, ChainSums } from './verifier.ts';

type Day = ChainCheck['days'][number];
type Block = ChainCheck['blocks'][number];

/** "4 Oct" in UTC. */
export const dayLabel = (t: number) => new Date(t * 1000).toLocaleDateString('en-GB', { day: 'numeric', month: 'short', timeZone: 'UTC' });
/** "2026-10-04 11:59" in UTC. */
export const stamp = (t: number) => new Date(t * 1000).toISOString().slice(0, 16).replace('T', ' ');
const hm = (t: number) => new Date(t * 1000).toISOString().slice(11, 16);

/** An integer in thousandths as a decimal, "4000" -> "4.000" (string math). */
export function milli(v: number | string): string {
	const n = BigInt(v);
	const neg = n < 0n;
	const a = neg ? -n : n;
	return `${neg ? '−' : ''}${group(a / 1000n)}.${(a % 1000n).toString().padStart(3, '0')}`;
}
/** Parts per million as a percentage with two decimals, "912345" -> "91.23%". */
export const pct = (ppm: number) => `${group(Math.floor(ppm / 10_000))}.${String(Math.floor(ppm / 100) % 100).padStart(2, '0')}%`;
/** Satoshis as whole BTC, rounded down ("123456789012" -> "1,234"). */
export const wholeBtc = (sats: string) => btc(sats).split('.')[0];
/** Seconds as minutes with one decimal, rounded down (string math). */
export function minutes(secs: string | number): string {
	const s = BigInt(secs);
	const neg = s < 0n;
	const a = neg ? -s : s;
	return `${neg ? '−' : ''}${a / 60n}.${((a % 60n) * 10n) / 60n}`;
}

/** A day's tooltip head: the date, and the hours it covers when it is partial. */
function head(d: Day): string {
	const full = hm(d.first_time) < '01:00' && hm(d.last_time) > '23:00';
	return full ? dayLabel(d.first_time) : `${dayLabel(d.first_time)} (${hm(d.first_time)}–${hm(d.last_time)} UTC)`;
}

const dayPoint = (d: Day, v: number, text: string): ChainPoint => ({
	v,
	text,
	top: dayLabel(d.first_time),
	sub: heightLabel(d.from),
	head: `${head(d)} · blocks ${int(d.from)}–${int(d.to)} (${d.blocks})`
});
const blockPoint = (b: Block, v: number, text: string): ChainPoint => ({
	v,
	text,
	top: dayLabel(b.time),
	sub: heightLabel(b.height),
	head: `#${int(b.height)} · ${stamp(b.time)} UTC`
});

export type ChainSpec = {
	id: string;
	title: string;
	/** The window's figure, next to the title. */
	summary: string;
	unit: string;
	mode: 'bars' | 'line';
	step: string;
	tick: (v: number) => string;
	points: ChainPoint[];
};

/** An axis value in the shared tick formats (chart.ts): a whole number grouped, else as `tickBtc`
 *  (one decimal from 1, two below), which is exact for every top `nice()` gives. */
const num = (v: number) => (Number.isInteger(v) ? int(v) : tickBtc(v));
const toBtc = (sats: string) => Number(BigInt(sats)) / 1e8;

/** The section's charts, in reading order. `w` is the window's sums. */
export function chainCharts(c: ChainCheck): ChainSpec[] {
	const w: ChainSums = c.window;
	const days = c.days;
	const blocks = c.blocks;
	return [
		{
			id: 'txs', title: 'Transactions', summary: `${int(w.txs)} in the window`, unit: 'per UTC day', mode: 'bars', step: 'days', tick: num,
			points: days.map((d) => dayPoint(d, d.txs, int(d.txs)))
		},
		{
			id: 'fees', title: 'Fees paid', summary: `${btc(w.fees_sats)} BTC`, unit: 'BTC per UTC day', mode: 'bars', step: 'days', tick: tickBtc,
			points: days.map((d) => dayPoint(d, toBtc(d.fees_sats), `${btc(d.fees_sats)} BTC`))
		},
		{
			id: 'feerate', title: 'Median fee rate', summary: 'per block, lower median of its transactions, unweighted by size', unit: 'sat/vB', mode: 'line', step: 'blocks',
			tick: num,
			points: blocks.map((b) => blockPoint(b, b.fee_rate_median_msat_vb / 1000, `${milli(b.fee_rate_median_msat_vb)} sat/vB`))
		},
		{
			id: 'supply', title: 'New supply', summary: `${btc(w.issued_sats)} BTC (subsidy allowed ${btc(w.subsidy_sats)})`, unit: 'BTC per UTC day', mode: 'bars', step: 'days', tick: tickBtc,
			points: days.map((d) => dayPoint(d, toBtc(d.issued_sats), `${btc(d.issued_sats)} BTC`))
		},
		{
			id: 'moved', title: 'Value moved', summary: `${wholeBtc(w.transfer_sats)} BTC, change included`, unit: 'BTC per UTC day', mode: 'bars', step: 'days', tick: tickBtc,
			points: days.map((d) => dayPoint(d, toBtc(d.transfer_sats), `${wholeBtc(d.transfer_sats)} BTC`))
		},
		{
			id: 'segwit', title: 'SegWit transactions', summary: `${pct(w.segwit_ppm)} of non-coinbase transactions`, unit: '% per UTC day', mode: 'bars', step: 'days', tick: num,
			points: days.map((d) => dayPoint(d, d.segwit_ppm / 10_000, pct(d.segwit_ppm)))
		},
		{
			id: 'taproot', title: 'Taproot spends', summary: `${pct(w.taproot_inputs_ppm)} of inputs; ${pct(w.taproot_outputs_ppm)} of outputs pay to Taproot`, unit: '% of inputs per UTC day', mode: 'bars', step: 'days', tick: num,
			points: days.map((d) => dayPoint(d, d.taproot_inputs_ppm / 10_000, pct(d.taproot_inputs_ppm)))
		},
		{
			id: 'fullness', title: 'Block fullness', summary: `${pct(w.fullness_ppm)} of the 4,000,000 weight-unit limit on average`, unit: '% per UTC day', mode: 'bars', step: 'days', tick: num,
			points: days.map((d) => dayPoint(d, d.fullness_ppm / 10_000, pct(d.fullness_ppm)))
		},
		{
			id: 'interval', title: 'Time between blocks', summary: `${minutes(w.mean_interval_secs)} minutes on average, by header time`, unit: 'mean minutes per UTC day', mode: 'bars', step: 'days', tick: num,
			points: days.map((d) => dayPoint(d, Number(d.mean_interval_secs) / 60, `${minutes(d.mean_interval_secs)} min (${d.blocks} blocks)`))
		},
		{
			id: 'active', title: 'Active scripts', summary: 'distinct addresses (spendable scripts) spent from or paid to, per block', unit: 'per block', mode: 'line', step: 'blocks', tick: num,
			points: blocks.map((b) => blockPoint(b, b.active_scripts, int(b.active_scripts)))
		},
		{
			id: 'cbd', title: 'Coin-blocks destroyed', summary: `${wholeBtc(w.coin_blocks_destroyed)} BTC-blocks: value spent times blocks it sat unspent`, unit: 'BTC-blocks per UTC day', mode: 'bars', step: 'days', tick: tickBtc,
			points: days.map((d) => dayPoint(d, toBtc(d.coin_blocks_destroyed), `${wholeBtc(d.coin_blocks_destroyed)} BTC-blocks`))
		}
	];
}
