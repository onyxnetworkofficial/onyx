<script lang="ts">
	// Markets: data markets as a working dashboard with practice credits. Every market asks what a
	// public source will report and settles by the Onyx observer check (replayed here). Prices move
	// as simulated traders trade against the market maker (src/lib/arena.ts). Parts: markets (Yes/No
	// and ranges), flash rounds, parlays, create a market, verify data (the stake simulator on the
	// testnet contract rules), portfolio, leaderboard and quests. No real money, no network request.
	// Above them, the real markets on Base Sepolia (NS11, LiveMarkets): read from the chain only when
	// the reader asks, in tONYX, a test token with no value.
	import { onMount } from 'svelte';
	import { arena, BADGES, CATEGORIES, QUESTS, type Category, type Market } from '#lib/arena-state.svelte.ts';
	import ArenaChart from '#lib/components/ArenaChart.svelte';
	import ArenaMarket from '#lib/components/ArenaMarket.svelte';
	import ArenaFlash from '#lib/components/ArenaFlash.svelte';
	import ArenaParlay from '#lib/components/ArenaParlay.svelte';
	import ArenaCreate from '#lib/components/ArenaCreate.svelte';
	import StakeSimulator from '#lib/components/StakeSimulator.svelte';
	import LiveMarkets from '#lib/components/LiveMarkets.svelte';
	import CirclePlus from '@lucide/svelte/icons/circle-plus';
	import ShieldCheck from '@lucide/svelte/icons/shield-check';
	import Confetti from '#lib/components/Confetti.svelte';
	import OnyxSlab from '#lib/components/OnyxSlab.svelte';
	import Flame from '@lucide/svelte/icons/flame';
	import Trophy from '@lucide/svelte/icons/trophy';
	import Zap from '@lucide/svelte/icons/zap';
	import Layers from '@lucide/svelte/icons/layers';
	import Wallet from '@lucide/svelte/icons/wallet';
	import Target from '@lucide/svelte/icons/target';
	import Activity from '@lucide/svelte/icons/activity';
	import Volume2 from '@lucide/svelte/icons/volume-2';
	import VolumeX from '@lucide/svelte/icons/volume-x';
	import Pause from '@lucide/svelte/icons/pause';
	import Play from '@lucide/svelte/icons/play';
	import TrendingUp from '@lucide/svelte/icons/trending-up';

	type Tab = 'markets' | 'flash' | 'parlay' | 'create' | 'verify' | 'portfolio' | 'board' | 'quests';
	const TABS: [Tab, string, typeof Flame][] = [['markets', 'Markets', TrendingUp], ['flash', 'Flash rounds', Zap], ['parlay', 'Parlay', Layers], ['create', 'Create', CirclePlus], ['verify', 'Verify data', ShieldCheck], ['portfolio', 'Portfolio', Wallet], ['board', 'Leaderboard', Trophy], ['quests', 'Quests', Target]];
	let tab = $state<Tab>('markets');
	let open = $state<string | null>(null);
	let cat = $state<Category | 'All'>('All');
	let sort = $state<'trending' | 'volume'>('trending');
	const shown = $derived(
		arena.markets
			.filter((m) => cat === 'All' || m.cat === cat)
			.toSorted((a, b) => (sort === 'volume' ? b.vol - a.vol : b.traders - a.traders))
	);
	const claimable = $derived(QUESTS.filter((q) => !arena.claimed.includes(q.id) && (arena.quests[q.id] ?? 0) >= q.goal).length);
	const ranked = $derived([...arena.board.map((b) => ({ ...b, you: false })), { who: 'You', pnl: Math.round(arena.pnl), you: true }].toSorted((a, b) => b.pnl - a.pnl));
	const openCount = $derived(arena.markets.filter((m) => m.status === 'open').length);
	const volume = $derived(arena.markets.reduce((n, m) => n + m.vol, 0));
	const traders = $derived(arena.markets.reduce((n, m) => n + m.traders, 0));
	const rank = $derived(ranked.findIndex((r) => r.you) + 1);
	// No rank before a first trade (a fresh account sat at #7 with 0 profit).
	const traded = $derived(arena.held !== 0 || Math.round(arena.pnl) !== 0);
	const RANGE = ['var(--pool-1)', 'var(--pool-2)', 'var(--pool-3)', 'var(--pool-4)', 'var(--pool-5)', 'var(--pool-6)'];
	const colors = (m: Market) => (m.kind === 'binary' ? ['var(--good)', 'var(--bad)'] : RANGE.slice(0, m.outcomes.length));
	const fmt = (v: number) => v.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 });
	const signed = (v: number) => `${v >= 0 ? '+' : '−'}${fmt(Math.abs(v))}`;
	const short = (v: number) => (v >= 1000 ? `${(v / 1000).toFixed(1)}k` : String(Math.round(v)));
	const pct = (v: number) => `${Math.round(v * 100)}%`;

	onMount(() => {
		arena.start();
		return () => arena.stop();
	});
	function openMarket(id: string) {
		open = id;
		tab = 'markets';
		scrollTo({ top: 0, behavior: 'smooth' });
	}
</script>

<svelte:head><title>Markets · Onyx</title><meta name="description" content="Data markets: trade on what a public source will report, settled by Onyx observers. Practice credits on this preview." /></svelte:head>
<Confetti fire={arena.celebrate} />

<OnyxSlab><h1>Call the next number.</h1><div class="slabside" data-testid="slabside"><p class="eyebrow">Data markets</p><p class="slablede">Trade on what a public source will report: debt, blocks, gas, filings, prices, weather. Every market settles on a value that Onyx observers fetch and agree on.</p><p class="practice" data-testid="arena-practice"><span class="dot" aria-hidden="true"></span>Test network only: live markets use tONYX test tokens, practice markets use simulated credits. No real money.</p></div></OnyxSlab>
<div class="wrap arena" data-testid="arena">
	<!-- MP3: the real markets on the test network come first; the practice game follows. Kept mounted
	     (hidden) while a practice market is open, so a read is not lost. -->
	<div hidden={!!open}><LiveMarkets /></div>
	<div class="practice-intro" data-testid="practice-intro">
		<p class="eyebrow">Practice · simulated in your browser</p>
		<h2>Practice markets</h2>
		<p>Try trading with practice credits. Prices, volume and other traders are simulated on this page; nothing here touches a chain.</p>
	</div>
	<div class="player panel" aria-label="Your practice account" data-testid="arena-player">
		<div class="acct"><span>Balance</span><b class="mono" data-testid="arena-balance">{fmt(arena.balance)}</b></div>
		<div class="acct held"><span>In positions</span><b class="mono">{fmt(arena.held)}</b></div>
		<div class="acct"><span>Profit</span><b class="mono" class:pos={arena.pnl >= 0} class:neg={arena.pnl < 0}>{signed(arena.pnl)}</b></div>
		<div class="acct lv">
			<span>Level {arena.level.level}</span>
			<div class="bar" role="progressbar" aria-label="Progress to the next level" aria-valuemin={0} aria-valuemax={arena.level.need} aria-valuenow={arena.level.into}><i style:width={`${(arena.level.into / arena.level.need) * 100}%`}></i></div>
			<small class="mono">{arena.level.into} / {arena.level.need} XP</small>
		</div>
		<div class="acct streak" class:hot={arena.streak >= 2}><span>Streak</span><b class="mono"><Flame size={18} strokeWidth={1.75} aria-hidden="true" />{arena.streak}</b></div>
		<div class="toggles">
			<button type="button" class="icon" aria-pressed={arena.live} aria-label={arena.live ? 'Pause live prices' : 'Resume live prices'} onclick={() => (arena.live = !arena.live)}>
				{#if arena.live}<Pause size={16} strokeWidth={1.75} aria-hidden="true" />{:else}<Play size={16} strokeWidth={1.75} aria-hidden="true" />{/if}
			</button>
			<button type="button" class="icon" aria-pressed={arena.sound} aria-label={arena.sound ? 'Turn sound off' : 'Turn sound on'} onclick={() => (arena.sound = !arena.sound)}>
				{#if arena.sound}<Volume2 size={16} strokeWidth={1.75} aria-hidden="true" />{:else}<VolumeX size={16} strokeWidth={1.75} aria-hidden="true" />{/if}
			</button>
		</div>
	</div>

	<div class="tabs" role="group" aria-label="Market sections">
		{#each TABS as [key, label, Icon]}
			<button type="button" class:on={tab === key} aria-pressed={tab === key} onclick={() => { tab = key; open = null; }} data-testid={`arena-tab-${key}`}>
				<Icon size={16} strokeWidth={1.75} aria-hidden="true" />{label}{#if key === 'quests' && claimable}<span class="count">{claimable}</span>{/if}
			</button>
		{/each}
	</div>

	{#if tab === 'markets' && open}
		<ArenaMarket market={arena.market(open)} back={() => (open = null)} {colors} />
	{:else if tab === 'markets'}
		<section class="section floor-sec" aria-labelledby="all-markets">
		<h2 class="sr" id="all-markets">All markets</h2>
		<p class="eyebrow practice-head" data-testid="arena-practice-head">Practice markets · simulated in your browser</p>
		<div class="overview" aria-label="Market overview" data-testid="arena-overview">
			<div><span>Open markets</span><b class="mono">{openCount}</b></div>
			<div><span>Volume</span><b class="mono">{short(volume)}</b></div>
			<div><span>Traders</span><b class="mono">{traders.toLocaleString('en-US')}</b></div>
			<div><span>Your rank</span><b class="mono">{traded ? `#${rank}` : 'Unranked'}</b></div>
		</div>
		<div class="filters">
			<div class="chips" role="group" aria-label="Category">
				{#each ['All', ...CATEGORIES] as c}
					<button type="button" class:on={cat === c} aria-pressed={cat === c} onclick={() => (cat = c as Category | 'All')}>{c}</button>
				{/each}
			</div>
			<label class="sort"><span>Sort</span>
				<select id="arena-sort" bind:value={sort}><option value="trending">Trending</option><option value="volume">Volume</option></select>
			</label>
		</div>
		<div class="floor">
			<div class="cards" data-testid="arena-cards">
				{#each shown as m (m.id)}
					{@const p = arena.price(m, 0)}
					{@const mv = arena.moved[m.id]}
					<article class="card {mv ? `flash-${mv.d > 0 ? 'up' : 'down'}-${mv.n % 2}` : ''}" class:closed={m.status !== 'open'}>
						<button type="button" class="hit" onclick={() => openMarket(m.id)} aria-label={`Open ${m.title}`} data-testid={`arena-open-${m.id}`}></button>
						<div class="ctop">
							<span class="cat">{m.cat}</span>
							{#if m.status === 'resolved' && m.winner !== null}<span class="res">Resolved · {m.outcomes[m.winner]}</span>{:else if m.vol > 35000}<span class="hotb"><Flame size={12} strokeWidth={2} aria-hidden="true" />Hot</span>{/if}
						</div>
						<h3>{m.title}</h3>
						{#if m.kind === 'binary'}
							<div class="odds">
								<span class="pctbig mono" class:pos={p >= 0.5} class:neg={p < 0.5}>{pct(p)}</span><span class="fine">chance</span>
								<div class="spark"><ArenaChart series={[m.hist[0]]} colors={[p >= 0.5 ? 'var(--good)' : 'var(--bad)']} small label={`Simulated Yes price trend for ${m.title}`} /></div>
							</div>
							<div class="yn">
								<button type="button" class="y" disabled={m.status !== 'open'} onclick={() => openMarket(m.id)}>Yes {Math.round(p * 100)}¢</button>
								<button type="button" class="n" disabled={m.status !== 'open'} onclick={() => openMarket(m.id)}>No {Math.round((1 - p) * 100)}¢</button>
							</div>
						{:else}
							{@const ps = m.outcomes.map((o, j) => ({ o, j, p: arena.price(m, j) })).toSorted((a, b) => b.p - a.p).slice(0, 3)}
							<ul class="range">
								{#each ps as r}
									<li><span>{r.o}</span><span class="rbar"><i style:width={pct(r.p)} style:background={colors(m)[r.j]}></i></span><b class="mono">{pct(r.p)}</b></li>
								{/each}
							</ul>
						{/if}
						<div class="cfoot"><span>{short(m.vol)} vol</span><span>{m.traders.toLocaleString('en-US')} traders</span><span>Closes {m.closes}</span></div>
					</article>
				{/each}
			</div>
			<div class="side">
			{#if arena.positions.length}
				<aside class="panel mypos" aria-label="Your positions">
					<p class="eyebrow"><Wallet size={14} strokeWidth={1.75} aria-hidden="true" /> Your positions</p>
					{#each arena.positions.slice(0, 5) as p}
						{@const m = arena.market(p.mid)}
						{@const v = arena.value(p)}
						<button type="button" class="prow" onclick={() => openMarket(p.mid)}>
							<span>{m.outcomes[p.i]} · {m.title}</span>
							<b class="mono" class:pos={v >= p.cost} class:neg={v < p.cost}>{signed(v - p.cost)}</b>
						</button>
					{/each}
				</aside>
			{/if}
			<aside class="feed panel" aria-label="Live trades (simulated)" data-testid="arena-feed">
				<p class="eyebrow"><Activity size={14} strokeWidth={1.75} aria-hidden="true" /> Live trades · simulated</p>
				<ol>
					{#each arena.feed as f (f.id)}
						<li class:you={f.you}>
							<span class="who">{f.who}</span>
							<span class="what">{f.sell ? 'sold' : 'bought'} <b class:pos={f.outcome === 'Yes'} class:neg={f.outcome === 'No'}>{f.outcome}</b> · {arena.market(f.mid).title}</span>
							<span class="amt mono">{fmt(f.spend)} @ {Math.round(f.price * 100)}¢</span>
						</li>
					{:else}
						<li class="fine">Trades appear here as they happen.</li>
					{/each}
				</ol>
			</aside>
			</div>
		</div>
		</section>
	{:else if tab === 'flash'}
		<ArenaFlash />
	{:else if tab === 'parlay'}
		<ArenaParlay />
	{:else if tab === 'create'}
		<ArenaCreate opened={(id) => openMarket(id)} />
	{:else if tab === 'verify'}
		<section aria-label="Verify data" data-testid="arena-verify">
			<StakeSimulator />
		</section>
	{:else if tab === 'portfolio'}
		<section class="portfolio" aria-labelledby="pf-title" data-testid="arena-portfolio">
			<p class="eyebrow">Portfolio</p>
			<h2 id="pf-title">Worth {fmt(arena.worth)} credits</h2>
			<div class="kpis">
				<div class="panel"><span>Trades</span><b class="mono">{arena.trades}</b></div>
				<div class="panel"><span>Wins / losses</span><b class="mono">{arena.wins} / {arena.losses}</b></div>
				<div class="panel"><span>Best streak</span><b class="mono">{arena.best}</b></div>
				<div class="panel"><span>Badges</span><b class="mono">{arena.badges.length} / {Object.keys(BADGES).length}</b></div>
			</div>
			{#if arena.positions.length}
				<div class="table-wrap">
					<table>
						<thead><tr><th scope="col">Market</th><th scope="col">Outcome</th><th scope="col">Shares</th><th scope="col">Cost</th><th scope="col">Value now</th><th scope="col">Profit</th><th scope="col"><span class="sr">Actions</span></th></tr></thead>
						<tbody>
							{#each arena.positions as p}
								{@const m = arena.market(p.mid)}
								{@const v = arena.value(p)}
								<tr>
									<td><button type="button" class="link" onclick={() => openMarket(p.mid)}>{m.title}</button></td>
									<td>{m.outcomes[p.i]}</td>
									<td class="mono">{p.shares.toFixed(1)}</td>
									<td class="mono">{fmt(p.cost)}</td>
									<td class="mono">{fmt(v)}</td>
									<td class="mono" class:pos={v >= p.cost} class:neg={v < p.cost}>{signed(v - p.cost)}</td>
									<td><button type="button" class="ghost" onclick={() => arena.cashOut(p.mid, p.i)}>Cash out</button></td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			{:else}
				<p class="empty">No open positions. Pick a market and buy Yes or No to start.</p>
			{/if}
			<div class="badges">
				{#each Object.entries(BADGES) as [id, name]}
					<span class="badge" class:got={arena.badges.includes(id)}><Trophy size={14} strokeWidth={1.75} aria-hidden="true" />{name}</span>
				{/each}
			</div>
			<div class="limits panel">
				<label class="sort"><span>Daily practice limit</span>
					<select id="arena-limit" bind:value={arena.limit} onchange={() => arena.save()}>
						<option value={0}>Off</option><option value={100}>100 credits</option><option value={250}>250 credits</option><option value={500}>500 credits</option>
					</select>
				</label>
				<p class="fine">Spent today: {fmt(arena.spentToday)} credits. A real market would offer the same limit.</p>
				<button type="button" class="ghost" onclick={() => arena.reset()} data-testid="arena-reset">Reset practice account</button>
			</div>
		</section>
	{:else if tab === 'board'}
		<section aria-labelledby="lb-title" data-testid="arena-board">
			<p class="eyebrow">Leaderboard · this week · simulated traders</p>
			<h2 id="lb-title">Climb the board.</h2>
			<ol class="board">
				{#each ranked as r, k (r.who)}
					<li class:you={r.you} class:top={k < 3}>
						<span class="rank mono">{k + 1}</span>
						<span class="who">{r.who}</span>
						<span class="mono" class:pos={r.pnl >= 0} class:neg={r.pnl < 0}>{signed(r.pnl)}</span>
					</li>
				{/each}
			</ol>
		</section>
	{:else}
		<section aria-labelledby="q-title" data-testid="arena-quests">
			<p class="eyebrow">Daily quests</p>
			<h2 id="q-title">Finish quests, earn bonus credits.</h2>
			<div class="quests">
				{#each QUESTS as q}
					{@const n = Math.min(q.goal, arena.quests[q.id] ?? 0)}
					{@const done = arena.claimed.includes(q.id)}
					<div class="quest panel" class:ready={n >= q.goal && !done}>
						<div><b>{q.text}</b><span class="fine">+{q.reward} credits · +{q.reward} XP</span></div>
						<div class="bar" role="progressbar" aria-label={q.text} aria-valuemin={0} aria-valuemax={q.goal} aria-valuenow={n}><i style:width={`${(n / q.goal) * 100}%`}></i></div>
						<button type="button" class="ghost" disabled={n < q.goal || done} onclick={() => arena.claim(q.id)} data-testid={`quest-${q.id}`}>{done ? 'Claimed' : n >= q.goal ? 'Claim' : `${n} / ${q.goal}`}</button>
					</div>
				{/each}
			</div>
		</section>
	{/if}
</div>

<div class="toasts" aria-live="polite">
	{#each arena.toasts as t (t.id)}
		<div class="toast {t.tone}">{t.text}</div>
	{/each}
</div>

<style>
	h1 { font-size: var(--text-h1); line-height: var(--lh-display); margin: 0 0 var(--sp-5); }
	.arena { padding-block: var(--sp-5) var(--sp-10); }
	.floor-sec { padding-top: 0; }
	.practice-head { margin-bottom: var(--sp-3); }
	.practice-intro { display: grid; gap: var(--sp-2); margin: var(--sp-4) 0 var(--sp-4); }
	.practice-intro .eyebrow { margin: 0; font: 500 var(--text-sm) / var(--lh-body) var(--mono); color: var(--muted); }
	.practice-intro h2 { margin: 0; }
	.practice-intro p:last-child { margin: 0; color: var(--muted); max-width: 46rem; line-height: var(--lh-body); }
	.practice { display: inline-flex; align-items: center; gap: var(--sp-2); margin-top: var(--sp-4); padding: var(--sp-2) var(--sp-3); border: 1px solid var(--line-strong); border-radius: var(--radius-pill); font-size: var(--text-xs); color: var(--muted); max-width: 26rem; }
	.dot { width: 8px; height: 8px; border-radius: 50%; background: var(--btc); flex-shrink: 0; }
	.panel { padding: var(--sp-4); border: 1px solid var(--line); border-radius: var(--radius-lg); background: var(--panel); }
	.player { display: flex; flex-wrap: wrap; align-items: center; gap: var(--sp-4) var(--sp-7); }
	.acct { display: grid; gap: var(--sp-1); }
	.acct > span { font-size: var(--text-xs); color: var(--muted); text-transform: uppercase; letter-spacing: 0.06em; }
	.acct b { font-size: var(--text-lg); font-weight: 500; display: inline-flex; align-items: center; gap: var(--sp-1); font-variant-numeric: tabular-nums; }
	.acct.lv { min-width: 10rem; }
	.acct.lv small { font-size: var(--text-xs); color: var(--muted); }
	.streak.hot b { color: var(--btc); }
	.bar { height: 6px; border-radius: var(--radius-pill); background: var(--line); overflow: hidden; }
	.bar i { display: block; height: 100%; background: var(--good); border-radius: inherit; transition: width var(--dur-slow) var(--ease); }
	.toggles { margin-left: auto; display: flex; gap: var(--sp-2); }
	.icon { display: inline-grid; place-items: center; width: 44px; height: 44px; border-radius: 50%; border: 1px solid var(--line-strong); background: transparent; color: var(--ink); }
	.icon:hover { background: var(--hover); }
	.mono { font-family: var(--mono); }
	.pos { color: var(--good); }
	.neg { color: var(--bad); }
	.fine { font-size: var(--text-xs); color: var(--muted); }
	.tabs { display: flex; flex-wrap: wrap; gap: var(--sp-2); padding-block: var(--sp-5); margin-bottom: var(--sp-5); border-bottom: 1px solid var(--line); }
	.tabs button { display: inline-flex; align-items: center; gap: var(--sp-2); min-height: 44px; padding-inline: var(--sp-4); border-radius: var(--radius-pill); border: 1px solid var(--line); background: transparent; color: var(--muted); }
	.tabs button:hover { color: var(--ink); background: var(--hover); }
	.tabs button.on { color: var(--ink); border-color: var(--accent-line); background: var(--accent-soft); }
	.count { display: inline-grid; place-items: center; min-width: 20px; height: 20px; padding-inline: var(--sp-1); border-radius: var(--radius-pill); background: var(--btc); color: var(--accent-ink); font-size: var(--text-xs); font-weight: 600; }
	.filters { display: flex; flex-wrap: wrap; justify-content: space-between; align-items: center; gap: var(--sp-3); margin-bottom: var(--sp-5); }
	.chips { display: flex; flex-wrap: wrap; gap: var(--sp-2); }
	.chips button { min-height: 44px; padding-inline: var(--sp-3); border-radius: var(--radius-pill); border: 1px solid var(--line); background: transparent; color: var(--muted); font-size: var(--text-sm); }
	.chips button:hover { color: var(--ink); }
	.chips button.on { color: var(--accent-ink); background: var(--accent); border-color: var(--accent); }
	.sort { display: inline-flex; align-items: center; gap: var(--sp-2); font-size: var(--text-sm); color: var(--muted); }
	.sort select { min-height: 44px; }
	.floor { display: grid; grid-template-columns: minmax(0, 1fr) 19rem; gap: var(--sp-5); align-items: start; }
	.cards { display: grid; grid-template-columns: repeat(auto-fill, minmax(16rem, 1fr)); gap: var(--sp-3); }
	.card { position: relative; display: flex; flex-direction: column; gap: var(--sp-3); padding: var(--sp-4); border: 1px solid var(--line); border-radius: var(--radius-lg); background: var(--panel); transition: border-color var(--dur) var(--ease), transform var(--dur) var(--ease); }
	.card:hover { border-color: var(--line-bright); transform: translateY(-2px); }
	.card.closed { opacity: 0.75; }
	.card.flash-up-0 { animation: flashup0 0.9s var(--ease); }
	.card.flash-up-1 { animation: flashup1 0.9s var(--ease); }
	.card.flash-down-0 { animation: flashdown0 0.9s var(--ease); }
	.card.flash-down-1 { animation: flashdown1 0.9s var(--ease); }
	/* Two names per direction, so a second move in a row restarts the flash. */
	@keyframes flashup0 { 0% { box-shadow: 0 0 0 1px var(--good), 0 0 24px var(--good-soft); } 100% { box-shadow: none; } }
	@keyframes flashup1 { 0% { box-shadow: 0 0 0 1px var(--good), 0 0 24px var(--good-soft); } 100% { box-shadow: none; } }
	@keyframes flashdown0 { 0% { box-shadow: 0 0 0 1px var(--bad); } 100% { box-shadow: none; } }
	@keyframes flashdown1 { 0% { box-shadow: 0 0 0 1px var(--bad); } 100% { box-shadow: none; } }
	.hit { position: absolute; inset: 0; border: 0; background: transparent; border-radius: inherit; cursor: pointer; }
	.yn, .yn button { position: relative; z-index: 1; }
	.ctop { display: flex; justify-content: space-between; align-items: center; gap: var(--sp-2); }
	.cat { font-size: var(--text-xs); color: var(--muted); text-transform: uppercase; letter-spacing: 0.06em; }
	.hotb { display: inline-flex; align-items: center; gap: var(--sp-1); padding: var(--sp-0) var(--sp-2); border-radius: var(--radius-pill); background: var(--btc-soft); color: var(--btc); font-size: var(--text-xs); }
	.res { padding: var(--sp-0) var(--sp-2); border-radius: var(--radius-pill); background: var(--good-soft); color: var(--good); font-size: var(--text-xs); }
	.card h3 { font-size: var(--text-base); line-height: var(--lh-snug); font-weight: 500; margin: 0; text-wrap: pretty; }
	.odds { display: grid; grid-template-columns: auto 1fr; grid-template-rows: auto auto; align-items: end; column-gap: var(--sp-3); }
	.pctbig { font-size: var(--text-2xl); line-height: var(--lh-none); grid-row: 1; }
	.odds .fine { grid-row: 2; grid-column: 1; }
	.spark { grid-row: 1 / span 2; grid-column: 2; align-self: center; }
	.yn { display: grid; grid-template-columns: 1fr 1fr; gap: var(--sp-2); margin-top: auto; }
	.yn button { min-height: 44px; border-radius: var(--radius-sm); border: 1px solid transparent; font: 500 var(--text-sm) var(--mono); transition: filter var(--dur) var(--ease), transform var(--dur) var(--ease); }
	.yn .y { background: var(--good-soft); color: var(--good); border-color: var(--good-line); }
	.yn .n { background: color-mix(in srgb, var(--bad) 10%, transparent); color: var(--bad); border-color: color-mix(in srgb, var(--bad) 35%, transparent); }
	.yn button:hover:not(:disabled) { filter: brightness(1.2); }
	.yn button:active:not(:disabled) { transform: scale(0.97); }
	.yn button:disabled { opacity: 0.5; }
	.range { list-style: none; padding: 0; margin: 0 0 auto; display: grid; gap: var(--sp-2); }
	.range li { display: grid; grid-template-columns: minmax(0, 1fr) 4rem 2.5rem; align-items: center; gap: var(--sp-2); font-size: var(--text-xs); }
	.range li span:first-child { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.rbar { height: 6px; border-radius: var(--radius-pill); background: var(--line); overflow: hidden; }
	.rbar i { display: block; height: 100%; transition: width var(--dur-slow) var(--ease); }
	.range b { text-align: right; font-weight: 500; }
	.cfoot { display: flex; flex-wrap: wrap; gap: var(--sp-1) var(--sp-3); font-size: var(--text-xs); color: var(--muted); border-top: 1px solid var(--line); padding-top: var(--sp-2); }
	.side { display: grid; gap: var(--sp-3); position: sticky; top: calc(env(safe-area-inset-top, 0px) + 88px); }
	.mypos { display: grid; gap: var(--sp-1); }
	.mypos .eyebrow { display: inline-flex; gap: var(--sp-2); align-items: center; }
	.prow { display: flex; justify-content: space-between; gap: var(--sp-3); align-items: center; min-height: 44px; padding: var(--sp-1) var(--sp-2); border: 0; border-radius: var(--radius-sm); background: transparent; color: var(--ink); text-align: left; font-size: var(--text-xs); line-height: var(--lh-snug); }
	.prow:hover { background: var(--hover); }
	.prow span { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.overview { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: var(--sp-3); margin-bottom: var(--sp-5); }
	.overview div { display: grid; gap: var(--sp-1); padding: var(--sp-3) var(--sp-4); border-left: 2px solid var(--accent-line); }
	.overview span { font-size: var(--text-xs); color: var(--muted); text-transform: uppercase; letter-spacing: 0.06em; }
	.overview b { font-size: var(--text-lg); font-weight: 500; }
		.feed ol { list-style: none; padding: 0; margin: var(--sp-3) 0 0; display: grid; gap: var(--sp-2); }
	.feed li { display: grid; gap: var(--sp-0); padding: var(--sp-2); border-radius: var(--radius-sm); font-size: var(--text-xs); line-height: var(--lh-snug); animation: slidein 0.4s var(--ease); }
	.feed li.you { background: var(--accent-soft); }
	/* The panel is sticky: eight trades keep it inside a laptop screen while the grid scrolls. */
	.feed li:nth-child(n + 9) { display: none; }
	@media (max-width: 1000px) { .feed li:nth-child(n + 7) { display: none; } }
	.feed .who { font-family: var(--mono); color: var(--muted); }
	.feed .what { color: var(--ink); }
	.feed .amt { color: var(--muted); }
	@keyframes slidein { from { opacity: 0; transform: translateY(-6px); } to { opacity: 1; transform: none; } }
	h2 { font-size: var(--text-xl); line-height: var(--lh-tight); margin: var(--sp-2) 0 var(--sp-5); }
	.kpis { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: var(--sp-3); margin-bottom: var(--sp-5); }
	.kpis .panel { display: grid; gap: var(--sp-1); }
	.kpis span { font-size: var(--text-xs); color: var(--muted); }
	.kpis b { font-size: var(--text-xl); font-weight: 500; }
	.table-wrap { overflow-x: auto; border: 1px solid var(--line); border-radius: var(--radius-lg); }
	table { width: 100%; border-collapse: collapse; font-size: var(--text-sm); }
	th, td { padding: var(--sp-3); text-align: left; border-bottom: 1px solid var(--line); white-space: nowrap; }
	th { font-size: var(--text-xs); color: var(--muted); font-weight: 500; }
	td:first-child { white-space: normal; min-width: 14rem; }
	.link { background: transparent; border: 0; color: var(--ink); text-align: left; padding: 0; text-decoration: underline; text-underline-offset: 3px; min-height: 44px; }
	.sr { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }
	.ghost { display: inline-flex; align-items: center; gap: var(--sp-1); min-height: 44px; padding: var(--sp-1) var(--sp-3); border: 1px solid var(--line-strong); border-radius: var(--radius-pill); background: transparent; color: var(--ink); font-size: var(--text-sm); }
	.ghost:hover:not(:disabled) { background: var(--hover); }
	.ghost:disabled { color: var(--muted); }
	.empty { color: var(--muted); padding: var(--sp-6); border: 1px dashed var(--line-strong); border-radius: var(--radius-lg); }
	.badges { display: flex; flex-wrap: wrap; gap: var(--sp-2); margin-block: var(--sp-6); }
	.badge { display: inline-flex; align-items: center; gap: var(--sp-1); padding: var(--sp-1) var(--sp-3); border-radius: var(--radius-pill); border: 1px dashed var(--line-strong); color: var(--muted); font-size: var(--text-sm); }
	.badge.got { border: 1px solid var(--btc); color: var(--btc); background: var(--btc-soft); }
	.limits { display: flex; flex-wrap: wrap; align-items: center; gap: var(--sp-4); }
	.limits .fine { margin: 0; flex: 1; min-width: 12rem; }
	.board { list-style: none; padding: 0; margin: 0; display: grid; gap: var(--sp-2); max-width: 40rem; }
	.board li { display: grid; grid-template-columns: 2.5rem 1fr auto; align-items: center; gap: var(--sp-3); padding: var(--sp-3) var(--sp-4); border: 1px solid var(--line); border-radius: var(--radius-md); background: var(--panel); }
	.board li.top .rank { color: var(--btc); }
	.board li.you { border-color: var(--accent); background: var(--accent-soft); }
	.board .who { font-family: var(--mono); }
	.quests { display: grid; gap: var(--sp-3); max-width: 44rem; }
	.quest { display: grid; grid-template-columns: minmax(0, 1fr) 8rem auto; align-items: center; gap: var(--sp-4); }
	.quest > div:first-child { display: grid; gap: var(--sp-1); }
	.quest.ready { border-color: var(--btc); }
	.toasts { position: fixed; right: calc(var(--sp-5) + env(safe-area-inset-right, 0px)); bottom: calc(var(--sp-5) + env(safe-area-inset-bottom, 0px)); display: grid; gap: var(--sp-2); z-index: 50; max-width: min(24rem, calc(100vw - 2 * var(--sp-5))); }
	.toast { padding: var(--sp-3) var(--sp-4); border-radius: var(--radius-md); background: var(--panel-solid); border: 1px solid var(--line-strong); box-shadow: var(--shadow-pop); font-size: var(--text-sm); animation: slidein 0.3s var(--ease); }
	.toast.good { border-color: var(--good-line); color: var(--good); }
	.toast.bad { border-color: color-mix(in srgb, var(--bad) 40%, transparent); color: var(--bad); }
	@media (max-width: 1000px) { .floor { grid-template-columns: 1fr; } .side { position: static; } }
	@media (max-width: 640px) { .overview { grid-template-columns: 1fr 1fr; } }
	/* MP3: on phones the section tabs sit in two even columns (they wrapped into ragged rows). */
	@media (max-width: 640px) { .tabs { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); } .tabs button { justify-content: flex-start; min-width: 0; } }
	@media (max-width: 640px) { .kpis { grid-template-columns: 1fr 1fr; } .quest { grid-template-columns: 1fr; } .player { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: var(--sp-3); } .player .held { display: none; } .acct b { font-size: var(--text-md); white-space: nowrap; } .acct.lv { min-width: 0; } .toggles { grid-column: 1 / -1; margin-left: 0; justify-content: flex-end; } }
	/* After the `font` shorthands above, which reset it. */
	.yn button { font-variant-numeric: tabular-nums; }
</style>
