<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { resolve } from '$app/paths';
	import Pickaxe from '@lucide/svelte/icons/pickaxe';
	import ShieldCheck from '@lucide/svelte/icons/shield-check';
	import Link from '@lucide/svelte/icons/link';
	import Receipt from '@lucide/svelte/icons/receipt';
	import Play from '@lucide/svelte/icons/play';
	import ArrowRight from '@lucide/svelte/icons/arrow-right';
	import Cpu from '@lucide/svelte/icons/cpu';
	import GitMerge from '@lucide/svelte/icons/git-merge';
	import Signature from '@lucide/svelte/icons/signature';
	import Scale from '@lucide/svelte/icons/scale';
	import Globe from '@lucide/svelte/icons/globe';
	import ChartLine from '@lucide/svelte/icons/chart-line';
	import Database from '@lucide/svelte/icons/database';
	import FileText from '@lucide/svelte/icons/file-text';
	import UserRound from '@lucide/svelte/icons/user-round';
	import type { PillState } from '#lib/components/Pill.svelte';
	import OnyxSlab from '#lib/components/OnyxSlab.svelte';
	import { FALLBACK, PUBLIC } from '#lib/mode.ts';
	import { HELD_BACK, HELD_WHY } from '#lib/components/HeldBack.svelte';
	import { motion } from '#lib/motion.svelte.ts';
	import HeroVisual from '#lib/components/HeroVisual.svelte';
	import Ticker from '#lib/components/Ticker.svelte';
	import Pipeline from '#lib/components/Pipeline.svelte';
	import Ring from '#lib/components/Ring.svelte';
	import MiniTree from '#lib/components/MiniTree.svelte';
	import BlockChart from '#lib/components/BlockChart.svelte';
	import FlowChart from '#lib/components/FlowChart.svelte';
	import { checks, loadDemo } from '#lib/checks.ts';
	import { loadInput, loadLevel2, loadSecHoldings, whenVisible, type Demo } from '#lib/data.ts';
	import { verifier, type FlowRow, type Inspected, type Pinned } from '#lib/verifier.ts';
	import { btc, group } from '#lib/format.ts';
	import { TESTNET_RUNS } from '#lib/runs.ts';
	import { RESERVE_MATCHES } from '#lib/reserves.ts';
	import { entitySeries, liveSums, type FlowPoint } from '#lib/live.ts';

	let demo = $state.raw<Demo | null>(null);
	let pin = $state.raw<Pinned | null>(null);
	let input = $state.raw<Inspected | null>(null);
	const labelCount = $derived(
		input ? input.label_set.labels.length : PUBLIC && pin && demo?.label_set.content_hash === pin.label_set ? demo.label_set.count : null
	);
	let liveRows = $state.raw<FlowRow[]>([]);
	let okxPoints = $state.raw<FlowPoint[]>([]);
	// Set once the OKX sums were tried: a slot still empty then (rows that did not check out) stops
	// shimmering, so it does not look like it is still loading.
	let okxTried = $state(false);
	// Set once the verifier was tried: a figure still empty then says so in words, instead of a dash.
	let warmed = $state(false);
	let loadError = $state('');
	let running = $state(false);
	let results = $state<Record<string, { state: PillState; text: string }>>(
		Object.fromEntries(checks.map((c) => [c.id, { state: 'idle' as PillState, text: '' }]))
	);
	// From the export, never a literal (AUD-1-F6).
	const fullFuel = $derived(demo?.runs.find((r) => r.id === 'full')?.fuel ?? null);
	const passed = $derived(checks.filter((c) => results[c.id].state === 'good').length);
	const failed = $derived(checks.filter((c) => results[c.id].state === 'bad').length);
	let console_: HTMLElement;
	let consoleHead = $state<HTMLElement>();

	// Every number below is decoded from checked bytes or pinned in the
	// verifier, never read from the export's own summary fields.
	// The page shows from the small demo file at once; the Rust verifier
	// (1.8 MiB gzip) and the inputs load only when a section that shows their
	// values is on screen, or when the checks run (W4).
	let statsEl = $state<HTMLElement>();
	let marketEl = $state<HTMLElement>();
	let liveEl = $state<HTMLElement>();
	// The full runs on Base Sepolia (run page; scripts/build-testnet-run.py), latest first.
	type NetHead = { verdict: string; network: string; date: string; totals: { sent_txs: number }; reserve?: { round: number } };
	let testnets = $state.raw<NetHead[]>([]);
	let netRead = $state(false);
	const passedRuns = $derived(testnets.filter((t) => t.verdict === 'REHEARSAL PASSED'));
	const testnet = $derived(passedRuns[0] ?? null);
	const times = (k: number) => (k === 1 ? '' : k === 2 ? ' twice' : ` ${k} times`);
	onMount(() => {
		// W4: the build copies the fields read here from each run file into run/heads.json (a few
		// hundred bytes instead of three 45-58 KB files); without it (dev server), the run files.
		const files = () => Promise.all(TESTNET_RUNS.map((f) => fetch(f).then((r) => (r.ok ? (r.json() as Promise<NetHead>) : null)).catch(() => null)));
		fetch('run/heads.json')
			.then((r) => (r.ok ? (r.json() as Promise<(NetHead | null)[]>) : files()))
			.catch(files)
			.then((js) => (testnets = js.filter((j): j is NetHead => j !== null)))
			.catch(() => {})
			.finally(() => (netRead = true));
		let pendingRun = false;
		let stop = () => {};
		let gone = false;
		loadDemo()
			.then((d) => {
				demo = d;
				if (pendingRun) checkNow();
				// W4: the verifier and (private preview) the recipe input load when a section that
				// shows their values comes on screen, or when the checks run; never with the first paint.
				tick().then(() => {
					if (!gone) stop = whenVisible([statsEl, marketEl, liveEl], warmUp);
				});
			})
			.catch((e) => (loadError = String(e)));
		Promise.all([loadLevel2(), loadSecHoldings()])
			.then((texts) => (l2Sources = texts.map((t) => String(JSON.parse(t).source?.name ?? '').split(':')[0]).filter(Boolean)))
			.catch(() => {});
		// The command palette asks for a run from any page.
		const onRun = () => (demo ? checkNow() : (pendingRun = true));
		addEventListener('onyx:run-checks', onRun);
		return () => {
			gone = true;
			stop();
			removeEventListener('onyx:run-checks', onRun);
		};
	});

	let warming: Promise<void> | null = null;
	function warmUp() {
		const d = demo;
		if (!d || warming) return;
		warming = (async () => {
			// The public build ships no label rows (AUD-2-F1): the count is the
			// export's, shown only when its label set is the pinned one.
			const [p, i] = await Promise.all([verifier.pinned(), PUBLIC ? null : verifier.inspectInput(await loadInput(d.runs[0].input))]);
			pin = p;
			input = i;
			// A rights-fallback build ships no live rows (ROADMAP L14).
			if (d.live && !FALLBACK) liveRows = await Promise.all(d.live.rows.map((r) => verifier.decodeLeaf(r.leaf)));
			// The preview draws only per-block values the verifier summed and checked.
			if (d.live?.per_block && !FALLBACK) {
				const s = await liveSums(d);
				if (s.ok) okxPoints = entitySeries(s, 'entity:okx');
			}
		})()
			.catch((e) => {
				loadError = String(e);
			})
			.finally(() => {
				okxTried = true;
				warmed = true;
			});
	}

	async function runAll() {
		if (!demo) return;
		warmUp();
		running = true;
		for (const c of checks) results[c.id] = { state: 'idle', text: '' };
		for (const c of checks) {
			results[c.id] = { state: 'run', text: '' };
			try {
				results[c.id] = { state: 'good', text: await c.run(demo) };
			} catch (e) {
				results[c.id] = { state: 'bad', text: e instanceof Error ? e.message : String(e) };
			}
		}
		running = false;
	}
	function checkNow() {
		console_?.scrollIntoView({ behavior: motion.off ? 'auto' : 'smooth', block: 'center' });
		// Focus follows the scroll (keyboard and screen-reader users land on the checks); a frame
		// later, so a closing command palette does not hand focus back to its opener over it.
		// Review of round 14: the checks start in that same frame, right after the focus move. They used to
		// start at once, and on a machine too loaded to draw (e2e at load 56) they ended in the worker before
		// that frame came, so focus reached the heading only after the results. `running` is set now, so the
		// button says "Checking…" from the click.
		if (running) {
			requestAnimationFrame(() => consoleHead?.focus({ preventScroll: true }));
			return;
		}
		if (!demo) return;
		running = true;
		requestAnimationFrame(() => {
			consoleHead?.focus({ preventScroll: true });
			runAll();
		});
	}

	const steps = [
		{ icon: Database, title: 'Sources', note: 'Pinned by hash' },
		{ icon: Pickaxe, title: 'Miners', note: 'Run the recipe, seal the answer' },
		{ icon: ShieldCheck, title: 'Validators', note: 'Rerun a sample; cheaters lose their bond' },
		{ icon: Link, title: 'Chain', note: 'Root on Base Sepolia' },
		{ icon: Receipt, title: 'Customers', note: 'Pay per call, check the proof' }
	];
	const milestones = [
		['M0', 'Foundation', 'done'],
		['M1', 'Recipe runtime', 'done'],
		['M2', 'Labels', 'done'],
		['M3', 'Sealed answers', 'done'],
		['M4', 'Validators', 'done'],
		['M5', 'Disputes', 'done'],
		['M6', 'Contracts', 'done'],
		['M7', 'Paid API', 'done'],
		['M8', 'Public demo', 'now']
	] as const;
	// The line fills up to the last milestone in the unbroken run of done ones.
	const trackFill = `${((milestones.findIndex(([, , s]) => s !== 'done') - 1) / (milestones.length - 1)) * 100}%`;
	const okx = $derived(liveRows.find((r) => r.entity === 'entity:okx'));

	// The proof ladder (docs/TRACKER.md N9): every data point carries the
	// strongest proof its kind allows. Status is honest: levels 1, 3
	// (OKX-signed labels), 4 (unsigned company lists, used by the demo market,
	// LBL-V2R-F2, L2R3-F2) and 5 (Brier) are in use today; level 2 is built
	// and run as recorded rounds on real open sources (named from their files), with
	// no live observer network yet.
	// Level 2's sources are the recorded rounds the site ships (AUD-2-F11), read from their files.
	let l2Sources = $state.raw<string[]>([]);
	const ladder = $derived([
		{ icon: Cpu, name: 'Recomputed', how: 'Anyone reruns the recipe on public inputs', eg: 'Open ledgers, public datasets', status: 'Live', live: true },
		{
			icon: Globe,
			name: 'Re-observed',
			how: 'Observers drawn at random from a bonded pool fetch the same source and seal their answers',
			eg: 'Prices, open APIs, filings',
			status: l2Sources.length ? `Built; recorded rounds on ${l2Sources.join(' and ')}, observers simulated` : 'Built; recorded rounds, observers simulated'
		},
		{ icon: Signature, name: 'Source-proven', how: 'The source signs it, or a TLS proof of the web response', eg: 'Signed feeds, sensors, keyed APIs', status: 'Live for signed labels; signed data feeds built, none connected yet', live: true },
		{ icon: Scale, name: 'Bonded claim', how: 'Submitter stakes; anyone can challenge with evidence', eg: 'Your own data, field reports, lists', status: 'Live for unsigned label lists; bond planned', live: true },
		{ icon: ChartLine, name: 'Track record', how: 'Scored when the truth arrives later', eg: 'Forecasts, labels', status: 'Built; no real records yet' }
	]);

	// NORTH-STAR NS1: Onyx is for any data; each kind names the proof level it can reach. The
	// Bitcoin demo on this page is one example (anyone can recompute it), not the limit.
	const kinds = [
		{ icon: Cpu, name: 'Open ledgers and public datasets', how: 'Anyone reruns the same program on the same inputs.', eg: 'Blockchain transactions, open data files', level: 1 },
		{ icon: Globe, name: 'Public APIs and websites', how: 'Several observers fetch the same source and compare.', eg: 'Prices, weather, earthquake feeds', level: 2 },
		{ icon: FileText, name: 'Filings and public records', how: 'Observers fetch the published record and seal what it says.', eg: 'Company filings, government statistics', level: 2 },
		{ icon: Signature, name: 'Signed sources', how: 'The source signs the data, or a TLS proof shows the exact web answer.', eg: 'Sensors, signed feeds, keyed APIs', level: 3 },
		{ icon: UserRound, name: 'Your own data', how: 'You stand behind it; anyone can challenge it with evidence.', eg: 'Research, measurements, field reports', level: 4 },
		{ icon: ChartLine, name: 'Forecasts', how: 'Scored against the real value when it arrives.', eg: 'Predictions of a future release', level: 5 }
	] as const;

	// Data-market concept: a question that settles on a checked row.
	let threshold = $state(100);
	const netBtc = $derived(okx ? Number(BigInt(okx.net_sats)) / 1e8 : null);
	const resolves = $derived(netBtc === null ? null : netBtc <= -threshold);
	// Decoding the row is not a check: the market says "checked" only after the live-root check
	// (checks.ts `liveroot`) passed here, and says so when it failed.
	const marketCheck = $derived.by(() => {
		const st = results.liveroot?.state ?? 'idle';
		return st === 'good' ? 'Checked against the pinned root' : st === 'bad' ? 'Check failed: the live rows do not hash to the pinned root' : st === 'run' ? 'Checking against the pinned root…' : 'Not yet checked';
	});
</script>

<svelte:head><title>How it works · Onyx</title></svelte:head>

<OnyxSlab>
	<div class="hero">
		<div class="pitch">
			<h1>How Onyx checks data</h1>
			<p class="lede" data-testid="home-what">
				Onyx is a contributor-run network for any kind of data: anyone can submit data, others check it by the strongest
				method the data allows, and buyers pay per row and check every answer against a root posted on Base Sepolia (testnet).
			</p>
			<div class="row cta">
				<button type="button" onclick={checkNow} disabled={!demo} data-testid="home-primary"><ShieldCheck size={16} />Verify it in your browser</button>
				<a class="btn ghost on-slab" href={resolve('/run')} data-testid="home-secondary">See the live run <ArrowRight size={16} /></a>
			</div>
			<p class="note" data-testid="home-note">
				<span class="dot" aria-hidden="true"></span><b>Testnet only · {PUBLIC ? 'early preview' : 'private preview'}</b>
				<span>Data is paid in test USDC on Base Sepolia, not switched on yet; markets use tONYX test tokens. No real money.</span>
			</p>
		</div>
		<div class="vizslot">
			{#if demo}
				<HeroVisual blocks={demo.blocks} recipe={demo.recipe_id} root={demo.runs[0].root} status={results.recipe?.state ?? 'idle'} />
			{/if}
		</div>
	</div>
</OnyxSlab>

<div class="wrap">
	{#if loadError}<p class="section">Could not load the demo data: {loadError}</p>{/if}

	<section class="proofs" aria-labelledby="proofs-h">
		<h2 id="proofs-h" class="eyebrow">Shown so far, each with its record</h2>
		<ol class="pgrid">
			<li>
				<div class="panel proof" data-testid="proof-run">
					<span class="pnum" aria-hidden="true">01</span>
					<span class="picon" aria-hidden="true"><Link size={18} /></span>
					<!-- The whole card is the link (its ::after covers the card); the link's text is the
					     title, so the spoken name is short and matches what is shown (W5 review). -->
					{#if testnet}
						<h3><a class="plink" href={resolve('/run')} data-testid="testnet-headline">Full run passed live on {testnet.network}{times(passedRuns.length)}</a></h3>
						<p>
							Latest <span class="nw">{testnet.date}</span>: {group(testnet.totals.sent_txs)} transactions{testnet.reserve ? ', reserve data sold on chain' : ''},
							every step linked to its transaction.{#if passedRuns.length > 1}{' '}Earlier: {#each passedRuns.slice(1) as t, i (t.date)}{i ? ', ' : ''}<span class="nw">{t.date}</span>{/each}.{/if}
						</p>
					{:else}
						<h3><a class="plink" href={resolve('/run')}>Full runs on Base Sepolia</a></h3>
						<p>{netRead ? 'The run records could not be read here; the Run page links every transaction.' : 'Reading the run records…'}</p>
					{/if}
					<div class="pfoot"><span class="pgo" aria-hidden="true">See the live run <ArrowRight size={16} /></span></div>
				</div>
			</li>
			<li>
				{#if FALLBACK}
				<!-- ROADMAP L14, ADR-0035: a rights-fallback build ships no reserve figure. -->
				<div class="panel proof" data-testid="proof-held-back">
					<span class="pnum" aria-hidden="true">02</span>
					<span class="picon" aria-hidden="true"><Scale size={18} /></span>
					<h3><a class="plink" href={resolve('/reserves')}>{HELD_BACK.replace(/\.$/, '')}</a></h3>
					<p>{HELD_WHY}</p>
					<div class="pfoot"><span class="pgo" aria-hidden="true">See the reserves page <ArrowRight size={16} /></span></div>
				</div>
				{:else}
				<div class="panel proof" data-testid="proof-reserves">
					<span class="pnum" aria-hidden="true">02</span>
					<span class="picon" aria-hidden="true"><Scale size={18} /></span>
					<h3>
						<a class="plink" href={resolve('/reserves')}>Reserves for {RESERVE_MATCHES.map((m) => m.short).join(' and ')} matched their own published figures exactly</a>
					</h3>
					<p>
						Rebuilt from Bitcoin blocks. {#each RESERVE_MATCHES as m (m.entity)}{m.name}:
							<span class="mono num">{m.onyx} BTC</span> over its {group(m.addresses)} {m.unit} at {m.at}, against {m.list}.{' '}{/each}Difference
						0.
					</p>
					<div class="pfoot"><span class="pgo" aria-hidden="true">See the reserves <ArrowRight size={16} /></span></div>
				</div>
				{/if}
			</li>
			<li>
				<div class="panel proof" data-testid="proof-verify">
					<span class="pnum" aria-hidden="true">03</span>
					<span class="picon" aria-hidden="true"><Cpu size={18} /></span>
					<h3>Verify in your browser</h3>
					<p>The network's own code, run in your browser, makes {checks.length} checks on this page: {FALLBACK ? 'hashes, block headers and signatures' : 'rows, proofs and roots'} against the values pinned in its code.</p>
					<div class="pfoot">
						<button type="button" class="pgo" onclick={checkNow} disabled={!demo}>
							{running ? 'Checking…' : passed + failed === checks.length ? `${passed} of ${checks.length} passed · run again` : `Run the ${checks.length} checks`}
							<Play size={16} />
						</button>
					</div>
				</div>
			</li>
		</ol>
	</section>

	<!-- Always rendered with fixed height, so nothing jumps when values arrive. -->
	<div class="stats" bind:this={statsEl}>
		<div class="stat"><b>{#if pin}<Ticker value={pin.live.to - pin.live.from + 1} />{:else if warmed}<span class="na">Could not load</span>{:else}<span class="na" aria-hidden="true">Loading…</span><span class="sr-only">Loading the live block count</span>{/if}</b><span>live blocks from our node</span></div>
		<div class="stat"><b>{#if labelCount !== null}<Ticker value={labelCount} />{:else if warmed}<span class="na">{PUBLIC ? 'Not in the public build' : 'Could not load'}</span>{:else}<span class="na" aria-hidden="true">Loading…</span><span class="sr-only">Loading the label count</span>{/if}</b><span>labels in the sample dataset</span></div>
		<div class="stat"><b>{#if demo?.bip322_vectors}<Ticker value={demo.bip322_vectors.length} />{:else}<span class="na" aria-hidden="true">Loading…</span><span class="sr-only">Loading the vector count</span>{/if}</b><span>signature test vectors</span></div>
		{#if FALLBACK}
			<div class="stat" data-testid="stat-checks"><b>{checks.length}</b><span>checks run in your browser</span></div>
		{:else}
			<div class="stat"><b class="btc">{#if okx}{btc(okx.net_sats).split('.')[0]} BTC{:else if warmed}<span class="na">Could not load</span>{:else}<span class="na" aria-hidden="true">Loading…</span><span class="sr-only">Loading the OKX net flow</span>{/if}</b><span>OKX net flow, live range</span></div>
		{/if}
	</div>

	<section class="section" aria-labelledby="kinds-h" data-testid="data-kinds">
		<div class="head">
			<h2 id="kinds-h">One network for any data</h2>
			<p class="muted">Each kind of data gets the strongest check it allows. The demo on this page uses Bitcoin, because anyone can recompute it from public blocks. It is one example, not the limit.</p>
		</div>
		<ul class="kinds">
			{#each kinds as k (k.name)}
				<li class="panel kind">
					<span class="kicon" aria-hidden="true"><k.icon size={18} /></span>
					<h3 class="ktitle">{k.name}</h3>
					<span class="klevel">Level {k.level}</span>
					<p class="muted">{k.how}</p>
					<span class="egtag">{k.eg}</span>
				</li>
			{/each}
		</ul>
	</section>

	<section class="section">
		<div class="head"><h2>Who does what</h2></div>
		<div class="panel pipe"><Pipeline {steps} /></div>
	</section>

	<section class="section" bind:this={console_}>
		<div class="panel console">
			<div class="ctop">
				<Ring {passed} {failed} total={checks.length} />
				<div class="head">
					<h2 id="checks-h" tabindex="-1" bind:this={consoleHead} data-testid="checks-heading">Run the checks</h2>
					<p class="meta">{checks.length} checks · the network's own code, run in your browser</p>
				</div>
				<button type="button" onclick={runAll} disabled={!demo || running}>
					<Play size={16} />{running ? 'Checking…' : `Run all ${checks.length} checks`}
				</button>
			</div>
			<ol class="checks" style="--rows: {Math.ceil(checks.length / 2)}">
				{#each checks as c (c.id)}
					<li class={results[c.id].state}>
						<div class="ct">
							<span class="ititle">{c.title}</span>
							<!-- A status, not a button: a small dot and a word (the `pill` class stays for the tests' waits). -->
							<span class="pill cstat {results[c.id].state}"><i aria-hidden="true"></i>{results[c.id].state === 'good' ? 'Passed' : results[c.id].state === 'bad' ? 'Failed' : results[c.id].state === 'run' ? 'Checking' : 'Ready'}</span>
						</div>
						{#if results[c.id].text}<span class="cd">{results[c.id].text}</span>{/if}
						<details class="why"><summary>What this checks</summary><span class="cd">{c.detail}</span></details>
					</li>
				{/each}
			</ol>
			<p class="tally mono" role="status" aria-live="polite">{passed} passed · {failed} failed</p>
		</div>
	</section>

	<section class="section">
		<div class="head">
			<h2>Any data, with its proof level</h2>
			<p class="muted">New data enters at the level it can prove and climbs as proof arrives. Buyers see the level.</p>
		</div>
		<ol class="ladder">
			{#each ladder as l, i (l.name)}
				<li class="rung" class:live={l.live} style="--w: {100 - i * 15}%">
					<span class="lv">{i + 1}</span>
					<span class="licon"><l.icon size={18} /></span>
					<div class="lt"><b class="ltitle">{l.name}</b><span class="muted">{l.how}</span></div>
					<span class="leg"><span class="egtag">{l.eg}</span></span>
					<span class="lstat">{l.status}</span>
					<i class="strength" aria-hidden="true"></i>
				</li>
			{/each}
		</ol>
	</section>

	{#if !FALLBACK}
	<section class="section">
		<div class="head">
			<h2>Markets that settle on data</h2>
			<p class="muted">Open a question on any value: a price, a rainfall total, a figure in a filing. This one is level 4 today (unsigned company labels in the set); OKX labels alone are level 3, and the rerun itself is level 1.</p>
		</div>
		<div class="panel market" bind:this={marketEl}>
			<div class="mq">
				<span class="meta">Concept · market contracts not built yet · settles on the live OKX row</span>
				<p class="q">Will OKX's net flow over blocks {#if pin}<span class="nw">{group(pin.live.from)}–{group(pin.live.to)}</span>{:else}<span aria-hidden="true">…</span><span class="sr-only">(loading the block range)</span>{/if} be below <b class="nw">−{threshold} BTC</b>?</p>
				<label for="threshold" class="muted small">Move the line: {threshold} BTC</label>
				<input id="threshold" type="range" min="10" max="300" step="5" bind:value={threshold} />
			</div>
			<div class="mres">
				{#if resolves === null}
					<span class="verdict"><span aria-hidden="true">…</span><span class="sr-only">Loading the OKX row</span></span>
				{:else}
					<span class="verdict {resolves ? 'yes' : 'no'}">{resolves ? 'YES' : 'NO'}</span>
					<span class="meta">Value {netBtc?.toFixed(8)} BTC</span>
					<!-- The row is only decoded here; it counts as checked once the live-root check
					     (the Merkle root over the live rows equals the pinned one) has passed in this browser. -->
					<span class="meta" data-testid="market-check">{marketCheck}</span>
				{/if}
			</div>
		</div>
	</section>
	{/if}

	<section class="section">
		<div class="head"><h2>What stops cheating</h2></div>
		<div class="bento">
			<div class="panel tile wide">
				<span class="ti ptitle"><Cpu size={18} /> One engine, one answer</span>
				<p class="muted">Every machine runs the same recipe on the same interpreter and gets the same bytes.</p>
				<div class="fuel"><b>{#if fullFuel !== null}<Ticker value={fullFuel} />{:else}<span aria-hidden="true">–</span><span class="sr-only">Loading the compute count</span>{/if}</b><span>compute units for the full test run, identical on every machine</span></div>
			</div>
			<div class="panel tile">
				<span class="ti ptitle"><GitMerge size={18} /> Rows come with proofs</span>
				<MiniTree leaves={8} lit={5} />
				<p class="muted">Row, sibling hashes, root.</p>
			</div>
			<div class="panel tile half" data-testid="tile-labels">
				<span class="ti ptitle"><Signature size={18} /> Signed by the source</span>
				<div class="seals"><span class="seal">BIP-322<small>address</small></span><span class="plus">+</span><span class="seal">EIP-191<small>company key</small></span></div>
				<p class="muted">Here an exchange signs its own addresses; both signatures make the label level 3. A sensor or a data feed signs the same way. Unsigned data stays a claim, level 4.</p>
			</div>
			<div class="panel tile half" data-testid="tile-bond">
				<span class="ti ptitle"><Scale size={18} /> Wrong answers lose the bond</span>
				<div class="bond"><div class="b ok"><i style="width: 100%"></i><span>Honest: bond kept</span></div><div class="b lost"><i style="width: 100%"></i><span>Wrong root: bond slashed</span></div></div>
				<p class="muted">Miners and validators post bonds. A rerun decides who was right: <a href={resolve('/run')}>a cheater caught on Base Sepolia</a>.</p>
			</div>
		</div>
	</section>

	{#if demo?.live}
		<section class="section" bind:this={liveEl}>
			<div class="head">
				<h2>Live sample: Bitcoin</h2>
				<p class="meta">Blocks {group(demo.live.from_height)}–{group(demo.live.to_height)} · read from our node</p>
			</div>
			<a class="panel preview" href={resolve('/data')}>
				<div class="pv">
					<span class="pl">Transactions per block</span>
					<BlockChart blocks={demo.live.blocks} compact />
				</div>
				{#if !FALLBACK}
				<div class="pv">
					<span class="pl">OKX in and out, per block</span>
					{#if okxPoints.length}<FlowChart points={okxPoints} compact clip />{:else}<div class="flowslot" class:tried={okxTried} aria-busy={okxTried ? undefined : 'true'}><span class="sr-only">Loading OKX in and out per block</span></div>{/if}
				</div>
				{/if}
				<span class="more-link">Open live data <ArrowRight size={16} /></span>
			</a>
		</section>
	{/if}

	<section class="section">
		<div class="head"><h2>Milestones</h2></div>
		<ol class="track" style:--fill={trackFill}>
			{#each milestones as [k, t, s] (k)}
				<li class={s}><i></i><span>{t}</span></li>
			{/each}
		</ol>
	</section>
</div>

<style>
	/* W5 home: one rhythm on an 8 px grid; the hero states what Onyx is, three proof
	   cards rise over the slab's lower edge, each linking its record. */
	/* W12 round 8: the drawing takes a fixed 400 px column (it was a 346 px card of 12 px text beside
	   the 80 px title) and the hero starts higher, so the proof cards' text reaches the first screen
	   at 1,280 x 900. The title column keeps its two lines down to 861 px. */
	.hero {
		display: grid;
		grid-template-columns: minmax(0, 1fr) clamp(300px, 34vw, 400px);
		gap: var(--sp-9);
		align-items: center;
		padding-block: 0 var(--sp-10);
		margin-top: calc(var(--sp-4) * -1);
	}
	@media (max-width: 860px) {
		.hero {
			grid-template-columns: minmax(0, 1fr);
			gap: var(--sp-7);
			padding-block: 0 var(--sp-9);
			margin-top: calc(var(--sp-6) * -1);
		}
	}
	.pitch {
		display: grid;
		gap: var(--sp-6);
		justify-items: start;
	}
	@media (max-width: 560px) {
		.pitch {
			gap: var(--sp-5);
		}
	}
	.pitch h1 {
		font-size: var(--text-hero);
		line-height: var(--lh-none);
		max-width: 18ch;
	}
	.pitch .lede {
		font-size: var(--text-md);
		line-height: var(--lh-body);
		max-width: 58ch;
		text-wrap: pretty;
		color: var(--slab-ink);
		opacity: 1;
	}
	.cta {
		gap: var(--sp-3);
		margin-top: var(--sp-2);
	}
	.cta button,
	.cta .btn {
		min-height: 48px;
		padding: var(--sp-3) var(--sp-6);
		font-size: var(--text-base);
	}
	.cta button:active:not(:disabled),
	.cta .btn:active {
		transform: translateY(1px);
		box-shadow: none;
	}
	.on-slab {
		color: var(--slab-ink);
		border-color: var(--slab-line);
		backdrop-filter: blur(6px);
	}
	.on-slab:hover {
		border-color: var(--line-bright);
		background: var(--hover);
	}
	.cta .btn :global(svg) {
		transition: transform var(--dur) var(--ease);
	}
	.cta .btn:hover :global(svg) {
		transform: translateX(3px);
	}
	/* W12 round 6: the box hugs its two lines (it ran 62 characters wide past a shorter second line):
	   the dot, then the label over the sentence. */
	.note {
		display: grid;
		grid-template-columns: auto minmax(0, max-content);
		justify-self: start;
		align-items: baseline;
		gap: var(--sp-1) var(--sp-3);
		font-size: var(--text-sm);
		color: var(--slab-muted);
		padding: var(--sp-3) var(--sp-4);
		border: 1px solid var(--slab-line);
		/* W12 round 10: a card on the slab takes the top-level radius, like the drawing beside it and
		   the inner pages' side cards (it was the slab's one 12 px card). */
		border-radius: var(--radius-lg);
		background: var(--slab-glass);
		max-width: 62ch;
	}
	.note b {
		font-family: var(--mono);
		font-weight: 500;
		font-size: var(--text-sm);
		letter-spacing: 0.02em;
		color: var(--slab-ink);
	}
	.note > span:not(.dot) {
		grid-column: 2;
	}
	.note .dot {
		align-self: center;
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--btc);
		box-shadow: var(--shadow-ring-btc);
	}
	.vizslot {
		min-height: 420px;
		display: grid;
		align-items: center;
	}
	/* Phones and narrow tablets: the proof cards come straight after the pitch; the
	   console below shows the same root check as this drawing. */
	@media (max-width: 860px) {
		.vizslot {
			display: none;
		}
	}
	@media (max-width: 560px) {
		.cta {
			display: grid;
			grid-template-columns: minmax(0, 1fr);
			justify-self: stretch;
		}
		.cta button,
		.cta .btn {
			justify-content: center;
		}
	}
	/* Proof cards: lifted over the slab's lower edge. */
	.proofs {
		position: relative;
		display: grid;
		gap: var(--sp-4);
		margin-top: calc(var(--sp-10) * -1);
	}
	.eyebrow {
		font-family: var(--mono);
		font-size: var(--text-sm);
		font-weight: 500;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--slab-muted);
		line-height: var(--lh-snug);
	}
	.pgrid {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		grid-template-columns: repeat(3, minmax(0, 1fr));
		gap: var(--sp-4);
	}
	/* W12 round 3: the reserves card holds two figures and their sources; its column is wider, so
	   the three cards end nearer one height (01 and 03 sat half empty beside it). */
	@media (min-width: 901px) {
		.pgrid:has([data-testid='proof-reserves']) {
			grid-template-columns: minmax(0, 1fr) minmax(0, 1.4fr) minmax(0, 1fr);
		}
	}
	@media (max-width: 900px) {
		.pgrid {
			grid-template-columns: minmax(0, 1fr);
			gap: var(--sp-3);
		}
	}
	.pgrid li {
		display: grid;
		min-width: 0;
	}
	.proof {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr);
		grid-template-rows: auto auto 1fr auto;
		align-content: start;
		gap: var(--sp-3) var(--sp-4);
		padding: var(--sp-6);
		color: inherit;
		text-decoration: none;
		transition: transform var(--dur) var(--ease),
			box-shadow var(--dur) var(--ease),
			background var(--dur) var(--ease);
	}
	.proof .pnum {
		grid-column: 1;
		font-family: var(--display);
		font-size: var(--text-2xl);
		line-height: var(--lh-none);
		color: var(--muted);
	}
	.proof .picon {
		grid-column: 2;
		justify-self: end;
		width: 38px;
		height: 38px;
		border-radius: var(--radius-md);
		display: grid;
		place-items: center;
		border: 1px solid var(--line-strong);
		color: var(--accent);
		background: var(--accent-soft);
	}
	.proof h3,
	.proof p,
	.proof .pfoot {
		grid-column: 1 / -1;
	}
	/* W12 round 4: each card's link is its footer row, under a hairline at one height across the
	   three cards (card 01 ended about 80 px above a link that floated at the card's foot). */
	.proof .pfoot {
		align-self: end;
		display: flex;
		padding-top: var(--sp-3);
	}
	/* W12 round 11: each card's title in the display face at the panel-title size, like the numbers
	   above them and every panel title (it was bold 18 px sans). */
	.proof h3 {
		font-family: var(--display);
		font-size: var(--text-xl);
		font-weight: 400;
		line-height: var(--lh-tight);
		letter-spacing: -0.01em;
		text-wrap: balance;
	}
	.proof p {
		font-size: var(--text-sm);
		line-height: var(--lh-body);
		color: var(--muted);
	}
	.proof .nw {
		white-space: nowrap;
	}
	.proof .num {
		font-size: var(--text-sm);
		color: var(--ink);
		white-space: nowrap;
	}
	.pgo {
		justify-self: start;
		align-self: end;
		display: inline-flex;
		align-items: center;
		gap: var(--sp-2);
		min-height: 44px;
		font-size: var(--text-base);
		font-weight: 500;
		color: var(--accent);
	}
	.pgo :global(svg) {
		transition: transform var(--dur) var(--ease);
	}
	button.pgo {
		padding: 0 var(--sp-4);
		margin-left: calc(var(--sp-0) * -1);
		background: var(--accent-soft);
		border: 1px solid var(--line-strong);
		box-shadow: none;
		color: var(--accent);
	}
	button.pgo:hover:not(:disabled) {
		transform: none;
		box-shadow: none;
		border-color: var(--accent-2);
	}
	/* The title link stretches over the card, so the whole card opens the record. */
	.plink {
		color: inherit;
		text-decoration: none;
		display: block;
		min-height: 44px;
	}
	.plink::after {
		content: '';
		position: absolute;
		inset: 0;
		border-radius: inherit;
		z-index: 1;
	}
	.proof:has(.plink):hover {
		transform: translateY(-3px);
		box-shadow: var(--shadow-lift);
	}
	.proof:has(.plink):hover .pnum,
	.proof:has(.plink:focus-visible) .pnum {
		color: var(--accent);
	}
	.proof:has(.plink):hover .pgo :global(svg),
	button.pgo:hover:not(:disabled) :global(svg) {
		transform: translateX(3px);
	}
	.proof:has(.plink):active {
		transform: translateY(-1px);
		box-shadow: var(--shadow);
	}
	/* W12 round 2: the keyboard ring goes round the whole card (the link covers it), with the
	   title underlined, instead of a ring round the title alone. */
	.plink:focus-visible {
		outline: none;
		box-shadow: var(--shadow-underline);
	}
	.plink:focus-visible::after {
		outline: 2px solid var(--accent);
		outline-offset: 2px;
	}
	.stats {
		margin-top: var(--sp-4);
	}
	.stat b.btc {
		color: var(--btc);
	}
	/* A figure not (yet) there reads as a quiet note, not a broken dash. */
	.stat b .na {
		font-family: var(--body);
		font-size: var(--text-sm);
		font-weight: 400;
		color: var(--muted);
	}
	.cstat {
		display: inline-flex;
		align-items: center;
		gap: var(--sp-2);
		flex: none;
		font-size: var(--text-sm);
		color: var(--muted);
		font-variant-numeric: tabular-nums;
	}
	.cstat i {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: currentColor;
		opacity: 0.7;
	}
	.cstat.good {
		color: var(--good);
	}
	.cstat.bad {
		color: var(--bad);
	}
	.cstat.run {
		color: var(--accent);
	}
	@media (prefers-reduced-motion: no-preference) {
		.cstat.run i {
			animation: cstat-pulse 1s ease-in-out infinite;
		}
	}
	@keyframes cstat-pulse {
		50% {
			opacity: 0.25;
		}
	}
	/* W12 round 10: the panel's own 24 px (was 32 / 16 px, its one use of that pair). */
	.ctop {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto;
		gap: var(--sp-4);
		align-items: center;
	}
	@media (max-width: 600px) {
		.ctop {
			grid-template-columns: auto minmax(0, 1fr);
		}
		.ctop button {
			grid-column: 1 / -1;
			justify-self: start;
		}
	}
	/* W12 round 1: the checks are rows of one list divided by hairlines (were seven bordered
	   cards inside the panel, a box in a box that took 50 px of width on a phone). A bar at
	   each row's left edge carries its state: quiet when ready, accent while checking, green
	   when passed, red when failed (the pill says the same in words). */
	.checks {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		border-top: 1px solid var(--line);
	}
	/* W12 round 5: two columns of checks on a wide screen (each row held a title at the left and its
	   status 600 px away at the right, seven rows deep); rows pair up, so their hairlines align.
	   Review: the checks fill the left column first, then the right (--rows is half the list,
	   rounded up), so each column reads top to bottom in the list's order, the same order a
	   screen reader and the Tab key take (row by row they zigzagged 1, 3, 5, 7 down the left). */
	@media (min-width: 900px) {
		.checks {
			grid-template-columns: repeat(2, minmax(0, 1fr));
			grid-template-rows: repeat(var(--rows), auto);
			grid-auto-flow: column;
			column-gap: var(--sp-7);
		}
	}
	.checks li {
		position: relative;
		display: grid;
		gap: var(--sp-1);
		align-content: start;
		padding: var(--sp-3) 0 var(--sp-3) var(--sp-5);
		border-bottom: 1px solid var(--line);
	}
	.checks li::before {
		content: '';
		position: absolute;
		left: 0;
		top: var(--sp-3);
		bottom: var(--sp-3);
		width: 2px;
		border-radius: var(--radius-xs);
		background: var(--line-strong);
		transition: background-color var(--dur) var(--ease);
	}
	.checks li.run::before {
		background: var(--accent);
	}
	.checks li.good::before {
		background: var(--good);
	}
	.checks li.bad::before {
		background: var(--bad);
	}
	/* W12 round 14: a check's name is an item title (`.ititle`, the display face at --text-lg), like
	   the pipeline's roles above it and Run's steps; it was medium 15 px sans, the last list of item
	   titles on Home not in the display face. */
	.ct {
		display: flex;
		justify-content: space-between;
		gap: var(--sp-3);
		align-items: center;
	}
	.cd {
		color: var(--muted);
		font-size: var(--text-sm);
		overflow-wrap: anywhere;
	}
	.why summary {
		cursor: pointer;
		font-size: var(--text-xs);
		color: var(--muted);
	}
	/* W12 round 2, phones: the fold keeps its 44 px tap target but tucks it into the row's
	   padding, so a check is 92 px tall instead of 112 (seven checks: 140 px less scrolling). */
	@media (max-width: 760px) {
		.checks li {
			gap: 0;
			padding-block: var(--sp-2);
		}
		/* W12 round 14: on a phone a check's name steps down one size (--text-md), so a row whose name
		   takes two lines stays under 100 px with its 44 px fold (round 2); at 22 px it was 104 px. */
		.ct > .ititle {
			font-size: var(--text-md);
		}
		.why summary {
			display: flex;
			align-items: center;
			gap: var(--sp-2);
			padding-block: 0;
			margin-bottom: calc(var(--sp-2) * -1);
		}
	}
	.tally {
		color: var(--muted);
		margin: 0;
	}
	/* W12 round 9: six columns, so the second row splits evenly (the labels tile was a third, its
	   title wrapping over a half-empty two-thirds bond tile). Row one keeps two thirds and a third;
	   in the half-width labels tile the two seals sit beside their sentence. */
	.bento {
		display: grid;
		grid-template-columns: repeat(6, minmax(0, 1fr));
		gap: var(--sp-3);
	}
	.tile {
		grid-column: span 2;
	}
	.tile.wide {
		grid-column: span 4;
	}
	.tile.half {
		grid-column: span 3;
	}
	@media (min-width: 761px) {
		[data-testid='tile-labels'] {
			grid-template-rows: auto 1fr;
			grid-template-columns: auto minmax(0, 1fr);
			column-gap: var(--sp-6);
			align-items: center;
		}
		[data-testid='tile-labels'] > .ti {
			grid-column: 1 / -1;
		}
	}
	@media (max-width: 760px) {
		.bento {
			grid-template-columns: minmax(0, 1fr);
		}
		.tile,
		.tile.wide,
		.tile.half {
			grid-column: auto;
		}
	}
	.tile {
		align-content: start;
	}
	.ti {
		display: inline-flex;
		gap: var(--sp-3);
		align-items: baseline;
	}
	/* The icon stands on the title's first baseline, like a capital, when the title wraps. */
	.ti :global(svg) {
		flex: none;
		color: var(--accent);
	}
	.fuel {
		display: grid;
		gap: var(--sp-0);
		padding: var(--sp-4);
		border-radius: var(--radius-md);
		background: var(--accent-soft);
	}
	.fuel b {
		font-family: var(--mono);
		font-size: var(--text-xl);
		font-weight: 500;
		color: var(--accent);
	}
	.fuel span {
		font-size: var(--text-sm);
		color: var(--muted);
	}
	.seals {
		display: flex;
		align-items: center;
		gap: var(--sp-2);
		padding-block: var(--sp-2);
	}
	/* W12 round 1: the two lines sit together at the circle's centre, so "company key" stays
	   inside the ring (grid rows had pushed it to the lower edge). */
	.seal {
		display: grid;
		place-items: center;
		align-content: center;
		gap: var(--sp-0);
		width: 96px;
		height: 96px;
		border-radius: 50%;
		border: 2px dashed var(--accent);
		font-family: var(--mono);
		font-size: var(--text-xs);
		color: var(--accent);
		text-align: center;
		line-height: var(--lh-tight);
	}
	.seal small {
		display: block;
		color: var(--muted);
		font-size: var(--text-xs);
		font-family: var(--body);
	}
	.plus {
		font-size: var(--text-lg);
		color: var(--muted);
	}
	.bond {
		display: grid;
		gap: var(--sp-2);
	}
	.b {
		position: relative;
		height: 30px;
		border-radius: var(--radius-sm);
		background: var(--code-bg);
		overflow: hidden;
	}
	.b i {
		position: absolute;
		inset: 0;
		opacity: 0.22;
	}
	.b.ok i {
		background: var(--good);
	}
	.b.lost i {
		background: repeating-linear-gradient(135deg, var(--bad) 0 6px, transparent 6px 12px);
	}
	.b span {
		position: relative;
		display: block;
		padding: var(--sp-1) var(--sp-3);
		font-size: var(--text-sm);
	}
	.small {
		font-size: var(--text-sm);
	}
	/* W12 round 2: while the OKX rows load, the chart's slot shimmers like the page's other
	   placeholders (was an empty 96 px gap; still under reduced motion). */
	.flowslot {
		height: 96px;
		border-radius: var(--radius-sm);
		background: linear-gradient(90deg, var(--code-bg), var(--hover), var(--code-bg));
		background-size: 200% 100%;
		animation: slot-shimmer 1.4s ease-in-out infinite;
	}
	.flowslot.tried {
		animation: none;
		background: var(--code-bg);
	}
	@keyframes slot-shimmer {
		to {
			background-position: -200% 0;
		}
	}
	.preview {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: var(--sp-5) var(--sp-7);
		padding: var(--sp-6);
		color: inherit;
		text-decoration: none;
		transition:
			transform var(--dur) var(--ease),
			box-shadow var(--dur) var(--ease);
	}
	/* W12 round 2: the preview card lifts like the proof cards (its border is a gradient, so the
	   old border-colour hover showed nothing), and its arrow steps forward. */
	.preview:hover {
		transform: translateY(-3px);
		box-shadow: var(--shadow-lift);
	}
	.preview:active {
		transform: translateY(-1px);
		box-shadow: var(--shadow);
	}
	.more-link :global(svg) {
		transition: transform var(--dur) var(--ease);
	}
	.preview:hover .more-link :global(svg) {
		transform: translateX(3px);
	}
	.pv {
		display: grid;
		grid-template-rows: auto 1fr;
		gap: var(--sp-3);
		align-items: end;
		min-width: 0;
	}
	.pl {
		font-size: var(--text-xs);
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--muted);
	}
	.more-link {
		grid-column: 1 / -1;
		display: inline-flex;
		align-items: center;
		gap: var(--sp-2);
		font-size: var(--text-sm);
		font-weight: 500;
		color: var(--accent);
	}
	/* One chart (a rights-fallback build has no OKX chart): it takes the card's full width. */
	.preview:not(:has(.pv + .pv)) {
		grid-template-columns: minmax(0, 1fr);
	}
	@media (max-width: 760px) {
		.preview {
			grid-template-columns: minmax(0, 1fr);
		}
	}
	/* NS1: the kinds of data, three per row on wide screens. */
	.kinds {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		grid-template-columns: repeat(3, minmax(0, 1fr));
		gap: var(--sp-4);
	}
	@media (max-width: 900px) {
		.kinds {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
	}
	@media (max-width: 600px) {
		.kinds {
			grid-template-columns: minmax(0, 1fr);
		}
	}
	.kind {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto;
		align-content: start;
		gap: var(--sp-3);
		padding: var(--sp-5);
	}
	.kind .kicon {
		color: var(--muted);
		display: flex;
		align-items: center;
	}
	.kind .ktitle {
		font-size: var(--text-lg);
		font-weight: 400;
		line-height: var(--lh-snug);
		margin: 0;
	}
	.kind .klevel {
		font-family: var(--mono);
		font-size: var(--text-sm);
		color: var(--accent);
		white-space: nowrap;
	}
	.kind p,
	.kind .egtag {
		grid-column: 1 / -1;
		margin: 0;
	}
	.kind .egtag {
		justify-self: start;
	}
	.ladder {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		gap: var(--sp-2);
	}
	/* W12 round 9: each level's name in the display face, like every panel title (it was the last
	   bold sans title on the page); its examples a quiet tag, so the middle column reads as one
	   object per row (it was a thin line of grey text); the "how" column wider, so it wraps less. */
	.rung {
		position: relative;
		display: grid;
		grid-template-columns: 34px 40px minmax(0, 1.3fr) minmax(0, 0.95fr) minmax(0, 0.95fr);
		gap: var(--sp-4) var(--sp-5);
		align-items: center;
		padding: var(--sp-4) var(--sp-4);
		border-radius: var(--radius-md);
		border: 1px solid var(--line);
		background: var(--panel-2);
		overflow: hidden;
	}
	.strength {
		position: absolute;
		left: 0;
		bottom: 0;
		height: 2px;
		width: var(--w);
		background: linear-gradient(90deg, var(--accent), transparent);
		opacity: 0.6;
	}
	.lv {
		font-family: var(--display);
		font-size: var(--text-xl);
		line-height: var(--lh-none);
		color: var(--muted);
	}
	.rung.live .lv {
		color: var(--ink);
	}
	.licon {
		width: 40px;
		height: 40px;
		border-radius: var(--radius-md);
		display: grid;
		place-items: center;
		border: 1px solid var(--line-strong);
		color: var(--muted);
	}
	.rung.live .licon {
		color: var(--accent);
		border-color: var(--accent);
	}
	.lt {
		display: grid;
		gap: var(--sp-0);
		min-width: 0;
	}
	.ltitle {
		font-family: var(--display);
		font-size: var(--text-xl);
		font-weight: 400;
		line-height: var(--lh-tight);
		letter-spacing: -0.01em;
		color: var(--ink);
	}
	.lt .muted,
	.leg {
		font-size: var(--text-sm);
	}
	.leg {
		display: flex;
		min-width: 0;
	}
	.egtag {
		padding: var(--sp-1) var(--sp-3);
		border: 1px solid var(--line-strong);
		border-radius: var(--radius-sm);
		background: var(--code-bg);
		color: var(--muted);
		line-height: var(--lh-snug);
	}
	/* W12: the status reads as a short line in the body face (was 130 px of right-set mono,
	   wrapping to six lines). */
	.lstat {
		font-size: var(--text-sm);
		line-height: var(--lh-snug);
		color: var(--muted);
	}
	.rung.live .lstat {
		color: var(--good);
	}
	/* A live level's status opens on a small ring (its words say "Live" too; colour is not the cue). */
	.lstat {
		position: relative;
		padding-left: var(--sp-5);
	}
	.lstat::before {
		content: '';
		position: absolute;
		left: 0;
		top: 0.45em;
		width: 8px;
		height: 8px;
		border-radius: 50%;
		border: 1.5px solid var(--line-bright);
	}
	.rung.live .lstat::before {
		border-color: var(--good);
		background: var(--good);
		box-shadow: var(--shadow-ring-good);
	}
	@media (max-width: 760px) {
		/* W12 round 2: the rung's lines stack close under its name (were 16 px apart, so each rung
		   ran 180-230 px tall); the number and icon centre on the name. */
		/* W12 round 10: a rung is a small card: the number, icon and name on its top row, then the
		   "how", the example tag and the status across the card's full width (they ran in a 224 px
		   column beside the number and icon at 390 px, so every line wrapped and the five rungs stood
		   82 px taller with round 9's larger names). Reading order is the source order. */
		.rung {
			grid-template-columns: 28px 40px minmax(0, 1fr);
			gap: var(--sp-2) var(--sp-3);
			align-items: center;
			padding: var(--sp-3) var(--sp-4);
		}
		.lt {
			display: contents;
		}
		.lt .muted,
		.leg,
		.lstat {
			grid-column: 1 / -1;
		}
	}
	/* W12 round 1: 240 px holds "Value 176.46741272 BTC" on one line (200 px put BTC on its own). */
	.market {
		grid-template-columns: minmax(0, 1fr) 240px;
		align-items: center;
		gap: var(--sp-6);
	}
	@media (max-width: 640px) {
		.market {
			grid-template-columns: minmax(0, 1fr);
		}
	}
	.mq {
		display: grid;
		gap: var(--sp-3);
	}
	.q {
		font-family: var(--display);
		font-size: var(--text-xl);
		line-height: var(--lh-tight);
	}
	.q b {
		font-weight: 400;
		color: var(--btc);
	}
	.mres {
		display: grid;
		justify-items: center;
		gap: var(--sp-1);
		/* W12 round 10: the inner card's padding (was 16 px all round). */
		padding: var(--sp-3) var(--sp-4);
		border-radius: var(--radius-md);
		border: 1px solid var(--line-strong);
		background: var(--panel-2);
	}
	.verdict {
		font-family: var(--mono);
		font-size: var(--text-2xl);
		line-height: var(--lh-none);
		letter-spacing: 0.04em;
	}
	.verdict.yes {
		color: var(--good);
	}
	.verdict.no {
		color: var(--bad);
	}
	.track {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		grid-template-columns: repeat(9, minmax(0, 1fr));
		position: relative;
	}
	.track::before {
		content: '';
		position: absolute;
		left: 5.5%;
		right: 5.5%;
		top: 7px;
		height: 2px;
		background: linear-gradient(90deg, var(--accent) 0 var(--fill), var(--line) var(--fill));
	}
	.track li {
		position: relative;
		display: grid;
		align-content: start;
		justify-items: center;
		gap: var(--sp-1);
		text-align: center;
		font-size: var(--text-sm);
	}
	.track i {
		width: 16px;
		height: 16px;
		border-radius: 50%;
		background: var(--panel-solid);
		border: 2px solid var(--line);
	}
	.track .done i {
		background: var(--accent);
		border-color: var(--accent);
	}
	.track .now i {
		border-color: var(--btc);
		box-shadow: var(--shadow-ring-btc);
	}
	.track span {
		color: var(--muted);
	}
	@media (max-width: 760px) {
		.track {
			grid-template-columns: minmax(0, 1fr);
			justify-items: start;
			gap: var(--sp-3);
		}
		.track::before {
			left: 7px;
			right: auto;
			top: 8px;
			bottom: 8px;
			width: 2px;
			height: auto;
			background: linear-gradient(180deg, var(--accent) 0 var(--fill), var(--line) var(--fill));
		}
		.track li {
			grid-template-columns: 16px auto;
			justify-items: start;
			align-items: center;
			gap: var(--sp-3);
		}
	}
</style>
