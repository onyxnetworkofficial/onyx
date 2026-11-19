<script lang="ts">
	// ROADMAP S27, READINESS B1 (ADR-0066 slice 4): how to become a publisher on the test network.
	// The applicant's ten steps with the guide's exact commands (#lib/publish-steps.ts, held against
	// docs/ops/bulk-publish.md by tests/publishers.test.mjs), what the reviewer checks, what a new
	// publisher gets, what becomes public and what is never asked, and the status list. The page's
	// one request by itself is the static status list of this site; step 7's form posts the origin
	// and the application's hash to this site's own node, only when the visitor presses its button.
	import { resolve } from '$app/paths';
	import OnyxSlab from '#lib/components/OnyxSlab.svelte';
	import PublisherPointer from '#lib/components/PublisherPointer.svelte';
	import PublisherStatus from '#lib/components/PublisherStatus.svelte';
	import { ONBOARDING_SINCE, RELEASE, REPO, hasOnboarding } from '#lib/release.ts';
	import { EXAMPLE_ID, EXAMPLE_ORIGIN, ISSUES, ISSUE_TITLE, TOOLS_FILE, TOOLS_URL, UNPACK, WORDS, buildFromSource, steps } from '#lib/publish-steps.ts';
	import { DECIDE_DAYS } from '#lib/publishers.ts';

	// The commands name the hosted test node wherever the page is served from (a mirror or a
	// preview of the site is not a node).
	const STEPS = steps();
	const BUILD = buildFromSource();
	// Until the linked release's tools have the commands of this page, no download is offered.
	const ready = hasOnboarding(RELEASE);
	const GUIDE = `${REPO}/blob/main/docs/ops/bulk-publish.md`;
	// The page is long (about 12,000 px on a phone, the status box last): a jump list opens it.
	// A hash route has no in-page anchors, so each entry scrolls to its section and moves the focus
	// to the section's heading.
	const JUMP = [
		['publish-first', 'Before you start'],
		['publish-tools', 'Get the tools'],
		['publish-steps', 'Ten steps'],
		['publish-review', 'What the reviewer checks'],
		['publish-get', 'What you get'],
		['publish-privacy', 'What is public'],
		['publish-status', 'Application status']
	];
	function jump(id: string) {
		const h = document.querySelector<HTMLElement>(`[data-testid="${id}"] h2`);
		if (!h) return;
		h.tabIndex = -1;
		h.scrollIntoView({ behavior: matchMedia('(prefers-reduced-motion: reduce)').matches ? 'auto' : 'smooth', block: 'start' });
		h.focus({ preventScroll: true });
	}
</script>

<svelte:head><title>Publish your own data · Onyx</title></svelte:head>

<OnyxSlab>
	<h1>Publish your own data</h1>
	<div class="slabside" data-testid="slabside">
		<p class="slablede">Sign your data with your own key and put it on the Onyx test network. A reviewer checks your application once; after that you publish alone.</p>
		<p class="meta slabmeta">Test network only · <span class="nw">no real money</span> · <span class="nw">a decision within {DECIDE_DAYS} days</span></p>
	</div>
</OnyxSlab>

<div class="wrap">
	<div class="jump" data-testid="publish-jump">
		<span class="jlabel" id="jump-label">On this page</span>
		<ul aria-labelledby="jump-label">
			{#each JUMP as [id, label] (id)}
				<li><button type="button" class="ghost" onclick={() => jump(id)} data-testid="publish-jump-to">{label}</button></li>
			{/each}
		</ul>
	</div>
	<section class="section" data-testid="publish-first">
		<h2>Before you start</h2>
		<div class="two">
			<div class="panel" data-testid="publish-testnet">
				<span class="ititle">This is a test network</span>
				<ul class="plain">
					<li>No real money is involved. Use a test key only, never a key that holds funds.</li>
					<li>One person reviews. One key can register a publisher; while its holder is away, no application is answered.</li>
					<li>A registration cannot be undone, so the review comes first.</li>
					<li>No application from outside has been reviewed yet. Yours may be among the first.</li>
				</ul>
			</div>
			<div class="panel" data-testid="publish-time">
				<span class="ititle">What it takes</span>
				<ul class="plain">
					<li data-testid="publish-total">About 40 minutes of your own work, a review of up to {DECIDE_DAYS} days in between, then a wait of 20 to 40 minutes before your upload (the test chain must first make your registration final). In all: about an hour and a half spread over up to {DECIDE_DAYS} days.</li>
					<li>Data you own or may share, under terms that let anyone pass it on, and that names no individual person.</li>
					<li>An HTTPS host you control, to put three small files on.</li>
					<li>A computer with a terminal. The download is for Linux x86-64; elsewhere you build the tools.</li>
				</ul>
				<p class="fine">Only know of data Onyx should have? <a href={resolve('/submit')} data-testid="publish-suggest">Suggest a dataset</a> instead: five questions, no key.</p>
			</div>
			<div class="panel" data-testid="publish-names">
				<span class="ititle">Pick a publisher id and an origin</span>
				<ul class="plain">
					<li>The id is a public, permanent label for the data, for example (made up) <span class="mono">{EXAMPLE_ID}</span>.</li>
					<li>The origin is an HTTPS host you control, or a host and a path, for example <span class="mono">{EXAMPLE_ORIGIN}</span>: your files go there.</li>
					<li>Both describe the data and name no person, company or product, and never Onyx.</li>
				</ul>
				<p class="swarn">The id and the origin are bound together for good. Lose the origin and you lose the proof of your id: keep the domain paid.</p>
			</div>
			<div class="panel" data-testid="publish-words">
				<span class="ititle">Words used below</span>
				<dl class="words">
					{#each WORDS as w (w.word)}
						<dt>{w.word}</dt>
						<dd>{w.means}</dd>
					{/each}
				</dl>
			</div>
		</div>
	</section>

	<section class="section" data-testid="publish-tools">
		<h2>Get the tools</h2>
		<p class="lede">Two programs: <code>onyx-publish</code> makes and signs your files, <code>onyx-check</code> proves what a node serves.</p>
		<div class="panel">
			{#if ready}
				<p>
					<a class="btn" href={TOOLS_URL} rel="noopener" data-testid="publish-download">Download {TOOLS_FILE}</a>
				</p>
				<p class="fine">
					From the test-network release {RELEASE}, for Linux x86-64. Unpack it and put its programs on the path of your terminal: every command
					below starts with <code>onyx-publish</code> or <code>onyx-check</code>.
				</p>
				<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
				<pre class="code cmd" tabindex="0" role="region" aria-label="Unpack the download" data-testid="publish-unpack">{UNPACK.join('\n')}</pre>
				<p>On another system, build the same release from the source. It takes about 15 minutes with a Rust toolchain.</p>
				<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
				<pre class="code cmd" tabindex="0" role="region" aria-label="Build the tools from the source" data-testid="publish-build">{BUILD.join('\n')}</pre>
				<p class="fine">The path line holds for the terminal it is typed in. In a new terminal, type it again from the same folder.</p>
			{:else}
				<p data-testid="publish-soon">
					The commands on this page arrive with the test-network release <span class="mono">{ONBOARDING_SINCE}</span>. The current release,
					<span class="mono">{RELEASE}</span>, is from before them, so no download is linked here yet. The steps below show what applying will take.
				</p>
			{/if}
		</div>
	</section>

	<section class="section" data-testid="publish-steps">
		<h2>Ten steps</h2>
		<p class="lede">
			Run every command in one folder and replace the values in <code>&lt;...&gt;</code>. At any time <code>onyx-publish doctor</code>, a command
			that checks your folder and says what is missing, names the next step and the command for it; it sends nothing.
		</p>
		<ol class="steps">
			{#each STEPS as s, i (s.title)}
				<li class="panel step" data-testid="publish-step">
					<h3 class="stitle"><span class="sn" aria-hidden="true">{i + 1}</span><span data-testid="publish-step-title">{s.title}</span></h3>
					<p class="smeta"><span class="nw">{s.time}</span> · <span class="nw">{s.leaves}</span></p>
					<p>{s.what}</p>
					{#if i === 6}
						<PublisherPointer />
						<h4 class="way" data-testid="publish-issue-way">Second way: a public issue on the source repository</h4>
						<p>Title it "{ISSUE_TITLE}" and put exactly these two lines in it.</p>
						<p class="fine" data-testid="publish-readme-note">The README inside the downloaded tool file may name only the issue: that file was packed before the form existed. Both ways reach the same reviewer, and one is enough.</p>
					{/if}
					{#each s.commands as c, j (c)}
						<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
						<pre class="code cmd" tabindex="0" role="region" aria-label={`Step ${i + 1}${s.commands.length > 1 ? `, command ${j + 1}` : i === 2 ? ', an example schema' : i === 6 ? ', the two lines' : ', command'}`} data-testid="publish-cmd">{c}</pre>
					{/each}
					{#if i === 6}
						<p><a href={ISSUES} target="_blank" rel="noopener" data-testid="publish-issues">Open an issue on the source repository</a> (it offers the template "Publisher application"). Never put a key file, a name or a contact detail in it.</p>
					{/if}
					{#if s.warn}<p class="swarn" data-testid="publish-warn">{s.warn}</p>{/if}
				</li>
			{/each}
		</ol>
		<p class="fine">
			After that you work alone, inside your limits: the rest of your history is version 2 of the dataset. The <a href={GUIDE} target="_blank" rel="noopener" data-testid="publish-guide">full guide</a>
			says how to add data later, what each error message means and what to do about a lost or stolen key.{#if ready}&nbsp;It is also in the download, as <span class="mono">onyx-publish/README.md</span>.{/if}
		</p>
	</section>

	<section class="section" data-testid="publish-review">
		<h2>What the reviewer checks</h2>
		<p class="lede">A program checks what can be computed. A person reads what cannot. The decision comes within {DECIDE_DAYS} days; an application lives 6 days.</p>
		<div class="two">
			<div class="panel">
				<span class="ititle">Checked by a program</span>
				<ul class="plain">
					<li>The files on your origin are the ones you pointed at, and your key signed them for this network.</li>
					<li>The keys document names your id and your key.</li>
					<li>The id and the origin follow their rules and are free in the signed book.</li>
					<li>The first dataset is at most 8 MiB, fits its schema and carries your signed statement that it may be shared.</li>
				</ul>
			</div>
			<div class="panel">
				<span class="ititle">Read by a person</span>
				<ul class="plain">
					<li>The id and the origin name no person, company or product, and never Onyx.</li>
					<li>Nothing reads as if a known organisation were speaking.</li>
					<li>The licence fits the data.</li>
					<li>No row is about one person.</li>
					<li>Nothing in the rows is illegal to host.</li>
				</ul>
			</div>
		</div>
	</section>

	<section class="section" data-testid="publish-get">
		<h2>What a new publisher gets</h2>
		<div class="two">
			<div class="panel" data-testid="publish-grant">
				<span class="ititle">On the hosted test node</span>
				<ul class="plain">
					<li>1 GiB of storage and 16 datasets by default. You may ask for more in your application; the reviewer grants at most what you ask.</li>
					<li>Your id, bound to your origin and your key in the signed book on the test chain.</li>
					<li>Rows anyone can read for free, each with a proof that it is exactly what you signed.</li>
				</ul>
			</div>
			<div class="panel">
				<span class="ititle">What stays yours to do</span>
				<ul class="plain">
					<li>Each later version needs one small transaction, paid with test ETH from a faucet.</li>
					<li>Register your spare key soon after approval: a further key needs the signature of one that is already registered.</li>
					<li>Registration is not a grant of disk on other nodes. Each node's operator lists whom it takes.</li>
				</ul>
			</div>
		</div>
	</section>

	<section class="section" data-testid="publish-privacy">
		<h2>What is public, what is never asked</h2>
		<div class="two">
			<div class="panel" data-testid="publish-public">
				<span class="ititle">Public</span>
				<ul class="plain">
					<li>Your publisher id, your origin and your key's address.</li>
					<li>The one sentence about your data, and your first dataset.</li>
					<li>The decision: its state and date, a yes for each line the reviewer checked, your limits and the two transactions.</li>
					<li>Of an application that is not approved: only its hash, the state, the date and a reason code. After a refusal also the refused key's address and the time until it is blocked.</li>
					<li>The form in step 7 needs no account. An issue shows the account that opens it, and a domain's registration record can name its owner. Check yours before you apply.</li>
				</ul>
			</div>
			<div class="panel" data-testid="publish-never">
				<span class="ititle">Never asked</span>
				<ul class="plain">
					<li>No name: not yours, not a company's.</li>
					<li>No contact detail of any kind, in the application, in the form or in the issue.</li>
					<li>No key file. Your keys never leave your computer.</li>
					<li>No wallet with funds, and no payment.</li>
				</ul>
			</div>
		</div>
	</section>

	<PublisherStatus />
</div>

<style>
	.two {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: var(--sp-4);
	}
	.two > .panel {
		align-content: start;
	}
	@media (max-width: 860px) {
		.two {
			grid-template-columns: minmax(0, 1fr);
		}
	}
	.plain {
		display: grid;
		gap: var(--sp-2);
	}
	.jump {
		display: grid;
		gap: var(--sp-2);
		padding-top: var(--sp-6);
	}
	.jlabel {
		font-family: var(--mono);
		font-size: var(--text-xs);
		color: var(--muted);
	}
	.jump ul {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-2);
	}
	.jump button {
		font-size: var(--text-sm);
	}
	.words {
		margin: 0;
		display: grid;
		gap: var(--sp-1);
	}
	.words dt {
		font-weight: 600;
	}
	.words dd {
		margin: 0 0 var(--sp-2);
		color: var(--muted);
	}
	.steps {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		gap: var(--sp-4);
	}
	.step {
		gap: var(--sp-3);
	}
	.stitle {
		display: flex;
		align-items: baseline;
		gap: var(--sp-3);
		font-size: var(--text-md);
	}
	.sn {
		flex: none;
		font-family: var(--mono);
		font-size: var(--text-xs);
		font-weight: 600;
		color: var(--accent);
		background: var(--accent-soft);
		border-radius: var(--radius-sm);
		padding: var(--sp-0) var(--sp-2);
	}
	.smeta {
		color: var(--muted);
		font-size: var(--text-sm);
	}
	.way {
		margin: 0;
		font-size: var(--text-base);
		font-weight: 600;
	}
	.swarn {
		margin: 0;
		padding: var(--sp-3) var(--sp-4);
		border-radius: var(--radius-md);
		border: 1px solid var(--line-strong);
		background: var(--btc-soft);
		max-width: none;
	}
	/* Commands: one line each while they fit, scrolled inside their box on a wide screen and
	   wrapped on a phone (a long unbroken line must never widen the page). */
	.cmd {
		min-width: 0;
		max-width: 100%;
	}
	@media (max-width: 760px) {
		pre.cmd {
			white-space: pre-wrap;
			overflow-wrap: anywhere;
			overflow-x: hidden;
		}
	}
	.btn {
		min-height: 44px;
	}
</style>
