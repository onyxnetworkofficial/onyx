<script lang="ts">
	// PP6: the full route reference, read from the same OpenAPI file every node serves at
	// /v1/openapi.json (scripts/check-openapi.py keeps the file equal to the code).
	import spec from '../../../static/openapi.json';
	import { byTag, errorCodes, runs } from '#lib/openapi.ts';
	const groups = byTag(spec as Record<string, unknown>);
	const codes = errorCodes(spec as Record<string, unknown>);
	const count = groups.reduce((n, g) => n + g.ops.length, 0);
	// Groups are named by the kind of data they serve (the spec's tags name proof levels 2, 3 and 5,
	// which read as if levels 1 and 4 were missing).
	const GROUP: Record<string, string> = { Rows: 'Rows and proofs', 'Level 2': 'Observations', 'Level 3': 'Signed facts', 'Level 5': 'Track records', Submit: 'Submit data' };
	// Plainer labels than the spec's own summary, where it speaks of itself.
	const SUMMARY: Record<string, string> = { '/v1/openapi.json': 'This API reference (OpenAPI file)' };
	// A path breaks only after a "/", so the method stays on the line with its path's start.
	const segs = (path: string) => path.split(/(?<=\/)/);
</script>

{#snippet text(t: string)}{#each runs(t) as r, i (i)}{#if r.code}<code>{r.text}</code>{:else}{r.text}{/if}{/each}{/snippet}

<section class="section" data-testid="api-reference" aria-labelledby="api-ref-heading">
	<h2 id="api-ref-heading">Reference</h2>
	<p class="lede">
		All {count} routes, read from <a href="openapi.json" download="onyx-openapi.json" data-testid="api-openapi-link">openapi.json</a> (OpenAPI 3.1). Every node also serves this file at
		<code>/v1/openapi.json</code>. A check in the test suite keeps the file equal to the code: the routes, the error codes and the shape of every answer the tests see.
		The hosted test node answers at <a href="https://onyx.cryptagraph.com/v1/health" target="_blank" rel="noopener"><code>https://onyx.cryptagraph.com/v1</code></a>; <code>GET /v1/health</code> shows its state.
	</p>
	{#each groups as g (g.tag)}
		<div class="group" data-testid="api-group">
			<h3>{GROUP[g.tag] ?? g.tag}</h3>
			{#each g.ops as op (op.id)}
				<details class="op" data-testid="api-op" data-op={op.id}>
					<summary>
						<span class="mp"><span class="verb" class:post={op.method !== 'GET'}>{op.method}</span> <code class="path">{#each segs(op.path) as sg, i (i)}{sg}<wbr />{/each}</code></span>
						<span class="sum">{SUMMARY[op.path] ?? op.summary}{#if op.paid}<span class="paid" data-testid="api-op-paid">{' · paid (x402)'}</span>{/if}</span>
					</summary>
					<div class="body">
						<p>{@render text(op.description)}</p>
						{#if op.params.length}
							<h4>Parameters</h4>
							<ul class="lines">
								{#each op.params as prm (prm.in + prm.name)}
									<li><code>{prm.name}</code> <span class="muted">{prm.in}{prm.required ? ', required' : ''}</span>{#if prm.description}: {@render text(prm.description)}{/if}</li>
								{/each}
							</ul>
						{/if}
						{#if op.body}
							<h4>Request body</h4>
							<p><code>{op.body}</code> as JSON.</p>
						{/if}
						<h4>Responses</h4>
						<ul class="lines">
							{#each op.responses.filter((r) => Number(r.status) < 400) as r (r.status)}
								<li><code>{r.status}</code> {@render text(r.description)}{#if r.schema && r.schema !== 'Error'}{' '}<span class="muted">({r.schema})</span>{/if}</li>
							{/each}
							{#if op.responses.some((r) => Number(r.status) >= 400)}
								<li data-testid="api-op-errors">Errors: {#each op.responses.filter((r) => Number(r.status) >= 400) as r, i (r.status)}{i ? ', ' : ''}<code>{r.status}</code>{/each}, each a JSON error with a stable code (the full list is below).</li>
							{/if}
						</ul>
					</div>
				</details>
			{/each}
		</div>
	{/each}
	<details class="op codes" data-testid="api-error-codes">
		<summary><span class="sum">All {codes.length} error codes</span></summary>
		<div class="body">
			<p>Every error is <code>{'{"error": {"code", "message"}}'}</code>. Codes are stable; messages may change.</p>
			<ul class="lines">
				{#each codes as c (c.code)}
					<li data-testid="api-error-code"><code>{c.status}</code> <code>{c.code}</code>: {c.description}</li>
				{/each}
			</ul>
		</div>
	</details>
</section>

<style>
	.group {
		display: grid;
		gap: var(--sp-2);
		margin-top: var(--sp-6);
	}
	.group h3 {
		margin: 0 0 var(--sp-1);
	}
	.op {
		border: 1px solid var(--line);
		border-radius: var(--radius-md);
		background: var(--panel);
		padding: var(--sp-3) var(--sp-4);
		min-width: 0;
	}
	/* The fold marker, then method and path on one line (the path breaks only after a "/"), then the
	   summary: beside them on a wide screen, on its own line under them on a phone. */
	.op > summary {
		cursor: pointer;
		display: grid;
		grid-template-columns: auto auto minmax(0, 1fr);
		align-items: baseline;
		gap: var(--sp-1) var(--sp-3);
		min-height: 44px;
		align-content: center;
	}
	.op > summary::before {
		grid-row: 1;
	}
	@media (max-width: 640px) {
		.op > summary {
			grid-template-columns: auto minmax(0, 1fr);
			column-gap: var(--sp-2);
		}
		.op > summary > .sum {
			grid-column: 2;
		}
	}
	.codes > summary {
		grid-template-columns: auto minmax(0, 1fr);
	}
	.mp {
		min-width: 0;
	}
	.verb {
		font-family: var(--mono);
		font-size: var(--text-xs);
		font-weight: 600;
		color: var(--accent);
		background: var(--accent-soft);
		border-radius: var(--radius-sm);
		padding: var(--sp-0) var(--sp-2);
		margin-right: var(--sp-1);
	}
	.verb.post {
		color: var(--ink);
		background: var(--line);
	}
	.path {
		overflow-wrap: normal;
	}
	.sum {
		color: var(--muted);
		font-size: var(--text-sm);
	}
	.paid {
		color: var(--ink);
	}
	.body {
		margin-top: var(--sp-3);
		font-size: var(--text-sm);
		line-height: var(--lh-body);
		max-width: 68ch;
	}
	.body p {
		margin: 0 0 var(--sp-3);
	}
	.body h4 {
		margin: var(--sp-3) 0 var(--sp-1);
		font-size: var(--text-sm);
	}
	.lines {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		gap: var(--sp-1);
	}
	.lines li,
	.body code {
		overflow-wrap: anywhere;
	}
	.codes {
		margin-top: var(--sp-6);
	}
</style>
