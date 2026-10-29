<script lang="ts">
	// Flash rounds in the Markets arena: will a simulated Ethereum gas index be higher or lower in
	// 30 seconds? Picks are open for 20 seconds, then locked for 10; winners split the pool (2% fee).
	import { arena } from '#lib/arena-state.svelte.ts';
	import ArenaChart from '#lib/components/ArenaChart.svelte';
	import ArrowUp from '@lucide/svelte/icons/arrow-up';
	import ArrowDown from '@lucide/svelte/icons/arrow-down';
	import Timer from '@lucide/svelte/icons/timer';
	let stake = $state('20');
	let error = $state('');
	const now = $derived(arena.flashSeries[arena.flashSeries.length - 1] ?? 0);
	const open = $derived(arena.flashT < 20);
	const left = $derived(Math.ceil((open ? 20 : 30) - arena.flashT));
	const frac = $derived(open ? arena.flashT / 20 : (arena.flashT - 20) / 10);
	const delta = $derived(now - arena.flashStart);
	const lineColor = $derived(delta >= 0 ? 'var(--good)' : 'var(--bad)');
	// The chart scales a 0..1 band: map the index around the round's start.
	const scaled = $derived([arena.flashSeries.map((v) => Math.min(0.98, Math.max(0.02, 0.5 + (v - arena.flashStart) / 4)))]);
	function enter(dir: 'up' | 'down') {
		error = arena.flashEnter(dir, Number(stake)) ?? '';
	}
	const fmt = (v: number) => v.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 });
</script>

<section class="flash" aria-labelledby="flash-title" data-testid="arena-flash">
	<div class="top">
		<div>
			<p class="eyebrow">Flash round #{arena.flashRound} · simulated index</p>
			<h2 id="flash-title">Ethereum gas: higher or lower in 30 seconds?</h2>
		</div>
		<div class="clock" class:locked={!open} style:--f={frac} role="timer" aria-label={open ? `${left} seconds to pick` : `Locked, ${left} seconds to result`}>
			<Timer size={16} strokeWidth={1.75} aria-hidden="true" />
			<b>{left}s</b>
			<span>{open ? 'to pick' : 'locked'}</span>
		</div>
	</div>
	<div class="board">
		<div class="panel chartbox">
			<div class="now">
				<span class="mono big">{now.toFixed(2)} <small>gwei</small></span>
				<span class="mono" class:pos={delta >= 0} class:neg={delta < 0}>{delta >= 0 ? '+' : ''}{delta.toFixed(2)} since the round opened at {arena.flashStart.toFixed(2)}</span>
			</div>
			<ArenaChart series={scaled} colors={[lineColor]} label="Simulated gas index over the last 30 seconds" />
			<div class="streakrow" aria-label="Last rounds">
				<span class="fine">Last rounds</span>
				{#each arena.flashHistory as h}<span class="pill" class:up={h === 'up'}>{h === 'up' ? '▲' : '▼'}</span>{/each}
			</div>
		</div>
		<div class="panel pick">
			<label class="field"><span>Stake (practice credits)</span><input id="flash-stake" bind:value={stake} inputmode="decimal" data-testid="flash-stake" /></label>
			<div class="two">
				<button type="button" class="up" disabled={!open || !!arena.flashPick} onclick={() => enter('up')} data-testid="flash-up">
					<ArrowUp size={20} strokeWidth={2} aria-hidden="true" /> Higher <small>pays {arena.flashMultiplier('up', Number(stake) || 0).toFixed(2)}×</small>
				</button>
				<button type="button" class="down" disabled={!open || !!arena.flashPick} onclick={() => enter('down')} data-testid="flash-down">
					<ArrowDown size={20} strokeWidth={2} aria-hidden="true" /> Lower <small>pays {arena.flashMultiplier('down', Number(stake) || 0).toFixed(2)}×</small>
				</button>
			</div>
			{#if error}<p role="alert" class="err">{error}</p>{/if}
			<div class="pools">
				<span>Higher pool <b class="mono">{fmt(arena.flashPools.up)}</b></span>
				<span>Lower pool <b class="mono">{fmt(arena.flashPools.down)}</b></span>
			</div>
			{#if arena.flashPick}
				<p class="you" data-testid="flash-pick">You picked <b>{arena.flashPick.dir === 'up' ? 'Higher' : 'Lower'}</b> with {fmt(arena.flashPick.stake)} credits. Now {delta >= 0 === (arena.flashPick.dir === 'up') ? 'winning' : 'losing'}.</p>
			{/if}
			{#if arena.flashLast}
				<p class="last" class:pos={arena.flashLast.won === true} class:neg={arena.flashLast.won === false} data-testid="flash-last">
					Last round: {arena.flashLast.from.toFixed(2)} → {arena.flashLast.to.toFixed(2)}.
					{#if arena.flashLast.won === true}You won {fmt(arena.flashLast.paid)} credits.{:else if arena.flashLast.won === false}Your pick missed.{:else}You sat that one out.{/if}
				</p>
			{/if}
			<p class="fine">A new round starts every 30 seconds. Streak: {arena.streak} · best {arena.best}.</p>
		</div>
	</div>
</section>

<style>
	.top { display: flex; justify-content: space-between; align-items: end; gap: var(--sp-5); margin-bottom: var(--sp-5); }
	h2 { font-size: var(--text-xl); line-height: var(--lh-tight); margin: var(--sp-2) 0 0; }
	.clock { --f: 0; display: grid; grid-template-columns: auto auto; align-items: center; gap: 0 var(--sp-2); padding: var(--sp-3) var(--sp-4); border-radius: var(--radius-md); border: 1px solid var(--good-line); background: linear-gradient(90deg, var(--good-soft) calc(var(--f) * 100%), transparent 0); color: var(--good); min-width: 9rem; }
	.clock.locked { border-color: var(--btc); color: var(--btc); background: linear-gradient(90deg, var(--btc-soft) calc(var(--f) * 100%), transparent 0); }
	.clock b { font: 600 var(--text-xl)/var(--lh-none) var(--mono); }
	.clock span { grid-column: 1 / -1; font-size: var(--text-xs); color: var(--muted); }
	.board { display: grid; grid-template-columns: minmax(0, 1.5fr) minmax(16rem, 1fr); gap: var(--sp-5); align-items: start; }
	.panel { padding: var(--sp-5); border: 1px solid var(--line); border-radius: var(--radius-lg); background: var(--panel); min-width: 0; }
	.now { display: flex; flex-wrap: wrap; justify-content: space-between; align-items: baseline; gap: var(--sp-2); margin-bottom: var(--sp-3); }
	.mono { font-family: var(--mono); font-variant-numeric: tabular-nums; }
	.big { font-size: var(--text-2xl); }
	.big small { font-size: var(--text-sm); color: var(--muted); }
	.pos { color: var(--good); }
	.neg { color: var(--bad); }
	.streakrow { display: flex; flex-wrap: wrap; gap: var(--sp-1); align-items: center; margin-top: var(--sp-3); }
	.pill { display: inline-grid; place-items: center; width: 24px; height: 24px; border-radius: var(--radius-xs); background: color-mix(in srgb, var(--bad) 14%, transparent); color: var(--bad); font-size: var(--text-xs); }
	.pill.up { background: var(--good-soft); color: var(--good); }
	.pick { display: grid; gap: var(--sp-4); }
	.field { display: grid; gap: var(--sp-1); }
	.field > span { font-size: var(--text-sm); color: var(--muted); }
	input { width: 100%; min-height: 48px; font: var(--text-field) var(--mono); }
	.two { display: grid; grid-template-columns: 1fr 1fr; gap: var(--sp-2); }
	.two button { display: grid; justify-items: center; gap: var(--sp-1); min-height: 88px; border-radius: var(--radius-md); border: 0; font-size: var(--text-md); font-weight: 600; color: var(--accent-ink); transition: transform var(--dur) var(--ease), filter var(--dur) var(--ease); }
	.two button small { font: var(--text-xs) var(--mono); }
	.up { background: var(--good); }
	.down { background: var(--bad); }
	.two button:hover:not(:disabled) { filter: brightness(1.08); }
	.two button:active:not(:disabled) { transform: scale(0.97); }
	.two button:disabled { background: var(--line-strong); color: var(--muted); }
	.pools { display: flex; justify-content: space-between; gap: var(--sp-2); font-size: var(--text-sm); color: var(--muted); }
	.pools b { color: var(--ink); font-weight: 500; }
	.you, .last { margin: 0; font-size: var(--text-sm); line-height: var(--lh-body); }
	.err { color: var(--bad); font-size: var(--text-sm); margin: 0; }
	.fine { font-size: var(--text-xs); color: var(--muted); margin: 0; }
	@media (max-width: 900px) { .board { grid-template-columns: 1fr; } }
	@media (max-width: 560px) { .top { flex-direction: column; align-items: start; } }
	/* After the `font` shorthands above, which reset it. */
	.two button { font-variant-numeric: tabular-nums; }
</style>
