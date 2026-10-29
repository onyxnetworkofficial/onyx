<script lang="ts">
	import { HELD_BACK } from '#lib/components/HeldBack.svelte';
	import { onMount } from 'svelte';
	import Pill from '#lib/components/Pill.svelte';
	import OnyxSlab from '#lib/components/OnyxSlab.svelte';
	import Skeleton from '#lib/components/Skeleton.svelte';
	import DataTable from '#lib/components/DataTable.svelte';
	import { COLOR } from '#lib/chart.ts';
	import { group, int, short } from '#lib/format.ts';
	import { TESTNET_RUNS } from '#lib/runs.ts';

	// A recorded run of scripts/demo-local.sh --dispute --json (M8 rehearsal on
	// a local anvil chain) and the shared Rust/Solidity fee-split vectors.
	type Step = { step: string; txs: number; secs: number; block: number };
	type Run = {
		chain: string;
		mode: string;
		data: string;
		rows: number;
		miners: number;
		validators: number;
		round: {
			round: number;
			root: string;
			open_tx: string;
			release_tx: string;
			checkers: number;
			agrees: number;
			verdicts: number;
			standbys: number;
			challenge_window_blocks: number;
			bond_micro_usdc: string;
			first_miner_micro_usdc: string;
			other_miner_micro_usdc: string;
			validator_fee_micro_usdc: string;
			/** `null` in a rights-fallback build (ROADMAP L14): a label-derived total. */
			okx: { net_sats: string; tx_count: number; matches_python_recount: boolean } | null;
			served: { unpaid_status: number; paid_status: number; price_micro_usdc: string; client_checked_chain_root: boolean; contract_verify_row: boolean };
		};
		dispute: {
			honest_root: string;
			forged_root: string;
			final_root: string;
			validators_disagreed: number;
			panel_size: number;
			honest_miner_credit_micro_usdc: string;
			cheater_credit_micro_usdc: string;
			cheater_bond_taken_micro_usdc: string;
		} | null;
		steps: Step[];
		totals: { txs: number; blocks: number; secs: number };
	};
	type Split = {
		amount: string;
		miners: number;
		validators: number;
		perMiner: string;
		perValidator: string;
		label: string;
		treasury: string;
		minersBps: number;
		validatorsBps: number;
		labelsBps: number;
		treasuryBps: number;
		labelSet: boolean;
	};

	// The full runs on Base Sepolia (scripts/build-testnet-run.py, from each run's
	// run log and console output): every transaction links to sepolia.basescan.org.
	type Tx = { fn: string; hash: string; url: string; src: string };
	type NetStep = { id: string; title: string; check: string; log: string[]; txs: Tx[] };
	type NetRun = {
		verdict: string;
		network: string;
		chain_id: number;
		date: string;
		started: string | null;
		explorer: string;
		contracts: { name: string; address: string; url: string }[];
		deploy: { txs: number; l2_gas: number };
		data: string;
		rows: number;
		round: { root: string; shard: string; seconds_to_base_finality: number; miners: number; validators: number };
		totals: { run_txs: number; sent_txs: number; l2_gas: number; l2_eth: string; l1_eth: string; secs: number; sweep_txs: number; linked_txs: number };
		// Runs from 2026-10-13 on: the metrics registered with a floor, and the API's indexer step (6a).
		registry?: { metric: string; floor: number }[];
		indexer?: { from_block: number; to_block: number; get_logs_calls: number; finalized_block: number; level: number } | null;
		// Runs from 2026-10-14 on (ROADMAP A2): a reserve round settled (step 5b) and one of its rows sold (step 8).
		reserve?: {
			round: number;
			blocks: string;
			gate: string;
			level: number;
			price_micro_usdc: string;
			/** `null` in a rights-fallback build (ROADMAP L14). */
			okx_btc: string | null;
			okx_block: number;
			okx_cross_check: string;
			binance_status: number;
		};
		steps: NetStep[];
	};

	// What the chain indexer (ADR-0033) finds on Base Sepolia: a read-only read by
	// scripts/read-indexer-sepolia.py, pinned to one block, of the settlement's Finalized
	// events and the contract's own getFinalizedRoot for each.
	type IdxEvent = {
		round: number;
		shard: string;
		root: string;
		binding: string;
		block: number;
		log_index: number;
		utc: string;
		tx: string;
		tx_url: string;
		run_step: string | null;
		run_fn: string | null;
		view: { root: string; binding: string; finalized_at: number };
		agrees: boolean;
		state: { status: string; closed: boolean };
	};
	type IdxRead = {
		generated_by: string;
		rpc: string;
		chain_id: number;
		settlement: string;
		settlement_url: string;
		read_at_block: number;
		read_at_utc: string;
		deploy: { block: number; code_hex_chars_before: number; code_hex_chars_at: number; ok: boolean };
		scan: { from: number; to: number; span: number; get_logs_calls: number };
		events: IdxEvent[];
		all_agree: boolean;
	};
	const INDEXER_READ = 'run/indexer-base-sepolia-2026-10-12.json';
	let idx = $state.raw<IdxRead | null>(null);
	let idxError = $state('');
	const agreeing = $derived(idx ? idx.events.filter((e) => e.agrees).length : 0);
	// Every recorded run, latest first; the picker chooses which one is drawn.
	let nets = $state.raw<{ file: string; run: NetRun | null; error: string }[]>(TESTNET_RUNS.map((file) => ({ file, run: null, error: '' })));
	let pick = $state(0);
	// The newest runs as buttons; every run in a dropdown once there are more (and always on phones).
	const CHIPS = 3;
	const label = (n: { file: string; run: NetRun | null }, i: number) =>
		`${n.run?.date ?? n.file.replace(/^.*-(\d{4}-\d{2}-\d{2})\.json$/, '$1')}${i === 0 ? ' · latest' : ''}${n.run ? ` · ${int(n.run.totals.sent_txs)} txs` : ''}`;
	// The run's date from its file name, so the heading has its final length before the file
	// loads (W9: the date arriving later wrapped the badges and moved the picker, CLS 0.033).
	const fileDate = (f: string) => f.replace(/^.*-(\d{4}-\d{2}-\d{2})\.json$/, '$1');
	const net = $derived(nets[pick]?.run ?? null);
	const netError = $derived(nets[pick]?.error ?? '');
	// The run whose settlement contract the indexer read was made on.
	const idxRun = $derived(idx ? (nets.find((n) => n.run?.contracts.some((c) => c.address === idx?.settlement))?.run ?? null) : null);
	const latest = $derived(nets[0]?.run ?? null);
	// A step's transactions sit behind one "Show N transactions" control past this many
	// (W2: about 200 small links made the page a long, hard-to-tap list on phones).
	const OPEN_MAX = 2;
	// How many transactions of each function a step sent, in first-seen order.
	const fnCounts = (txs: Tx[]) => {
		const m = new Map<string, number>();
		for (const t of txs) m.set(t.fn, (m.get(t.fn) ?? 0) + 1);
		return [...m].map(([fn, k]) => (k > 1 ? `${fn} ×${k}` : fn)).join(' · ');
	};

	let run = $state.raw<Run | null>(null);
	let splits = $state.raw<Split[]>([]);
	// The free-read rule's numbers, from the defaults in the API code
	// (fixtures/revenue/contributor-policy.json, drift-checked by a Rust test).
	type Policy = { readsPerMinute: number; rowsPerDay: number; windowDays: number };
	let policy = $state.raw<Policy | null>(null);
	// The prices, from the defaults in the API code
	// (fixtures/revenue/pricing-defaults.json, drift-checked by a Rust test; AUD-2-F11).
	type Prices = { answerMicroUsdc: number; datasetMicroUsdc: number; datasetFloorRows: number };
	let prices = $state.raw<Prices | null>(null);
	const CENTS = new Intl.NumberFormat('en-US', { maximumFractionDigits: 4 });
	const cents = (micro: number) => `${CENTS.format(micro / 10_000)}¢`;
	let loadError = $state('');

	onMount(() => {
		TESTNET_RUNS.forEach((file, i) =>
			fetch(file)
				.then((r) => (r.ok ? r.json() : Promise.reject(new Error(`${file}: HTTP ${r.status}`))))
				.then((j: NetRun) => (nets = nets.map((n, k) => (k === i ? { ...n, run: j } : n))))
				.catch((e) => (nets = nets.map((n, k) => (k === i ? { ...n, error: String(e) } : n))))
		);
		fetch(INDEXER_READ)
			.then((r) => (r.ok ? r.json() : Promise.reject(new Error(`${INDEXER_READ}: HTTP ${r.status}`))))
			.then((j) => (idx = j))
			.catch((e) => (idxError = String(e)));
		fetch('run/live-run.json')
			.then((r) => (r.ok ? r.json() : Promise.reject(new Error(`run/live-run.json: HTTP ${r.status}`))))
			.then((j) => (run = j))
			.catch((e) => (loadError = String(e)));
		fetch('data/revenue-vectors.json')
			.then((r) => (r.ok ? r.json() : Promise.reject(new Error(`revenue vectors: HTTP ${r.status}`))))
			.then((j: { cases: Split[] }) => (splits = j.cases))
			.catch((e) => (loadError = String(e)));
		fetch('data/contributor-policy.json')
			.then((r) => (r.ok ? r.json() : Promise.reject(new Error(`contributor policy: HTTP ${r.status}`))))
			.then((j: Policy) => (policy = j))
			.catch((e) => (loadError = String(e)));
		fetch('data/pricing-defaults.json')
			.then((r) => (r.ok ? r.json() : Promise.reject(new Error(`pricing defaults: HTTP ${r.status}`))))
			.then((j: Prices) => (prices = j))
			.catch((e) => (loadError = String(e)));
	});

	// What each recorded step means; the numbers come from the run.
	const NAMES: Record<string, string> = {
		setup: 'Stakes and bonds posted',
		'r1.mine': 'Miners run the recipe',
		'r1.open': 'Round opens',
		'r1.commit_reveal': 'Commit, then reveal',
		'r1.verdicts': 'Validators recheck',
		'r1.finalize': 'Root is final',
		'r1.challenge_window': 'Nobody challenges',
		'r1.withdraw': 'Everyone is paid',
		'r1.serve_pay_verify': 'Served, paid, verified',
		'r2.mine': 'Miners run the recipe',
		'r2.open': 'Round opens',
		'r2.commit_reveal': 'One miner reveals a fake root',
		'r2.verdicts': 'Validators disagree',
		'r2.finalize': 'Not final: held',
		'r2.rerun_case_and_draw': 'A panel is drawn',
		'r2.panel_votes_and_resolve': 'The panel reruns and votes',
		'r2.settle_case': 'The cheater loses its bond'
	};

	const usdc = (micro: string | bigint) => {
		const v = BigInt(micro);
		const whole = v / 1_000_000n;
		const frac = (v % 1_000_000n).toString().padStart(6, '0').replace(/0+$/, '');
		return frac ? `${whole}.${frac}` : `${whole}`;
	};
	// Text from the run records: long numbers get thousands separators ("48113584" -> "48,113,584",
	// "971192-971199" -> "971,192–971,199"). Dates, ports, chain ids, decimals and hex stay as they are.
	const nums = (t: string) =>
		t
			.replace(/(?<![\w.:…\-−+])(\d{4,})(?:-|\.\.)(\d{4,})(?![\w-])/g, (_, a: string, b: string) => `${int(a)}–${int(b)}`)
			.replace(/(?<![\w.:…\-−+]|chain |chain id )(\d{4,})(?![\w-]|\.\d)/g, (m: string) => int(m));
	// A rights-fallback build replaces each company BTC figure in the records with a bracketed
	// marker; here it reads as plain words.
	const plain = (t: string) =>
		nums(
			t
				.replace(/\(OKX \[company total held back\] in the spec's params\)/g, "(a made-up OKX total in the spec's params)")
				.replace(/\[company total held back\] = the scan/g, 'the same company total as the scan (held back until data rights are cleared)')
				.replace(/\[company total held back\]/g, 'a company total (held back until data rights are cleared)')
		);
	// A step's recorded check in plain words: HTTP codes, contract method and RPC names say what
	// they mean. The exact recorded text moves into the step's Log fold (see `logOf`).
	const CHECK_WORDS: [RegExp, string][] = [
		[/\bOKX 402 unpaid, 200 paid\b/g, 'OKX refused until paid, then served'],
		[/^402 unpaid, 200 paid\b/g, 'Refused until paid, then served'],
		[/client matched getFinalizedRoot/g, "the client matched the contract's final root"],
		[/; verifyRow true; verifyRowLeveled true at level (\d+)/g, '; both row checks passed at proof level $1'],
		[/\bBinance 403\b/g, 'Binance refused (no independent cross-check)'],
		[/ in (\d+) eth_getLogs calls/g, ' in $1 log reads'],
		[/getFinalizedRoot and the held rows agree/g, "the contract's final root and the held rows agree"],
		[/\bOnyxRevenue's reporter\b/g, "the reporter for Onyx's revenue contract"],
		[/\bin OnyxRevenue\b/g, "in Onyx's revenue contract"]
	];
	const say = (t: string) => plain(CHECK_WORDS.reduce((a, [re, to]) => a.replace(re, to), t));
	const logOf = (s: { check: string; log: string[] }) => (say(s.check) === plain(s.check) ? s.log : [`check: ${s.check}`, ...s.log]);
	// Contract names in words ("OnyxRevenue (x402 payTo)" -> "Onyx's revenue contract"); the code name stays in the tooltip.
	const contractName = (n: string) => n.replace(/^Onyx([A-Z][a-z]+)(?: \(.*\))?$/, (_, w: string) => `Onyx's ${w.toLowerCase()} contract`);
	// Seconds also as minutes ("2,334 s" -> "39 min").
	const mins = (secs: number) => {
		const m = Math.round(secs / 60);
		return m < 60 ? `${m} min` : `${Math.floor(m / 60)} h ${m % 60} min`;
	};
	// ETH with many decimals, rounded to two significant figures for display.
	const eth = (v: string) => {
		const x = Number(v);
		return x === 0 ? '0' : x.toPrecision(2).replace(/(\.\d*?)0+$/, '$1').replace(/\.$/, '');
	};
	const btcNet = (sats: string) => {
		const v = BigInt(sats);
		const a = v < 0n ? -v : v;
		return `${v < 0n ? '−' : ''}${a / 100_000_000n}.${(a % 100_000_000n).toString().padStart(8, '0').slice(0, 2)}`;
	};

	// Steps placed on one block axis, so each bar is to scale.
	const placed = $derived.by(() => {
		if (!run) return [];
		let prev = 0;
		return run.steps.map((s) => {
			const from = prev;
			prev = s.block;
			return { ...s, from, name: NAMES[s.step] ?? s.step, group: s.step.startsWith('r2.') ? 'dispute' : 'honest' };
		});
	});
	const total = $derived(run?.totals.blocks ?? 1);
	const honest = $derived(placed.filter((p) => p.group === 'honest'));
	// W6: the bars' numbers as a folded table, like every chart's.
	const STEP_COLS = [{ label: 'Step' }, { label: 'From block', right: true }, { label: 'To block', right: true }, { label: 'Transactions', right: true }, { label: 'Seconds', right: true }];
	const stepRows = (list: typeof placed) => list.map((s) => [s.name, int(s.from), int(s.block), int(s.txs), s.secs.toFixed(1)]);
	const dispute = $derived(placed.filter((p) => p.group === 'dispute'));
	const detail = (s: { step: string }): string => {
		if (!run) return '';
		const r = run.round;
		switch (s.step) {
			case 'setup':
				return `${run.validators} validators stake`;
			case 'r1.commit_reveal':
				return `${run.miners} miners, ${usdc(r.bond_micro_usdc)} USDC bond each`;
			case 'r1.verdicts':
				return `${r.checkers} rechecked, ${r.agrees}/${r.verdicts} agree, ${r.standbys} stand by`;
			case 'r1.challenge_window':
				return `${r.challenge_window_blocks}-block window`;
			case 'r1.withdraw':
				return `checked payouts`;
			case 'r1.serve_pay_verify':
				return r.served.unpaid_status === 402 && r.served.paid_status === 200 ? 'refused → paid → served' : `${r.served.unpaid_status} → paid → ${r.served.paid_status}`;
			case 'r2.verdicts':
				return run.dispute ? `${run.dispute.validators_disagreed} disagree` : '';
			case 'r2.rerun_case_and_draw':
				return run.dispute ? `${run.dispute.panel_size} standbys` : '';
			case 'r2.settle_case':
				return run.dispute ? `${usdc(run.dispute.cheater_bond_taken_micro_usdc)} USDC` : '';
			default:
				return '';
		}
	};

	// Two real vectors: the per-answer price, and one that leaves rounding dust.
	const DEFAULT = (c: Split) => c.minersBps === 5000 && c.validatorsBps === 2000 && c.labelsBps === 1000 && c.treasuryBps === 2000 && c.labelSet && c.miners === 1 && c.validators === 4;
	const main = $derived(splits.find((c) => DEFAULT(c) && c.amount === (run?.round.served.price_micro_usdc ?? '2500')) ?? null);
	const dust = $derived(splits.find((c) => DEFAULT(c) && c.amount === '1000003') ?? null);
	const parts = $derived.by(() => {
		if (!main) return [];
		const amt = Number(main.amount);
		const v = [
			{ key: 'miners', name: 'Miner', share: BigInt(main.perMiner) * BigInt(main.miners), note: `${main.miners} miner` },
			{ key: 'validators', name: 'Validators', share: BigInt(main.perValidator) * BigInt(main.validators), note: `${main.validators} × ${int(Number(main.perValidator))}` },
			{ key: 'labels', name: 'Label maker', share: BigInt(main.label), note: 'label set payee' },
			{ key: 'treasury', name: 'Onyx', share: BigInt(main.treasury), note: 'treasury' }
		];
		let x = 0;
		return v.map((p) => {
			const w = (Number(p.share) / amt) * 100;
			const out = { ...p, x, w };
			x += w;
			return out;
		});
	});
	const dustLeft = $derived(
		dust ? BigInt(dust.amount) - BigInt(dust.perMiner) * BigInt(dust.miners) - BigInt(dust.perValidator) * BigInt(dust.validators) - BigInt(dust.label) - (BigInt(dust.amount) * BigInt(dust.treasuryBps)) / 10_000n : 0n
	);

	// ROADMAP P5: what each role needs, from docs/ops/hardware.md. Every figure there is measured
	// (machine, date and method named), copied from the client's own page with the date read, a
	// limit in Onyx's shipped code or config, or the sum of those; nothing here is new. "Recommended" is a machine Onyx was measured on. Dates use non-breaking
	// hyphens (U+2011) so a narrow cell never splits one.
	const HARDWARE: { role: string; min: string; rec: string; from: string }[] = [
		{ role: 'Contributor: Bitcoin full node + recipes', min: '2 CPUs · 6 GiB RAM · 836 GiB disk, unpruned', rec: '12 CPUs · 60 GiB · NVMe', from: "Bitcoin Core's 1,907 MiB (bitcoin.org) + 4,112 MiB for a recipe run, rounded up; chain size measured 2026‑10‑18" },
		{ role: 'Validator: reruns the recipes', min: 'As a contributor, plus 4 GiB free per run at once', rec: '12 CPUs · 60 GiB · NVMe', from: "The host's memory cap; measured machine" },
		{ role: 'Entity indexer and reserve path', min: '2,033 GiB disk (node, indexes, block store)', rec: '24 CPUs · 91 GiB · NVMe', from: "Measured on the maintainer's indexer, 2026‑10‑18" },
		{ role: 'Node operator: serves the API', min: '1 CPU · 1 GiB free', rec: '2 GiB RAM, the shipped cap', from: '25.2 MiB peak in a 133‑minute soak' },
		{ role: 'Buyer: checks in the browser', min: 'A browser; the Data page loads 2,138 KiB once', rec: 'Nothing to install', from: 'Lighthouse, mobile, 2026‑10‑11' },
		{ role: 'Coordinator: settles rounds', min: 'Not measured yet', rec: '—', from: 'Only run with the miners so far' }
	];
</script>

<svelte:head><title>Live run · Onyx</title></svelte:head>

<OnyxSlab>
	<h1>Live run</h1>
	<div class="slabside" data-testid="slabside">
		<p class="slablede">Base Sepolia, a public testnet: {TESTNET_RUNS.length} full runs, every transaction linked · and an earlier rehearsal on a local test chain</p>
	</div>
</OnyxSlab>

<div class="wrap">
	<section class="section" data-testid="testnet-run">
		<!-- W12 round 10: on a wide screen the run's heading sits at the left with its badges and the run
		     picker in a column beside it (the picker had a row of its own under the heading). -->
		<div class="runhead" class:pickhead={nets.length > 1 && nets.length <= CHIPS}>
		<div class="sh">
			<h2>On Base Sepolia{#if nets[pick]}, {net?.date ?? fileDate(nets[pick].file)}{/if}</h2>
			<span class="badges">
				<span class="tag">Public testnet{#if net}{" "}· chain {net.chain_id}{/if}</span>
				{#if net}<Pill state="good" text={net.verdict === 'REHEARSAL PASSED' ? 'Run passed' : net.verdict} />{/if}
			</span>
		</div>
		{#if nets.length > 1}
			<div class="picker" class:many={nets.length > CHIPS} data-testid="net-picker">
				<div class="seg chips" role="group" aria-label="Latest runs">
					{#each nets.slice(0, CHIPS) as n, i (n.file)}
						<button type="button" class="chipbtn" aria-pressed={i === pick} onclick={() => (pick = i)} data-testid="net-pick">
							<span class="mono">{n.run?.date ?? n.file.replace(/^.*-(\d{4}-\d{2}-\d{2})\.json$/, '$1')}</span>{#if n.run}<span class="sub">{i === 0 ? 'latest · ' : ''}{int(n.run.totals.sent_txs)} txs</span>{/if}
						</button>
					{/each}
				</div>
				<label class="picksel">
					<span class="meta">{nets.length} runs</span>
					<select bind:value={pick} aria-label="Choose a run ({nets.length} recorded)" data-testid="net-select">
						{#each nets as n, i (n.file)}
							<option value={i}>{label(n, i)}</option>
						{/each}
					</select>
				</label>
			</div>
		{/if}
		</div>
		{#if netError}
			<p>Could not load the Base Sepolia run: {netError}</p>
		{:else if !net}
			<Skeleton rows={3} label="Loading the Base Sepolia run" />
		{:else}
			<div class="stats" data-testid="net-body" data-date={net.date}>
				<!-- Sent: the deploys plus the run's own transactions. Linked: the run's own (the deploys
				     are on each contract's page), the leftover-gas returns, and the payment transactions
				     the API side sent (read from the console output, not counted as sent by the run). -->
				<div class="stat"><b data-testid="net-sent">{int(net.totals.sent_txs)}</b><span>transactions sent by the run, {int(net.deploy.txs)} contract deploys included</span></div>
				<div class="stat"><b>{int(net.totals.linked_txs)}</b><span>linked below: {int(net.totals.run_txs)} run steps, {int(net.totals.sweep_txs)} gas returns{#if net.totals.linked_txs - net.totals.run_txs - net.totals.sweep_txs > 0}, {int(net.totals.linked_txs - net.totals.run_txs - net.totals.sweep_txs)} payments{/if}</span></div>
				<div class="stat"><b>{mins(net.round.seconds_to_base_finality)}</b><span>root to Base finality ({int(net.round.seconds_to_base_finality)} s)</span></div>
				<div class="stat"><b title="{net.totals.l2_eth} ETH">{eth(net.totals.l2_eth)}</b><span>ETH of gas</span></div>
			</div>
			<div class="notebar" class:duo={!!net.reserve} data-testid="net-notes">
			<p class="fine">
				{nums(net.data)} · {net.round.miners} miners, {net.round.validators} validators rechecking · {int(net.totals.secs)} s ({mins(net.totals.secs)}) start to finish · {int(net.totals.l2_gas)} L2 gas ·
				{net.deploy.txs} deploy + {net.totals.run_txs} run transactions, then {net.totals.sweep_txs} returning leftover gas
			</p>
			{#if net.reserve}
				{@const r = net.reserve}
				<p class="fine" data-testid="net-reserve">
					Reserve round {r.round} (blocks {nums(r.blocks)}): one OKX reserve row, {r.okx_btc === null ? 'its value held back until data rights are cleared,' : `${r.okx_btc} BTC`} at block {int(r.okx_block)}, sold for {int(Number(r.price_micro_usdc))} micro-USDC after serving check
					<span class="mono">{r.gate}</span> cleared it (proof level {r.level}, {r.okx_cross_check === 'published-por-file' ? "checked against OKX's published proof-of-reserves file" : `cross-check ${r.okx_cross_check}`}); Binance refused before any payment (no independent cross-check).
				</p>
			{/if}
			</div>
			<div class="facts contracts">
				{#each net.contracts as c (c.address)}
					<a class="fact" href={c.url} target="_blank" rel="noopener" data-testid="net-contract">
						<span class="muted" title={c.name}>{contractName(c.name)}</span>
						<b class="mono" title={c.address}>{short(c.address, 12, 8)}</b>
					</a>
				{/each}
			</div>
			<div class="panel netbox">
			<!-- W12 round 6: each step reads across on a wide screen (what happened and its check on the
			     left, its transactions and log on the right), its number on a rail; 14 steps ran 2,500 px. -->
			<ol class="netsteps across" data-testid="net-steps">
				{#each net.steps as s, i (s.id)}
					<li data-testid="net-step">
						<span class="n mono">{i + 1}</span>
						<div class="body">
							<div class="what">
							<b class="ititle">{plain(s.title)}</b>
							<span class="chk"><span aria-hidden="true">✓</span> {say(s.check)}</span>
							</div>
							{#snippet txList(txs: Tx[])}
								<ol class="txrows">
									{#each txs as t (t.hash)}
										<li>
											<a class="txrow" href={t.url} target="_blank" rel="noopener" title={t.hash} data-testid="net-tx"
												><span class="fn mono">{t.fn}</span> <span class="mono">{short(t.hash, 8, 4)}</span><span class="ext" aria-hidden="true">↗</span></a
											>
										</li>
									{/each}
								</ol>
							{/snippet}
							{#if s.txs.length || s.log.length}
							<div class="did">
							{#if s.txs.length > OPEN_MAX}
								<details class="txset" data-testid="net-txset">
									<summary><span class="show"><span class="closed">Show {s.txs.length} transactions</span><span class="opened">Hide {s.txs.length} transactions</span></span><span class="fns mono">{fnCounts(s.txs)}</span></summary>
									{@render txList(s.txs)}
								</details>
							{:else if s.txs.length}
								{@render txList(s.txs)}
							{/if}
							{#if s.log.length}
								<details class="logset">
									<summary>Log</summary>
									{#each logOf(s) as l, li (li)}<p class="mono log">{plain(l)}</p>{/each}
								</details>
							{/if}
							</div>
							{/if}
						</div>
					</li>
				{/each}
			</ol>
			<!-- One column: two CSS columns split this note mid-sentence. -->
			<div class="pfoot">
			<p class="fine">
				Contracts deployed by the run ({net.deploy.txs} transactions, on each contract's page).{#if run && run.round.root === net.round.root}{' '}
					Root <span class="mono" title={net.round.root}>{short(net.round.root)}</span> is the same as the local rehearsal's below: same live blocks, same recipe.{/if}
				This page is built from the run's own log and console output.
			</p>
			<details class="built">
				<summary>How this was built</summary>
				<p class="fine">Script: <span class="mono">scripts/build-testnet-run.py</span>, from the run log and the console output of <span class="mono">scripts/demo-testnet.sh --testnet</span>.</p>
			</details>
			</div>
			</div>
		{/if}
	</section>

	<section class="section rehearsal" data-testid="indexer-run">
		<div class="sh">
			<h2>The API finds its shards on chain</h2>
			<span class="badges">
				<span class="tag">Read-only{#if idx}&nbsp;· block {int(idx.read_at_block)}{/if}{#if idxRun}&nbsp;· the <span class="nw">{idxRun.date}</span> run's contract{/if}</span>
				{#if idx}<Pill state={idx.all_agree ? 'good' : 'bad'} text="{agreeing} of {idx.events.length} roots agree" />{/if}
			</span>
		</div>
		{#if idxError}
			<p>Could not load the indexer read: {idxError}</p>
		{:else if !idx}
			<Skeleton rows={3} label="Loading the indexer read" />
		{:else}
			<!-- W12 round 6: the two ledes take a column each on a wide screen (app.css .duo). -->
			<div class="duo">
			<p class="muted lede">
				Nobody hands the API a list of servable shards. Its indexer reads the settlement contract's “root is final” events from the block the contract was deployed in, then asks the contract
				itself whether each root is final. The event is only a hint; the API serves a shard only when the contract agrees and the rows it holds hash to that root.
			</p>
			<p class="muted lede" data-testid="idx-scope">
				This read is of the settlement contract{#if idxRun}{' '}the {idxRun.date} run deployed{/if}.{#if latest?.indexer && latest !== idxRun}{' '}
					In the {latest.date} run the API's own indexer did the same during the run: blocks {int(latest.indexer.from_block)}–{int(latest.indexer.to_block)} in {latest.indexer.get_logs_calls} log
					reads, round 1 found final at block {int(latest.indexer.finalized_block)} and served at level {latest.indexer.level} (the step "The API finds the root on chain" above).{/if}
			</p>
			</div>
			<div class="stats">
				<div class="stat"><b data-testid="idx-events">{idx.events.length}</b><span>“root is final” events found</span></div>
				<div class="stat"><b data-testid="idx-calls">{idx.scan.get_logs_calls}</b><span>log reads of at most {idx.scan.span} blocks each</span></div>
				<div class="stat"><b>{int(idx.scan.to - idx.scan.from + 1)}</b><span>blocks read</span></div>
				<div class="stat"><b data-testid="idx-agree">{agreeing}/{idx.events.length}</b><span>roots the contract confirms</span></div>
			</div>
			<ul class="idxchecks panel" data-testid="idx-checks">
				<li class="chk" data-testid="idx-deploy"><span aria-hidden="true">✓</span> Deploy block {int(idx.deploy.block)}: no contract code at block {int(idx.deploy.block - 1)}, code at {int(idx.deploy.block)}, so no event is older</li>
				<li class="chk"><span aria-hidden="true">✓</span> Blocks {int(idx.scan.from)}–{int(idx.scan.to)} read in ranges of at most {idx.scan.span} blocks (the public node's cap)</li>
				<li class="chk"><span aria-hidden="true">✓</span> Each root read back from the contract at block {int(idx.read_at_block)} ({idx.read_at_utc} UTC)</li>
			</ul>
			<div class="panel netbox">
			<ol class="netsteps" data-testid="idx-events-list">
				{#each idx.events as e (e.tx + e.log_index)}
					<li data-testid="idx-event">
						<span class="n mono">{e.round}</span>
						<div class="body">
							<b class="ititle">Round {e.round}: Finalized at block {int(e.block)}</b>
							<span class="meta emit">
								<span>{e.utc} UTC</span>{' '}<span><span class="sep">·</span> emitted by</span>
								<a class="tx mono" href={e.tx_url} target="_blank" rel="noopener" title={e.tx} data-testid="idx-tx">{e.run_fn ?? 'tx'} {short(e.tx, 8, 4)}</a>
								{#if e.run_step}<span><span class="sep">·</span> the run's {e.run_step.startsWith('r2.') ? 'dispute' : 'round 1'} step</span>{/if}
							</span>
							<span class="mono kv"><span class="muted">shard</span> <span title={e.shard} data-testid="idx-shard">{short(e.shard)}</span></span>
							<span class="mono kv"><span class="muted">event root</span> <span title={e.root} data-testid="idx-root">{short(e.root)}</span></span>
							<span class="mono kv"><span class="muted">contract says</span> <span title={e.view.root}>{short(e.view.root)}</span></span>
							{#if e.agrees}
								<span class="chk" data-testid="idx-agrees"><span aria-hidden="true">✓</span> The contract names the same root and binding, final since block {int(e.view.finalized_at)}{e.state.closed ? ', after a dispute case' : ''}</span>
							{:else}
								<span class="bad">✗ The contract does not confirm this root: not served</span>
							{/if}
						</div>
					</li>
				{/each}
			</ol>
			<div class="pfoot">
			<p class="fine">
				This read only reads (no key, no transaction) from Base Sepolia's public node; a rerun writes the same file. The API's own indexer applies the same rules, and serves a shard only when
				the rows it holds also hash to the contract's root. <a href={idx.settlement_url} target="_blank" rel="noopener">The settlement contract</a> on basescan.
			</p>
			<details class="built">
				<summary>How this was built</summary>
				<p class="fine">Read by <span class="mono">{idx.generated_by}</span> from <span class="mono">{idx.rpc}</span>. The API's indexer: <span class="mono">crates/onyx-api/src/indexer.rs</span>.</p>
			</details>
			</div>
			</div>
		{/if}
	</section>

	{#if loadError}
		<p class="section">Could not load the run: {loadError}</p>
	{:else if !run}
		<Skeleton rows={4} label="Loading the rehearsal run" />
	{:else}
		<section class="section rehearsal" data-testid="anvil-run">
			<div class="sh">
				<h2>Local rehearsal</h2>
				<span class="tag">Local test chain · not a public network</span>
			</div>
			<div class="stats">
				<div class="stat"><b data-testid="run-txs">{int(run.totals.txs)}</b><span>transactions</span></div>
				<div class="stat"><b>{int(run.totals.blocks)}</b><span>blocks</span></div>
				<div class="stat"><b>{Math.round(run.totals.secs)} s</b><span>start to finish</span></div>
				<div class="stat"><b>{run.miners} + {run.validators}</b><span>miners + validators</span></div>
			</div>
			<div class="notebar">
				<p class="fine">{run.chain.replace(/\(anvil, /, '(')} · {nums(run.data)} · recorded with a dispute round</p>
				<details class="built">
					<summary>How this was built</summary>
					<p class="fine">Recorded with <span class="mono">scripts/demo-local.sh --dispute --json</span>.</p>
				</details>
			</div>
		</section>

		<section class="section">
			<div class="sh">
				<h2>An honest round</h2>
				<Pill state="good" text="Served and verified" />
			</div>
			<div class="panel ganttbox">
			<ol class="gantt" data-testid="honest">
				{#each honest as s, i (s.step)}
					<li>
						<span class="n mono">{i + 1}</span>
						<div class="lab">
							<b>{s.name}</b>
							<span class="muted">{detail(s)}</span>
						</div>
						<div class="track" aria-hidden="true">
							<i style:left="{(s.from / total) * 100}%" style:width="max(3px, {((s.block - s.from) / total) * 100}%)"></i>
						</div>
						<div class="num mono"><span data-testid="txs">{s.txs}</span> tx · {s.secs.toFixed(1)} s</div>
					</li>
				{/each}
			</ol>
			<div class="ganttaxis" aria-hidden="true"><span>block 0</span><span>block {int(total)}</span></div>
			<DataTable caption="An honest round, step by step, on the rehearsal's block axis" columns={STEP_COLS} rows={stepRows(honest)} testid="honest-table" />
			<div class="pfoot"><p class="fine">Bars sit on one block axis (0–{int(total)}), so their lengths are to scale.</p></div>
			</div>
			<div class="facts fit">
				<div class="fact">
					<span class="muted">Root on chain</span>
					<b class="mono" title={run.round.root}>{short(run.round.root)}</b>
				</div>
				<div class="fact">
					<span class="muted">First miner / other miner</span>
					<b class="mono">{usdc(run.round.first_miner_micro_usdc)} / {usdc(run.round.other_miner_micro_usdc)} USDC</b>
				</div>
				<div class="fact">
					<span class="muted">OKX net over the range</span>
					<b class="mono">{run.round.okx ? `${btcNet(run.round.okx.net_sats)} BTC` : 'Held back'}</b>
				</div>
				<div class="fact">
					<span class="muted">Client checks</span>
					<b><span class="nw">{run.round.served.client_checked_chain_root ? 'own chain read' : '–'}</span> · <span class="nw">{run.round.served.contract_verify_row ? 'contract row check' : '–'}</span></b>
				</div>
			</div>
			{#if !run.round.okx}
				<div class="notebar"><p class="fine" data-testid="run-held-back">{HELD_BACK} Payment used a local mock facilitator: no token moved.</p></div>
			{:else if run.round.okx.matches_python_recount}
				<div class="notebar"><p class="fine">Same OKX numbers as an independent Python recount. Payment used a local mock facilitator: no token moved.</p></div>
			{/if}
		</section>

		{#if run.dispute}
			<section class="section">
				<div class="sh">
					<h2>A cheater is caught</h2>
					<Pill state="good" text="Fake root rejected" />
				</div>
				<div class="panel ganttbox">
				<ol class="gantt" data-testid="dispute">
					{#each dispute as s, i (s.step)}
						<li>
							<span class="n mono">{i + 1}</span>
							<div class="lab">
								<b>{s.name}</b>
								<span class="muted">{detail(s)}</span>
							</div>
							<div class="track" aria-hidden="true">
								<i class="d" style:left="{(s.from / total) * 100}%" style:width="max(3px, {((s.block - s.from) / total) * 100}%)"></i>
							</div>
							<div class="num mono">{s.txs} tx · {s.secs.toFixed(1)} s</div>
						</li>
					{/each}
				</ol>
				<div class="ganttaxis" aria-hidden="true"><span>block 0</span><span>block {int(total)}</span></div>
				<DataTable caption="A cheater is caught, step by step, on the rehearsal's block axis" columns={STEP_COLS} rows={stepRows(dispute)} testid="dispute-table" />
				<!-- W12 round 4: the chain is the panel's last row (it floated between the panel and the
				     payouts); each arrow travels with the step after it, so a phone never strands one. -->
				<div class="pfoot">
					<div class="eq mono" data-testid="dispute-chain">
						<span class="bad" title={run.dispute.forged_root}>fake {short(run.dispute.forged_root)}</span>
						<span class="eqp"><span class="op">→</span><span>panel of {run.dispute.panel_size}</span></span>
						<span class="eqp"><span class="op">→</span><span class="ok" title={run.dispute.final_root}>final {short(run.dispute.final_root)}</span></span>
					</div>
				</div>
				</div>
				<div class="facts">
					<div class="fact"><span class="muted">Honest miner</span><b class="mono">+{usdc(run.dispute.honest_miner_credit_micro_usdc)} USDC</b></div>
					<div class="fact"><span class="muted">Cheater</span><b class="mono bad" data-testid="cheater">+{usdc(run.dispute.cheater_credit_micro_usdc)} USDC · bond {usdc(run.dispute.cheater_bond_taken_micro_usdc)} taken</b></div>
				</div>
			</section>
		{/if}

		<section class="section">
			<div class="sh">
				<h2>Who earns</h2>
				<span class="meta">Split by the contract</span>
			</div>
			{#if main}
				<div class="panel split" data-testid="split">
					<p class="lead"><b class="mono">{int(Number(main.amount))}</b> micro-USDC ({usdc(main.amount)} USDC, one paid answer) becomes:</p>
					<div class="bar" aria-hidden="true">
						{#each parts as p, i (p.key)}
							<i style:left="{p.x}%" style:width="{p.w}%" style:background={COLOR.cat(i)}></i>
						{/each}
					</div>
					<ul class="shares">
						{#each parts as p, i (p.key)}
							<li>
								<i class="key" style:background={COLOR.cat(i)}></i>
								<span>{p.name}</span>
								<b class="mono" data-testid="share-{p.key}">{int(Number(p.share))}</b>
								<span class="muted">{p.note}</span>
							</li>
						{/each}
					</ul>
					<div class="pfoot">
					{#if dust}
						<p class="fine">
							Rounding dust goes to Onyx: {int(Number(dust.amount))} → miner {int(Number(dust.perMiner))} · validators {dust.validators} × {int(Number(dust.perValidator))} · labels {int(Number(dust.label))} · Onyx {int(Number(dust.treasury))} (+{dustLeft.toString()} dust).
						</p>
					{/if}
					<div class="fine">
						Real cases from the shared test vectors: Rust and the contract must both produce them.
						<details class="built">
							<summary>How this was built</summary>
							<span class="mono">fixtures/revenue/vectors.json</span>
						</details>
					</div>
					</div>
				</div>
			{:else}
				<Skeleton rows={2} label="Loading the fee split" />
			{/if}
			<div class="tiles">
				<div class="tile">
					<b class="ititle">Add data, get data</b>
					{#if policy}
						<span data-testid="contrib-rule">Accepted work in the last {policy.windowDays} days reads fresh rows free: {group(policy.readsPerMinute)} reads a minute, {group(policy.rowsPerDay)} rows a day. Full exports stay paid.</span>
					{:else}
						<span>Accepted work reads fresh rows free. Full exports stay paid.</span>
					{/if}
				</div>
				<div class="tile">
					<b class="ititle">Agents pay per call</b>
					{#if prices}
						<span data-testid="price-rule"><b class="mono">{cents(prices.answerMicroUsdc)}</b> an answer (recommended), <b class="mono">{cents(prices.datasetMicroUsdc)}</b> a fresh dataset: never more than buying every row, never less than {prices.datasetFloorRows} rows' worth.</span>
					{:else}
						<span>A fraction of a cent an answer; a flat price per fresh dataset.</span>
					{/if}
				</div>
				<div class="tile">
					<b class="ititle">Old data is free</b>
					<span>After its fresh window a row costs nothing, and still comes with its proof.</span>
				</div>
			</div>
		</section>
	{/if}

	<!-- P5: drawn once the rehearsal has loaded or failed, as the sections above it: drawn earlier it
	     sat on the short loading page's first screen and so never faded in (W12 round 13). -->
	{#if run || loadError}
	<section class="section" data-testid="hardware">
		<div class="sh">
			<h2>Hardware</h2>
			<span class="meta">Per role, Bitcoin · Ethereum planned</span>
		</div>
		<div class="panel">
			<div class="tablebox"><table class="hw stack">
				<thead><tr><th>Role</th><th>Minimum</th><th>Recommended (measured machine)</th><th>From</th></tr></thead>
				<tbody>
					{#each HARDWARE as h (h.role)}
						<tr data-testid="hw-row">
							<td data-label="Role">{h.role}</td>
							<td data-label="Minimum" class:muted={h.rec === '—'}>{h.min}</td>
							<td data-label="Recommended" class:muted={h.rec === '—'}>{h.rec}</td>
							<td class="muted" data-label="From">{h.from}</td>
						</tr>
					{/each}
				</tbody>
			</table></div>
			<div class="pfoot">
				<p class="fine" data-testid="hw-note">
					Every figure is measured, with its machine and date, read from bitcoin.org, reth.rs or geth.ethereum.org on 2026‑10‑18, a limit in Onyx's shipped code, or a sum of those.
					Below the measured machine nothing is measured. Ethereum has no Onyx adapter yet: reth lists 8 GB+ RAM and at least 1.2 TB of disk, TLC NVMe recommended, for a full node.
				</p>
				<div class="fine">
					The node checks its machine at every start and prints PASS, WARN, FAIL or SKIP per check.
					<details class="built">
						<summary>How this was built</summary>
						Figures: <span class="mono">docs/ops/hardware.md</span>. Check a machine before you start:
						<span class="mono cmd"><span class="nw">onyx-node</span> doctor <span class="nw">--role</span> contributor <span class="nw">--data-dir</span> &lt;dir&gt;</span>.
					</details>
				</div>
			</div>
		</div>
	</section>
	{/if}
</div>

<style>
	.badges {
		display: inline-flex;
		flex-wrap: wrap;
		gap: var(--sp-2);
		align-items: center;
		/* A pill's height (Pill.svelte), so the status pill arriving with the run moves nothing. */
		min-height: calc(var(--text-sm) * var(--lh-body) + 2 * var(--sp-1) + 2px);
	}
	.tag {
		font-family: var(--mono);
		font-size: var(--text-xs);
		color: var(--muted);
		padding: var(--sp-1) var(--sp-3);
		border-radius: var(--radius-pill);
		border: 1px solid var(--line-strong);
	}
	/* Narrow screens: the head's parts are the section's own rows, as before. */
	.runhead {
		display: contents;
	}
	@media (min-width: 1100px) {
		.runhead.pickhead {
			display: grid;
			grid-template-columns: auto minmax(0, 1fr);
			grid-template-rows: auto auto;
			gap: var(--sp-2) var(--sp-8);
			align-items: center;
		}
		.runhead.pickhead > .sh {
			display: contents;
		}
		.runhead.pickhead h2 {
			grid-column: 1;
			grid-row: 1 / span 2;
		}
		.runhead.pickhead .badges {
			grid-column: 2;
			grid-row: 1;
		}
		.runhead.pickhead > .picker {
			grid-column: 2;
			grid-row: 2;
			margin-bottom: 0;
		}
		/* W12 round 11: the switch spans its column to the section's right edge (it stopped 47 px
		   short at 1,280 px); the runs share the spare width, each on one line, so a run's count
		   arriving moves neither outer edge. */
		.runhead.pickhead > .picker > .chips {
			flex: 1 1 auto;
		}
		.runhead.pickhead .chipbtn {
			flex: 1 1 auto;
			white-space: nowrap;
		}
	}
	.picker {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--sp-3) var(--sp-4);
		margin-bottom: var(--sp-4);
	}
	/* Up to three runs: buttons. More: the buttons for the newest and a dropdown of all. On
	   phones only the dropdown, so the picker never wraps into several rows. */
	.chips {
		flex-wrap: wrap;
		max-width: 100%;
		border-radius: var(--radius-lg);
	}
	.picksel {
		display: none;
		align-items: center;
		gap: var(--sp-3);
	}
	.picker.many .picksel {
		display: inline-flex;
	}
	.picksel select {
		font: inherit;
		font-family: var(--mono);
		font-size: var(--text-sm);
		min-height: 44px;
		padding: 0 var(--sp-3);
		color: var(--ink);
		background: var(--panel-solid);
		border: 1px solid var(--line-strong);
		border-radius: var(--radius-md);
		max-width: 100%;
	}
	@media (max-width: 560px) {
		.picker .chips {
			display: none;
		}
		.picker .picksel {
			display: flex;
			width: 100%;
		}
		.picksel select {
			flex: 1;
		}
	}
	/* The switch's look is app.css .seg / .chipbtn (W12 round 11); a run option holds its date and count. */
	.chipbtn {
		display: inline-flex;
		justify-content: center;
		gap: var(--sp-2);
	}
	.chipbtn .sub {
		opacity: 0.8;
	}
	.idxchecks {
		list-style: none;
		margin: 0 0 var(--sp-3);
		/* W12 round 10: the dense panel step on every side (was 12 / 16 px). */
		padding: var(--sp-4);
		display: grid;
		gap: var(--sp-2);
	}
	.kv {
		font-size: var(--text-sm);
		overflow-wrap: anywhere;
	}
	.bad {
		color: var(--bad);
	}
	/* W12 round 3: the step lists are a panel's content, with the run's note as the panel's footer.
	   W12 round 11: the panel's own 24 px on every side (was 8 / 16 / 16 px, the last panel padding
	   exception); the first step keeps 4 px of its top padding (its ring rises 4 px above its row),
	   so the ring's top sits 24 px down like a panel's first row. */
	.netbox {
		gap: 0;
	}
	.netsteps > li:first-child {
		padding-top: var(--sp-1);
	}
	/* Phones: the step lists keep 16 px sides (24 px made the 14 steps' text wrap more, the page
	   294 px taller at 390 px); top and bottom stay 24 px. */
	@media (max-width: 560px) {
		.netbox,
		.ganttbox {
			padding-inline: var(--sp-4);
		}
	}
	.netsteps {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
	}
	/* W12 round 6: each number sits in a ring on a rail that runs from the first step to the last
	   (it was a small grey figure), so the eye can run down the list. The rail is drawn, not read. */
	.netsteps > li {
		position: relative;
		display: grid;
		grid-template-columns: 28px minmax(0, 1fr);
		gap: var(--sp-4);
		padding: var(--sp-4) 0;
		border-bottom: 1px solid var(--line);
	}
	.netsteps > li::before {
		content: '';
		position: absolute;
		left: 13.5px;
		top: 0;
		bottom: 0;
		width: 1px;
		background: var(--line-strong);
	}
	.netsteps > li:first-child::before {
		top: var(--sp-2);
	}
	.netsteps > li:last-child::before {
		bottom: auto;
		height: var(--sp-6);
	}
	@media (max-width: 560px) {
		.netsteps > li {
			gap: var(--sp-3);
		}
	}
	.netsteps > li:only-child::before {
		display: none;
	}
	.netsteps > li:last-child {
		border-bottom: 0;
	}
	/* The ring is shared with the local rehearsal's steps (.gantt .n, round 7). */
	.netsteps .n,
	.gantt .n {
		position: relative;
		display: grid;
		place-items: center;
		width: 28px;
		height: 28px;
		margin-top: calc(var(--sp-1) * -1);
		border-radius: 50%;
		border: 1px solid var(--accent-line);
		background: var(--panel-solid);
		color: var(--ink);
		font-size: var(--text-xs);
		line-height: var(--lh-none);
		font-variant-numeric: tabular-nums;
	}
	.body,
	.what,
	.did {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: var(--sp-2);
		min-width: 0;
	}
	/* W12 round 12: the step and round titles take .ititle (display face, app.css); the medium sans
	   weight that sat here is gone. */
	/* Wide screens: what happened and its check on the left, the transactions and the log on the
	   right (source order: left column first). */
	@media (min-width: 900px) {
		.across .body {
			grid-template-columns: minmax(0, 5fr) minmax(0, 6fr);
			gap: var(--sp-6);
			align-items: start;
		}
	}
	.chk {
		font-size: var(--text-sm);
		color: var(--good);
		overflow-wrap: anywhere;
	}
	.txrows {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(min(100%, 15rem), 1fr));
		gap: var(--sp-2);
	}
	.txset .txrows {
		margin-top: var(--sp-2);
	}
	/* Closed: the rows are out of layout (not tap targets, not Tab stops). */
	.txset:not([open]) .txrows {
		display: none;
	}
	.txrow {
		display: flex;
		align-items: center;
		gap: var(--sp-3);
		min-height: 44px;
		padding: 0 var(--sp-3);
		border-radius: var(--radius-md);
		background: var(--code-bg);
		border: 1px solid var(--line);
		text-decoration: none;
		font-size: var(--text-sm);
		font-variant-numeric: tabular-nums;
	}
	.txrow:hover {
		border-color: var(--accent);
	}
	.txrow .fn {
		color: var(--muted);
		flex: 0 1 auto;
		min-width: 3ch;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.txrow .ext {
		margin-left: auto;
		color: var(--muted);
	}
	.txset > summary {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--sp-0) var(--sp-3);
		min-height: 44px;
		padding: var(--sp-2) var(--sp-4);
		border-radius: var(--radius-md);
		border: 1px solid var(--line-strong);
		background: var(--panel-2);
		list-style: none;
	}
	.txset > summary::-webkit-details-marker {
		display: none;
	}
	.txset > summary .show::before {
		content: '+ ';
		font-family: var(--mono);
		color: var(--accent);
	}
	.txset[open] > summary .show::before {
		content: '− ';
	}
	.txset > summary .opened,
	.txset[open] > summary .closed {
		display: none;
	}
	.txset[open] > summary .opened {
		display: inline;
	}
	.txset > summary .show {
		font-weight: 500;
		font-size: var(--text-sm);
	}
	.txset > summary .fns {
		color: var(--muted);
		font-size: var(--text-xs);
		overflow-wrap: anywhere;
	}
	.tx {
		padding: var(--sp-0) var(--sp-2);
		border-radius: var(--radius-sm);
		background: var(--code-bg);
		border: 1px solid var(--line);
		text-decoration: none;
		font-variant-numeric: tabular-nums;
	}
	.tx:hover {
		border-color: var(--accent);
	}
	@media (max-width: 760px) {
		.tx {
			display: inline-flex;
			align-items: center;
			min-height: 44px;
			padding: 0 var(--sp-3);
		}
	}
	details summary {
		cursor: pointer;
	}
	/* A step's log: a small, quiet toggle (it repeats on every step). */
	.logset > summary {
		width: fit-content;
		/* A 44 px tap target on phones, however short the word. */
		min-width: 44px;
		font-size: var(--text-xs);
		color: var(--muted);
	}
	.logset > summary:hover {
		color: var(--ink);
	}
	/* Repo paths and commands sit folded under their note. */
	.built {
		margin-top: var(--sp-2);
	}
	.built > summary {
		width: fit-content;
		font-size: var(--text-xs);
		color: var(--muted);
	}
	.built > :not(summary) {
		margin-top: var(--sp-1);
	}
	/* Phones: the time, the transaction chip and the step each take a line (a wrap left "by" alone). */
	.emit > span {
		white-space: nowrap;
	}
	@media (max-width: 560px) {
		.emit {
			display: grid;
			justify-items: start;
			gap: var(--sp-1);
		}
		.emit .sep {
			display: none;
		}
	}
	.log {
		margin: var(--sp-1) 0 0;
		color: var(--muted);
		overflow-wrap: anywhere;
	}
	.stats {
		margin-bottom: var(--sp-3);
	}
	/* W12 round 11: the panel's 24 px (was 12 / 16 / 16 px); the first row's own 8 px goes. */
	.ganttbox {
		gap: var(--sp-2);
	}
	.gantt > li:first-child {
		padding-top: 0;
	}
	.gantt {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
	}
	/* The block axis under the tracks, in the charts' axis style (W6). */
	/* Same columns as the rows, so "block 0" and the end sit under the track's ends. */
	.ganttaxis {
		display: grid;
		grid-template-columns: 28px minmax(0, 15rem) minmax(0, 1fr) 7.5rem;
		column-gap: var(--sp-4);
		font-family: var(--mono);
		font-size: var(--text-xs);
		color: var(--muted);
		font-variant-numeric: tabular-nums;
	}
	.ganttaxis span {
		grid-column: 3;
		grid-row: 1;
	}
	.ganttaxis span:last-child {
		justify-self: end;
	}
	/* W12 round 7: the rehearsal's steps take the Base Sepolia steps' look: each step's number in a
	   ring on a rail from the first step to the last. The number is the step's own span (as Base
	   Sepolia's), so a screen reader reads it too: an ol with list-style none is not a list in
	   Safari, and the step table has no number column (round 7 review). */
	.gantt li {
		position: relative;
		display: grid;
		grid-template-columns: 28px minmax(0, 15rem) minmax(0, 1fr) 7.5rem;
		align-items: center;
		gap: var(--sp-4);
		padding: var(--sp-2) 0;
		border-bottom: 1px solid var(--line);
	}
	.gantt .n {
		z-index: 1;
		margin-top: 0;
		grid-column: 1;
		grid-row: 1;
		align-self: center;
	}
	.gantt > li::after {
		content: '';
		position: absolute;
		left: 13.5px;
		top: 0;
		bottom: 0;
		width: 1px;
		background: var(--line-strong);
	}
	.gantt > li:first-child::after {
		top: 50%;
	}
	.gantt > li:last-child::after {
		bottom: 50%;
	}
	.gantt li:last-child {
		border-bottom: 0;
	}
	.lab {
		display: grid;
		min-width: 0;
	}
	/* W12 round 13: a step's name one step above its detail (--text-base over --text-sm; both were
	   13 px, told apart by weight alone, beside 22 px step titles above). It stays in the body face:
	   the rows are a chart, their names data labels (round 12). */
	.lab b {
		font-weight: 500;
		font-size: var(--text-base);
	}
	.lab span {
		font-size: var(--text-sm);
		overflow-wrap: anywhere;
	}
	.track {
		position: relative;
		height: 8px;
		border-radius: var(--radius-xs);
		background: var(--panel-2);
		border: 1px solid var(--line);
	}
	.track i {
		position: absolute;
		top: -1px;
		bottom: -1px;
		border-radius: var(--radius-xs);
		background: var(--accent);
	}
	.track i.d {
		background: var(--btc);
	}
	.num {
		font-size: var(--text-sm);
		color: var(--muted);
		text-align: right;
		font-variant-numeric: tabular-nums;
	}
	/* Phones: name and numbers on one line, the to-scale bar below. */
	@media (max-width: 640px) {
		.gantt li {
			grid-template-columns: 28px minmax(0, 1fr) auto;
			gap: var(--sp-2) var(--sp-3);
		}
		.gantt .n {
			grid-row: 1 / span 2;
		}
		.track {
			grid-column: 2 / -1;
			grid-row: 2;
		}
		.ganttaxis {
			grid-template-columns: 28px minmax(0, 1fr);
			column-gap: var(--sp-3);
		}
		.ganttaxis span {
			grid-column: 2;
		}
	}
	.bad {
		color: var(--bad);
	}
	.ok {
		color: var(--good);
	}
	.split {
		padding: var(--sp-4);
		display: grid;
		gap: var(--sp-4);
	}
	.lead {
		margin: 0;
	}
	.bar {
		position: relative;
		height: 22px;
		border-radius: var(--radius-sm);
		overflow: hidden;
		background: var(--panel-2);
	}
	.bar i {
		position: absolute;
		top: 0;
		bottom: 0;
		border-right: 2px solid var(--bg);
	}
	.shares {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 11rem), 1fr));
		gap: var(--sp-3);
	}
	.shares li {
		display: grid;
		grid-template-columns: 12px minmax(0, 1fr) auto;
		align-items: center;
		gap: var(--sp-0) var(--sp-2);
		font-size: var(--text-sm);
	}
	.shares li .muted {
		grid-column: 2 / -1;
		font-size: var(--text-xs);
	}
	.shares .key {
		margin-right: 0;
	}
	/* W12 round 12: the round's four facts size to their content where they share a row (four even
	   columns wrapped "own chain read · contract verifyRow" onto two lines beside three one-line
	   facts); spare width is shared out, a fact that cannot fit takes a row of its own. Phones keep
	   the grouped card, a row per fact (app.css). */
	@media (min-width: 561px) {
		.facts.fit {
			display: flex;
			flex-wrap: wrap;
		}
		.facts.fit > .fact {
			flex: 1 0 auto;
			max-width: 100%;
		}
	}
	.tiles {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 15rem), 1fr));
		gap: var(--sp-3);
		margin-top: var(--sp-3);
	}
	/* The card surface is the shared one (app.css, W12 round 3). */
	.tile {
		gap: var(--sp-2);
		padding: var(--sp-4) var(--sp-5);
	}
	.tile > span {
		color: var(--muted);
		font-size: var(--text-sm);
	}
	.tile span b {
		color: var(--ink);
		font-weight: 500;
	}
	/* P5: the hardware table, as Reserves' tables: cells wrap and sit at the top; phones stack
	   each row (app.css). */
	.hw td {
		vertical-align: top;
		white-space: normal;
	}
	.hw td:first-child {
		min-width: 18ch;
	}
	@media (max-width: 560px) {
		.hw td {
			padding: var(--sp-1) 0 var(--sp-1) 42%;
			border: 0;
		}
	}
</style>
