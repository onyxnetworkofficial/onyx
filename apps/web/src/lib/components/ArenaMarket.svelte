<script lang="ts">
	// One market in the Markets arena: the price chart, the buy ticket with its live payout, the
	// viewer's position with a cash-out, and the Onyx resolution replayed step by step (7 observers
	// fetch the source, a lottery draws 5, all 5 must agree). Practice credits only.
	import { arena, type Market } from '#lib/arena-state.svelte.ts';
	import { quote, prices } from '#lib/arena.ts';
	import ArenaChart from '#lib/components/ArenaChart.svelte';
	import ChevronLeft from '@lucide/svelte/icons/chevron-left';
	import ShieldCheck from '@lucide/svelte/icons/shield-check';
	import RotateCcw from '@lucide/svelte/icons/rotate-ccw';
	let { market, back, colors }: { market: Market; back: () => void; colors: (m: Market) => string[] } = $props();

	let pick = $state(0);
	let amount = $state('25');
	let error = $state('');
	let step = $state(-1);
	let chosen = $state<number | null>(null);
	let drawn = $state<number[]>([]);
	const p = $derived(prices(market.q, market.b));
	const spend = $derived(Number(amount));
	const qt = $derived(spend > 0 && Number.isFinite(spend) ? quote(market.q, market.b, pick, spend) : null);
	const mine = $derived(arena.positions.filter((x) => x.mid === market.id));
	const pal = $derived(colors(market));
	const pct = (v: number) => `${Math.round(v * 100)}%`;
	const cents = (v: number) => `${(v * 100).toFixed(1)}¢`;
	const fmt = (v: number) => v.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 });

	function buy() {
		error = arena.buy(market.id, pick, spend) ?? '';
	}
	function add(v: number | 'max') {
		const now = Number(amount) || 0;
		amount = String(v === 'max' ? Math.floor(arena.balance * 100) / 100 : Math.round((now + v) * 100) / 100);
		error = '';
	}

	/** Replay the Onyx check: observers fetch, the lottery draws 5, they agree, the market pays. */
	function resolve(force: number | null) {
		if (market.status !== 'open') return;
		let w = force;
		if (w === null) {
			let r = Math.random();
			w = p.length - 1;
			for (let k = 0; k < p.length; k++) {
				r -= p[k];
				if (r <= 0) {
					w = k;
					break;
				}
			}
		}
		chosen = w;
		market.status = 'resolving';
		const all = [0, 1, 2, 3, 4, 5, 6];
		drawn = all.sort(() => Math.random() - 0.5).slice(0, 5).sort();
		step = 0;
		const next = () => {
			step += 1;
			if (step < 3) setTimeout(next, 1100);
			else arena.settle(market.id, w as number);
		};
		setTimeout(next, 1100);
	}
	function again() {
		step = -1;
		chosen = null;
		arena.reopen(market.id);
	}
</script>

<section class="detail" aria-labelledby="mk-title" data-testid="arena-detail">
	<button type="button" class="back" onclick={back}><ChevronLeft size={16} strokeWidth={1.75} aria-hidden="true" /> All markets</button>
	<div class="head">
		<div>
			<p class="eyebrow">{market.cat} · {market.kind === 'binary' ? 'Yes or No' : 'Range'} · closes {market.closes}</p>
			<h2 id="mk-title">{market.title}</h2>
			<p class="meta">{fmt(market.vol)} credits traded · {market.traders.toLocaleString('en-US')} traders · simulated</p>
		</div>
		{#if market.status === 'resolved' && market.winner !== null}
			<span class="tag good" data-testid="arena-resolved">Resolved · {market.outcomes[market.winner]}</span>
		{:else}
			{@const lead = market.kind === 'binary' ? 0 : p.indexOf(Math.max(...p))}
			<span class="big" class:up={market.kind === 'binary' && p[0] >= 0.5} class:range={market.kind === 'range'}>{pct(p[lead])}<small>{market.kind === 'binary' ? 'chance of Yes' : `most likely: ${market.outcomes[lead]}`}</small></span>
		{/if}
	</div>
	<div class="grid">
		<div class="main">
			<div class="panel chart">
				<ArenaChart series={market.hist} colors={pal} label={`Simulated price history for ${market.title}`} />
				<ul class="legend">
					{#each market.outcomes as o, j}
						<li><span class="sw" style:background={pal[j]}></span>{o} <b>{pct(p[j])}</b></li>
					{/each}
				</ul>
			</div>
			<div class="panel resolve" data-testid="arena-resolution">
				<p class="eyebrow"><ShieldCheck size={14} strokeWidth={1.75} aria-hidden="true" /> How this market resolves</p>
				<p>Seven independent observers read <b>{market.source}</b> after the market closes. A public lottery draws five of them, and all five must report the same value. The value, the signatures and the draw go on the receipt, so anyone can check the result. A crowd or a stake vote never decides it.</p>
				<p class="fine">Onyx dataset: <code>{market.dataset}</code></p>
				{#if step >= 0}
					<ol class="steps" aria-live="polite">
						<li class:on={step >= 0}>
							<span>Observers fetch the source</span>
							<span class="dots">{#each [0, 1, 2, 3, 4, 5, 6] as o}<i class:lit={step >= 0} class:drawn={step >= 1 && drawn.includes(o)}></i>{/each}</span>
						</li>
						<li class:on={step >= 1}><span>Lottery draws 5 of 7</span><span class="mono">{step >= 1 ? drawn.map((d) => `#${d + 1}`).join(' ') : '…'}</span></li>
						<li class:on={step >= 2}><span>5 of 5 agree</span><span class="mono">{step >= 2 && chosen !== null ? market.evidence[chosen] : '…'}</span></li>
						<li class:on={step >= 3}><span>Market settles</span><span class="mono">{step >= 3 && chosen !== null ? `${market.outcomes[chosen]} wins` : '…'}</span></li>
					</ol>
				{/if}
				{#if market.status === 'open'}
					<div class="presenter">
						<p class="fine">Demo controls (the real market waits for the source):</p>
						<div class="row">
							<button type="button" class="ghost" onclick={() => resolve(null)} data-testid="arena-resolve">Resolve now</button>
							{#each market.outcomes as o, j}
								<button type="button" class="ghost" onclick={() => resolve(j)} data-testid={`arena-force-${j}`}>Force {o}</button>
							{/each}
						</div>
					</div>
				{:else if market.status === 'resolved'}
					<button type="button" class="ghost" onclick={again} data-testid="arena-reopen"><RotateCcw size={14} strokeWidth={1.75} aria-hidden="true" /> Reopen this market</button>
				{/if}
			</div>
		</div>
		<aside class="panel ticket" aria-label="Buy">
			<div class="outs" role="group" aria-label="Choose an outcome">
				{#each market.outcomes as o, j}
					<button type="button" class="out" class:yes={market.kind === 'binary' && j === 0} class:no={market.kind === 'binary' && j === 1} class:sel={pick === j} aria-pressed={pick === j} onclick={() => { pick = j; error = ''; }} data-testid={`arena-pick-${j}`}>
						<span>{o}</span><b>{cents(p[j])}</b>
					</button>
				{/each}
			</div>
			<label class="field"><span>Amount (practice credits)</span>
				<input id="arena-amount" bind:value={amount} oninput={() => (error = '')} inputmode="decimal" data-testid="arena-amount" />
			</label>
			<div class="chips">
				{#each [1, 10, 50, 100] as v}<button type="button" onclick={() => add(v)}>+{v}</button>{/each}
				<button type="button" onclick={() => add('max')}>Max</button>
			</div>
			<div class="win" data-testid="arena-towin">
				<span>To win</span>
				<b>{qt ? fmt(qt.payout) : '0.00'}</b>
			</div>
			{#if qt}
				<dl class="calc">
					<div><dt>Average price</dt><dd>{cents(qt.avg)}</dd></div>
					<div><dt>Price after your buy</dt><dd>{cents(qt.priceAfter)}</dd></div>
					<div><dt>Profit if right</dt><dd class="pos">+{fmt(qt.payout - spend)}</dd></div>
				</dl>
			{/if}
			{#if error}<p role="alert" class="err">{error}</p>{/if}
			<button type="button" class="buy" class:no={market.kind === 'binary' && pick === 1} disabled={market.status !== 'open'} onclick={buy} data-testid="arena-buy">
				{market.status === 'open' ? `Buy ${market.outcomes[pick]}` : 'Market closed'}
			</button>
			<p class="fine">Balance {fmt(arena.balance)} credits</p>
			{#if mine.length}
				<div class="mine" data-testid="arena-mine">
					<p class="eyebrow">Your position</p>
					{#each mine as m}
						{@const val = arena.value(m)}
						<div class="pos-row">
							<span>{m.shares.toFixed(1)} × {market.outcomes[m.i]}</span>
							<span class:pos={val >= m.cost} class:neg={val < m.cost}>{fmt(val)} ({val >= m.cost ? '+' : ''}{fmt(val - m.cost)})</span>
							<button type="button" class="ghost" onclick={() => arena.cashOut(market.id, m.i)}>Cash out</button>
						</div>
					{/each}
				</div>
			{/if}
		</aside>
	</div>
</section>

<style>
	.back { display: inline-flex; align-items: center; gap: var(--sp-1); min-height: 44px; background: transparent; border: 0; color: var(--muted); padding: 0; }
	.back:hover { color: var(--ink); }
	.head { display: flex; justify-content: space-between; gap: var(--sp-5); align-items: start; margin-block: var(--sp-3) var(--sp-5); }
	h2 { font-size: var(--text-xl); line-height: var(--lh-tight); margin: var(--sp-2) 0; max-width: 32ch; text-wrap: balance; }
	.meta { color: var(--muted); font-size: var(--text-sm); margin: 0; }
	.big { display: grid; justify-items: end; font: var(--text-2xl)/var(--lh-none) var(--mono); color: var(--bad); font-variant-numeric: tabular-nums; }
	.big.up { color: var(--good); }
	.big.range { color: var(--ink); }
	.big small { font: var(--text-xs)/var(--lh-snug) var(--body); color: var(--muted); margin-top: var(--sp-1); }
	.tag { padding: var(--sp-1) var(--sp-3); border-radius: var(--radius-pill); border: 1px solid var(--good-line); background: var(--good-soft); color: var(--good); font-size: var(--text-sm); white-space: nowrap; }
	.grid { display: grid; grid-template-columns: minmax(0, 1.6fr) minmax(17rem, 1fr); gap: var(--sp-5); align-items: start; }
	.main { display: grid; gap: var(--sp-5); min-width: 0; }
	.panel { padding: var(--sp-5); border: 1px solid var(--line); border-radius: var(--radius-lg); background: var(--panel); }
	.legend { list-style: none; padding: 0; margin: var(--sp-3) 0 0; display: flex; flex-wrap: wrap; gap: var(--sp-2) var(--sp-4); font-size: var(--text-sm); color: var(--muted); }
	.legend li { display: inline-flex; align-items: center; gap: var(--sp-2); }
	.legend b { color: var(--ink); font-family: var(--mono); font-weight: 500; }
	.sw { width: 10px; height: 10px; border-radius: 50%; }
	.resolve p { line-height: var(--lh-body); color: var(--muted); max-width: 68ch; }
	.resolve p b { color: var(--ink); font-weight: 500; }
	.resolve .eyebrow { display: inline-flex; gap: var(--sp-2); align-items: center; }
	code { font: var(--text-sm) var(--mono); color: var(--ink); }
	.steps { list-style: none; padding: 0; margin: var(--sp-4) 0; display: grid; gap: var(--sp-2); }
	.steps li { display: flex; justify-content: space-between; gap: var(--sp-4); align-items: center; padding: var(--sp-3); border: 1px solid var(--line); border-radius: var(--radius-md); color: var(--muted); font-size: var(--text-sm); opacity: 0.45; transition: opacity var(--dur-slow) var(--ease), border-color var(--dur-slow) var(--ease); }
	.steps li.on { opacity: 1; color: var(--ink); border-color: var(--good-line); }
	.mono { font-family: var(--mono); text-align: right; }
	.dots { display: inline-flex; gap: var(--sp-1); }
	.dots i { width: 10px; height: 10px; border-radius: 50%; border: 1px solid var(--line-strong); transition: background var(--dur-slow) var(--ease); }
	.dots i.lit { background: var(--line-strong); }
	.dots i.drawn { background: var(--good); border-color: var(--good); }
	.presenter .row { display: flex; flex-wrap: wrap; gap: var(--sp-2); }
	.ghost { display: inline-flex; align-items: center; gap: var(--sp-1); min-height: 44px; padding: var(--sp-1) var(--sp-3); border: 1px solid var(--line-strong); border-radius: var(--radius-pill); background: transparent; color: var(--ink); font-size: var(--text-sm); }
	.ghost:hover { background: var(--hover); }
	.ticket { position: sticky; top: calc(env(safe-area-inset-top, 0px) + 88px); display: grid; gap: var(--sp-4); }
	.outs { display: grid; grid-template-columns: repeat(auto-fit, minmax(7rem, 1fr)); gap: var(--sp-2); }
	.out { display: flex; justify-content: space-between; align-items: center; gap: var(--sp-2); min-height: 48px; padding: var(--sp-2) var(--sp-3); border: 1px solid var(--line-strong); border-radius: var(--radius-md); background: transparent; color: var(--ink); text-align: left; font-size: var(--text-sm); }
	.out b { font-family: var(--mono); font-weight: 500; }
	.out:hover { background: var(--hover); }
	.out.sel { border-color: var(--accent); background: var(--accent-soft); }
	.out.yes.sel { border-color: var(--good); background: var(--good-soft); color: var(--good); }
	.out.no.sel { border-color: var(--bad); background: color-mix(in srgb, var(--bad) 10%, transparent); color: var(--bad); }
	.field { display: grid; gap: var(--sp-1); }
	.field > span { font-size: var(--text-sm); color: var(--muted); }
	input { width: 100%; min-height: 48px; font: var(--text-field) var(--mono); }
	.chips { display: flex; flex-wrap: wrap; gap: var(--sp-2); }
	.chips button { min-height: 44px; padding-inline: var(--sp-3); border: 1px solid var(--line); border-radius: var(--radius-pill); background: var(--panel-2); color: var(--ink); font: var(--text-sm) var(--mono); }
	.chips button:hover { border-color: var(--line-strong); background: var(--hover); }
	.win { display: flex; justify-content: space-between; align-items: baseline; padding: var(--sp-4); border-radius: var(--radius-md); background: var(--good-soft); border: 1px solid var(--good-line); }
	.win span { color: var(--muted); font-size: var(--text-sm); }
	.win b { font: 600 var(--text-2xl)/var(--lh-none) var(--mono); color: var(--good); font-variant-numeric: tabular-nums; }
	.calc { margin: 0; display: grid; }
	.calc div { display: flex; justify-content: space-between; padding-block: var(--sp-1); font-size: var(--text-sm); }
	dt { color: var(--muted); }
	dd { margin: 0; font-family: var(--mono); }
	.pos { color: var(--good); }
	.neg { color: var(--bad); }
	.err { color: var(--bad); font-size: var(--text-sm); margin: 0; }
	.buy { min-height: 52px; border: 0; border-radius: var(--radius-md); background: var(--good); color: var(--accent-ink); font-size: var(--text-md); font-weight: 600; transition: transform var(--dur) var(--ease), filter var(--dur) var(--ease); }
	.buy.no { background: var(--bad); }
	.buy:hover:not(:disabled) { filter: brightness(1.08); }
	.buy:active:not(:disabled) { transform: scale(0.98); }
	.buy:disabled { background: var(--line-strong); color: var(--muted); }
	.fine { font-size: var(--text-xs); color: var(--muted); margin: 0; }
	.mine { border-top: 1px solid var(--line); padding-top: var(--sp-3); display: grid; gap: var(--sp-2); }
	.pos-row { display: flex; flex-wrap: wrap; justify-content: space-between; align-items: center; gap: var(--sp-2); font-size: var(--text-sm); }
	.pos-row span:nth-child(2) { font-family: var(--mono); }
	@media (max-width: 900px) { .grid { grid-template-columns: 1fr; } .ticket { position: static; } }
	@media (max-width: 560px) { .head { flex-direction: column; } .big { justify-items: start; } .steps li { flex-direction: column; align-items: start; } .mono { text-align: left; } }
	/* After the `font` shorthands above, which reset it. */
	.out, .chips button { font-variant-numeric: tabular-nums; }
</style>
