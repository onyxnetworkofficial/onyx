// The live Bitcoin range (export-demo with ONYX_LIVE): header checks and the
// per-block sums, both run by the Rust verifier.
import type { PillState } from '#lib/components/Pill.svelte';
import type { Demo } from '#lib/data.ts';
import { verifier, type BlockSums, type Pinned, type SeriesSums } from '#lib/verifier.ts';

export type Pill = { state: PillState; text: string };

/** Recheck every live header (proof of work, hash, parent link) and that the
 *  chain is the pinned range ending at the pinned tip. */
export async function checkLiveHeaders(demo: Demo, pin: Pinned, onBlock: (height: number, s: PillState) => void): Promise<Pill> {
	const blocks = demo.live?.blocks ?? [];
	if (blocks.length !== pin.live.to - pin.live.from + 1 || blocks.some((b, k) => b.height !== pin.live.from + k)) {
		return { state: 'bad', text: 'Heights are not the pinned range' };
	}
	let prev: string | null = null;
	let bad = 0;
	for (const b of blocks) {
		onBlock(b.height, 'run');
		try {
			const h = await verifier.checkHeader(b.header);
			const ok = h.hash === b.hash && (prev === null || h.parent === prev);
			onBlock(b.height, ok ? 'good' : 'bad');
			if (!ok) bad++;
			prev = h.hash;
		} catch {
			onBlock(b.height, 'bad');
			bad++;
		}
	}
	if (bad) return { state: 'bad', text: `${bad} header(s) failed` };
	if (prev !== pin.live.tip) return { state: 'bad', text: 'Chain does not end at the pinned tip' };
	return { state: 'good', text: `All ${blocks.length} headers pass proof of work` };
}

/** Per-block rows against the range rows under the pinned root. `omit` leaves
 *  one block out (the tamper demo: a miner skipping a block). */
export function liveSums(demo: Demo, omit?: number): Promise<BlockSums> {
	const live = demo.live;
	if (!live?.per_block) return Promise.reject(new Error('no per-block rows in the export'));
	const blocks = live.per_block.filter((b) => b.height !== omit).map((b) => ({ height: b.height, rows: b.rows.map((r) => r.leaf) }));
	return verifier.blockSums(
		live.rows.map((r) => r.leaf),
		blocks
	);
}

/** The series rows against the range rows under the pinned roots. `drop`
 *  leaves one series row out (the tamper demo: a miner hiding a block). */
export function liveSeries(demo: Demo, drop?: number): Promise<SeriesSums> {
	const live = demo.live;
	if (!live?.series) return Promise.reject(new Error('no series rows in the export'));
	// A quiet range commits one NO_ACTIVITY leaf instead of rows (R-SERIES-1).
	const quiet = live.series.no_activity && live.series.no_activity_leaf;
	return verifier.seriesSums(
		live.rows.map((r) => r.leaf),
		quiet ? [quiet] : live.series.rows.filter((_, i) => i !== drop).map((r) => r.leaf)
	);
}

/** One entity's checked series over `from..=to`, zero where it has no row (R-SERIES-1). */
export function seriesPoints(s: { entities: { entity: string; points: SeriesSums['entities'][number]['points'] }[] }, entity: string, from: number, to: number): FlowPoint[] {
	const e = s.entities.find((x) => x.entity === entity);
	const by = new Map((e?.points ?? []).map((p) => [p.height, p]));
	const out: FlowPoint[] = [];
	for (let h = from; h <= to; h++) {
		const p = by.get(h);
		out.push({
			height: h,
			received: BigInt(p?.received_sats ?? 0),
			sent: BigInt(p?.sent_sats ?? 0),
			net: BigInt(p?.net_sats ?? 0),
			txs: p?.tx_count ?? 0
		});
	}
	return out;
}

/** One entity's per-block values from a checked sum, as BigInt sats. */
export type FlowPoint = { height: number; received: bigint; sent: bigint; net: bigint; txs: number };
export function entitySeries(s: BlockSums, entity: string): FlowPoint[] {
	return s.blocks.map((b) => {
		const r = b.rows.find((x) => x.entity === entity);
		return {
			height: b.height,
			received: BigInt(r?.received_sats ?? 0),
			sent: BigInt(r?.sent_sats ?? 0),
			net: BigInt(r?.net_sats ?? 0),
			txs: r?.tx_count ?? 0
		};
	});
}

/** Smallest "nice" number >= x (1, 1.5, 2, 2.5, 3, 4, 5, 6, 8 × 10^k). */
export function nice(x: number): number {
	if (!(x > 0)) return 1;
	const p = 10 ** Math.floor(Math.log10(x));
	for (const m of [1, 1.5, 2, 2.5, 3, 4, 5, 6, 8, 10]) if (m * p >= x - 1e-9) return m * p;
	return 10 * p;
}

/** Display name of an entity id ("entity:bitwise-bitb" -> "Bitwise BITB"). */
export function entityName(id: string): string {
	const known: Record<string, string> = {
		'entity:okx': 'OKX',
		'entity:bitwise-bitb': 'Bitwise BITB',
		'entity:binance': 'Binance',
		'entity:bitmex': 'BitMEX',
		// Label set v3 (ROADMAP P2a).
		'entity:bybit': 'Bybit',
		'entity:bitfinex': 'Bitfinex',
		'entity:htx': 'HTX',
		'entity:coinex': 'CoinEx',
		'entity:mexc': 'MEXC',
		'entity:crypto-com': 'Crypto.com',
		'entity:deribit': 'Deribit'
	};
	return known[id] ?? id.replace(/^entity:/, '');
}

/** Kind of a labelled company (`LABELS_V2_ENTITIES`; ADR-0031 adds `Custodian`). */
export function entityKind(id: string): 'Exchange' | 'ETF' | 'Custodian' | 'Other' {
	const kinds: Record<string, 'Exchange' | 'ETF' | 'Custodian'> = {
		'entity:okx': 'Exchange',
		'entity:binance': 'Exchange',
		'entity:bitmex': 'Exchange',
		'entity:bitwise-bitb': 'ETF',
		'entity:bybit': 'Exchange',
		'entity:bitfinex': 'Exchange',
		'entity:htx': 'Exchange',
		'entity:coinex': 'Exchange',
		'entity:mexc': 'Exchange',
		'entity:crypto-com': 'Exchange',
		'entity:deribit': 'Exchange'
	};
	return kinds[id] ?? 'Other';
}

/** UTC clock time of a unix timestamp, "22:59". */
export const utc = (t: number) => new Date(t * 1000).toISOString().slice(11, 16);
