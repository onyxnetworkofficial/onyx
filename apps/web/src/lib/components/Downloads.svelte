<script lang="ts">
	// The test-network release's programs (scripts/release-build.sh): what to download to add data from
	// your own computer. Every file is on the public repository's release, with its SHA-256 in
	// SHA256SUMS. The release tag lives in #lib/release.ts, the one place to change for a release.
	// `app` adds the link to the desktop app's Download page (on Add data); the Download page itself
	// shows this list under "For developers", without that link and with the single-file programs.
	import { resolve } from '$app/paths';
	import { BASE, CLI_FILES, PAGE, hasDesktop, programs } from '#lib/release.ts';
	let { app = true, heading = 'Programs to download' }: { app?: boolean; heading?: string } = $props();
	const PROGRAMS = programs();
	// The single-file contributor program (no Node.js) ships with the desktop releases only.
	const single = $derived(!app && hasDesktop());
</script>

<section class="section side" data-testid="downloads">
	<h2>{heading}</h2>
	<div class="dlbody">
		{#if app}
			<p class="app" data-testid="download-app">
				<a class="btn" href={resolve('/download')} data-testid="download-app-link">Get the Onyx app for Windows, macOS or Linux</a>
			</p>
		{/if}
		<p class="muted">Test network only: no real money. Each program’s guide is inside its download.</p>
		<ul class="dl">
			{#if single}
				<li data-testid="download-single">
					<b>Contributor app, one file</b>
					<span>The same contributor app as a single program: nothing else to install. Run <code>onyx-contrib ui</code> for its window in your browser, or <code>onyx-contrib mcp</code> from an MCP client.</span>
					<span class="muted">Needs: nothing; Linux x86-64, macOS (Apple silicon or Intel) or Windows x64</span>
					<span class="files">
						{#each CLI_FILES as f (f)}<a href={BASE + f} rel="noopener" data-testid="download-cli-file">{f}</a>{/each}
					</span>
				</li>
			{/if}
			{#each PROGRAMS as p (p.id)}
				<li data-testid="download-{p.id}">
					<b>{p.title}</b>
					<span>{p.what}</span>
					<span class="muted">Needs: {p.needs}</span>
					<span class="files">
						{#each p.files as f (f)}<a href={BASE + f} rel="noopener" data-testid="download-file">{f}</a>{/each}
					</span>
				</li>
			{/each}
		</ul>
		<p class="muted sums">
			<a href={PAGE} target="_blank" rel="noopener" data-testid="download-release">All files and their SHA-256 sums</a>
		</p>
	</div>
</section>

<style>
	.dlbody {
		display: grid;
		gap: var(--sp-4);
		min-width: 0;
	}
	.app {
		margin: 0;
	}
	.dl {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		gap: var(--sp-4);
	}
	.dl li {
		display: grid;
		gap: var(--sp-1);
		overflow-wrap: anywhere;
	}
	.files {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-3);
		font-family: var(--mono);
		font-size: var(--text-xs);
	}
	/* Standalone links: a 44 px tap target on phones (W12). */
	.app .btn,
	.files a,
	.sums a {
		display: inline-flex;
		align-items: center;
		min-height: 44px;
	}
</style>
