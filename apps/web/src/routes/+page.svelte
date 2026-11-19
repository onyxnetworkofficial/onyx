<script lang="ts">
	import { resolve } from '$app/paths';
	import ArrowRight from '@lucide/svelte/icons/arrow-right';
	import Table from '@lucide/svelte/icons/table';
	import Code from '@lucide/svelte/icons/code';
	import Bot from '@lucide/svelte/icons/bot';
	import OnyxSlab from '#lib/components/OnyxSlab.svelte';
	import { onMount } from 'svelte';
	import { STALE_DAYS, countUrl, freshCountUrl, freshCounts, mainPublisher, page as readPage, publisherCounts } from '#lib/datasets.ts';
	import { group } from '#lib/format.ts';

	// How many datasets the node holds and how many have data from the last STALE_DAYS days: the
	// node's own two counts (two lists of one). Until both answer, the text names no number.
	let all = $state<number | null>(null);
	let fresh = $state<number | null>(null);
	const counts = $derived(freshCounts(all, fresh));
	// Newcomer test 3 (2026-11-18): the page said "many sources" while one publisher held nearly
	// every dataset. The same count answer lists each publisher's catalog, so the share is the
	// node's own; without it the text names no number.
	let publishers = $state.raw<{ publisher: string; datasets: number }[]>([]);
	const main = $derived(mainPublisher(publishers, all));
	onMount(() => {
		const one = (url: string) =>
			fetch(url, { headers: { accept: 'application/json' } })
				.then((r) => (r.ok && (r.headers.get('content-type') ?? '').includes('json') ? r.json() : null))
				.catch(() => null);
		void Promise.all([one(countUrl()), one(freshCountUrl())]).then(([a, f]) => {
			[all, fresh] = [readPage(a)?.total || null, readPage(f)?.total || null];
			publishers = publisherCounts(a);
		});
	});
	const many = $derived(counts ? group(counts.all) : 'about 9,000');
</script>

<svelte:head><title>Onyx · Data anyone can check</title></svelte:head>

<!-- READINESS A19: the first screen speaks to a data user (what Onyx is, what works today), then to a
     contributor. The dataset counts are the node's own (asked when the page opens); without an
     answer the text says "about 9,000" and names no count of updated datasets. -->
<OnyxSlab>
	<div class="wrap welcome">
		<div class="intro">
			<p class="eyebrow">Open data network · testnet</p>
			<h1>Data anyone<br />can check.</h1>
			<p class="lede" data-testid="home-what">Onyx is a network built to hold data from many publishers. Every answer comes with a proof you can verify yourself. Today you can browse {many} datasets, get them by API, or connect an AI assistant.</p>
			<div class="actions">
				<a class="btn" href={resolve('/datasets')} data-testid="home-primary">Browse datasets <ArrowRight size={18} /></a>
				<a class="btn ghost" href={resolve('/submit')} data-testid="home-secondary">Add your data <ArrowRight size={18} /></a>
			</div>
			<p class="preview-note" data-testid="home-testnet">
				Testnet preview. Reading is free, and test tokens have no value.
				{#if counts}<span data-testid="home-freshness">{group(counts.fresh)} of {group(counts.all)} datasets were updated in the last {STALE_DAYS} days{counts.stale ? `; the other ${group(counts.stale)} were not` : ''}.</span> Each one shows its newest day.{:else}Some datasets are not being updated right now: each one shows its newest day.{/if}
			</p>
			<p class="preview-note proof-note" data-testid="home-publishers">
				{#if main}Today {group(main.datasets)} of {group(main.total)} datasets come from one publisher, <span class="mono">{main.publisher}</span>.{:else}Today almost all datasets come from one publisher.{/if}
				Onyx does not check its numbers: <a href={resolve('/datasets')}>each dataset says where they come from</a>.
			</p>
			<p class="preview-note proof-note" data-testid="home-proof">The proof shows these are exactly the rows the publisher signed and anchored on the test chain. It does not show that the numbers are right.</p>
		</div>
		<aside class="starting-point" aria-labelledby="starting-title" data-testid="data-users">
			<p class="eyebrow">What you can do now</p>
			<h2 id="starting-title">One place, one API,<br />one format.</h2>
			<ul>
				<li><a href={resolve('/datasets')} data-testid="users-datasets"><Table size={22} /><span><h3>Browse {many} datasets</h3><p>Search by name and read the newest rows of any of them.</p></span><ArrowRight size={18} /></a></li>
				<li><a href={resolve('/api')} data-testid="users-api"><Code size={22} /><span><h3>Get them by API</h3><p>The same data over HTTP, from a script or a notebook. No key.</p></span><ArrowRight size={18} /></a></li>
				<li><a href={`${resolve('/submit')}?assistant`} data-testid="users-assistant"><Bot size={22} /><span><h3>Connect an AI assistant</h3><p>One connector for ChatGPT, Claude, Cursor and others. It finds datasets and reads rows. To check a proof you run the onyx-check program on your own machine.</p></span><ArrowRight size={18} /></a></li>
			</ul>
		</aside>
	</div>
</OnyxSlab>

<div class="wrap">
	<section class="section next" aria-labelledby="next-title" data-testid="contribution-steps">
		<div class="section-heading"><p class="eyebrow">Have data to share?</p><h2 id="next-title">Your data could be someone’s next discovery.</h2><p>A dataset you maintain, a source worth following, or a forecast: start with a description. You don’t need to choose a verification method. Nothing is published before a review.</p></div>
		<ol>
			<li><span class="step-number" aria-hidden="true">01</span><h3>Tell us what you have</h3><p>Describe the data, where it comes from, and whether you have permission to share it.</p></li>
			<li><span class="step-number" aria-hidden="true">02</span><h3>Find the right checks</h3><p>Onyx suggests how the data could be checked. You’ll see what is supported and what still needs review.</p></li>
			<li><span class="step-number" aria-hidden="true">03</span><h3>Keep track of the next step</h3><p>Send it from this site, an AI assistant or the API, and you get a receipt to follow its review.</p></li>
		</ol>
		<p class="next-action"><a class="btn ghost" href={resolve('/submit')} data-testid="home-add">Describe your data <ArrowRight size={18} /></a></p>
	</section>
	<section class="learn" aria-labelledby="learn-title">
		<div><p class="eyebrow">Curious about the checks?</p><h2 id="learn-title">See how trust is earned.</h2><p>See how a row is proven, run a check in your browser, and inspect the testnet demo.</p></div>
		<a class="btn ghost" href={resolve('/how-it-works')} data-testid="home-technical">How it works <ArrowRight size={18} /></a>
	</section>
	<section class="market-preview" aria-labelledby="market-preview-title">
		<div><p class="eyebrow">Past test markets · test network</p><h2 id="market-preview-title">Markets that settle on observed data.</h2><p>Test markets on the U.S. Treasury’s debt that settle from the value Onyx observers recorded, and test insurance on the weather and earthquakes. The bets were placed by the project’s own test accounts in tONYX, a test token. No real money. The page reads them from the chain; nothing is traded there.</p></div>
		<a class="btn ghost" href={resolve('/markets')} data-testid="home-markets">See the test markets <ArrowRight size={18} /></a>
	</section>
</div>

<style>
	.welcome { display: grid; grid-template-columns: 1.5fr 1fr; gap: var(--sp-9); align-items: center; padding-block: var(--sp-10); }
	.intro { max-width: 740px; }
	.eyebrow { font: 500 var(--text-sm)/var(--lh-body) var(--mono); color: var(--muted); margin-bottom: var(--sp-4); }
	h1 { font-size: var(--text-hero); line-height: var(--lh-display); letter-spacing: -.025em; margin: 0 0 var(--sp-6); }
	.lede { max-width: 54ch; font-size: var(--text-md); line-height: var(--lh-body); }
	.actions { display: flex; align-items: center; flex-wrap: wrap; gap: var(--sp-4); margin-top: var(--sp-7); }
	.preview-note { max-width: 55ch; margin: var(--sp-6) 0 0; color: var(--muted); font-size: var(--text-sm); line-height: var(--lh-body); }
	.proof-note { margin-top: var(--sp-3); }
	.starting-point { background: var(--panel); border: 1px solid var(--line); border-radius: var(--radius-lg); padding: var(--sp-7); }
	.starting-point h2 { font-size: var(--text-2xl); line-height: var(--lh-tight); }
	.starting-point ul { list-style: none; padding: 0; margin: var(--sp-6) 0; }
	.starting-point ul { margin-bottom: 0; }
	.starting-point li { border-top: 1px solid var(--line); }
	.starting-point li a { display: flex; align-items: flex-start; gap: var(--sp-4); min-height: 44px; padding-block: var(--sp-4); color: inherit; text-decoration-color: transparent; }
	.starting-point li a > span { flex: 1; }
	/* The row's title underlines on hover and focus; its description (inline-block) does not. */
	.starting-point li a:hover, .starting-point li a:focus-visible { text-decoration-color: currentColor; }
	.starting-point li a p { display: inline-block; }
	.starting-point :global(li a > svg) { flex-shrink: 0; margin-top: var(--sp-1); color: var(--muted); }
	h3 { font-size: var(--text-lg); font-weight: 400; margin: 0 0 var(--sp-2); }
	.starting-point p:not(.eyebrow), .next li p { font-size: var(--text-base); color: var(--muted); line-height: var(--lh-body); margin: 0; }
	.section-heading { max-width: 64ch; margin-bottom: var(--sp-7); }
	.section-heading h2, .learn h2 { font-size: var(--text-h2); line-height: var(--lh-tight); margin-bottom: var(--sp-4); }
	.section-heading > p:last-child, .learn p:last-child { color: var(--muted); line-height: var(--lh-body); }
	p { text-wrap: pretty; }
	.next ol { display: grid; grid-template-columns: repeat(3, 1fr); gap: var(--sp-7); padding: 0; margin: 0; list-style: none; }
	.next li { border-top: 1px solid var(--line); padding-top: var(--sp-6); }
	.step-number { display: block; font: var(--text-sm)/var(--lh-body) var(--mono); color: var(--muted); margin-bottom: var(--sp-4); }
	.next-action { margin: var(--sp-7) 0 var(--sp-9); }
	.learn, .market-preview { display: flex; align-items: center; justify-content: space-between; gap: var(--sp-7); padding-block: var(--sp-9); border-top: 1px solid var(--line); margin-bottom: var(--sp-7); }
	.learn > div { max-width: 62ch; }
	.learn > a { flex-shrink: 0; }
	.market-preview > div { max-width:62ch; }
	/* MP4: --text-h3 is not a token (it fell back to body size); the twin block's size. */
	.market-preview h2 { font-size: var(--text-h2); line-height: var(--lh-tight); margin-bottom: var(--sp-4); }
	.market-preview p:last-child { color:var(--muted); line-height:var(--lh-body); }
	.market-preview > a { flex-shrink:0; }
	@media (max-width: 1000px) { .welcome { gap: var(--sp-7); grid-template-columns: 1.2fr 1fr; } .starting-point { padding: var(--sp-6); } }
	@media (max-width: 760px) { .welcome { grid-template-columns: 1fr; padding-block: var(--sp-8); } .next ol { grid-template-columns: 1fr; gap: var(--sp-6); } .learn, .market-preview { align-items: flex-start; flex-direction: column; } }
</style>
