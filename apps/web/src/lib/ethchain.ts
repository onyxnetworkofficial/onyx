// The Data page's Ethereum section (ADR-0044): chart points from the verifier's checked
// `eth-chain-stats` window. Every figure is a field of `EthChainCheck`, which the Rust verifier
// computed from the leaves it checked against the pinned commitment; this file only turns those
// integers into labels (string math for exact values, numbers only for drawing).
import { group, int } from './format.ts';
import { heightLabel, tickBtc } from './chart.ts';
import type { EthChainBlock, EthChainCheck, EthChainSums } from './verifier.ts';

/** One point of an Ethereum chart: a block (line) or a run of blocks (bar). */
export type EthPoint = {
	/** Drawing value, in the chart's unit (only the drawing uses it). */
	v: number;
	/** The exact value, as the tooltip and the table print it. */
	text: string;
	/** Upper x label (a UTC time) and lower one (a block number). */
	top: string;
	sub: string;
	/** Tooltip head and the table's first column. */
	head: string;
};

export type EthSpec = {
	id: string;
	title: string;
	/** The window's figure, next to the title. */
	summary: string;
	unit: string;
	mode: 'bars' | 'line';
	step: string;
	tick: (v: number) => string;
	points: EthPoint[];
};

/** "2026-10-19 08:14" in UTC. */
export const stamp = (t: number) => new Date(t * 1000).toISOString().slice(0, 16).replace('T', ' ');
const hm = (t: number) => new Date(t * 1000).toISOString().slice(11, 16);

/** A wei amount (decimal string) in a unit of 10^`exp` wei, with `dp` decimals rounded down
 *  (string math, never a float): `units('1500000000000000000', 18, 2)` is "1.50". */
export function units(wei: string, exp: number, dp: number): string {
	const n = BigInt(wei);
	const base = 10n ** BigInt(exp);
	const whole = group(n / base);
	if (dp === 0) return whole;
	const frac = ((n % base) * 10n ** BigInt(dp)) / base;
	return `${whole}.${frac.toString().padStart(dp, '0')}`;
}
/** Wei as ether with four decimals, rounded down. */
export const eth = (wei: string, dp = 4) => units(wei, 18, dp);
/** Wei as gwei with three decimals, rounded down. */
export const gwei = (wei: string, dp = 3) => units(wei, 9, dp);
/** Parts per million as a percentage with two decimals, rounded down: "912345" -> "91.23%". */
export const pct = (ppm: number) => `${group(Math.floor(ppm / 10_000))}.${String(Math.floor(ppm / 100) % 100).padStart(2, '0')}%`;

/** Gas per blob (EIP-4844). */
const GAS_PER_BLOB = 131_072;
/** An axis value in the shared tick formats (chart.ts): a whole number grouped, else as `tickBtc`
 *  (one decimal from 1, two below), and two significant digits below 0.01 (a base fee of a few
 *  thousandths of a gwei). Every top `nice()` gives prints exactly. */
const num = (v: number) => (Number.isInteger(v) ? int(v) : Math.abs(v) >= 0.01 ? tickBtc(v) : String(Number(v.toPrecision(2))));
const toUnit = (wei: string, exp: number) => Number(BigInt(wei)) / 10 ** exp;

const blockPoint = (b: EthChainBlock, v: number, text: string): EthPoint => ({
	v,
	text,
	top: hm(b.time),
	sub: heightLabel(b.number),
	head: `#${int(b.number)} · ${stamp(b.time)} UTC`
});
type Group = EthChainCheck['groups'][number];
const groupPoint = (g: Group, v: number, text: string): EthPoint => ({
	v,
	text,
	top: hm(g.first_time),
	sub: heightLabel(g.from),
	head: `#${int(g.from)}–${int(g.to)} · ${hm(g.first_time)}–${hm(g.last_time)} UTC`
});

/** The section's charts, in reading order. */
export function ethCharts(c: EthChainCheck): EthSpec[] {
	const w: EthChainSums = c.window;
	const gs = c.groups;
	const bs = c.blocks;
	const per = `per ${c.group_blocks} blocks`;
	const blobs = (gas: string | number) => Number(BigInt(gas) / BigInt(GAS_PER_BLOB));
	const limit = bs.length ? bs[0].gas_limit : 0;
	return [
		{
			id: 'txs', title: 'Transactions', summary: `${int(w.txs)} in ${int(w.blocks)} blocks; ${int(w.failed_txs)} reverted (their gas is still paid)`,
			unit: per, mode: 'bars', step: 'bars', tick: num,
			points: gs.map((g) => groupPoint(g, g.txs, `${int(g.txs)} (${int(g.failed_txs)} reverted)`))
		},
		{
			id: 'gas', title: 'Gas used', summary: `${pct(w.fullness_ppm)} of the gas limit on average (${int(limit)} gas per block)`,
			unit: '% of the block gas limit', mode: 'line', step: 'blocks', tick: num,
			points: bs.map((b) => blockPoint(b, b.fullness_ppm / 10_000, `${pct(b.fullness_ppm)} (${int(b.gas_used)} gas)`))
		},
		{
			id: 'basefee', title: 'Base fee', summary: `${gwei(w.min_base_fee_wei)}–${gwei(w.max_base_fee_wei)} gwei per gas (EIP-1559)`,
			unit: 'gwei per gas', mode: 'line', step: 'blocks', tick: num,
			points: bs.map((b) => blockPoint(b, toUnit(b.base_fee_wei, 9), `${gwei(b.base_fee_wei)} gwei`))
		},
		{
			id: 'burned', title: 'ETH burned', summary: `${eth(w.burned_wei)} ETH: base fee x gas used (blob fees counted apart)`,
			unit: `ETH ${per}`, mode: 'bars', step: 'bars', tick: num,
			points: gs.map((g) => groupPoint(g, toUnit(g.burned_wei, 18), `${eth(g.burned_wei)} ETH`))
		},
		{
			id: 'priority', title: 'Priority fees', summary: `${eth(w.priority_fees_wei)} ETH above the base fee, paid to each block's fee recipient`,
			unit: `ETH ${per}`, mode: 'bars', step: 'bars', tick: num,
			points: gs.map((g) => groupPoint(g, toUnit(g.priority_fees_wei, 18), `${eth(g.priority_fees_wei)} ETH`))
		},
		{
			id: 'blobs', title: 'Blobs', summary: `${int(blobs(w.blob_gas_used))} blobs in ${int(w.blob_txs)} transactions; ${gwei(w.blob_fee_wei)} gwei burned as blob fees`,
			unit: per, mode: 'bars', step: 'bars', tick: num,
			points: gs.map((g) => groupPoint(g, blobs(g.blob_gas_used), `${int(blobs(g.blob_gas_used))} blobs, ${gwei(g.blob_fee_wei)} gwei in fees`))
		},
		{
			id: 'blobfee', title: 'Blob base fee', summary: 'per block, from its excess blob gas (EIP-4844, BPO2 parameters)',
			unit: 'wei per blob gas', mode: 'line', step: 'blocks', tick: num,
			points: bs.map((b) => blockPoint(b, Number(BigInt(b.blob_base_fee_wei)), `${int(b.blob_base_fee_wei)} wei`))
		},
		{
			id: 'creations', title: 'Contract creations', summary: `${int(w.contract_creations)} successful transactions without a recipient (reverted ones and contracts made by other contracts are not counted)`,
			unit: per, mode: 'bars', step: 'bars', tick: num,
			points: gs.map((g) => groupPoint(g, g.contract_creations, int(g.contract_creations)))
		},
		{
			id: 'value', title: 'Value sent', summary: `${eth(w.value_wei, 2)} ETH in the value field of successful transactions (reverted ones send nothing; transfers inside contracts are not counted)`,
			unit: `ETH ${per}`, mode: 'bars', step: 'bars', tick: num,
			points: gs.map((g) => groupPoint(g, toUnit(g.value_wei, 18), `${eth(g.value_wei, 2)} ETH`))
		}
	];
}
