<script lang="ts">
	// Newcomer test 3 (2026-11-18): four first-time readers could not find what Onyx is, whether
	// money is real, who runs it, under which licence, or how to reach the project. One short page,
	// linked in the footer of every page. Every statement here is one the repository's public files
	// make (README.md, SECURITY.md, LICENSE, DATA-LICENSE.md): nothing names who runs the node or
	// a publisher, and the security route is quoted from SECURITY.md, which gives no e-mail address.
	import { resolve } from '$app/paths';
	import OnyxSlab from '#lib/components/OnyxSlab.svelte';
	import { HOSTED_NODE } from '#lib/quickstart.ts';
	import { REPO } from '#lib/release.ts';

	const FILE = `${REPO}/blob/main`;
	const node = new URL(HOSTED_NODE).host;
</script>

<svelte:head><title>About · Onyx</title></svelte:head>

<OnyxSlab>
	<h1>About Onyx</h1>
	<div class="slabside" data-testid="slabside">
		<p class="slablede">What Onyx is, what it is not yet, who runs it, and how to reach the project.</p>
		<p class="meta slabmeta">Test network only · <span class="nw">no real money</span></p>
	</div>
</OnyxSlab>

<div class="wrap">
	<section class="section" data-testid="about-what">
		<h2>What Onyx is</h2>
		<ol class="lines">
			<li>Onyx is an open network for data: publishers add datasets, and anyone can read them.</li>
			<li>Every row comes with a proof that it is exactly what its publisher signed and anchored on a public test chain.</li>
			<li>The proof does not show that a number is right. Onyx says how each dataset can be checked, in five <a href={resolve('/how-it-works')}>proof levels</a>.</li>
			<li>You can <a href={resolve('/datasets')}>browse the datasets</a>, read them <a href={resolve('/api')}>by API</a>, or connect an AI assistant. No key, no account.</li>
		</ol>
	</section>

	<section class="section" data-testid="about-testnet">
		<h2>A test network, with no real money</h2>
		<p class="lede">Onyx runs on Base Sepolia, a public test chain. Its tokens (tONYX, test USDC, test ETH) have no value, and the test markets and insurance on this site hold none. Payments are switched off on the hosted test node: everything is free to read today. The site and the node can change or stop at any time.</p>
	</section>

	<section class="section" data-testid="about-who">
		<h2>Who runs it</h2>
		<p class="lede">The hosted node at <span class="mono">{node}</span> is run by one operator. This site does not say who that is, and it does not say who any publisher is: a publisher is known here only by its id, its public key and what it states about its data. Today almost all datasets come from one publisher, and nobody else checks its numbers. <a href={resolve('/datasets')}>Each dataset says where its numbers come from.</a></p>
	</section>

	<section class="section" data-testid="about-licences">
		<h2>Licences</h2>
		<ul class="links">
			<li><a href={`${FILE}/LICENSE`} target="_blank" rel="noopener" data-testid="about-license">LICENSE</a><span>The code is under Apache-2.0.</span></li>
			<li><a href={`${FILE}/DATA-LICENSE.md`} target="_blank" rel="noopener" data-testid="about-data-license">DATA-LICENSE.md</a><span>Data is not covered by the code's licence: each data set keeps the terms of its source.</span></li>
		</ul>
		<p class="fine">A dataset a publisher adds names its own licence on its card in <a href={resolve('/datasets')}>All datasets</a>.</p>
	</section>

	<section class="section" data-testid="about-contact">
		<h2>How to reach the project</h2>
		<ul class="links">
			<li><a href={`${REPO}/issues`} target="_blank" rel="noopener" data-testid="about-issues">A public issue on the repository</a><span>For questions, mistakes on this site and requests. An issue is public and shows the account that opens it.</span></li>
			<li><a href={`${FILE}/SECURITY.md`} target="_blank" rel="noopener" data-testid="about-security">SECURITY.md</a><span>For a vulnerability, the file says: “Use GitHub private vulnerability reporting: open the repository's Security tab and choose Report a vulnerability. Only the maintainers see the report. Onyx has no e-mail contact. Do not open a public issue for a vulnerability.”</span></li>
		</ul>
	</section>

	<section class="section" data-testid="about-terms">
		<h2>Terms of service</h2>
		<p class="lede">There are none. No terms of service exist for this test network: nothing here is an offer or a promise, and nobody is asked to agree to anything to read the data.</p>
	</section>
</div>

<style>
	.lines {
		display: grid;
		gap: var(--sp-3);
		max-width: 72ch;
		margin: 0;
		padding-left: var(--sp-6);
		line-height: var(--lh-body);
	}
	.lede {
		max-width: 72ch;
		line-height: var(--lh-body);
	}
	.links {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		gap: var(--sp-3);
		max-width: 72ch;
	}
	.links li {
		display: grid;
		gap: var(--sp-1);
	}
	/* Standalone links: 44 px tap targets on phones (W2). */
	.links a {
		display: inline-flex;
		align-items: center;
		justify-self: start;
		min-height: 44px;
		font-weight: 500;
	}
	.links span {
		color: var(--muted);
		line-height: var(--lh-body);
	}
	.fine {
		margin-top: var(--sp-3);
	}
</style>
