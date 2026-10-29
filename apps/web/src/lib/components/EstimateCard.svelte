<script lang="ts">
	// NS14 (ADR-0050): one estimate, shown apart from checked values. It says it is not checked,
	// shows its method and its error band, and carries no proof-level badge (an estimate is level
	// E, not a rung of the ladder). Nothing on the page adds it to a checked total.
	import { btc } from '#lib/format.ts';
	import type { SiteEstimate } from '#lib/estimates.ts';
	import { dayText } from '#lib/catalog.ts';
	let { e, testid = 'estimate' }: { e: SiteEstimate; testid?: string } = $props();
	const two = (sats: string) => btc(sats).replace(/(\.\d{2})\d+$/, '$1');
	const pct = (ppm: number) => `${Math.floor(ppm / 10_000)}%`;
</script>

<article class="est" data-testid={testid}>
	<header>
		<b>{e.title}</b>
		<span class="tag" data-testid="{testid}-tag">Estimate, not checked</span>
	</header>
	<p class="what" data-testid="{testid}-what">Average of {e.inputs.length} days, 95% range</p>
	<p class="val"><span class="num mono" data-testid="{testid}-value">{two(e.value)}</span> <span class="unit">{e.unit}</span></p>
	<p class="band" data-testid="{testid}-band">
		{pct(e.coveragePpm)} range: <span class="mono">{two(e.lo)}</span> to <span class="mono">{two(e.hi)}</span> BTC
	</p>
	<dl>
		<dt>Made from</dt>
		<dd data-testid="{testid}-inputs">{e.inputs.length} checked whole days (<span class="nw">{dayText(e.inputs[0])}</span> to <span class="nw">{dayText(e.inputs[e.inputs.length - 1])}</span>)</dd>
		<dt>Metric</dt>
		<dd class="mono">{e.metric}</dd>
	</dl>
	<details class="more" data-testid="{testid}-method">
		<summary>Method</summary>
		<p class="fine"><span class="mono">{e.method}</span>: {e.methodText}</p>
	</details>
	<p class="fine">
		The days it averages are checked (level 1). The average says what a typical day outside the window looks like, which no
		check can prove, so it is an estimate: no proof level, and never added into a checked total.
	</p>
</article>

<style>
	.est {
		border: 1px dashed var(--line);
		border-radius: var(--radius-md);
		padding: var(--sp-4);
		display: grid;
		gap: var(--sp-2);
		min-width: 0;
	}
	header {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-2);
		align-items: center;
		justify-content: space-between;
	}
	.tag {
		font-size: var(--text-sm);
		font-weight: 600;
		color: var(--warn);
		border: 1px dashed currentColor;
		border-radius: var(--radius-pill);
		padding: var(--sp-1) var(--sp-3);
		white-space: nowrap;
	}
	.val,
	.what {
		margin: 0;
	}
	.what {
		color: var(--muted);
		font-size: var(--text-sm);
	}
	.num {
		font-size: var(--text-xl);
		font-weight: 700;
		font-variant-numeric: tabular-nums;
	}
	.unit,
	.band {
		color: var(--muted);
	}
	.band {
		margin: 0;
	}
	dl {
		display: grid;
		grid-template-columns: max-content minmax(0, 1fr);
		gap: var(--sp-1) var(--sp-3);
		margin: 0;
	}
	dt {
		color: var(--muted);
	}
	dd {
		margin: 0;
		overflow-wrap: anywhere;
	}
	.fine {
		margin: 0;
	}
	.nw {
		white-space: nowrap;
	}
</style>
