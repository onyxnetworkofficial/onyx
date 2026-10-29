<script lang="ts">
	// Create a market (Markets dashboard): pick an Onyx dataset, a direction, a threshold and a
	// date; the question is written for you and the market opens for trading at once. Only data a
	// source publishes can be listed, so every market can settle by the observer check.
	import { arena, DATASETS, dayLabel } from '#lib/arena-state.svelte.ts';
	let { opened }: { opened: (id: string) => void } = $props();
	let dataset = $state(DATASETS[0].id);
	let above = $state(true);
	const ds = $derived(DATASETS.find((d) => d.id === dataset)!);
	// A starting threshold 1% past the latest value, at the dataset's own precision.
	const suggest = (d: (typeof DATASETS)[number]) => (d.latest * 1.01).toFixed((String(d.step).split('.')[1] ?? '').length);
	let threshold = $state(suggest(DATASETS[0]));
	const inMonth = new Date(Date.now() + 30 * 86_400_000).toISOString().slice(0, 10);
	let date = $state(inMonth);
	let chance = $state(50);
	let error = $state('');
	const when = $derived(Number.isFinite(Date.parse(`${date}T00:00:00Z`)) ? dayLabel(Date.parse(`${date}T00:00:00Z`)) : '…');
	const question = $derived(`${ds.name} ${above ? 'above' : 'below'} ${threshold || '…'} ${ds.unit} on ${when}?`);
	function pickDataset() {
		threshold = suggest(ds);
		error = '';
	}
	function launch(e: SubmitEvent) {
		e.preventDefault();
		const r = arena.addMarket({ dataset, above, threshold: Number(threshold), date, chance });
		if ('error' in r) error = r.error;
		else opened(r.id);
	}
</script>

<section class="create" aria-labelledby="create-title" data-testid="arena-create">
	<p class="eyebrow">Create a market</p>
	<h2 id="create-title">List a question that data can answer.</h2>
	<div class="grid">
		<form onsubmit={launch} class="panel">
			<label class="field"><span>Dataset</span>
				<select id="create-dataset" bind:value={dataset} onchange={pickDataset} data-testid="create-dataset">
					{#each DATASETS as d}<option value={d.id}>{d.name}</option>{/each}
				</select>
			</label>
			<p class="fine">Latest: <b class="mono">{ds.latest.toLocaleString('en-US')} {ds.unit}</b>{ds.sample ? ' (sample value)' : ' (agreed by Onyx observers)'} · source: {ds.source}</p>
			<div class="dir" role="group" aria-label="Direction">
				<button type="button" class:on={above} aria-pressed={above} onclick={() => (above = true)}>Above</button>
				<button type="button" class:on={!above} aria-pressed={!above} onclick={() => (above = false)}>Below</button>
			</div>
			<div class="pair">
				<label class="field"><span>Threshold ({ds.unit})</span><input id="create-threshold" bind:value={threshold} inputmode="decimal" data-testid="create-threshold" /></label>
				<label class="field"><span>Date (UTC)</span><input id="create-date" type="date" bind:value={date} data-testid="create-date" /></label>
			</div>
			<label class="field"><span>Opening chance of Yes: <b class="mono">{chance}%</b></span>
				<input id="create-chance" type="range" min="5" max="95" step="1" bind:value={chance} />
			</label>
			{#if error}<p role="alert" class="err" data-testid="create-error">{error}</p>{/if}
			<button type="submit" class="buy" data-testid="create-launch">List this market</button>
		</form>
		<aside class="panel preview" aria-label="Preview">
			<p class="eyebrow">Preview</p>
			<h3 data-testid="create-question">{question}</h3>
			<div class="yn"><span class="y">Yes {chance}¢</span><span class="n">No {100 - chance}¢</span></div>
			<dl>
				<div><dt>Settles on</dt><dd>{ds.source}</dd></div>
				<div><dt>Onyx dataset</dt><dd class="mono">{ds.id}</dd></div>
				<div><dt>Trading closes</dt><dd>The day before</dd></div>
				<div><dt>Who decides</dt><dd>5 of 7 observers must agree on the value</dd></div>
				<div><dt>No value by the deadline</dt><dd>The market is void and every trade is refunded</dd></div>
			</dl>
		</aside>
	</div>
</section>

<style>
	h2 { font-size: var(--text-xl); line-height: var(--lh-tight); margin: var(--sp-2) 0 var(--sp-5); }
	.grid { display: grid; grid-template-columns: minmax(0, 1.2fr) minmax(16rem, 1fr); gap: var(--sp-5); align-items: start; }
	.panel { padding: var(--sp-5); border: 1px solid var(--line); border-radius: var(--radius-lg); background: var(--panel); }
	form { display: grid; gap: var(--sp-4); }
	.field { display: grid; gap: var(--sp-1); }
	.field > span { font-size: var(--text-sm); color: var(--muted); }
	input:not([type='range']), select { width: 100%; min-height: 48px; font-size: var(--text-field); }
	input[type='range'] { width: 100%; accent-color: var(--good); min-height: 32px; }
	.pair { display: grid; grid-template-columns: 1fr 1fr; gap: var(--sp-3); }
	.dir { display: grid; grid-template-columns: 1fr 1fr; gap: var(--sp-2); }
	.dir button { min-height: 44px; border-radius: var(--radius-md); border: 1px solid var(--line-strong); background: transparent; color: var(--muted); }
	.dir button:hover { background: var(--hover); }
	.dir button.on { color: var(--ink); border-color: var(--accent); background: var(--accent-soft); }
	.mono { font-family: var(--mono); }
	.fine { font-size: var(--text-xs); color: var(--muted); margin: 0; line-height: var(--lh-body); }
	.fine b { color: var(--ink); font-weight: 500; }
	.err { color: var(--bad); font-size: var(--text-sm); margin: 0; }
	.buy { min-height: 52px; border: 0; border-radius: var(--radius-md); background: var(--good); color: var(--accent-ink); font-size: var(--text-md); font-weight: 600; }
	.buy:hover { filter: brightness(1.08); }
	.preview { display: grid; gap: var(--sp-4); position: sticky; top: calc(env(safe-area-inset-top, 0px) + 88px); }
	.preview h3 { font-size: var(--text-md); line-height: var(--lh-snug); margin: 0; text-wrap: balance; }
	.yn { display: grid; grid-template-columns: 1fr 1fr; gap: var(--sp-2); }
	.yn span { display: grid; place-items: center; min-height: 40px; border-radius: var(--radius-sm); font: 500 var(--text-sm) var(--mono); }
	.yn .y { background: var(--good-soft); color: var(--good); border: 1px solid var(--good-line); }
	.yn .n { background: color-mix(in srgb, var(--bad) 10%, transparent); color: var(--bad); border: 1px solid color-mix(in srgb, var(--bad) 35%, transparent); }
	dl { margin: 0; display: grid; }
	dl div { display: grid; gap: var(--sp-0); padding-block: var(--sp-2); border-top: 1px solid var(--line); }
	dt { font-size: var(--text-xs); color: var(--muted); }
	dd { margin: 0; font-size: var(--text-sm); overflow-wrap: anywhere; }
	@media (max-width: 900px) { .grid { grid-template-columns: 1fr; } .preview { position: static; } }
	@media (max-width: 480px) { .pair { grid-template-columns: 1fr; } }
</style>
