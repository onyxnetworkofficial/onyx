<script lang="ts">
	// The test-network release's programs (scripts/release-build.sh): what to download to add data from
	// your own computer. Every file is on the public repository's release, with its SHA-256 in
	// SHA256SUMS. The release tag is the one place to change for a new release.
	const RELEASE = 'v0.2.0-testnet';
	const BASE = `https://github.com/onyxnetworkofficial/onyx/releases/download/${RELEASE}/`;
	const PAGE = `https://github.com/onyxnetworkofficial/onyx/releases/tag/${RELEASE}`;
	const PROGRAMS = [
		{
			id: 'contrib',
			title: 'Contributor app',
			what: 'Keep your API keys in your computer’s keychain and add data from those APIs through Claude Desktop, Claude Code or another MCP client. The keys never leave your computer.',
			needs: 'Node.js 20 or later; Linux, macOS or Windows',
			files: [`onyx-contrib-${RELEASE}.zip`, `onyx-contrib-${RELEASE}.tar.gz`]
		},
		{
			id: 'join',
			title: 'Observer kit',
			what: 'Take part in level-2 rounds: your computer fetches the public source itself and posts its own signed answer on the test chain.',
			needs: 'Linux x86-64',
			files: ['onyx-join-linux-x86_64.tar.gz']
		},
		{
			id: 'publish',
			title: 'Publisher tools',
			what: 'Publish a large dataset of your own: sign it, anchor its fingerprint on the test chain and upload it. Anyone can then prove each row they read.',
			needs: 'Linux x86-64',
			files: ['onyx-publish-linux-x86_64.tar.gz']
		}
	];
</script>

<section class="section side" data-testid="downloads">
	<h2>Programs to download</h2>
	<p class="muted">Test network only: no real money. Each program’s guide is inside its download.</p>
	<ul class="dl">
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
</section>

<style>
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
	.files a,
	.sums a {
		display: inline-flex;
		align-items: center;
		min-height: 44px;
	}
</style>
