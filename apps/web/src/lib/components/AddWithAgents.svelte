<script lang="ts">
	// Meeting polish MP1 (docs/website-plan.md): the Add data page's second way in. The same proposal the
	// form builds can come from an AI assistant over the hosted MCP connector (apps/agent/src/http.mjs,
	// onyx_propose_dataset: checks first, records only after the user confirms) or straight from the API
	// (/v1/submit/check records nothing; /v1/submit records it for review; /v1/submit/{id} is its status).
	// Every command below was run against the live API on 2026-10-29. Static text only: nothing is sent.
	import { resolve } from '$app/paths';
	import Copy from '@lucide/svelte/icons/copy';
	import Check from '@lucide/svelte/icons/check';
	import X from '@lucide/svelte/icons/x';

	let { open = $bindable(false) }: { open?: boolean } = $props();
	let tab = $state<'ai' | 'api'>('ai');
	let copied = $state('');

	// The hosted node: the page's own origin when it is served by one, else the public site.
	const origin = typeof location !== 'undefined' && location.protocol === 'https:' ? location.origin : 'https://onyx.cryptagraph.com';
	const mcp = `${origin}/mcp`;
	const claudeCode = `claude mcp add --transport http onyx ${mcp}`;
	// MP15 (owner: "only Claude can be connected"): the same address in each common client. Config
	// shapes from each client's own docs (2026-10-29): Cursor `url`, VS Code `servers` + `type: http`,
	// Gemini CLI `httpUrl` (its `url` means SSE). The connector needs no key or sign-in.
	const json = (o: unknown) => JSON.stringify(o, null, 2);
	type Client = { id: string; name: string; steps: string; code?: string; file?: string };
	const CLIENTS: Client[] = [
		{ id: 'chatgpt', name: 'ChatGPT', steps: 'Settings → Apps & Connectors → Advanced: turn on Developer mode (paid plans). Then create a connector named Onyx with this MCP server URL and no authentication.', code: mcp },
		{ id: 'claude', name: 'Claude app', steps: 'Settings → Connectors → Add custom connector. Name it Onyx and paste this URL.', code: mcp },
		{ id: 'claude-code', name: 'Claude Code', steps: 'Run this in a terminal:', code: claudeCode },
		{ id: 'cursor', name: 'Cursor', steps: 'Add this to ~/.cursor/mcp.json (or .cursor/mcp.json in a project):', code: json({ mcpServers: { onyx: { url: mcp } } }) },
		{ id: 'vscode', name: 'VS Code', steps: 'Add this to .vscode/mcp.json, then start the server from the file (GitHub Copilot agent mode):', code: json({ servers: { onyx: { type: 'http', url: mcp } } }) },
		{ id: 'gemini', name: 'Gemini CLI', steps: 'Add this to ~/.gemini/settings.json:', code: json({ mcpServers: { onyx: { httpUrl: mcp } } }) },
		{ id: 'other', name: 'Other', steps: 'Any assistant or agent framework that takes a remote MCP server over Streamable HTTP. No key and no sign-in.', code: mcp }
	];
	let client = $state('chatgpt');
	const chosen = $derived(CLIENTS.find((c) => c.id === client) ?? CLIENTS[0]);
	const ask = 'Does Onyx have the U.S. Treasury’s daily cash balance? If not, propose it as a new dataset.';
	const follow = 'What is the status of my Onyx proposal sub_…?';
	const proposal = JSON.stringify(
		{
			answers: { what: 'public_api', noLogin: true, rights: 'open_license', namesPerson: false, changes: 'daily', licenseId: 'onyx:us-treasury-fiscal-data-open' },
			title: 'U.S. total public debt outstanding',
			description: "The Treasury's Debt to the Penny, one record per business day.",
			sourceName: 'U.S. Treasury Fiscal Data: Debt to the Penny',
			sourceUrl: 'https://fiscaldata.treasury.gov/datasets/debt-to-the-penny/debt-to-the-penny',
			endpointUrl: 'https://api.fiscaldata.treasury.gov/services/api/fiscal_service/v2/accounting/od/debt_to_penny?filter=record_date:eq:2026-10-27',
			jsonPointer: '/data/0/tot_pub_debt_out_amt',
			unit: 'USD'
		},
		null,
		2
	);
	const check = `curl -s ${origin}/v1/submit/check \\\n  -H 'content-type: application/json' \\\n  -d @proposal.json`;
	const send = `curl -s ${origin}/v1/submit \\\n  -H 'content-type: application/json' \\\n  -d @proposal.json`;
	const status = `curl -s ${origin}/v1/submit/sub_…`;

	async function copy(id: string, text: string) {
		try {
			await navigator.clipboard.writeText(text);
			copied = id;
			setTimeout(() => copied === id && (copied = ''), 1400);
		} catch {
			// Clipboard refused (some embedded views): the text stays selectable on the page.
		}
	}
</script>

{#snippet code(id: string, text: string, label: string, scroll = false)}
	<div class="snip" class:scroll>
		<!-- A scrolling block takes the focus so a keyboard can scroll it (axe scrollable-region-focusable). -->
		{#if scroll}
			<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
			<pre class="code" tabindex="0" role="region" aria-label={label}><code>{text}</code></pre>
		{:else}
			<pre class="code"><code>{text}</code></pre>
		{/if}
		<button type="button" class="cp" onclick={() => copy(id, text)} aria-label="Copy {label}" data-testid="copy-{id}">
			{#if copied === id}<Check size={16} /> Copied{:else}<Copy size={16} /> Copy{/if}
		</button>
	</div>
{/snippet}

{#if open}
	<section class="panel agents" id="add-with-agents" aria-labelledby="agents-title" data-testid="add-with-agents">
		<div class="top">
			<div>
				<p class="eyebrow">No forms needed</p>
				<h2 id="agents-title" tabindex="-1">Add data with an AI assistant or the API</h2>
				<p class="intro">Both take the same proposal as the form below and give the same receipt. Nothing is published before a review.</p>
			</div>
			<button type="button" class="ghost close" onclick={() => (open = false)} aria-label="Close" data-testid="agents-close"><X size={18} /></button>
		</div>

		<div class="tabs" role="group" aria-label="Way to add data">
			<button type="button" class="ghost" aria-pressed={tab === 'ai'} onclick={() => (tab = 'ai')} data-testid="agents-tab-ai">AI assistant (MCP)</button>
			<button type="button" class="ghost" aria-pressed={tab === 'api'} onclick={() => (tab = 'api')} data-testid="agents-tab-api">API</button>
		</div>

		{#if tab === 'ai'}
			<ol class="how" data-testid="agents-ai">
				<li>
					<h3>Connect your assistant</h3>
					<p>Onyx is one connector address. It works in any assistant that supports MCP, the open standard for connecting AI tools.</p>
					<div class="clients" role="group" aria-label="Your assistant">
						{#each CLIENTS as c (c.id)}
							<button type="button" class="ghost" aria-pressed={client === c.id} onclick={() => (client = c.id)} data-testid="agents-client-{c.id}">{c.name}</button>
						{/each}
					</div>
					<p data-testid="agents-client-steps">{chosen.steps}</p>
					{#if chosen.code}{#key chosen.id}{@render code(`mcp-${chosen.id}`, chosen.code, `the ${chosen.name} setup`, chosen.code.startsWith('{'))}{/key}{/if}
				</li>
				<li>
					<h3>Ask in your own words</h3>
					{@render code('mcp-ask', ask, 'the example question')}
					<p>It searches Onyx first. If the data is missing, it drafts a proposal and checks it with Onyx. Nothing is recorded yet.</p>
				</li>
				<li>
					<h3>Say yes, get a receipt</h3>
					<p>Your assistant shows you the check and sends the proposal only after you agree. You get a receipt id that starts with <code>sub_</code>.</p>
				</li>
				<li>
					<h3>Follow it</h3>
					{@render code('mcp-follow', follow, 'the follow-up question')}
				</li>
			</ol>
			<p class="fine">Public, openly licensed sources only, up to 5 proposals an hour. For a source that needs your own API key, or for your own file, use the form below: your key never leaves your browser.</p>
		{:else}
			<ol class="how api" data-testid="agents-api">
				<li>
					<h3>Write the proposal</h3>
					<p>Save it as <code>proposal.json</code>. The form below builds the same file: its last step can download it.</p>
					{@render code('api-json', proposal, 'the example proposal', true)}
				</li>
				<li>
					<h3>Check it (records nothing)</h3>
					{@render code('api-check', check, 'the check command')}
					<p>The answer says which proof level the data can reach and, for a public source, previews the value.</p>
				</li>
				<li>
					<h3>Send it for review</h3>
					{@render code('api-send', send, 'the send command')}
					<p>The answer is a receipt with an id that starts with <code>sub_</code>. If the call times out, check the receipt before you send again.</p>
				</li>
				<li>
					<h3>Follow it</h3>
					{@render code('api-status', status, 'the status command')}
				</li>
			</ol>
			<p class="fine">Every route, with its fields and answers, is on the <a href={resolve('/api')}>API page</a>. Test network only; no key and no payment are needed to propose data.</p>
		{/if}
	</section>
{/if}

<style>
	.agents {
		margin-top: var(--sp-6);
		gap: var(--sp-5);
	}
	.top {
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		gap: var(--sp-4);
	}
	.eyebrow {
		font: 500 var(--text-sm) / var(--lh-body) var(--mono);
		color: var(--muted);
		margin: 0 0 var(--sp-2);
	}
	h2 {
		margin: 0;
	}
	h2:focus {
		outline: none;
	}
	.intro,
	.fine,
	.how p {
		margin: 0;
		line-height: var(--lh-body);
		max-width: 68ch;
		text-wrap: pretty;
	}
	.intro {
		margin-top: var(--sp-2);
		color: var(--muted);
	}
	.fine {
		color: var(--muted);
		font-size: var(--text-sm);
	}
	.close {
		flex: none;
		padding: var(--sp-2);
		line-height: 0;
	}
	.tabs {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-2);
	}
	.clients {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-2);
	}
	.clients button {
		min-height: 44px;
		padding: var(--sp-1) var(--sp-3);
		font-size: var(--text-sm);
	}
	.clients button[aria-pressed='true'],
	.tabs button[aria-pressed='true'] {
		border-color: var(--accent);
		color: var(--ink);
		background: var(--accent-soft);
	}
	.how {
		list-style: none;
		counter-reset: step;
		margin: 0;
		padding: 0;
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: var(--sp-5) var(--sp-6);
	}
	.how li {
		counter-increment: step;
		display: grid;
		gap: var(--sp-3);
		align-content: start;
		min-width: 0;
	}
	.how h3 {
		margin: 0;
		display: flex;
		align-items: baseline;
		gap: var(--sp-3);
	}
	.how h3::before {
		content: counter(step, decimal-leading-zero);
		font: 500 var(--text-sm) / var(--lh-none) var(--mono);
		color: var(--accent);
	}
	/* The API steps read top to bottom, like the reference: each command keeps its lines. */
	.how.api {
		grid-template-columns: minmax(0, 1fr);
		max-width: 860px;
	}
	.snip {
		position: relative;
		min-width: 0;
	}
	.snip pre.code {
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		padding-right: var(--sp-10);
	}
	/* The proposal keeps its JSON lines and scrolls sideways instead of wrapping mid-value. */
	.snip.scroll pre.code {
		white-space: pre;
		overflow-x: auto;
		overflow-wrap: normal;
		max-height: 22rem;
	}
	.cp {
		position: absolute;
		top: var(--sp-2);
		right: var(--sp-2);
		gap: var(--sp-1);
		padding: var(--sp-1) var(--sp-2);
		font-size: var(--text-xs);
		background: var(--panel-2);
		border: 1px solid var(--line-strong);
		color: var(--muted);
		box-shadow: none;
	}
	.cp:hover:not(:disabled) {
		color: var(--ink);
		transform: none;
		box-shadow: none;
	}
	@media (max-width: 760px) {
		.how {
			grid-template-columns: minmax(0, 1fr);
		}
		.snip pre.code {
			padding-right: var(--sp-4);
			padding-top: var(--sp-8);
		}
	}
</style>
