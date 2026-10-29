<script lang="ts">
	import { onMount } from 'svelte';
	import Pill, { type PillState } from '#lib/components/Pill.svelte';
	import OnyxSlab from '#lib/components/OnyxSlab.svelte';
	import Skeleton from '#lib/components/Skeleton.svelte';
	import BlockChart from '#lib/components/BlockChart.svelte';
	import FlowChart from '#lib/components/FlowChart.svelte';
	import SeriesChart from '#lib/components/SeriesChart.svelte';
	import TableGroup from '#lib/components/TableGroup.svelte';
	import PoolShares from '#lib/components/PoolShares.svelte';
	import ChartLegend from '#lib/components/ChartLegend.svelte';
	import { COLOR, FLOW_KEYS, NET_KEYS } from '#lib/chart.ts';
	import DataMap from '#lib/components/DataMap.svelte';
	import HeldBack from '#lib/components/HeldBack.svelte';
	import DatasetCatalog from '#lib/components/DatasetCatalog.svelte';
	import TrackRecords from '#lib/components/TrackRecords.svelte';
	import ChainChart from '#lib/components/ChainChart.svelte';
	import { chainCharts } from '#lib/onchain.ts';
	import SignedFact from '#lib/components/SignedFact.svelte';
	import LevelBadge from '#lib/components/LevelBadge.svelte';
	import { loadLadders, type LadderFile } from '#lib/ladder.ts';
	import { dayText } from '#lib/catalog.ts';
	import { FALLBACK } from '#lib/mode.ts';
	import RotateCcw from '@lucide/svelte/icons/rotate-ccw';
	import EyeOff from '@lucide/svelte/icons/eye-off';
	import { DAILY_SOURCES, type DailySource } from '#lib/data.ts';
	import { loadChainInput, loadChainRecipe, loadChainStats, loadDemo, loadHistoryDay, loadHistoryIndex, loadLevel2, loadDaily, loadPools, loadSecHoldings, loadEtfHoldings, whenIdle, type Demo } from '#lib/data.ts';
	import { verifier, type BlockSums, type ChainCheck, type ChainRerun, type HistoryDayCheck, type EdgarCheck, type EtfCheck, type HistoryIndexCheck, type Level2Check, type Pinned, type PoolsCheck, type SeriesSums } from '#lib/verifier.ts';
	import { checkLiveHeaders, entityKind, entityName, entitySeries, liveSeries, liveSums, seriesPoints, utc, type FlowPoint, type Pill as PillT } from '#lib/live.ts';
	import { btc, int, short } from '#lib/format.ts';
	// ADR-0044 (ROADMAP P4a): the Ethereum section.
	import EthChart from '#lib/components/EthChart.svelte';
	import { eth as ethUnits, ethCharts, pct as ethPct } from '#lib/ethchain.ts';
	import EstimateCard from '#lib/components/EstimateCard.svelte';
	import { feeMeanEstimate } from '#lib/estimates.ts';
	import { loadEthChainInput, loadEthChainRecipe, loadEthChainStats } from '#lib/data.ts';
	import type { EthChainCheck, EthChainRerun } from '#lib/verifier.ts';

	let demo = $state.raw<Demo | null>(null);
	let pin = $state.raw<Pinned | null>(null);
	let loadError = $state('');
	let pow = $state<Record<number, PillState>>({});
	let powPill = $state<PillT>({ state: 'idle', text: 'Not checked' });
	// The honest sums, kept for the chart; `shown` is the last check's result.
	let sums = $state.raw<BlockSums | null>(null);
	let shown = $state.raw<BlockSums | null>(null);
	let sumPill = $state<PillT>({ state: 'idle', text: 'Not checked' });
	let omitted = $state<number | null>(null);
	// The series check: honest result for the chart, last result for the panel.
	let series = $state.raw<SeriesSums | null>(null);
	let seriesShown = $state.raw<SeriesSums | null>(null);
	let seriesPill = $state<PillT>({ state: 'idle', text: 'Not checked' });
	let dropped = $state<number | null>(null);
	// One week of history: the checked index, each day's checked leaves (what
	// the charts draw), and the last results for the panel.
	let hist = $state.raw<HistoryIndexCheck | null>(null);
	let histText = '';
	let histDays = $state.raw<Record<string, HistoryDayCheck>>({});
	let histFailed = $state.raw<string[]>([]);
	let histShown = $state.raw<{ index: HistoryIndexCheck; day: HistoryDayCheck | null } | null>(null);
	let histPill = $state<PillT>({ state: 'idle', text: 'Not checked' });
	let histEdited = $state(false);
	let histRange = $state<'week' | 'day'>('week');
	let histDay = $state('');
	let entity = $state('entity:okx');
	// The card renders only the verifier's checked output (L2SRC-R2-F3).
	let l2Check = $state<Level2Check | null>(null);
	let l2Pill = $state<PillT>({ state: 'idle', text: 'Waiting' });
	// NS3: the daily sources' recorded rounds, each checked like the Treasury round.
	let dailyChecks = $state<Partial<Record<DailySource, Level2Check>>>({});
	let dailyPill = $state<PillT>({ state: 'idle', text: 'Waiting' });
	// SEC filings card: also only the verifier's checked output.
	let secCheck = $state<EdgarCheck | null>(null);
	let secPill = $state<PillT>({ state: 'idle', text: 'Waiting' });
	let etfCheck = $state<EtfCheck | null>(null);
	let etfPill = $state<PillT>({ state: 'idle', text: 'Waiting' });
	let zoomed = $state(false);
	// Ethereum window (ADR-0044): `eth` is the honest check the charts draw; `ethShown` is the last
	// check's result for the panel (the tamper demo replaces it, never the charts). Public chain
	// data, no label set: every build checks and draws it.
	let eth = $state.raw<EthChainCheck | null>(null);
	let ethShown = $state.raw<EthChainCheck | null>(null);
	let ethPill = $state<PillT>({ state: 'idle', text: 'Waiting' });
	let ethEdited = $state(false);
	let ethRun = $state.raw<EthChainRerun | null>(null);
	let ethRunPill = $state<PillT>({ state: 'idle', text: 'Not run' });
	let ethRunMs = $state(0);
	const ethSpecs = $derived(eth ? ethCharts(eth) : []);

	onMount(() => whenIdle(() => void checkEth()));

	async function checkEth() {
		ethEdited = false;
		ethPill = { state: 'run', text: 'Checking' };
		try {
			const c = await verifier.ethChainStats(await loadEthChainStats());
			ethShown = c;
			if (c.ok) eth = c;
			ethPill = c.ok ? { state: 'good', text: 'Checks out' } : { state: 'bad', text: 'Does not check out' };
		} catch (e) {
			ethPill = { state: 'bad', text: String(e) };
		}
	}

	/** Edit one digit of one leaf and recheck: the commitment no longer matches. */
	async function tamperEth(edit: boolean) {
		if (!edit) return checkEth();
		ethEdited = true;
		try {
			const j = JSON.parse(await loadEthChainStats());
			const leaves: string[] = j.days[0].leaves;
			const k = Math.floor(leaves.length / 2);
			// The last hex digit is in the value-sent field: the row still decodes.
			leaves[k] = leaves[k].slice(0, -1) + (parseInt(leaves[k].slice(-1), 16) ^ 1).toString(16);
			const c = await verifier.ethChainStats(JSON.stringify(j));
			ethShown = c;
			ethPill = c.ok ? { state: 'good', text: 'Checks out' } : { state: 'bad', text: 'Does not check out' };
		} catch (e) {
			ethPill = { state: 'bad', text: String(e) };
		}
	}

	/** Rerun the recipe here on one block's full input (canonical engine, validator's fuel rules). */
	async function rerunEth() {
		ethRunPill = { state: 'run', text: 'Running' };
		try {
			const [wasm, input, doc] = await Promise.all([loadEthChainRecipe(), loadEthChainInput(), loadEthChainStats()]);
			const t = performance.now();
			const r = await verifier.ethChainRerun(wasm, input, doc);
			ethRunMs = performance.now() - t;
			ethRun = r;
			ethRunPill = r.ok ? { state: 'good', text: 'Same root' } : { state: 'bad', text: r.problems[0] ?? 'Different root' };
		} catch (e) {
			ethRunPill = { state: 'bad', text: String(e) };
		}
	}
	// Pool shares: only the verifier's checked leaves are drawn (ADR-0032).
	let poolCheck = $state<PoolsCheck | null>(null);
	let poolPill = $state<PillT>({ state: 'idle', text: 'Waiting' });
	// On-chain window (ADR-0043): `chain` is the honest check the charts draw; `chainShown` is
	// the last check's result for the panel (the tamper demo replaces it, never the charts).
	let chain = $state.raw<ChainCheck | null>(null);
	let chainShown = $state.raw<ChainCheck | null>(null);
	let chainPill = $state<PillT>({ state: 'idle', text: 'Waiting' });
	let chainEdited = $state(false);
	let chainRun = $state.raw<ChainRerun | null>(null);
	let chainRunPill = $state<PillT>({ state: 'idle', text: 'Not run' });
	let chainRunMs = $state(0);

	// NS5 (ADR-0048): the Base Sepolia values' level and its public history.
	let ladderFile = $state<LadderFile | null>(null);
	let ladderError = $state('');
	const LADDER_LABEL: Record<string, string> = { usgs: 'Earthquakes', ncei: 'Denver high' };

	onMount(() => {
		loadLadders()
			.then((f) => (ladderFile = f))
			.catch((e) => (ladderError = String(e)));
		loadDemo()
			.then((d) => {
				demo = d;
				whenIdle(() => {
					verifier
						.pinned()
						.then(async (p) => {
							pin = p;
							// A rights-fallback build ships no company rows or history (ROADMAP L14).
							await Promise.all(FALLBACK ? [checkPow(), checkChain(), checkPools(), checkLevel2(), checkDaily(), checkSec(), checkEtf()] : [checkSums(null), checkSeries(null), checkPow(), checkChain(), checkHistory(), checkPools(), checkLevel2(), checkDaily(), checkSec(), checkEtf()]);
						})
						.catch((e) => (loadError = String(e)));
				});
			})
			.catch((e) => (loadError = String(e)));
	});

	async function checkChain() {
		chainEdited = false;
		chainPill = { state: 'run', text: 'Checking' };
		try {
			const c = await verifier.chainStats(await loadChainStats());
			chainShown = c;
			if (c.ok) chain = c;
			chainPill = c.ok ? { state: 'good', text: 'Checks out' } : { state: 'bad', text: 'Does not check out' };
		} catch (e) {
			chainPill = { state: 'bad', text: String(e) };
		}
	}

	/** Edit one digit of one leaf and recheck: the commitment no longer matches. */
	async function tamperChain(edit: boolean) {
		if (!edit) return checkChain();
		chainEdited = true;
		try {
			const j = JSON.parse(await loadChainStats());
			const day = j.days[Math.floor(j.days.length / 2)];
			const leaf: string = day.leaves[0];
			// The last hex digit is in the coin-blocks-destroyed field: the row still decodes.
			day.leaves[0] = leaf.slice(0, -1) + (parseInt(leaf.slice(-1), 16) ^ 1).toString(16);
			const c = await verifier.chainStats(JSON.stringify(j));
			chainShown = c;
			chainPill = c.ok ? { state: 'good', text: 'Checks out' } : { state: 'bad', text: 'Does not check out' };
		} catch (e) {
			chainPill = { state: 'bad', text: String(e) };
		}
	}

	/** Rerun the recipe here on one block's full input (canonical engine, validator's fuel rules). */
	async function rerunChain() {
		chainRunPill = { state: 'run', text: 'Running' };
		try {
			const [wasm, input, doc] = await Promise.all([loadChainRecipe(), loadChainInput(), loadChainStats()]);
			const t = performance.now();
			const r = await verifier.chainRerun(wasm, input, doc);
			chainRunMs = performance.now() - t;
			chainRun = r;
			chainRunPill = r.ok ? { state: 'good', text: 'Same root' } : { state: 'bad', text: r.problems[0] ?? 'Different root' };
		} catch (e) {
			chainRunPill = { state: 'bad', text: String(e) };
		}
	}

	// Owner review: a chart's one-line summary reads in 2 decimals ("4,250.00 BTC"); the chart's own data table keeps every satoshi.
	const chainSpecs = $derived(chain ? chainCharts(chain).map((c) => ({ ...c, summary: c.summary.replace(/(\.\d{2})\d{6}(?!\d)/g, '$1') })) : []);

	async function checkPools() {
		poolPill = { state: 'run', text: 'Checking' };
		try {
			const c = await verifier.pools(await loadPools());
			poolCheck = c;
			poolPill = c.ok ? { state: 'good', text: 'Checks out' } : { state: 'bad', text: c.problems[0] ?? 'Does not check out' };
		} catch (e) {
			poolPill = { state: 'bad', text: String(e) };
		}
	}

	/** Pools by week share from the checked totals, unknown last. */
	const poolRank = $derived(
		poolCheck?.ok
			? Object.entries(poolCheck.totals)
					.sort((a, b) => (a[0] === 'unknown' ? 1 : b[0] === 'unknown' ? -1 : b[1] - a[1] || a[0].localeCompare(b[0])))
			: []
	);
	const poolTotal = $derived(poolRank.reduce((a, [, n]) => a + n, 0));
	const poolNamed = $derived(poolRank.filter(([p]) => p !== 'unknown').slice(0, 6).map(([p]) => p));

	async function checkLevel2() {
		l2Pill = { state: 'run', text: 'Checking' };
		try {
			const text = await loadLevel2();
			const c = await verifier.level2Round(text);
			l2Check = c;
			l2Pill = c.ok ? { state: 'good', text: 'Checks out' } : { state: 'bad', text: c.problems[0] ?? 'Does not check out' };
		} catch (e) {
			l2Pill = { state: 'bad', text: String(e) };
		}
	}

	async function checkDaily() {
		dailyPill = { state: 'run', text: 'Checking' };
		try {
			const sources = Object.keys(DAILY_SOURCES) as DailySource[];
			const out = await Promise.all(sources.map(async (s) => [s, await verifier.dailyRound(s, await loadDaily(s))] as const));
			dailyChecks = Object.fromEntries(out);
			const bad = out.find(([, c]) => !c.ok);
			dailyPill = bad ? { state: 'bad', text: bad[1].problems[0] ?? 'Does not check out' } : { state: 'good', text: 'Checks out' };
		} catch (e) {
			dailyPill = { state: 'bad', text: String(e) };
		}
	}

	async function checkSec() {
		secPill = { state: 'run', text: 'Checking' };
		try {
			const c = await verifier.edgarRounds(await loadSecHoldings());
			secCheck = c;
			secPill = c.ok ? { state: 'good', text: 'Checks out' } : { state: 'bad', text: c.problems[0] ?? 'Does not check out' };
		} catch (e) {
			secPill = { state: 'bad', text: String(e) };
		}
	}

	// ROADMAP P2c (THREATS ETF-4): the verifier recomputes the whole ETF file from the recorded
	// filings compiled into it; the page shows only the summary it returns, never the shipped file.
	async function checkEtf() {
		etfPill = { state: 'run', text: 'Checking' };
		try {
			const c = await verifier.etfHoldings(await loadEtfHoldings());
			etfCheck = c;
			etfPill = c.ok ? { state: 'good', text: 'Checks out' } : { state: 'bad', text: c.problems[0] ?? 'Does not check out' };
		} catch (e) {
			etfPill = { state: 'bad', text: String(e) };
		}
	}

	/** A filed decimal "138505.756804" -> "138,505.756804"; a signed BTC change keeps its sign. */
	function dec(v: string): string {
		const sign = v.startsWith('-') || v.startsWith('+') ? v[0] : '';
		const [i, f] = v.slice(sign.length).split('.');
		const t = f ? f.replace(/0+$/, '') : '';
		return sign + i.replace(/\B(?=(\d{3})+(?!\d))/g, ',') + (t ? '.' + t : '');
	}

	/** Copy from the checked ETF file, said plainly: internal plan codes become "planned", and the
	 *  note's clause about the SEC's User-Agent rule is dropped (display only; the file is unchanged). */
	const plain = (t: string) =>
		t
			.replace(/;[^;]*refuses Onyx's User-Agent.*$/s, '.')
			.replace(/the pinned fetcher/g, 'the fetch code, fixed by its hash,')
			.replace(/\s*\((?:ROADMAP|THREATS) [^)]*\)/g, ' (planned)');

	/** "846000" -> "846,000" (string math, no floats). */
	const grouped = (digits: string) => digits.replace(/\B(?=(\d{3})+(?!\d))/g, ',');

	const stamp = (t: number) => new Date(t * 1000).toISOString().slice(0, 16).replace('T', ' ');
	/** UTC date of a unix timestamp, "2026-10-11". */
	const day = (t: number) => new Date(t * 1000).toISOString().slice(0, 10);
	/** A unix time as prose: "4 Oct 2026" and "4 Oct 2026, 12:21 UTC". */
	const dayWords = (t: number) => dayText(day(t));
	const stampWords = (t: number) => `${dayWords(t)}, ${utc(t)} UTC`;
	/** A filed ISO date in prose ("2026-10-08" -> "8 Oct 2026"). */
	const isoWords = (iso: string) => dayText(iso);

	/** "40305316210829.72" -> "40,305,316,210,829.72" (string math, no floats). */
	function usd(v: string): string {
		const [i, f] = v.split('.');
		return i.replace(/\B(?=(\d{3})+(?!\d))/g, ',') + (f ? '.' + f : '');
	}

	async function checkPow() {
		if (!demo || !pin) return;
		powPill = { state: 'run', text: 'Checking' };
		powPill = await checkLiveHeaders(demo, pin, (h, s) => (pow[h] = s));
	}

	async function checkSums(omit: number | null) {
		if (!demo) return;
		omitted = omit;
		sumPill = { state: 'run', text: 'Checking' };
		try {
			const s = await liveSums(demo, omit ?? undefined);
			shown = s;
			if (omit === null && s.ok) sums = s;
			sumPill = s.ok ? { state: 'good', text: 'Adds up' } : { state: 'bad', text: 'Does not add up' };
		} catch (e) {
			sumPill = { state: 'bad', text: e instanceof Error ? e.message : String(e) };
		}
	}

	async function checkSeries(drop: number | null) {
		if (!demo) return;
		dropped = drop;
		seriesPill = { state: 'run', text: 'Checking' };
		try {
			const s = await liveSeries(demo, drop ?? undefined);
			seriesShown = s;
			if (drop === null && s.ok) series = s;
			seriesPill = s.ok ? { state: 'good', text: 'Adds up' } : { state: 'bad', text: 'Does not add up' };
		} catch (e) {
			seriesPill = { state: 'bad', text: e instanceof Error ? e.message : String(e) };
		}
	}

	// Check the week: the index first (headers, shards, the pinned commitment),
	// then each day's leaves, newest day first. A day is drawn only once its
	// leaves check out against the index.
	async function checkHistory() {
		histEdited = false;
		histPill = { state: 'run', text: 'Checking' };
		try {
			histText = await loadHistoryIndex();
			const r = await verifier.historyIndex(histText);
			histShown = { index: r, day: null };
			if (!r.ok) {
				histPill = { state: 'bad', text: 'Does not check out' };
				return;
			}
			hist = r;
			histDay ||= r.days[r.days.length - 1]?.date ?? '';
			let bad = false;
			// W4: the days are checked one by one, but the charts are drawn once, from all the
			// checked days together (drawing them after each day was 7 renders of every chart).
			const checked: Record<string, HistoryDayCheck> = { ...histDays };
			const failed = [...histFailed];
			let last: HistoryDayCheck | null = null;
			for (const d of [...r.days].reverse()) {
				// HIST-W2-F1: each day is loaded and checked on its own. A missing,
				// truncated or refused file names that day as failed; the rest go on.
				try {
					const c = await verifier.historyDay(histText, d.date, await loadHistoryDay(d.file));
					// HIST-W1-F1: a day counts only for its own slot (date and heights).
					if (c.ok && c.date === d.date && c.from === d.from && c.to === d.to) checked[d.date] = c;
					else {
						bad = true;
						failed.push(d.date);
					}
					last = c;
				} catch {
					bad = true;
					failed.push(d.date);
				}
				histPill = { state: 'run', text: `${Object.keys(checked).length} of ${r.days.length} days checked` };
			}
			histDays = checked;
			histFailed = failed;
			if (last) histShown = { index: r, day: last };
			histPill = bad ? { state: 'bad', text: 'Does not check out' } : { state: 'good', text: 'Checks out' };
		} catch (e) {
			histPill = { state: 'bad', text: e instanceof Error ? e.message : String(e) };
		}
	}

	// The tamper demo: flip one bit in one leaf of the shown day (a number in a
	// row, still a valid row) and check that day again. The charts keep the
	// honest results.
	async function tamperHistory(edit: boolean) {
		if (!hist) return;
		histEdited = edit;
		const d = hist.days.find((x) => x.date === histDay) ?? hist.days[hist.days.length - 1];
		try {
			let t = await loadHistoryDay(d.file);
			if (edit) {
				const j = JSON.parse(t);
				const sh = j.shards.find((x: { series_leaves: string[] }) => x.series_leaves.length > 1);
				const leaf: string = sh.series_leaves[0];
				sh.series_leaves[0] = leaf.slice(0, -1) + (parseInt(leaf.slice(-1), 16) ^ 1).toString(16);
				t = JSON.stringify(j);
			}
			const c = await verifier.historyDay(histText, d.date, t);
			histShown = { index: hist, day: c };
			// HIST-W2-F2: "Checks out" only while no day of the week has failed.
			histPill = c.ok && histFailed.length === 0 ? { state: 'good', text: 'Checks out' } : { state: 'bad', text: 'Does not check out' };
		} catch (e) {
			histPill = { state: 'bad', text: e instanceof Error ? e.message : String(e) };
		}
	}

	const histTimes = $derived(Object.fromEntries((hist?.blocks ?? []).filter((b) => b.time !== null).map((b) => [b.height, b.time as number])));
	const histChecked = $derived((hist?.days ?? []).filter((d) => histDays[d.date]));
	const histPending = $derived((hist?.days.length ?? 0) - histChecked.length - histFailed.length);
	const histDayInfo = $derived(hist?.days.find((d) => d.date === histDay) ?? null);
	// Week view: one point per six-block bucket, only from checked days, and
	// only the buckets recomputed from each day's checked rows (never the
	// index's own numbers, which the pinned commitment does not bind).
	const bucketEnds = $derived(Object.fromEntries(histChecked.flatMap((d) => histDays[d.date].buckets.map((k) => [k.from, k.to]))));
	function weekPoints(entity: string): FlowPoint[] {
		return histChecked.flatMap((d) =>
			histDays[d.date].buckets.map((k) => {
				const e = k.entities.find((x) => x.entity === entity);
				return {
					height: k.from,
					received: BigInt(e?.received_sats ?? 0),
					sent: BigInt(e?.sent_sats ?? 0),
					net: BigInt(e?.net_sats ?? 0),
					txs: e?.tx_count ?? 0
				};
			})
		);
	}
	// Companies with flow in the shown range, biggest first, with their net over it.
	const histActive = $derived.by(() => {
		if (!hist) return [];
		const names = new Set<string>();
		for (const d of histChecked) for (const e of histDays[d.date].entities) names.add(e.entity);
		return [...names]
			.map((entity) => {
				const pts =
					histRange === 'week'
						? weekPoints(entity)
						: histDayInfo && histDays[histDay]
							? seriesPoints(histDays[histDay], entity, histDayInfo.from, histDayInfo.to)
							: [];
				let net = 0n;
				let vol = 0n;
				let active = 0;
				for (const p of pts) {
					net += p.net;
					vol += p.received + p.sent;
					if (p.txs > 0) active++;
				}
				return { entity, pts, active, net, vol };
			})
			.filter((e) => e.active > 0)
			.sort((a, b) => (b.vol > a.vol ? 1 : b.vol < a.vol ? -1 : 0));
	});
	// "All labelled exchanges": the sum of every labelled exchange's checked
	// points, bucket by bucket (or block by block), from the same rows the
	// per-company charts use. Net sums exactly (a transfer between two labelled
	// exchanges cancels); transaction counts do not (one transaction can touch
	// two companies), so the line shows no count (IDX-R1-F4).
	const histExchanges = $derived.by(() => {
		const ex = histActive.filter((e) => entityKind(e.entity) === 'Exchange');
		if (ex.length < 2) return null;
		const at = new Map<number, FlowPoint>();
		for (const e of ex)
			for (const p of e.pts) {
				const q = at.get(p.height) ?? { height: p.height, received: 0n, sent: 0n, net: 0n, txs: 0 };
				at.set(p.height, { height: p.height, received: q.received + p.received, sent: q.sent + p.sent, net: q.net + p.net, txs: q.txs + p.txs });
			}
		const pts = [...at.values()].sort((a, b) => a.height - b.height);
		let net = 0n;
		for (const p of pts) net += p.net;
		return { names: ex.map((e) => entityName(e.entity)), pts, net };
	});
	const HIST_TOP = 4;
	let histShowAll = $state(false);
	const histShownRows = $derived(histShowAll ? histActive : histActive.slice(0, HIST_TOP));
	const histNames = $derived(new Set(histChecked.flatMap((d) => histDays[d.date].entities.map((e) => e.entity))).size);
	const histQuiet = $derived(histNames - histActive.length);
	const dayLabel = (t: number, year = false) => (year ? dayWords(t) : dayWords(t).replace(/ \d{4}$/, ''));
	const histSpan = $derived.by(() => {
		if (!hist) return '';
		const [a, b] =
			histRange === 'week' || !histDayInfo
				? [histChecked[0]?.first_time, histChecked[histChecked.length - 1]?.last_time]
				: [histDayInfo.first_time, histDayInfo.last_time];
		return a && b ? `${stampWords(a)} – ${stampWords(b)}` : '';
	});
	// HIST-W2-F3: the week view names the days it actually draws.
	const histWhole = $derived(!!hist && histChecked.length === hist.days.length);
	const histShownFrom = $derived(histRange === 'week' || !histDayInfo ? (histChecked[0]?.from ?? hist?.from ?? 0) : histDayInfo.from);
	const histShownTo = $derived(
		histRange === 'week' || !histDayInfo ? (histChecked[histChecked.length - 1]?.to ?? hist?.to ?? 0) : histDayInfo.to
	);
	// "One week: 1,008 blocks, 4–10 Oct 2026" from the checked headers.
	const histTitle = $derived.by(() => {
		if (!hist) return 'One week of Bitcoin';
		const a = hist.days[0]?.first_time;
		const b = hist.days[hist.days.length - 1]?.last_time;
		const sameMonth = a && b && dayLabel(a).split(' ')[1] === dayLabel(b).split(' ')[1];
		const span = a && b ? (sameMonth ? `, ${dayLabel(a).split(' ')[0]}–${dayLabel(b, true)}` : `, ${dayLabel(a)} – ${dayLabel(b, true)}`) : '';
		const whole = `One week: ${int(hist.to - hist.from + 1)} blocks${span}`;
		return histWhole ? whole : `${whole} (${histChecked.length} of ${hist.days.length} days checked)`;
	});
	const histRows = $derived(Object.values(histDays).reduce((a, d) => a + d.rows, 0));

	const live = $derived(demo?.live ?? null);
	const seriesRows = $derived(live?.series?.rows ?? []);
	// The tamper demo hides the row with the biggest net flow.
	const dropTarget = $derived.by(() => {
		let best = -1;
		let size = -1n;
		seriesRows.forEach((r, i) => {
			const v = BigInt(r.net_sats);
			const a = v < 0n ? -v : v;
			if (a > size) {
				size = a;
				best = i;
			}
		});
		return best < 0 ? null : best;
	});
	const seriesActive = $derived((series?.entities ?? []).filter((e) => e.points.length > 0));
	const seriesQuiet = $derived((series?.entities.length ?? 0) - seriesActive.length);
	const blocks = $derived(live?.blocks ?? []);
	const totalTx = $derived(blocks.reduce((a, b) => a + b.txs, 0));
	const totalOut = $derived(blocks.reduce((a, b) => a + BigInt(b.output_sats ?? 0), 0n));
	const medianGap = $derived.by(() => {
		const g = blocks.slice(1).map((b, i) => ((b.time ?? 0) - (blocks[i].time ?? 0)) / 60).sort((a, b) => a - b);
		return g.length ? g[Math.floor(g.length / 2)] : null;
	});
	const entities = $derived(sums?.entities ?? []);
	// Companies with any flow in the range, biggest first; the rest are labelled but quiet.
	const volume = (r: { received_sats: string; sent_sats: string }) => BigInt(r.received_sats) + BigInt(r.sent_sats);
	const active = $derived(
		(live?.rows ?? []).filter((r) => volume(r) > 0n).sort((a, b) => (volume(b) > volume(a) ? 1 : volume(b) < volume(a) ? -1 : 0))
	);
	const quiet = $derived((live?.rows.length ?? 0) - active.length);
	const row = $derived(entities.find((e) => e.entity === entity) ?? null);
	const points = $derived(sums ? entitySeries(sums, entity) : []);
	// The block the tamper demo leaves out: the one with the most flow.
	const biggest = $derived.by(() => {
		if (!points.length) return null;
		return points.reduce((a, p) => (p.received + p.sent > a.received + a.sent ? p : a)).height;
	});
	const span = $derived(live && blocks[0]?.time && blocks[blocks.length - 1]?.time ? `${dayWords(blocks[0].time!)} · ${utc(blocks[0].time!)}–${utc(blocks[blocks.length - 1].time!)} UTC` : '');
</script>

<svelte:head><title>Browse data · Onyx</title></svelte:head>

<OnyxSlab>
	<h1>Browse data</h1>
	<div class="slabside" data-testid="slabside">
		{#if live}
			<p class="meta slabmeta">Live sample: Bitcoin mainnet · <span class="nw">blocks {int(live.from_height)}–{int(live.to_height)}</span> · <span class="nw">{span}</span></p>
		{:else}
			<p class="meta slabmeta">Live sample: Bitcoin mainnet</p>
		{/if}
	</div>
</OnyxSlab>

<div class="wrap">
	{#if loadError}
		<p class="section">Could not load the data: {loadError}</p>
	{:else if !demo}
		<Skeleton rows={4} label="Loading the live blocks" />
	{:else if !live}
		<p class="section muted">This build has no live range.</p>
	{:else}
		<!-- PRODUCT-PLAN.md U5 (PP4): the datasets first; each card opens its section below. -->
		<DatasetCatalog />
		<section class="section" data-testid="kinds">
			<div class="head">
				<h2>Every kind of data</h2>
				<p class="muted">Placed by the strongest proof it can carry. Live means running in code today.</p>
			</div>
			<div class="panel mapbox"><DataMap /></div>
		</section>
		<section class="section statlead" data-testid="blocks">
			<div class="sh">
				<h2>Bitcoin, block by block</h2>
				<Pill live state={powPill.state} text={powPill.text} />
			</div>
			<div class="stats">
				<div class="stat"><b>{blocks.length}</b><span>blocks</span></div>
				<div class="stat"><b>{int(totalTx)}</b><span>transactions</span></div>
				<div class="stat"><b>{btc(totalOut).split('.')[0]}</b><span>BTC in outputs</span></div>
				<div class="stat"><b>{medianGap === null ? '–' : medianGap.toFixed(1)}</b><span>median minutes apart</span></div>
			</div>
			<div class="panel chartbox">
				<BlockChart {blocks} {pow} />
				<div class="pfoot"><p class="fine">Read from our Bitcoin node. Each header is rechecked here: hash, proof of work, link to the one before; a green dot marks a header that passed. An orange bar below zero is a block whose time stamp is earlier than the block before it, which Bitcoin allows within limits.</p></div>
			</div>
			
		</section>

		<!-- ADR-0043 (ROADMAP P3): Bitcoin metrics computed by Onyx's own recipe from public chain data only, so every
		     build shows them (no label set, no price). Every figure is a field of the verifier's checked window. -->
		<section class="section" data-testid="onchain">
			<div class="sh">
				<h2>Bitcoin on chain</h2>
				<Pill live state={chainPill.state} text={chainPill.text} />
			</div>
			<p class="muted lede">
				Computed by Onyx's Bitcoin statistics recipe over every block of the window, one shard per block, from the blocks
				alone. No label set and no price goes in, so anyone who rebuilds the input from their own node reruns every number (level 1). Amounts
				are whole satoshis and fee rates whole thousandths of a sat/vB, as the recipe commits them.
			</p>
			{#if chain}
				<div class="stats" data-testid="onchain-stats">
					<div class="stat"><b>{int(chain.window.blocks)}</b><span>blocks, {int(chain.from)}–{int(chain.to)}</span></div>
					<div class="stat"><b>{int(chain.window.txs)}</b><span>transactions</span></div>
					<div class="stat"><b>{btc(chain.window.fees_sats).replace(/(\.\d{2})\d+$/, '$1')}</b><span>BTC in fees</span></div>
					<div class="stat"><b>{btc(chain.window.issued_sats).replace(/(\.\d{2})\d+$/, '$1')}</b><span>BTC new supply</span></div>
				</div>
				<TableGroup testid="onchain-tables">
				{#snippet children(tables)}
				<div class="panel multiples">
					<ChartLegend items={[]} unit="one scale per chart" testid="onchain-legend">
						{@render tables()}
					</ChartLegend>
					<p class="meta mono" data-testid="onchain-span">
						<span class="nw">Blocks {int(chain!.from)}–{int(chain!.to)}</span> · <span class="nw">{stampWords(chain!.window.first_time)}</span> to <span class="nw">{stampWords(chain!.window.last_time)}</span>
					</p>
					<p class="fine">Bars: one per UTC day (the first and last days are partial). Lines: one point per block.</p>
					{#each chainSpecs as c (c.id)}
						<div class="mrow" data-testid="onchain-{c.id}">
							<div class="mname">
								<b>{c.title}</b>
								<span class="mono">{c.summary}</span>
							</div>
							<ChainChart points={c.points} name={c.title} unit={c.unit} mode={c.mode} step={c.step} tick={c.tick} testid="onchain-chart-{c.id}" />
						</div>
					{/each}
					<div class="pfoot duo">
						<p class="fine" data-testid="onchain-source">
							Every chart: computed by Onyx's Bitcoin statistics recipe from
							<span class="nw">blocks {int(chain!.from)}–{int(chain!.to)}</span>, read from our Bitcoin node.
							An independent recount from the node's own JSON agrees on every field of every block.
							Fees, fee rates, coin-blocks destroyed, Taproot spends and active scripts also read each spent output's value, script and the height
							that created it, which the node supplies from earlier blocks: the rerun here takes them as shipped, a rerun from your own node checks them.
						</p>
						<p class="fine" data-testid="onchain-planned">
							Not here yet, and why: daily active addresses (a count across a whole day needs a shard that holds the day), realized cap,
							MVRV and SOPR (they need a price for every block, and no licensed price source is in Onyx yet), and exchange and ETF
							flows (they need the company labels; planned). This window is {int(chain!.window.blocks)} blocks; the full history to its tip is
							{int(chain!.to + 1)} blocks and needs an unpruned node.
						</p>
					</div>
				</div>
				{/snippet}
				</TableGroup>
			{:else if chainPill.state === 'bad' && !chainEdited}
				<p class="panel muted" data-testid="onchain-failed">The on-chain window did not pass the check, so nothing is drawn.</p>
			{:else}
				<div class="panel"><Skeleton rows={3} label="Checking the on-chain window" /></div>
			{/if}

			<div class="panel check" data-testid="onchain-check">
				<div class="sh">
					<div class="ct">
						<b class="ptitle">Does the window check out?</b>
						<span class="muted"
							>Your browser decodes every block's leaf, checks the heights run without a gap to the pinned tip and each block's interval links to
							the one before, and recomputes the commitment this page pins from Onyx's own run. Then it can rerun the recipe itself on one block's
							full input: the same engine and fuel rules a validator uses.</span
						>
					</div>
					<Pill live state={chainPill.state} text={chainPill.text} />
				</div>
				{#if chainShown}
					<div class="eq mono" data-testid="onchain-eq">
						<span class="eqp"><span>{int(chainShown.blocks.length)} leaves</span><span class="op">·</span></span>
						<span>{chainShown.days.length} days</span>
						<span class="eqp"><span class="op">→</span><span title={chainShown.commitment}>commitment {short(chainShown.commitment)}</span></span>
						<span class="res" class:ok={chainShown.commitment_ok}>{chainShown.commitment_ok ? 'pinned' : 'not pinned'}</span>
					</div>
					{#if !chainShown.ok}
						<ul class="problems mono" data-testid="onchain-problems">
							{#each chainShown.problems.slice(0, 3) as p (p)}<li>{p}</li>{/each}
						</ul>
					{/if}
				{/if}
				{#if chainRun}
					<div class="eq mono" data-testid="onchain-rerun">
						<span class="eqp"><span>block {int(chainRun.height)}, {int(chainRun.txs_in_input)} transactions</span><span class="op">·</span></span>
						<span>{int(chainRun.fuel)} fuel, {(chainRunMs / 1000).toFixed(1)} s here</span>
						<span class="eqp"><span class="op">→</span><span title={chainRun.root}>root {short(chainRun.root)}</span></span>
						<span class="res" class:ok={chainRun.ok}>{chainRun.ok ? 'same as the window' : 'different'}</span>
					</div>
				{/if}
				<div class="row">
					<button type="button" class="btn" data-testid="onchain-rerun-btn" disabled={!chain || chainRunPill.state === 'run'} onclick={rerunChain}
						>Rerun block {pin ? int(pin.chain.rerun_height) : ''} here</button
					>
					<span class="runstat {chainRunPill.state}"><i aria-hidden="true"></i><span>{chainRunPill.text}</span></span>
					{#if !chainEdited}
						<button type="button" class="btn ghost" disabled={!chain} onclick={() => tamperChain(true)}><EyeOff size={16} />Change one number</button>
					{:else}
						<button type="button" class="btn ghost" onclick={() => tamperChain(false)}><RotateCcw size={16} />Put it back</button>
					{/if}
				</div>
			</div>
			<details class="more repro" data-testid="onchain-repro">
				<summary>Reproduce it yourself</summary>
				{#if chain}<p class="fine" data-testid="onchain-recipe">The recipe: <span class="mono">btc-chain-stats</span>, id <span class="mono" title={chain.recipe_id}>{short(chain.recipe_id)}</span>.</p>{/if}
				<p class="fine">With your own Bitcoin node: fetch the blocks, export the window, then run the golden test.</p>
				<pre class="code cmds" data-testid="onchain-rerun-cmd">scripts/fetch-btc-node.sh
# or: scripts/refresh-onchain.py --rest &lt;your node&gt;
cargo run --release -p onyx-source-btc --example export-chain-stats -- &lt;out&gt; &lt;blocks&gt;
ONYX_CHAIN_DIRS=… cargo test -p onyx-source-btc --test golden_chain
# independent recount from the node's JSON:
scripts/recount-chain-stats.py</pre>
			</details>
		</section>

		<!-- NS14 (ADR-0050): estimates, apart from every checked value. Made here from the checked window's
		     whole days; shown with method and error, no proof level, never in a total. -->
		{#if chainShown}
			{@const est = feeMeanEstimate(chainShown.days)}
			{#if est}
				<section class="section" data-testid="estimates">
					<details class="secfold" data-testid="estimates-fold">
						<summary class="sh"><h2>Estimates</h2><p class="foldnote">Values Onyx did not check, such as averages: kept apart from checked data, always with their method and error.</p></summary>
						<p class="muted lede">
							An estimate is a value Onyx did not check: a model, an average, an interpolation. It lives under its own
							<span class="mono nw">estimate.</span> names, always shows its method and its error, never gets a proof level,
							and is never added into a checked total. The query API serves estimates at <span class="mono nw">/v1/estimates</span> only.
						</p>
						<div class="panel">
							<EstimateCard e={est} />
						</div>
					</details>
				</section>
			{/if}
		{/if}

		{#if FALLBACK}
			<HeldBack
				title="Company flows"
				what="Each company's flows per block, its flows over time and the week of per-company history are left out of this build."
				testid="flows-held-back"
			/>
		{:else}
		<section class="section" data-testid="flows">
			<div class="sh">
				<h2>Company flows, per block</h2>
				<div class="seg multi" role="group" aria-label="Company">
					{#each active as r (r.entity)}
						<button type="button" class="chipbtn" aria-pressed={entity === r.entity} onclick={() => (entity = r.entity)}>{entityName(r.entity)}</button>
					{/each}
				</div>
			</div>
			{#if quiet > 0}
				<p class="fine quiet">{quiet} more labelled {quiet === 1 ? 'company' : 'companies'} had no flows in these blocks.</p>
			{/if}
			{#if !sums && sumPill.state === 'bad' && omitted === null}
				<p class="panel muted">The per-block rows did not pass the check, so nothing is drawn.</p>
			{:else if !sums}
				<Skeleton rows={2} label="Checking the per-block rows" />
			{:else}
				<div class="stats">
					<div class="stat"><b>{row ? btc(row.received_sats).replace(/(\.\d{2})\d+$/, '$1') : '–'}</b><span>BTC in</span></div>
					<div class="stat"><b class="out">{row ? btc(row.sent_sats).replace(/(\.\d{2})\d+$/, '$1') : '–'}</b><span>BTC out</span></div>
					<div class="stat"><b>{row ? btc(row.net_sats).replace(/(\.\d{2})\d+$/, '$1') : '–'}</b><span>BTC net</span></div>
					<div class="stat"><b>{row ? int(row.tx_count) : '–'}</b><span>transactions</span></div>
				</div>
				<div class="panel chartbox">
					{#if row && row.tx_count === 0}
						<p class="chart-empty">No flows for {entityName(entity)} in these {blocks.length} blocks.</p>
					{:else}
						<div class="ctl">
							<ChartLegend items={FLOW_KEYS} unit="BTC" testid="flow-legend">
								<div class="seg" role="group" aria-label="Scale">
									<button type="button" class="chipbtn" aria-pressed={!zoomed} onclick={() => (zoomed = false)}>Full scale</button>
									<button type="button" class="chipbtn" aria-pressed={zoomed} onclick={() => (zoomed = true)}>Up to 10 BTC</button>
								</div>
							</ChartLegend>
						</div>
						<FlowChart {points} zoom={zoomed ? 10 : null} />
					{/if}
				</div>
			{/if}

			<div class="panel check" data-testid="sums">
				<div class="sh">
					<div class="ct">
						<b class="ptitle">Do the blocks add up?</b>
						<span class="muted">Each block's rows, summed, must equal the range row under the pinned root.</span>
					</div>
					<Pill live state={sumPill.state} text={sumPill.text} />
				</div>
				<div class="eq mono">
					<span>{omitted === null ? blocks.length : blocks.length - 1} block rows</span>
					<span class="eqp"><span class="op">Σ</span><span>range row</span></span>
					<span class="eqp"><span class="op">→</span><span title={live.root}>root {short(live.root)}</span></span>
					{#if shown}<span class="res" class:ok={shown.root_ok}>{shown.root_ok ? 'pinned' : 'not pinned'}</span>{/if}
				</div>
				{#if shown && !shown.ok}
					<ul class="problems mono">
						{#each shown.problems.slice(0, 3) as p (p)}<li>{p}</li>{/each}
					</ul>
				{/if}
				<div class="row">
					{#if omitted === null}
						<button type="button" class="btn ghost" disabled={!sums || biggest === null} onclick={() => checkSums(biggest)}>
							<EyeOff size={16} />Leave out block {biggest === null ? '' : int(biggest)}
						</button>
					{:else}
						<button type="button" class="btn ghost" onclick={() => checkSums(null)}><RotateCcw size={16} />Put it back</button>
					{/if}
				</div>
			</div>
			
		</section>

		{#if live.series}
			<section class="section" data-testid="series">
				<div class="sh">
					<h2>Flows over time</h2>
					<Pill live state={seriesPill.state} text={seriesPill.text} />
				</div>
				<p class="muted lede">
					One Onyx recipe gives each company's net flow in every block, under one root anyone can
					rerun. {int(live.blocks.length)} blocks today; longer history needs more blocks fetched from our node.
				</p>
				{#if !series && seriesPill.state === 'bad' && dropped === null}
					<p class="panel muted">The series did not pass the check, so nothing is drawn.</p>
				{:else if !series}
					<Skeleton rows={2} label="Checking the series" />
				{:else}
					<TableGroup testid="series-tables">
					{#snippet children(tables)}
					<div class="panel multiples">
						<ChartLegend items={NET_KEYS} unit="BTC, one scale per company" testid="series-legend">{@render tables()}</ChartLegend>
						{#each seriesActive as e (e.entity)}
							<div class="mrow" data-testid="series-row">
								<div class="mname">
									<b>{entityName(e.entity)}</b>
									<span class="mono">{btc(e.total.net_sats).replace(/(\.\d{2})\d+$/, '$1')} BTC net · {e.points.length} active blocks</span>
								</div>
								<SeriesChart name={entityName(e.entity)} points={seriesPoints(series!, e.entity, live.from_height, live.to_height)} />
							</div>
						{/each}
						{#if series!.no_activity}
							<p class="fine" data-testid="series-no-activity">No labelled company moved coins in these blocks: the recipe's checked result is an all-zero series.</p>
						{/if}
						{#if seriesQuiet > 0}
							<p class="fine">{seriesQuiet} more labelled {seriesQuiet === 1 ? 'company' : 'companies'} had no flows in these blocks.</p>
						{/if}
					</div>
					{/snippet}
					</TableGroup>
				{/if}

				<div class="panel check" data-testid="series-check">
					<div class="sh">
						<div class="ct">
							<b class="ptitle">Does the series add up?</b>
							<span class="muted">Each company's rows, summed, must equal its range row, and both row sets must give their pinned roots.</span>
						</div>
						<Pill live state={seriesPill.state} text={seriesPill.text} />
					</div>
					<div class="eq mono">
						<span>{dropped === null ? seriesRows.length : seriesRows.length - 1} series rows</span>
						<span class="eqp"><span class="op">Σ</span><span>range rows</span></span>
						<span class="eqp"><span class="op">→</span><span title={live.series.root}>root {short(live.series.root)}</span></span>
						{#if seriesShown}<span class="res" class:ok={seriesShown.series_root_ok}>{seriesShown.series_root_ok ? 'pinned' : 'not pinned'}</span>{/if}
					</div>
					{#if seriesShown && !seriesShown.ok}
						<ul class="problems mono">
							{#each seriesShown.problems.slice(0, 3) as p (p)}<li>{p}</li>{/each}
						</ul>
					{/if}
					<div class="row">
						{#if dropped === null}
							<button type="button" class="btn ghost" disabled={!series || dropTarget === null} onclick={() => checkSeries(dropTarget)}>
								<EyeOff size={16} />Hide one row{dropTarget === null ? '' : ` (${entityName(seriesRows[dropTarget].entity)}, block ${int(seriesRows[dropTarget].height)})`}
							</button>
						{:else}
							<button type="button" class="btn ghost" onclick={() => checkSeries(null)}><RotateCcw size={16} />Put it back</button>
						{/if}
					</div>
				</div>
			</section>
		{/if}

		<section class="section" data-testid="history">
			<div class="sh">
				<h2 data-testid="history-title">{histTitle}</h2>
				<Pill live state={histPill.state} text={histPill.text} />
			</div>
			<!-- W12 round 5: the two ledes sit side by side on a wide screen (app.css .duo). -->
			<div class="duo">
			<p class="muted lede">
				Computed by Onyx's company-flow recipe over one week of Bitcoin, in shards that each run within half
				their fuel budget and never cross a UTC day. Labels: the second version of the label set (exchanges and an ETF). OKX's labels are signed (level 3), the rest are
				company-published lists, so every flow here is a claim (level 4). Test labels, sharing rights not cleared.
			</p>
			<p class="muted lede" data-testid="history-trust">
				Onyx sets this page's pin from its own run. The checked headers prove the block times, not the amounts. A full rerun needs your
				own Bitcoin node (see "Reproduce it yourself" below).
			</p>
			</div>
			{#if !hist && histPill.state === 'bad' && !histEdited}
				<p class="panel muted">The history did not pass the check, so nothing is drawn.</p>
			{:else if !hist || histChecked.length === 0}
				<Skeleton rows={2} label="Checking the history" />
			{:else}
				<TableGroup testid="history-tables">
				{#snippet children(tables)}
				<div class="panel multiples">
					<ChartLegend items={NET_KEYS} unit="BTC, one scale per company" testid="history-legend">
						{@render tables()}
						<div class="seg" role="group" aria-label="Range">
							<button type="button" class="chipbtn" data-testid="history-range-day" aria-pressed={histRange === 'day'} onclick={() => (histRange = 'day')}
								>1 day</button
							>
							<button type="button" class="chipbtn" data-testid="history-range-week" aria-pressed={histRange === 'week'} onclick={() => (histRange = 'week')}
								>7 days</button
							>
						</div>
					</ChartLegend>
					{#if histRange === 'day'}
						<div class="seg multi days" role="group" aria-label="Day" data-testid="history-days">
							{#each hist!.days as d (d.date)}
								<button type="button" class="chipbtn" aria-pressed={histDay === d.date} disabled={!histDays[d.date]} onclick={() => (histDay = d.date)}
									>{dayLabel(d.first_time)}</button
								>
							{/each}
						</div>
					{/if}
					<p class="meta mono" data-testid="history-span">
						<span class="nw">Blocks {int(histShownFrom)}–{int(histShownTo)}</span> · {#each histSpan.split(' – ') as part, i (i)}{i ? ' – ' : ''}<span class="nw">{part}</span>{/each}{histRange === 'week' ? ' · one bar per six blocks (about an hour)' : ''}
					</p>
					{#if histRange === 'week' && !histWhole}
						<p class="fine bad" data-testid="history-partial">
							Totals cover {histChecked.length} of {hist!.days.length} days only: {histChecked.map((d) => d.date).join(', ')}.
						</p>
					{/if}
					{#if histPending > 0}
						<p class="fine" data-testid="history-pending">{histPending} more {histPending === 1 ? 'day is' : 'days are'} still being checked; unchecked days are not drawn.</p>
					{/if}
					{#if histFailed.length > 0}
						<p class="fine bad" data-testid="history-failed">
							{histFailed.length === 1 ? 'One day' : `${histFailed.length} days`} failed the check and {histFailed.length === 1 ? 'is' : 'are'} not drawn: {histFailed.join(', ')}.
						</p>
					{/if}
					{#if histExchanges}
						<div class="mrow" data-testid="history-all-exchanges" data-net={String(histExchanges.net)}>
							<div class="mname">
								<b>All labelled exchanges</b>
								<span class="mono"
									>{btc(histExchanges.net).replace(/(\.\d{2})\d+$/, '$1')} BTC net · {histExchanges.names.join(', ')}</span
								>
							</div>
							<SeriesChart name="All labelled exchanges" points={histExchanges.pts} times={histTimes} ends={histRange === 'week' ? bucketEnds : undefined} showTxs={false} />
						</div>
					{/if}
					{#each histShownRows as e (e.entity)}
						<div class="mrow" data-testid="history-row" data-entity={e.entity} data-kind={entityKind(e.entity)} data-net={String(e.net)}>
							<div class="mname">
								<b>{entityName(e.entity)} <span class="kind">{entityKind(e.entity)}</span></b>
								<span class="mono"
									><span class="nw">{btc(e.net).replace(/(\.\d{2})\d+$/, '$1')} BTC net</span> · <span class="nw">{e.active} active {histRange === 'week' ? 'six-block steps' : 'blocks'}</span></span
								>
							</div>
							<SeriesChart name={entityName(e.entity)} points={e.pts} times={histTimes} ends={histRange === 'week' ? bucketEnds : undefined} />
						</div>
					{/each}
					{#if histActive.length > HIST_TOP}
						<button type="button" class="chipbtn" data-testid="history-show-all" aria-pressed={histShowAll} onclick={() => (histShowAll = !histShowAll)}
							>{histShowAll ? `Show the top ${HIST_TOP}` : `Show all ${histActive.length} companies`}</button
						>
					{/if}
					{#if histQuiet > 0}
						<p class="fine">{histQuiet} more labelled {histQuiet === 1 ? 'company' : 'companies'} had no flows in these blocks.</p>
					{/if}
				</div>
				{/snippet}
				</TableGroup>
			{/if}

			<div class="panel check" data-testid="history-check">
				<div class="sh">
					<div class="ct">
						<b class="ptitle">Does the history check out?</b>
						<span class="muted"
							>Your browser checks every header's proof of work and link, the one commitment this page pins over every shard's roots, then each
							day's leaves: their roots, their sums, and the six-block totals the 7-day chart draws. It does not rerun the recipe here: the
							blocks are about 6 MB each.</span
						>
					</div>
					<Pill live state={histPill.state} text={histPill.text} />
				</div>
				{#if histShown}
					<div class="eq mono" data-testid="history-eq">
						<span class="eqp"><span>{int(histShown.index.blocks.length)} headers</span><span class="op">·</span></span>
						<span class="eqp"><span>{histShown.index.shards.length} shards</span><span class="op">·</span></span>
						<span>{histChecked.length} of {histShown.index.days.length} days · {int(histRows)} rows</span>
						<span class="eqp"><span class="op">→</span><span title={histShown.index.commitment}>commitment {short(histShown.index.commitment)}</span></span>
						<span class="res" class:ok={histShown.index.commitment_ok}>{histShown.index.commitment_ok ? 'pinned' : 'not pinned'}</span>
					</div>
					{#if !histShown.index.ok || (histShown.day && !histShown.day.ok)}
						<ul class="problems mono" data-testid="history-problems">
							{#each [...histShown.index.problems, ...(histShown.day?.problems ?? [])].slice(0, 3) as p (p)}<li>{p}</li>{/each}
						</ul>
					{/if}
				{/if}
				<div class="row">
					{#if !histEdited}
						<button type="button" class="btn ghost" disabled={!hist || histPending > 0 || histFailed.length > 0} onclick={() => tamperHistory(true)}
							><EyeOff size={16} />Change one number</button
						>
					{:else}
						<button type="button" class="btn ghost" onclick={() => tamperHistory(false)}><RotateCcw size={16} />Put it back</button>
					{/if}
				</div>
			</div>
			<details class="more repro" data-testid="history-repro">
				<summary>Reproduce it yourself</summary>
				<p class="fine">The recipe: <span class="mono">btc-entity-flow-series</span>, with label set v2.</p>
				<p class="fine">With your own Bitcoin node: fetch the week's blocks, then run the golden test over them.</p>
				<pre class="code cmds">scripts/fetch-btc-node.sh
ONYX_HISTORY_DIR=… cargo test --release -p onyx-source-btc --test golden_history</pre>
			</details>
		</section>

		{/if}

		<section class="section" data-testid="pools">
			<div class="sh">
				<h2>Who mined the blocks</h2>
				<Pill live state={poolPill.state} text={poolPill.text} />
			</div>
			<p class="muted lede">
				Level 4, a claim: anyone with the blocks can rerun the count, but which pool owns an address or a tag comes from a published
				list, and a shard is only as strong as its weakest input. Each block goes to the pool its coinbase pays, else the pool whose tag is in its
				coinbase, else "unknown". Pool names come from the MIT-licensed
				<a href="https://github.com/bitcoin-data/mining-pools" target="_blank" rel="noopener">bitcoin-data/mining-pools</a> list
				(<a href="data/LICENSE-mining-pools.txt" target="_blank" rel="noopener" data-testid="pool-license">MIT notice</a>), reviewed
				down to pool companies; coinbase text itself is never shown. {FALLBACK ? 'One week of blocks' : 'Same week as above'}, one shard per UTC day.
			</p>
			{#if poolCheck?.ok}
				<div class="panel">
					<ul class="poollegend" data-testid="pool-legend">
						{#each poolRank as [p, n], i (p)}
							{#if i < 6 || p === 'unknown'}
								<li data-testid="pool-share-{p === 'unknown' ? 'unknown' : i}">
									<i class="key" class:hatch={p === 'unknown'} style:background={p === 'unknown' ? undefined : COLOR.cat(i)}></i>
									<span>{p === 'unknown' ? 'Unknown (not credited to a reviewed pool)' : p}</span>
									<span class="mono muted">{int(n)} · {((n * 100) / poolTotal).toFixed(1)}%</span>
								</li>
							{/if}
						{/each}
						{#if poolRank.filter(([p]) => p !== 'unknown').length > 6}
							<li>
								<i class="key" style:background={COLOR.other}></i>
								<span>Other listed pools</span>
								<span class="mono muted"
									>{int(poolRank.filter(([p]) => p !== 'unknown').slice(6).reduce((a, [, n]) => a + n, 0))}</span
								>
							</li>
						{/if}
					</ul>
					<PoolShares days={poolCheck.days} named={poolNamed} />
					<!-- W12 round 5: the closing note is the panel's footer row (it was body text after the table fold). -->
					<div class="pfoot">
					<p class="fine" data-testid="pool-note">
						{int(poolTotal)} blocks, {poolCheck.days.length} days. Your browser rebuilt every day's root from its leaves, checked each day counts its
						blocks exactly once, and recomputed the commitment this page pins from Onyx's own run. It did not rerun the recipe: that needs the raw
						blocks (see "Reproduce it yourself" below). A pool can mine for
						another or change its tag; the table is the list's claim, not proof of ownership.
					</p>
					</div>
				</div>
			{:else if poolCheck && !poolCheck.ok}
				<p class="panel muted" data-testid="pools-failed">The pool history did not pass the check, so nothing is shown.</p>
			{:else}
				<div class="panel"><Skeleton rows={3} label="Checking the pool history" /></div>
			{/if}
			<details class="more repro" data-testid="pools-repro">
				<summary>Reproduce it yourself</summary>
				<p class="fine">The recipe reruns from the raw blocks, in the Bitcoin adapter (adapters/source-btc):</p>
				<pre class="code cmds">examples/export-pools.rs
tests/golden_pools.rs</pre>
			</details>
		</section>

		<!-- ADR-0044 (ROADMAP P4a): Ethereum metrics computed by Onyx's own recipe from public chain data only, so every
		     build shows them (no label set, no price). Every figure is a field of the verifier's checked window. -->
		<section class="section" data-testid="eth-onchain">
			<div class="sh">
				<h2>Ethereum on chain</h2>
				<Pill live state={ethPill.state} text={ethPill.text} />
			</div>
			<p class="muted lede">
				Computed by Onyx's Ethereum statistics recipe over every block of the window, one shard per block. The recipe itself
				checks each block's hash, its link to the block before, the EIP-1559 base fee, and that the transactions and receipts rebuild the
				header's trie roots. <span data-testid="eth-trust">Trusted, not checked: finality, that the node's finalized chain is Ethereum's.</span>
				No label set and no price goes in: level 1. Amounts are whole wei, as the recipe commits them.
			</p>
			{#if eth}
				<div class="stats" data-testid="eth-stats">
					<div class="stat"><b>{int(eth.window.blocks)}</b><span>blocks, {int(eth.from)}–{int(eth.to)}</span></div>
					<div class="stat"><b>{int(eth.window.txs)}</b><span>transactions</span></div>
					<div class="stat"><b>{ethUnits(eth.window.burned_wei, 2)}</b><span>ETH burned</span></div>
					<div class="stat"><b>{ethPct(eth.window.fullness_ppm)}</b><span>of the gas limit used</span></div>
				</div>
				<TableGroup testid="eth-tables">
				{#snippet children(tables)}
				<div class="panel multiples">
					<ChartLegend items={[]} unit="one scale per chart" testid="eth-legend">
						{@render tables()}
					</ChartLegend>
					<p class="meta mono" data-testid="eth-span">
						<span class="nw">Blocks {int(eth!.from)}–{int(eth!.to)}</span> · <span class="nw">{stampWords(eth!.window.first_time)}</span> to <span class="nw">{stampWords(eth!.window.last_time)}</span>
					</p>
					<p class="fine">Bars: one per {eth!.group_blocks} blocks (5 minutes). Lines: one point per block.</p>
					{#each ethSpecs as c (c.id)}
						<div class="mrow" data-testid="eth-{c.id}">
							<div class="mname">
								<b>{c.title}</b>
								<span class="mono">{c.summary}</span>
							</div>
							<EthChart points={c.points} name={c.title} unit={c.unit} mode={c.mode} step={c.step} tick={c.tick} testid="eth-chart-{c.id}" />
						</div>
					{/each}
					<div class="pfoot duo">
						<p class="fine" data-testid="eth-source">
							<span data-testid="eth-by">Every chart: computed by Onyx's Ethereum statistics recipe from
							<span class="nw">blocks {int(eth!.from)}–{int(eth!.to)}</span></span>, fetched from a public Ethereum RPC and cross-checked by block hash with a second operator, at or
							below the finalized block. An independent recount from the RPC's own JSON fields agrees on every field of every block.
						</p>
						<p class="fine" data-testid="eth-planned">
							Not here yet, and why: exchange and issuer balances on Ethereum (they need the company labels; planned), USDT and USDC supply and flows
							(they need token transfer logs; planned), staking and transfers made inside contracts (they need execution traces, not just blocks). This window is
							{int(eth!.window.blocks)} blocks, about an hour; it does not show the chain's history.
						</p>
					</div>
				</div>
				{/snippet}
				</TableGroup>
			{:else if ethPill.state === 'bad' && !ethEdited}
				<p class="panel muted" data-testid="eth-failed">The Ethereum window did not pass the check, so nothing is drawn.</p>
			{:else}
				<div class="panel"><Skeleton rows={3} label="Checking the Ethereum window" /></div>
			{/if}

			<div class="panel check" data-testid="eth-check">
				<div class="sh">
					<div class="ct">
						<b class="ptitle">Does the Ethereum window check out?</b>
						<span class="muted"
							>Your browser decodes every block's leaf, checks the blocks run without a gap to the pinned tip and each block's interval links to
							the one before, and recomputes the commitment this page pins from Onyx's own run. Then it can rerun the recipe itself on one block's
							full input, hashes and trie roots included: the same engine and fuel rules a validator uses.</span
						>
					</div>
					<Pill live state={ethPill.state} text={ethPill.text} />
				</div>
				{#if ethShown}
					<div class="eq mono" data-testid="eth-eq">
						<span class="eqp"><span>{int(ethShown.blocks.length)} leaves</span><span class="op">·</span></span>
						<span>{ethShown.days.length} {ethShown.days.length === 1 ? 'day' : 'days'}</span>
						<span class="eqp"><span class="op">→</span><span title={ethShown.commitment}>commitment {short(ethShown.commitment)}</span></span>
						<span class="res" class:ok={ethShown.commitment_ok}>{ethShown.commitment_ok ? 'pinned' : 'not pinned'}</span>
					</div>
					{#if !ethShown.ok}
						<ul class="problems mono" data-testid="eth-problems">
							{#each ethShown.problems.slice(0, 3) as p (p)}<li>{p}</li>{/each}
						</ul>
					{/if}
				{/if}
				{#if ethRun}
					<div class="eq mono" data-testid="eth-rerun">
						<span class="eqp"><span>block {int(ethRun.number)}, {int(ethRun.txs_in_input)} transactions</span><span class="op">·</span></span>
						<span>{int(ethRun.fuel)} fuel, {(ethRunMs / 1000).toFixed(1)} s here</span>
						<span class="eqp"><span class="op">→</span><span title={ethRun.root}>root {short(ethRun.root)}</span></span>
						<span class="res" class:ok={ethRun.ok}>{ethRun.ok ? 'same as the window' : 'different'}</span>
					</div>
				{/if}
				<div class="row">
					<button type="button" class="btn" data-testid="eth-rerun-btn" disabled={!eth || ethRunPill.state === 'run'} onclick={rerunEth}
						>Rerun block {eth ? int(eth.rerun_number) : ''} here</button
					>
					<span class="runstat {ethRunPill.state}"><i aria-hidden="true"></i><span>{ethRunPill.text}</span></span>
					{#if !ethEdited}
						<button type="button" class="btn ghost" disabled={!eth} onclick={() => tamperEth(true)}><EyeOff size={16} />Change one number</button>
					{:else}
						<button type="button" class="btn ghost" onclick={() => tamperEth(false)}><RotateCcw size={16} />Put it back</button>
					{/if}
				</div>
			</div>
			<details class="more repro" data-testid="eth-repro">
				<summary>Reproduce it yourself</summary>
				{#if eth}<p class="fine" data-testid="eth-recipe">The recipe: <span class="mono">eth-chain-stats</span>, id <span class="mono" title={eth.recipe_id}>{short(eth.recipe_id)}</span>.</p>{/if}
				<p class="fine">With your own Ethereum node: fetch the window, export it, then run the golden test.</p>
				<pre class="code cmds" data-testid="eth-rerun-cmd">scripts/refresh-eth-onchain.py --rpc &lt;your node&gt; --to &lt;last block&gt; --no-cross-check --fetch-only
ONYX_ETH_UNCHECKED=1 cargo run --release -p onyx-source-eth --example export-eth-chain-stats -- &lt;out&gt; &lt;window&gt;
ONYX_ETH_DIR=… cargo test -p onyx-source-eth --test golden_eth_chain
# independent recount from the RPC's JSON:
scripts/recount-eth-chain-stats.py</pre>
			</details>
		</section>

		<section class="section" data-testid="level2">
			<div class="sh">
				<h2>Public records, re-observed</h2>
				<Pill live state={l2Pill.state} text={l2Pill.text} />
			</div>
			<p class="muted lede">
				Level 2, re-observed: observers drawn at random from a bonded pool fetch the same public record, seal their answers, then reveal
				them. The majority value settles. This round is a local rehearsal: the observer pool is simulated, and the draw was announced on a
				local test chain before its beacon block existed, with that block's hash as the beacon. That chain no longer exists, so the browser
				checks the recorded draw's consistency only. Each observer fetched the source itself over HTTPS.
			</p>
			{#if l2Check?.ok}
				{@const l2 = l2Check}
				<div class="panel l2" data-testid="level2-card">
					<p class="l2label">U.S. total public debt outstanding, <span class="nw">{isoWords(l2.record_date)}</span></p>
					{#if l2.sealed}
						<p class="l2value sealed" data-testid="level2-sealed">Available via the paid API</p>
					{:else}
						<p class="l2value mono"><span data-testid="level2-value">${usd(l2.value)}</span> <span class="real-tag" data-testid="level2-tag">real value · test round</span></p>
					{/if}
					<dl class="l2facts">
						<div><dt>Source</dt><dd><a href={l2.terms_url} target="_blank" rel="noopener" data-testid="level2-source">{l2.source_name}</a></dd></div>
						<div><dt>License</dt><dd data-testid="level2-license">"{l2.terms_quote}"</dd></div>
						<div><dt>Window</dt><dd><span class="nw">{dayWords(l2.window_start)},</span> <span class="nw">{utc(l2.window_start)}–{utc(l2.window_end)} UTC</span></dd></div>
						<div><dt>Draws</dt><dd class="mono" data-testid="level2-draws">{l2.draws} {l2.draws === 1 ? 'draw' : 'draws'}, {l2.without_value} without a value</dd></div>
						{#if !l2.sealed}
							<div><dt>Observers</dt><dd class="mono" data-testid="level2-observers">{l2.agreed} of {l2.observers} agreed</dd></div>
							<div><dt>Draw</dt><dd data-testid="level2-draw">simulated pool of {l2.pool_size}, beacon from a local test chain</dd></div>
							<div><dt>Result</dt><dd class="mono" title={l2.result_hash}>{short(l2.result_hash)}</dd></div>
						{/if}
					</dl>
					<!-- SIG-R1-F3: the copy says only what this build's check did. -->
					{#if l2.sealed}
						<p class="muted" data-testid="level2-copy">
							The paid API sells this value, so this page ships neither the value nor its result hash. Your browser recomputed the observer draw
							from the pool and beacon, checked the draw against its recorded announcement (consistency only), and checked all
							{l2.commits_checked} sealed commits, each signed by its observer's own key. {l2.attribution}.
						</p>
					{:else}
						<p class="muted" data-testid="level2-copy">
							Your browser reran the fetch code, fixed by its hash, on the recorded response, recomputed the observer draw from the pool and beacon, checked the draw against its recorded announcement (consistency only), all
							{l2.commits_checked} sealed commits, each signed by its observer's own key, and the result hash, which must equal the one pinned from Onyx's own run.
							<span data-testid="level2-real">The value is the Treasury's real published figure for <span class="nw">{isoWords(l2.record_date)}</span>, fetched from fiscaldata.treasury.gov on <span class="nw">{dayWords(l2.window_start)}</span> (UTC). Only the round was a test: the observers and the draw were a local rehearsal.</span> {l2.attribution}.
						</p>
					{/if}
				</div>
			{:else if l2Check && !l2Check.ok}
				<p class="panel muted">The recorded round did not pass the check, so nothing is shown.</p>
			{:else}
				<div class="panel l2"><Skeleton rows={2} label="Checking the round" /></div>
			{/if}
		</section>


		<!-- NS3: data outside finance, on the same level-2 round (ADR-0027 amendment 17). -->
		<section class="section" data-testid="daily">
			<div class="sh">
				<h2>Earthquakes and weather, re-observed</h2>
				<Pill live state={dailyPill.state} text={dailyPill.text} />
			</div>
			<div class="duo">
				<p class="muted lede">
					The same level-2 round on data outside finance. A day is read only after the source has finished revising it: earthquakes 100 to
					120 days later, temperatures 30 to 60 days later. Like the round above, these are local rehearsals: simulated observer pools, each
					draw announced on a local test chain, every observer fetching the source itself over HTTPS.
				</p>
				<!-- ROADMAP B6: the same rounds on Base Sepolia, recorded by the registry and served by the hosted API. -->
				<p class="muted lede" data-testid="daily-testnet">
					The same rounds also ran live on Base Sepolia, a public test network, on <span class="nw">27 Oct 2026</span>: earthquakes on <span class="nw">19 Jul 2026</span> and the Denver high on
					<span class="nw">27 Sep 2026</span>. Seven observer processes with their own keys, five drawn; each posted its own commit on chain, and the registry records
					both values (<a href="https://sepolia.basescan.org/address/0x497389e04f7d58e07a79ac8d395c3927aecbdf2b#events" target="_blank" rel="noopener" data-testid="daily-testnet-registry">registry on Basescan</a>).
					All seven run on one machine for one operator, so this tests the round, not independence. Values from free public sources are free to
					read: the query API serves both with their full transcript, at no charge: <a href="https://onyx.cryptagraph.com/v1/observations/usgs.earthquakes.m5_count?subject=usgs.earthquakes.m5_world:2026-07-19" data-testid="daily-testnet-usgs">earthquakes</a>,
					<a href="https://onyx.cryptagraph.com/v1/observations/noaa.ghcnd.usw00003017.tmax?subject=noaa.ghcnd.usw00003017:2026-09-27" data-testid="daily-testnet-ncei">temperature</a>.
				</p>
			</div>
			<!-- NS5 (ADR-0048): each Base Sepolia value's level and when it got there, as the client check accepted it. -->
			{#if ladderFile}
				<div class="ladders" data-testid="ladders">
					{#each ladderFile.values as v (v.id)}
						<div class="panel ladder" data-testid={`ladder-${v.id}`}>
							<p class="l2label">{LADDER_LABEL[v.id] ?? v.metric}, <span class="nw">{isoWords(v.subject.split(':').pop() ?? '')}</span>, on Base Sepolia</p>
							<LevelBadge level={2} ladder={v.ladder} testid={`ladder-${v.id}`} />
						</div>
					{/each}
				</div>
			{:else if ladderError}
				<p class="panel muted" data-testid="ladders-failed">The level history did not load.</p>
			{/if}
			<div class="dailygrid">
				{#each [{ id: 'usgs' as const, label: 'Earthquakes of magnitude 5.0 or more, worldwide', fmt: (v: string) => `${v} earthquakes` }, { id: 'ncei' as const, label: 'Highest temperature at Denver International Airport', fmt: (v: string) => `${v} °C` }] as d (d.id)}
					{@const c = dailyChecks[d.id]}
					{#if c?.ok}
						<div class="panel l2" data-testid={`daily-${d.id}`}>
							<p class="l2label">{d.label}, <span class="nw">{isoWords(c.record_date)}</span></p>
							{#if c.sealed}
								<p class="l2value sealed" data-testid={`daily-${d.id}-sealed`}>Available via the paid API</p>
							{:else}
								<p class="l2value mono"><span data-testid={`daily-${d.id}-value`}>{d.fmt(c.value)}</span> <span class="real-tag">real value · test round</span></p>
							{/if}
							<dl class="l2facts">
								<div><dt>Source</dt><dd><a href={c.terms_url} target="_blank" rel="noopener" data-testid={`daily-${d.id}-source`}>{c.source_name}</a></dd></div>
								<div><dt>License</dt><dd>"{c.terms_quote}"</dd></div>
								<div><dt>Window</dt><dd><span class="nw">{dayWords(c.window_start)},</span> <span class="nw">{utc(c.window_start)}–{utc(c.window_end)} UTC</span></dd></div>
								{#if !c.sealed}
									<div><dt>Observers</dt><dd class="mono" data-testid={`daily-${d.id}-observers`}>{c.agreed} of {c.observers} agreed</dd></div>
									<div><dt>Result</dt><dd class="mono" title={c.result_hash}>{short(c.result_hash)}</dd></div>
								{/if}
							</dl>
							<p class="muted">
								{#if c.sealed}
									The paid API sells this value. Your browser recomputed the observer draw and checked all {c.commits_checked} sealed commits.
								{:else}
									Your browser reran the fetch code, fixed by its hash, on the recorded response, recomputed the observer draw, checked all {c.commits_checked} sealed commits and the result hash pinned from Onyx's own run.
								{/if}
								{c.attribution}.
							</p>
						</div>
					{:else if c && !c.ok}
						<p class="panel muted" data-testid={`daily-${d.id}-failed`}>This recorded round did not pass the check, so nothing is shown.</p>
					{:else}
						<div class="panel l2"><Skeleton rows={2} label="Checking the round" /></div>
					{/if}
				{/each}
			</div>
		</section>

		<section class="section" data-testid="sec">
			<div class="sh">
				<h2>Crypto-asset units tagged in SEC filings</h2>
				<Pill live state={secPill.state} text={secPill.text} />
			</div>
			<p class="muted lede">
				Level 2: re-observed from public filings, not an on-chain fact. Each value is the <span class="nw">company-wide</span> total the company tagged as
				its number of crypto-asset units in its SEC filing for the period (all crypto assets together, not a
				per-asset figure), read from EDGAR by several observers. For {secCheck?.ok ? `these ${secCheck.companies.length}` : 'these'} companies it agrees with their own public
				bitcoin totals within rounding, for the periods checked. As tagged: filings may round, and EDGAR's API omits XBRL precision. Onyx does not check the coins exist. Local rehearsal: the observers
				and their pool were simulated on one machine, each fetching EDGAR itself; each draw was announced on a local test chain before
				its beacon block existed.
			</p>
			{#if secCheck?.ok}
				{@const sc = secCheck}
				<div class="seccards">
					{#each sc.companies as co (co.key)}
						<div class="panel l2" data-testid="sec-{co.key}">
							<p class="l2label">{co.name}, period ending <span class="nw">{isoWords(co.period_end)}</span></p>
							{#if co.sealed}
								<p class="l2value sealed" data-testid="sec-sealed-{co.key}">Available via the paid API</p>
							{:else}
								<p class="l2value mono"><span data-testid="sec-value-{co.key}">{grouped(co.value)}</span> <span class="real-tag" data-testid="sec-tag-{co.key}">real value · test round</span></p>
							{/if}
							<dl class="l2facts">
								<div><dt>Unit</dt><dd>{co.unit_meaning}</dd></div>
								<div><dt>Filings</dt><dd><a href={co.filings_url} target="_blank" rel="noopener">EDGAR, CIK {co.cik}</a></dd></div>
								<div><dt>Draws</dt><dd class="mono" data-testid="sec-draws-{co.key}">{co.draws} {co.draws === 1 ? 'draw' : 'draws'}, {co.without_value} without a value</dd></div>
								{#if !co.sealed}
									<div><dt>Observers</dt><dd class="mono" data-testid="sec-observers-{co.key}">{co.agreed} of {co.observers} agreed</dd></div>
									<div><dt>Draw</dt><dd data-testid="sec-draw-{co.key}">simulated pool of {co.pool_size}, beacon from a local test chain</dd></div>
									<div><dt>Result</dt><dd class="mono" title={co.result_hash}>{short(co.result_hash)}</dd></div>
								{/if}
							</dl>
						</div>
					{/each}
				</div>
				<!-- SIG-R1-F3: the copy says only what this build's check did. -->
				<!-- W12 round 4: the source note sits in a quiet bar under the two cards (it floated as a
				     grey paragraph between sections). -->
				<!-- W12 round 5: the bar spans the cards above it and its note reads in two columns on a wide
				     screen (a 600 px box of ten lines beside an empty half). -->
				<div class="notebar secnote">
				{#if sc.companies.some((co) => co.sealed)}
					<p class="muted" data-testid="sec-copy">
						Source: <a href={sc.terms_url} target="_blank" rel="noopener" data-testid="sec-terms">{sc.source_name}</a>. "{sc.terms_quote}" The paid
						API sells these values, so this page ships neither the values nor their result hashes. Your browser recomputed each observer draw
						from its pool and beacon and checked that each observer signed its own sealed commit. {sc.attribution}.
					</p>
				{:else}
					<p class="muted" data-testid="sec-copy">
						Source: <a href={sc.terms_url} target="_blank" rel="noopener" data-testid="sec-terms">{sc.source_name}</a>. "{sc.terms_quote}" Your browser
						reran the fetch code, fixed by its hash, on each recorded response, recomputed every sealed commit and result hash against the pins from Onyx's own
						run, and checked that each observer signed its own commit and reveal. <span data-testid="sec-real">Values shown are the companies' real filed figures, fetched from data.sec.gov on {[...new Set(sc.companies.map((co) => dayWords(co.window_start)))].join(' and ')} (UTC). Only the rounds were a test: the observers and the draws were a local rehearsal.</span> {sc.attribution}.
					</p>
				{/if}
				</div>
			{:else if secCheck && !secCheck.ok}
				<p class="panel muted" data-testid="sec-failed">The recorded filing rounds did not pass the check, so nothing is shown.</p>
			{:else}
				<div class="panel l2"><Skeleton rows={2} label="Checking the filing rounds" /></div>
			{/if}
			<details class="more repro" data-testid="sec-repro">
				<summary>Reproduce it yourself</summary>
				<p class="fine">The fact read from each filing: <span class="mono">us-gaap:CryptoAssetNumberOfUnits</span>. The cross-check against each company's own bitcoin totals is in the EDGAR adapter's provenance file.</p>
			</details>
		</section>

		<!-- ROADMAP P2c: spot bitcoin ETF holdings from SEC filings, level 2. Filing data is public
		     U.S. government information, not a sold round, so every build shows it (loop decision 2026-10-19,
		     docs/TRACKER.md; the public-build guard admits only the committed file, THREATS ETF-10). -->
		<section class="section" data-testid="etf">
			<div class="sh">
				<h2>Spot bitcoin ETF holdings from SEC filings</h2>
				<Pill live state={etfPill.state} text={etfPill.text} />
			</div>
			<p class="muted lede" data-testid="etf-lede">
				From public filings, not an on-chain fact. Each figure is the bitcoin a US spot fund tagged in a <span class="nw">10-Q</span> or <span class="nw">10-K</span> for the period end (the
				earliest filing that tags it; for a few periods that is a later report repeating it as a comparative), read from EDGAR's data.sec.gov
				by fetch code fixed by its hash and converted exactly to satoshis. As tagged: a fund may round (iShares tags whole bitcoin). Onyx does not check
				that the coins exist. A change in holdings is creations minus redemptions and the sponsor's fee paid in bitcoin, not net creations alone.
			</p>
			{#if etfCheck?.ok && etfCheck.summary}
				{@const es = etfCheck.summary}
				<div class="panel">
					<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
					<div class="tablebox" tabindex="0" role="region" aria-label="ETF holdings from filings">
						<table class="stack etfstack" data-testid="etf-table">
							<thead><tr><th class="fundcol">Fund</th><th>Period end</th><th class="r">BTC held, as filed</th><th class="r">Change since previous tagged period</th><th>Filing</th></tr></thead>
							<tbody>
								{#each es.funds as f (f.key)}
									{@const h = f.holdings[f.holdings.length - 1]}
									{@const fl = f.flows[f.flows.length - 1]}
									<tr data-testid="etf-{f.key}">
										<td class="wrap" data-label="Fund"><span class="fund"><span><strong>{f.ticker}</strong> {f.fund_name}</span><span class="cik mono">CIK {f.cik}</span></span></td>
										<td class="mono" data-label="Period end" data-testid="etf-date-{f.key}">{h.period_end}</td>
										<td class="mono r" data-label="BTC held, as filed" data-testid="etf-value-{f.key}">{dec(h.as_filed)}</td>
										<td class="mono r" data-label="Change" data-testid="etf-flow-{f.key}">{#if fl}<span>{dec(fl.change_btc)} <span class="nw">(since {fl.from})</span></span>{:else}first filing in range{/if}</td>
										<td class="wrap" data-label="Filing"><span><a href={h.filing_url} target="_blank" rel="noopener" data-testid="etf-filing-{f.key}">{h.form} {h.accn}</a>, filed <span class="nw">{h.filed}</span></span></td>
									</tr>
								{/each}
							</tbody>
						</table>
					</div>
					<details class="more">
						<summary>Every filed period and flow ({es.funds.reduce((a, f) => a + f.holdings.length, 0)} periods)</summary>
						<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
						<div class="tablebox" tabindex="0" role="region" aria-label="Every filed period">
							<table class="stack etfstack" data-testid="etf-flows">
								<thead><tr><th>Fund</th><th>Period end</th><th class="r">BTC held</th><th class="r">Change from the tagged period before</th><th>Filing</th></tr></thead>
								<tbody>
									{#each es.funds as f (f.key)}
										{#each f.holdings as h (h.period_end)}
											{@const prev = f.flows.find((x) => x.to === h.period_end)}
											<tr>
												<td data-label="Fund">{f.ticker}</td>
												<td class="mono" data-label="Period end">{h.period_end}</td>
												<td class="mono r" data-label="BTC held">{dec(h.as_filed)}</td>
												<td class="mono r" data-label="Change">{prev ? `${dec(prev.change_btc)} over ${prev.days} days` : '–'}</td>
												<td data-label="Filing"><span>{h.form} <a href={h.filing_url} target="_blank" rel="noopener">{h.accn}</a></span></td>
											</tr>
										{/each}
									{/each}
								</tbody>
							</table>
						</div>
						<p class="muted" data-testid="etf-facts">Facts read, all <span class="mono">us-gaap</span> without XBRL dimensions: {es.funds.map((f) => `${f.ticker} ${f.fact.replace('us-gaap:', '')} (unit ${f.unit})`).join('; ')}.</p>
					</details>
				</div>
				<div class="notebar secnote duo">
					<p class="muted" data-testid="etf-flows-note">{es.flows_note} Your browser recomputed every figure above from the recordings this build ships; that catches an edited file, not a recording changed before the build.</p>
					<p class="muted" data-testid="etf-source">
						Source: <a href={es.source.terms_url} target="_blank" rel="noopener">{es.source.name}</a>. "{es.source.terms_quote}" Recorded from data.sec.gov on <span class="nw">{dayWords(es.window_start)}</span> (UTC). {es.source.attribution}. The independent check is each filing's own link on sec.gov.
					</p>
				</div>
				<div class="etfrerun" data-testid="etf-rerun">
					<p class="muted" data-testid="etf-status">{plain(es.status)}</p>
					<details class="more repro">
						<summary>Reproduce it yourself</summary>
						<p class="fine">Rerun, offline, from the recorded responses:</p>
						<pre class="code cmds">{es.rerun}</pre>
					</details>
				</div>
				<details class="more etfuncovered" data-testid="etf-uncovered">
					<summary>Not covered yet ({es.us_not_covered.length + es.outside_us.length} funds: {es.us_not_covered.length} in the US, {es.outside_us.length} outside it)</summary>
					<div class="etfnc">
						<div class="panel" data-testid="etf-not-covered">
							<p class="l2label">US funds not covered yet</p>
							<p class="muted">{plain(es.not_covered_note)}</p>
							<ul class="muted">
								{#each es.us_not_covered as n (n.ticker)}
									<li><strong>{n.ticker}</strong> {n.name} (CIK {n.cik}): {plain(n.reason)}.</li>
								{/each}
							</ul>
						</div>
						<div class="panel" data-testid="etf-outside-us">
							<p class="l2label">Outside the US: no SEC filings</p>
							<ul class="muted">
								{#each es.outside_us as n (n.ticker)}
									<li><strong>{n.name}</strong>: {plain(n.reason)}.</li>
								{/each}
							</ul>
						</div>
					</div>
				</details>
			{:else if etfCheck && !etfCheck.ok}
				<p class="panel muted" data-testid="etf-failed">The ETF holdings file does not match the recorded filings, so nothing is shown.</p>
			{:else}
				<div class="panel"><Skeleton rows={2} label="Checking the ETF filings" /></div>
			{/if}
		</section>


	{/if}
	<!-- ROADMAP C61: a level-3 answer, the local-anvil rehearsal (no live book yet), checked
	     against its pinned chain answers; shown in every build (a free answer derived from no
	     label set: TRACKER 2026-10-18). Outside the demo block: it loads its own file. -->
	<SignedFact />
	<!-- ROADMAP C6: level-5 track records (no real one yet; the labelled example). Outside the
	     demo block: it loads and checks its own file. -->
	<TrackRecords />
</div>

<style>
	/* Phones: the pill always takes its own line, so its text can grow from
	   "Not checked" to the result without pushing the chart down (CLS). */
	@media (max-width: 560px) {
		.sh h2 {
			flex-basis: 100%;
		}
		.slabmeta {
			min-height: 3.1em;
		}
	}
	.stats {
		margin-bottom: var(--sp-3);
	}
	/* W12 round 10: on a wide screen the first section's heading and status sit at the left and its four
	   figure tiles beside them (the tiles had a row of their own, so the block chart started at 618 px).
	   Round 12: the heading spans the section above them (the chart starts 25 px lower, at 606 px). */
	@media (min-width: 1100px) {
		.section.statlead {
			grid-template-columns: minmax(0, 1fr) minmax(0, 768px);
			align-items: end;
		}
		.section.statlead > :global(*) {
			grid-column: 1 / -1;
		}
		/* W12 round 12: the heading runs across the section on one line (in the 248 px left column it
		   broke after "block"); its status takes the left column beside the tiles, centred on their row.
		   The header box steps aside so its two children place on the section's grid. */
		.section.statlead > .sh {
			display: contents;
		}
		.section.statlead > .sh > h2 {
			grid-column: 1 / -1;
		}
		.section.statlead > .sh > :global(.pill) {
			grid-column: 1;
			justify-self: start;
			align-self: center;
		}
		.section.statlead > .stats {
			grid-column: 2;
			margin-bottom: 0;
		}
	}
	.stat b.out {
		color: var(--btc);
	}
	/* W12 round 10: a chart panel's padding is the dense step on every side (was 20 / 16 / 16 px). */
	.chartbox {
		padding: var(--sp-4);
	}
	.ctl {
		margin: 0 0 var(--sp-4);
	}
	.seg.days {
		flex-wrap: wrap;
		margin-block: var(--sp-3) var(--sp-0);
		/* W12 round 4 review: the day switch hugs its days (the panel's grid stretched it into a
		   full-width bar, mostly empty on a wide screen). */
		justify-self: start;
	}
	/* W12 round 4: was .seg.wrap, which also took the page container's .wrap (1,080 px max, auto
	   margins, 20 px side padding): the company switch sat mid-row at 1,280 px and centred on a
	   phone. */
	.seg.multi {
		flex-wrap: wrap;
		max-width: 100%;
		border-radius: var(--radius-lg);
	}
	.quiet {
		margin-top: calc(var(--sp-2) * -1);
	}
	.check {
		margin-top: var(--sp-3);
		display: grid;
		gap: var(--sp-4);
	}
	.check .sh {
		margin: 0;
	}
	/* Owner review: a rerun's status beside its button is a dot and plain text (a pill there read as a
	   second, disabled button). */
	.runstat {
		display: inline-flex;
		align-items: center;
		gap: var(--sp-2);
		font-size: var(--text-sm);
		color: var(--muted);
	}
	.runstat > i {
		width: var(--sp-2);
		height: var(--sp-2);
		border-radius: 50%;
		background: currentColor;
		flex: none;
	}
	.runstat.run {
		color: var(--accent);
	}
	.runstat.good {
		color: var(--good);
	}
	.runstat.bad {
		color: var(--bad);
	}
	.runstat.warn {
		color: var(--warn);
	}
	.ct {
		display: grid;
		gap: var(--sp-1);
	}
	.ct span {
		font-size: var(--text-sm);
	}
	.res {
		color: var(--bad);
	}
	.res.ok {
		color: var(--good);
	}
	.problems {
		margin: 0;
		padding-left: var(--sp-4);
		font-size: var(--text-sm);
		color: var(--bad);
		overflow-wrap: anywhere;
	}
	.l2 {
		display: grid;
		gap: var(--sp-3);
	}
	/* ROADMAP P2c: the ETF section. The rerun command and accession links wrap on a phone. */
	.etfnc {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 320px), 1fr));
		gap: var(--sp-4);
		margin-top: var(--sp-3);
	}
	/* The fund name and the filing wrap inside their cells, so the table fits a wide screen. */
	[data-testid='etf-table'] td.wrap {
		white-space: normal;
	}
	/* Owner review: the fund's CIK sits under its name, so the Fund column takes the width the CIK
	   column had (names wrapped to three or four lines). */
	@media (min-width: 901px) {
		[data-testid='etf-table'] .fundcol {
			min-width: 16rem;
		}
		[data-testid='etf-table'] th {
			white-space: normal;
		}
	}
	.fund {
		display: grid;
		gap: var(--sp-0);
	}
	.cik {
		font-size: var(--text-xs);
		color: var(--muted);
	}
	/* Above the phone layout only: on a phone the stacked cells wrap anywhere (W2). */
	@media (min-width: 561px) {
		[data-testid='etf-table'] a,
		[data-testid='etf-flows'] a {
			white-space: nowrap;
		}
	}
	.etfnc > .panel {
		min-width: 0;
		align-content: start;
	}
	.etfrerun {
		margin-top: var(--sp-3);
	}
	.etfrerun > p {
		margin: 0 0 var(--sp-1);
	}
	.etfnc .panel > p {
		margin: 0 0 var(--sp-2);
	}
	[data-testid='etf'] ul {
		align-content: start;
		margin: var(--sp-2) 0 0;
		padding-left: var(--sp-4);
		display: grid;
		gap: var(--sp-2);
	}
	[data-testid='etf'] details.more {
		margin-top: var(--sp-3);
	}
	.seccards {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 280px), 1fr));
		gap: var(--sp-4);
		margin-bottom: var(--sp-3);
	}
	.ladders {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 22rem), 1fr));
		gap: var(--sp-4);
		margin-bottom: var(--sp-4);
	}
	.ladder {
		display: flex;
		flex-direction: column;
		gap: var(--sp-2);
		min-width: 0;
	}
	.dailygrid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 22rem), 1fr));
		gap: var(--sp-4);
	}
	.l2label {
		margin: 0;
		color: var(--muted);
	}
	.l2value {
		margin: 0;
		font-size: var(--text-xl);
		overflow-wrap: anywhere;
	}
	/* "real value · test round": a real fetched value from a rehearsal round (W8). */
	.real-tag {
		font-family: inherit;
		font-size: var(--text-xs);
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--muted);
		border: 1px solid currentColor;
		border-radius: var(--radius-xs);
		padding: var(--sp-0) var(--sp-2);
		vertical-align: middle;
		white-space: nowrap;
	}
	.l2facts {
		display: grid;
		gap: var(--sp-2);
		margin: 0;
	}
	.l2facts div {
		display: grid;
		grid-template-columns: minmax(0, 120px) minmax(0, 1fr);
		gap: var(--sp-3);
	}
	.l2facts dt {
		color: var(--muted);
	}
	.l2facts dd {
		margin: 0;
		min-width: 0;
		overflow-wrap: anywhere;
	}
	/* W12 round 4: on a phone each fact's name sits over its value (a 120 px name column left the
	   values 150 px, so "1 draw, 0 without a value" took two lines and the draw three). */
	@media (max-width: 560px) {
		.l2facts {
			gap: var(--sp-3);
		}
		.l2facts div {
			grid-template-columns: minmax(0, 1fr);
			gap: 0;
		}
		.l2facts dt {
			font-size: var(--text-sm);
		}
	}
	/* W12 round 2: on a wide screen the round's facts and what this browser checked sit side by
	   side under the value (the note had held a 62-character column with the card's right half
	   empty). */
	@media (min-width: 900px) {
		[data-testid='level2-card'] {
			grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
			column-gap: var(--sp-7);
		}
		[data-testid='level2-card'] > .l2label,
		[data-testid='level2-card'] > .l2value {
			grid-column: 1 / -1;
		}
		[data-testid='level2-card'] > [data-testid='level2-copy'] {
			padding-left: var(--sp-7);
			border-left: 1px solid var(--line);
		}
	}
	.lede {
		margin: calc(var(--sp-2) * -1) 0 var(--sp-4);
		max-width: 68ch;
	}
	/* W12 round 5: a long lede in .duo (app.css: two text columns on a wide screen); the wrapper
	   takes the lede's margins. */
	.duo {
		margin: calc(var(--sp-2) * -1) 0 var(--sp-4);
	}
	.duo > .lede {
		margin: 0;
	}
	.duo > .lede + .lede {
		margin-top: var(--sp-4);
	}
	/* The bar's width and its two columns are the shared .notebar.duo rule (app.css, round 6). */
	.secnote {
		margin: 0;
	}
	@media (min-width: 900px) {
		.duo > .lede + .lede {
			margin-top: 0;
		}
	}
	.multiples {
		display: grid;
		gap: var(--sp-4);
		padding: var(--sp-4);
	}
	.mrow {
		display: grid;
		gap: var(--sp-2);
		min-width: 0;
	}
	/* W12 round 4: with one table switch per panel, a hairline parts one company's chart from the
	   next (the per-chart folds had done that). */
	.mrow + .mrow {
		padding-top: var(--sp-4);
		border-top: 1px solid var(--line);
	}
	.mname {
		display: flex;
		flex-wrap: wrap;
		justify-content: space-between;
		gap: var(--sp-1) var(--sp-3);
		font-size: var(--text-sm);
	}
	.mname span {
		color: var(--muted);
		font-size: var(--text-sm);
		font-variant-numeric: tabular-nums;
	}
	.mname b .kind {
		margin-left: var(--sp-2);
		font-weight: 400;
		font-size: var(--text-xs);
		letter-spacing: 0.04em;
		text-transform: uppercase;
	}
	.mapbox {
		padding: var(--sp-4);
		overflow-x: auto;
	}
	td.r {
		text-align: right;
	}
	.poollegend {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-2) var(--sp-4);
		font-size: var(--text-sm);
	}
	.poollegend li {
		display: inline-flex;
		align-items: center;
		gap: var(--sp-2);
	}
	.poollegend .key {
		margin-right: 0;
	}
	/* Data review: a less central section folds under its heading (Estimates here; Signed by the
	   source and Track records in their components, inside this page's .wrap). The heading row is the
	   fold's label; the site's "+" / "−" marker leads it at the heading's size. */
	.wrap :global(details.secfold > summary.sh) {
		justify-content: flex-start;
		flex-wrap: wrap;
	}
	.wrap :global(details.secfold > summary.sh::before) {
		font-size: var(--text-lg);
	}
	.wrap :global(details.secfold > summary.sh > h2) {
		margin: 0 auto 0 0;
	}
	.wrap :global(details.secfold[open] > :not(summary)) {
		margin-top: var(--sp-6);
	}
	/* Owner review: a folded section says in one line what it holds (a bare heading read as an empty gap). */
	/* The heading row is a grid so the note takes its own row under the heading, at reading width. */
	.wrap :global(details.secfold > summary.sh:has(> .foldnote)) {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto;
		align-items: center;
	}
	.wrap :global(details.secfold > summary.sh > .foldnote) {
		grid-column: 2 / -1;
		max-width: 68ch;
		margin: 0;
		font-family: var(--body);
		font-size: var(--text-sm);
		font-weight: 400;
		line-height: var(--lh-body);
		color: var(--muted);
		text-wrap: pretty;
	}
	@media (max-width: 560px) {
		.wrap :global(details.secfold[open] > :not(summary)) {
			margin-top: var(--sp-4);
		}
		.wrap :global(details.secfold > summary.sh > h2) {
			flex-basis: auto;
		}
	}
	/* Data review: on a phone a check's chain of values reads one step per line, so a leading "→"
	   or "Σ" starts its own step instead of hanging alone at a wrap. */
	@media (max-width: 560px) {
		.check .eq {
			flex-direction: column;
			align-items: flex-start;
		}
		/* Owner review: stacked as rows, the steps need no "·", "→" or "Σ" between them (one hung alone on a line). */
		.check .eq .op {
			display: none;
		}
	}
	/* "Reproduce it yourself": a section's commands, folded, one per line, wrapping on a phone. */
	.cmds {
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}
	.repro > p {
		margin-bottom: 0;
	}
	.etfuncovered {
		margin-top: var(--sp-3);
	}
	.nw {
		white-space: nowrap;
	}
	/* Data review: the ETF tables read as label/value rows below 900 px (at 768 px the six columns
	   ran past the panel's edge). The label takes its own grid column, so a long label pushes its row
	   down instead of overlapping the next one (app.css draws it absolutely, for phones only). */
	@media (max-width: 900px) {
		table.etfstack,
		table.etfstack tbody,
		table.etfstack tr {
			display: block;
			width: 100%;
		}
		table.etfstack thead {
			position: absolute;
			width: 1px;
			height: 1px;
			overflow: hidden;
			clip-path: inset(50%);
		}
		table.etfstack tr {
			padding-block: var(--sp-2);
			border-bottom: 1px solid var(--line);
		}
		.panel table.etfstack tbody tr:last-child {
			border-bottom: 0;
		}
		table.etfstack td,
		table.etfstack td.r {
			display: grid;
			grid-template-columns: minmax(0, 32%) minmax(0, 1fr);
			align-items: start;
			gap: var(--sp-3);
			width: 100%;
			padding: var(--sp-1) 0;
			border: 0;
			white-space: normal;
			overflow-wrap: anywhere;
			text-align: left;
		}
		table.etfstack td::before {
			content: attr(data-label);
			position: static;
			width: auto;
			color: var(--muted);
			font-family: var(--body);
			font-size: var(--text-sm);
			white-space: normal;
		}
	}
	/* MP: a sealed value is a note, not a figure (it sat in the big number type and read as a placeholder). */
	.l2value.sealed {
		font-family: var(--body);
		font-size: var(--text-sm);
		font-weight: 400;
		color: var(--muted);
	}
</style>
