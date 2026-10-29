<script lang="ts">
	import { resolve } from '$app/paths';
	import ArrowRight from '@lucide/svelte/icons/arrow-right';
	import Database from '@lucide/svelte/icons/database';
	import Link from '@lucide/svelte/icons/link';
	import ChartNoAxesCombined from '@lucide/svelte/icons/chart-no-axes-combined';
	import OnyxSlab from '#lib/components/OnyxSlab.svelte';
</script>

<svelte:head><title>Onyx · Data anyone can check</title></svelte:head>

<OnyxSlab>
	<div class="wrap welcome">
		<div class="intro">
			<p class="eyebrow">A network built from what you know</p>
			<h1>Your data could be<br />someone’s next discovery.</h1>
			<p class="lede" data-testid="home-what">Have a useful dataset, a source worth following, or a forecast to share? Add it to Onyx. Start with a description. We’ll help work out how it can be checked.</p>
			<div class="actions">
				<a class="btn" href={resolve('/submit')} data-testid="home-primary">Add your data <ArrowRight size={18} /></a>
				<a class="btn ghost" href={resolve('/data')} data-testid="home-secondary">Browse data <ArrowRight size={18} /></a>
			</div>
			<p class="preview-note">Early preview. Submissions need review before they can be published. Data is paid in test USDC on Base Sepolia, not switched on yet; markets use tONYX test tokens. No real money.</p>
		</div>
		<aside class="starting-point" aria-labelledby="starting-title">
			<p class="eyebrow">Start with what you have</p>
			<h2 id="starting-title">Useful data comes<br />from everywhere.</h2>
			<ul>
				<li><Database size={22} /><div><h3>Your own data</h3><p>Research, measurements, or a dataset you maintain.</p></div></li>
				<li><Link size={22} /><div><h3>A source you follow</h3><p>A public API, website, or onchain source.</p></div></li>
				<li><ChartNoAxesCombined size={22} /><div><h3>A forecast</h3><p>A prediction that can be compared with what happens.</p></div></li>
			</ul>
			<a href={resolve('/submit')}>Describe your data <ArrowRight size={16} /></a>
		</aside>
	</div>
</OnyxSlab>

<div class="wrap">
	<section class="section next" aria-labelledby="next-title" data-testid="contribution-steps">
		<div class="section-heading"><p class="eyebrow">What happens next</p><h2 id="next-title">From your idea to useful data.</h2><p>You don’t need to choose a verification method to get started.</p></div>
		<ol>
			<li><span class="step-number" aria-hidden="true">01</span><h3>Tell us what you have</h3><p>Describe the data, where it comes from, and whether you have permission to share it.</p></li>
			<li><span class="step-number" aria-hidden="true">02</span><h3>Find the right checks</h3><p>Onyx suggests how the data could be checked. You’ll see what is supported and what still needs review.</p></li>
			<li><span class="step-number" aria-hidden="true">03</span><h3>Keep track of the next step</h3><p>Send it from this site, an AI assistant or the API, and you get a receipt to follow its review.</p></li>
		</ol>
	</section>
	<section class="learn" aria-labelledby="learn-title">
		<div><p class="eyebrow">Curious about the checks?</p><h2 id="learn-title">See how trust is earned.</h2><p>Explore the verification methods, run the browser checks, and inspect the testnet demo.</p></div>
		<a class="btn ghost" href={resolve('/how-it-works')} data-testid="home-technical">How it works <ArrowRight size={18} /></a>
	</section>
	<section class="market-preview" aria-labelledby="market-preview-title">
		<div><p class="eyebrow">Live on the test network</p><h2 id="market-preview-title">Markets that settle on observed data.</h2><p>Two markets on the U.S. Treasury’s debt: one settled from the value Onyx observers recorded, one open for test bets. Plus test insurance that pays on the weather and earthquakes. Markets use tONYX test tokens. No real money.</p></div>
		<a class="btn ghost" href={resolve('/markets')} data-testid="home-markets">Open markets <ArrowRight size={18} /></a>
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
	.starting-point { background: var(--panel); border: 1px solid var(--line); border-radius: var(--radius-lg); padding: var(--sp-7); }
	.starting-point h2 { font-size: var(--text-2xl); line-height: var(--lh-tight); }
	.starting-point ul { list-style: none; padding: 0; margin: var(--sp-6) 0; }
	.starting-point li { display: flex; align-items: flex-start; gap: var(--sp-4); padding-block: var(--sp-4); border-top: 1px solid var(--line); }
	.starting-point :global(li > svg) { flex-shrink: 0; margin-top: var(--sp-1); color: var(--muted); }
	h3 { font-size: var(--text-lg); font-weight: 400; margin: 0 0 var(--sp-2); }
	.starting-point p:not(.eyebrow), .next li p { font-size: var(--text-base); color: var(--muted); line-height: var(--lh-body); margin: 0; }
	.starting-point > a { min-height: 44px; display: flex; align-items: center; justify-content: space-between; gap: var(--sp-4); border-top: 1px solid var(--line); padding-top: var(--sp-4); }
	.section-heading { max-width: 64ch; margin-bottom: var(--sp-7); }
	.section-heading h2, .learn h2 { font-size: var(--text-h2); line-height: var(--lh-tight); margin-bottom: var(--sp-4); }
	.section-heading > p:last-child, .learn p:last-child { color: var(--muted); line-height: var(--lh-body); }
	p { text-wrap: pretty; }
	.next ol { display: grid; grid-template-columns: repeat(3, 1fr); gap: var(--sp-7); padding: 0; margin: 0; list-style: none; }
	.next li { border-top: 1px solid var(--line); padding-top: var(--sp-6); }
	.step-number { display: block; font: var(--text-sm)/var(--lh-body) var(--mono); color: var(--muted); margin-bottom: var(--sp-4); }
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
