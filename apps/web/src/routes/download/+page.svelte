<script lang="ts">
	// CL10, ADR-0061: the Onyx desktop app, as easy as downloading a wallet from its project's site:
	// one big button for the visitor's system, the other systems under it, the SHA-256 sums, plain
	// steps for the warnings an unsigned installer raises, and the command-line programs for
	// developers. The release and the file names live in #lib/release.ts (RELEASE is the one place to
	// change); until RELEASE carries the installers (DESKTOP_SINCE) no installer is linked.
	import { onMount } from 'svelte';
	import Download from '@lucide/svelte/icons/download';
	import OnyxSlab from '#lib/components/OnyxSlab.svelte';
	import Downloads from '#lib/components/Downloads.svelte';
	import { BASE, DESKTOP, DESKTOP_SINCE, RELEASE, SUMS, detectSystem, hasDesktop, type System } from '#lib/release.ts';

	const ready = hasDesktop(RELEASE);
	// Read once in the browser; 'other' (a phone, a tablet, an unknown system) shows every system alike.
	let system = $state<System | null>(null);
	onMount(() => {
		const nav = navigator as Navigator & { userAgentData?: { platform?: string } };
		system = detectSystem(nav.userAgent, nav.platform, nav.userAgentData?.platform ?? '', nav.maxTouchPoints ?? 0);
	});
	const mine = $derived(DESKTOP.find((d) => d.id === system) ?? null);
	// Under the button: the detected system's other installer (Windows .msi, Linux .deb), then the other systems.
	const rest = $derived(mine ? [...(mine.files.length > 1 ? [{ ...mine, name: `${mine.name}, other installer`, files: mine.files.slice(1) }] : []), ...DESKTOP.filter((d) => d.id !== mine.id)] : DESKTOP);
</script>

<svelte:head><title>Download · Onyx</title></svelte:head>

<OnyxSlab>
	<h1>Download Onyx</h1>
	<div class="slabside" data-testid="slabside">
		<p class="slablede">The Onyx app keeps your API keys on your computer and adds data from them to Onyx. Install it like any program: no terminal, nothing else to install.</p>
		<p class="meta slabmeta">Test network only · <span class="nw">no real money</span> · <span class="nw">release {RELEASE}</span></p>
	</div>
</OnyxSlab>

<div class="wrap">
	<section class="section" data-testid="download-get" data-system={system ?? ''}>
		<h2>Get the app</h2>
		{#if ready}
			{#if mine}
				<div class="hero" data-testid="download-hero">
					<a class="btn big" href={BASE + mine.files[0].file} rel="noopener" data-testid="download-button"><Download size={22} aria-hidden="true" />Download Onyx for {mine.name}</a>
					<p class="muted">{mine.needs} · {mine.files[0].kind}</p>
				</div>
			{:else if system}
				<p class="lede" data-testid="download-pick">The Onyx app runs on a computer: Windows, macOS or Linux. Pick your system below, or open this page on your computer.</p>
			{/if}
			<ul class="systems" aria-label={mine ? 'Other systems' : 'Every system'} data-testid="download-systems">
				{#each rest as d (d.id)}
					<li class="panel sys" data-testid="download-system-{d.id}">
						<span class="ititle">{d.name}</span>
						<span class="muted">{d.needs}</span>
						<span class="files">
							{#each d.files as f (f.file)}<a href={BASE + f.file} rel="noopener" data-testid="download-desktop-file">{f.kind}</a>{/each}
						</span>
					</li>
				{/each}
			</ul>
			<p class="fine sums">
				<a href={BASE + SUMS} rel="noopener" data-testid="download-sums">SHA256SUMS</a>
				<span>The SHA-256 of every file in this release, to check a download: <code>sha256sum -c SHA256SUMS --ignore-missing</code></span>
			</p>
		{:else}
			<p class="lede" data-testid="download-soon">
				The installers for Windows, macOS and Linux arrive with the test-network release <span class="mono">{DESKTOP_SINCE}</span>. Until then, the contributor app
				below runs the same way from a terminal, with Node.js.
			</p>
		{/if}
	</section>

	{#if ready}
		<section class="section" data-testid="download-warnings">
			<h2>If your computer warns you</h2>
			<p class="lede">The installers are not signed yet (signing comes later), so the first start shows a warning. The app is the project's own build, with its SHA-256 above.</p>
			<div class="warn3">
				<div class="panel">
					<span class="ititle">Windows</span>
					<ol>
						<li>Run the installer you downloaded.</li>
						<li>If "Windows protected your PC" appears, choose More info.</li>
						<li>Choose Run anyway.</li>
					</ol>
				</div>
				<div class="panel">
					<span class="ititle">macOS</span>
					<ol>
						<li>Open the .dmg and drag Onyx to Applications.</li>
						<li>In Applications, Control-click (or right-click) Onyx, choose Open, then Open again.</li>
						<li>Or, after a refused start: System Settings, Privacy &amp; Security, Open Anyway.</li>
					</ol>
				</div>
				<div class="panel">
					<span class="ititle">Linux</span>
					<ol>
						<li>AppImage: in the file's Properties allow executing it (or <code>chmod +x</code> it), then double-click it.</li>
						<li>.deb: open it with Software Install, or run <code class="cmd"><span class="nw">sudo apt install</span> <span class="nw">./onyx-desktop-linux-amd64.deb</span></code></li>
					</ol>
				</div>
			</div>
		</section>
	{/if}

	<Downloads app={false} heading="For developers" />
</div>

<style>
	.hero {
		display: grid;
		gap: var(--sp-3);
		justify-items: start;
	}
	.big {
		font-size: var(--text-lg);
		padding: var(--sp-4) var(--sp-7);
		gap: var(--sp-3);
		min-height: 44px;
	}
	.systems {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
		gap: var(--sp-4);
	}
	.sys {
		align-content: start;
		gap: var(--sp-2);
	}
	.files {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-1) var(--sp-4);
	}
	/* Standalone links: 44 px tap targets on phones (W2). */
	.files a,
	.sums a {
		display: inline-flex;
		align-items: center;
		min-height: 44px;
	}
	.sums {
		display: grid;
		gap: var(--sp-1);
	}
	.warn3 {
		display: grid;
		grid-template-columns: repeat(3, minmax(0, 1fr));
		gap: var(--sp-4);
	}
	.warn3 .panel {
		align-content: start;
	}
	.warn3 ol {
		display: grid;
		gap: var(--sp-2);
	}
	@media (max-width: 860px) {
		.warn3 {
			grid-template-columns: minmax(0, 1fr);
		}
	}
</style>
