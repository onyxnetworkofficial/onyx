<script lang="ts">
	// Parlays in the Markets arena: pick 2 to 4 Yes/No legs; the payout multiplies, and every leg
	// must hit. Legs reveal one at a time, each drawn with its price as its probability.
	import { arena, type Leg } from '#lib/arena-state.svelte.ts';
	import Check from '@lucide/svelte/icons/check';
	import X from '@lucide/svelte/icons/x';
	import { parlayPayout } from '#lib/arena.ts';
	let legs = $state<Leg[]>([]);
	let stake = $state('10');
	let error = $state('');
	let revealing = $state(false);
	const binary = $derived(arena.markets.filter((m) => m.kind === 'binary' && m.status === 'open'));
	const legPrices = $derived(legs.map((l) => arena.price(arena.market(l.mid), l.i)));
	const payout = $derived(parlayPayout(Number(stake) || 0, legPrices));
	const fmt = (v: number) => v.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 });
	function toggle(mid: string, i: number) {
		error = '';
		const k = legs.findIndex((l) => l.mid === mid);
		if (k >= 0 && legs[k].i === i) legs = legs.filter((_, j) => j !== k);
		else if (k >= 0) legs[k] = { mid, i };
		else if (legs.length < 4) legs = [...legs, { mid, i }];
		else error = 'A parlay has at most 4 legs.';
	}
	function place() {
		error = arena.placeParlay(legs, Number(stake)) ?? '';
		if (!error) {
			legs = [];
			reveal();
		}
	}
	function reveal() {
		revealing = true;
		const go = () => {
			if (arena.revealLeg(0)) setTimeout(go, 1200);
			else revealing = false;
		};
		setTimeout(go, 1200);
	}
</script>

<section class="parlay" aria-labelledby="parlay-title" data-testid="arena-parlay">
	<p class="eyebrow">Parlay · combine 2 to 4 picks</p>
	<h2 id="parlay-title">Stack your calls. The payout multiplies.</h2>
	<div class="grid">
		<ul class="list">
			{#each binary as m (m.id)}
				{@const sel = legs.find((l) => l.mid === m.id)}
				<li>
					<span class="q">{m.title}</span>
					<span class="btns">
						{#each [0, 1] as i}
							<button type="button" class:yes={i === 0} class:no={i === 1} class:sel={sel?.i === i} aria-pressed={sel?.i === i} onclick={() => toggle(m.id, i)} data-testid={`parlay-${m.id}-${i}`}>
								{m.outcomes[i]} {Math.round(arena.price(m, i) * 100)}¢
							</button>
						{/each}
					</span>
				</li>
			{/each}
		</ul>
		<aside class="slip panel" aria-label="Parlay slip">
			<p class="eyebrow">Your slip · {legs.length} of 4 legs</p>
			{#if legs.length === 0}
				<p class="fine">Pick Yes or No on two or more markets.</p>
			{/if}
			{#each legs as l}
				{@const m = arena.market(l.mid)}
				<div class="leg"><span>{m.outcomes[l.i]} · {m.title}</span><b class="mono">{Math.round(arena.price(m, l.i) * 100)}¢</b></div>
			{/each}
			<label class="field"><span>Stake (practice credits)</span><input id="parlay-stake" bind:value={stake} inputmode="decimal" data-testid="parlay-stake" /></label>
			<div class="win"><span>Pays if all hit</span><b data-testid="parlay-payout">{fmt(legs.length >= 2 ? payout : 0)}</b></div>
			<p class="fine">{legs.length >= 2 ? `${(payout / (Number(stake) || 1)).toFixed(2)}× your stake` : 'Add at least 2 legs'}</p>
			{#if error}<p role="alert" class="err">{error}</p>{/if}
			<button type="button" class="buy" disabled={legs.length < 2 || revealing} onclick={place} data-testid="parlay-place">Place parlay</button>
		</aside>
	</div>
	{#if arena.parlays.length}
		<div class="tickets">
			<p class="eyebrow">Your parlays</p>
			{#each arena.parlays as t, k}
				<div class="ticket panel" class:won={t.state === 'won'} class:lost={t.state === 'lost'} data-testid={k === 0 ? 'parlay-latest' : undefined}>
					<div class="thead"><b>{t.state === 'won' ? `Hit · +${fmt(t.payout)}` : t.state === 'lost' ? 'Missed' : 'Revealing…'}</b><span class="mono">{fmt(t.stake)} → {fmt(t.payout)}</span></div>
					{#each t.legs as l}
						<div class="leg" class:hit={l.hit === true} class:miss={l.hit === false}>
							<span class="mark" aria-hidden="true">{#if l.hit === true}<Check size={14} strokeWidth={2} />{:else if l.hit === false}<X size={14} strokeWidth={2} />{:else}·{/if}</span>
							<span>{l.outcome} · {l.title}</span><b class="mono">{Math.round(l.price * 100)}¢</b>
						</div>
					{/each}
				</div>
			{/each}
		</div>
	{/if}
</section>

<style>
	h2 { font-size: var(--text-xl); line-height: var(--lh-tight); margin: var(--sp-2) 0 var(--sp-5); }
	.grid { display: grid; grid-template-columns: minmax(0, 1.6fr) minmax(16rem, 1fr); gap: var(--sp-5); align-items: start; }
	.list { list-style: none; margin: 0; padding: 0; display: grid; gap: var(--sp-2); }
	.list li { display: flex; justify-content: space-between; align-items: center; gap: var(--sp-3); padding: var(--sp-3) var(--sp-4); border: 1px solid var(--line); border-radius: var(--radius-md); background: var(--panel); }
	.q { font-size: var(--text-sm); line-height: var(--lh-snug); min-width: 0; }
	.btns { display: flex; gap: var(--sp-2); flex-shrink: 0; }
	.btns button { min-height: 44px; min-width: 5.5rem; padding-inline: var(--sp-3); border-radius: var(--radius-sm); border: 1px solid var(--line-strong); background: transparent; color: var(--ink); font: var(--text-sm) var(--mono); }
	.btns button:hover { background: var(--hover); }
	.btns .yes.sel { background: var(--good-soft); border-color: var(--good); color: var(--good); }
	.btns .no.sel { background: color-mix(in srgb, var(--bad) 12%, transparent); border-color: var(--bad); color: var(--bad); }
	.panel { padding: var(--sp-5); border: 1px solid var(--line); border-radius: var(--radius-lg); background: var(--panel); }
	.slip { position: sticky; top: calc(env(safe-area-inset-top, 0px) + 88px); display: grid; gap: var(--sp-3); }
	.leg { display: flex; justify-content: space-between; gap: var(--sp-3); font-size: var(--text-sm); line-height: var(--lh-snug); }
	.mono { font-family: var(--mono); font-weight: 500; white-space: nowrap; }
	.field { display: grid; gap: var(--sp-1); }
	.field > span { font-size: var(--text-sm); color: var(--muted); }
	input { width: 100%; min-height: 48px; font: var(--text-field) var(--mono); }
	.win { display: flex; justify-content: space-between; align-items: baseline; padding: var(--sp-4); border-radius: var(--radius-md); background: var(--good-soft); border: 1px solid var(--good-line); }
	.win span { color: var(--muted); font-size: var(--text-sm); }
	.win b { font: 600 var(--text-2xl)/var(--lh-none) var(--mono); color: var(--good); }
	.buy { min-height: 52px; border: 0; border-radius: var(--radius-md); background: var(--good); color: var(--accent-ink); font-size: var(--text-md); font-weight: 600; }
	.buy:disabled { background: var(--line-strong); color: var(--muted); }
	.buy:hover:not(:disabled) { filter: brightness(1.08); }
	.fine { font-size: var(--text-xs); color: var(--muted); margin: 0; }
	.err { color: var(--bad); font-size: var(--text-sm); margin: 0; }
	.tickets { display: grid; gap: var(--sp-3); margin-top: var(--sp-7); }
	.ticket { display: grid; gap: var(--sp-2); }
	.ticket.won { border-color: var(--good-line); background: var(--good-soft); }
	.ticket.lost { opacity: 0.7; }
	.thead { display: flex; justify-content: space-between; gap: var(--sp-3); }
	.leg.hit { color: var(--good); }
	.leg.miss { color: var(--bad); }
	.mark { display: inline-grid; place-items: center; width: 18px; flex-shrink: 0; }
	.leg > span:nth-child(2) { flex: 1; min-width: 0; }
	@media (max-width: 900px) { .grid { grid-template-columns: 1fr; } .slip { position: static; } }
	@media (max-width: 560px) { .list li { flex-direction: column; align-items: stretch; } .btns button { flex: 1; } }
	/* After the `font` shorthands above, which reset it. */
	.btns button { font-variant-numeric: tabular-nums; }
</style>
