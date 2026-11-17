<script lang="ts">
	// The publish page's status box (ADR-0066 item 9): the status list `publishers.json`, a static
	// file of this site, read once. A visitor looks an application up by the first digits of its hash,
	// or a publisher by its id; the lookup runs here and sends nothing. Every value shown passed
	// #lib/publishers.ts (the list's exact shape, or nothing) and is printed as text, never as HTML:
	// this file builds no markup and no link from the list (tests/publishers.test.mjs reads it for that).
	import { onMount } from 'svelte';
	import Pill, { type PillState } from '#lib/components/Pill.svelte';
	import { APPLICATION_STATES, BEFORE_RECORD, MIN_PREFIX, PUBLISHER_STATES, REASONS, lookup, nextStep, parseStatusText, type Lookup, type StatusList } from '#lib/publishers.ts';

	let list = $state<StatusList | null>(null);
	let pill = $state<{ state: PillState; text: string }>({ state: 'run', text: 'Reading the status list' });
	let failed = $state(false);
	let query = $state('');
	let found = $state<Lookup>({ kind: 'empty' });

	onMount(async () => {
		try {
			const r = await fetch('publishers.json', { cache: 'no-store', credentials: 'omit' });
			list = r.ok ? parseStatusText(await r.text()) : null;
		} catch {
			list = null;
		}
		if (list) {
			const p = list.publishers.length;
			const a = list.applications.length;
			pill = { state: 'good', text: `${p} ${p === 1 ? 'publisher' : 'publishers'} · ${a} ${a === 1 ? 'application' : 'applications'} not approved` };
		} else {
			failed = true;
			pill = { state: 'warn', text: 'Status list not available' };
		}
	});

	function find(e: SubmitEvent) {
		e.preventDefault();
		if (list) found = lookup(list, query);
	}
</script>

<section class="section" data-testid="publish-status">
	<h2>Application status</h2>
	<p class="lede">
		The public record of every decision, as a list. A registered publisher is listed under its id. An application that is not approved
		is listed under its hash only: no id, origin or text of it is kept.
	</p>
	<div class="panel">
		<div class="row"><Pill state={pill.state} text={pill.text} live /></div>
		{#if failed}
			<p data-testid="status-failed">
				The status list could not be read, so no state is shown here. This says nothing about any application. An application filed
				as an issue is also answered in that issue.
			</p>
		{:else if list}
			<form class="find" onsubmit={find} data-testid="status-form">
				<label class="field" for="status-query"><span>The first {MIN_PREFIX} or more digits of your application's hash, or a publisher id</span></label>
				<div class="findrow">
					<input id="status-query" type="text" bind:value={query} maxlength="128" spellcheck="false" autocomplete="off" autocapitalize="off" data-testid="status-query" />
					<button type="submit" data-testid="status-find">Look up</button>
				</div>
				<p class="fine">The lookup runs in your browser. What you type is not sent anywhere.</p>
			</form>
			<div class="result" role="status" aria-live="polite" data-testid="status-result" data-kind={found.kind}>
				{#if found.kind === 'invalid'}
					<p>That is neither a publisher id nor at least {MIN_PREFIX} digits of a hash (the digits 0 to 9 and the letters a to f).</p>
				{:else if found.kind === 'none'}
					<p>
						Nothing in the list matches. An application appears here once the reviewer has recorded it. An approved application is listed
						under its publisher id, not under its hash.
					</p>
				{:else if found.kind === 'found'}
					<ul class="rows">
						{#each found.applications as a (a.application)}
							<li class="hit" data-testid="status-application">
								<span class="state" data-testid="status-state">{a.state}</span>
								<span>{APPLICATION_STATES[a.state]}</span>
								<dl class="kv">
									<dt>Application</dt>
									<dd class="hash">{a.application}</dd>
									<dt>Date</dt>
									<dd>{a.date}</dd>
									{#if a.reason}
										<dt>Reason</dt>
										<dd data-testid="status-reason"><span class="mono">{a.reason}</span>: {REASONS[a.reason]}</dd>
									{/if}
								</dl>
								<p data-testid="status-next">{nextStep(a)}</p>
							</li>
						{/each}
						{#each found.publishers as p (p.publisher)}
							<li class="hit" data-testid="status-publisher">
								<span class="state" data-testid="status-state">{p.state}</span>
								<span>{PUBLISHER_STATES[p.state]}</span>
								<dl class="kv">
									<dt>Publisher id</dt>
									<dd class="hash">{p.publisher}</dd>
									{#if p.state !== BEFORE_RECORD}
										<dt>Date</dt>
										<dd>{p.date}</dd>
									{/if}
									{#if p.reason}
										<dt>Reason</dt>
										<dd data-testid="status-reason"><span class="mono">{p.reason}</span>: {REASONS[p.reason]}</dd>
									{/if}
								</dl>
							</li>
						{/each}
					</ul>
				{/if}
			</div>
			<h3 class="ititle">Publishers</h3>
			{#if list.publishers.length === 0}
				<p data-testid="status-nobody">The list names no publisher yet.</p>
			{:else}
				<ul class="rows" data-testid="status-list">
					{#each list.publishers as p (p.publisher)}
						<li class="pub" data-testid="status-row">
							<span class="hash id" data-testid="status-id">{p.publisher}</span>
							<span class="state" data-testid="status-row-state">{p.state}</span>
							<span class="says">{PUBLISHER_STATES[p.state]}</span>
							<!-- Review S4-R1 M2: a publisher from before the record shows its id and state only. -->
							{#if p.state !== BEFORE_RECORD}
							<dl class="kv">
								<dt>Date</dt>
								<dd>{p.date}</dd>
								<dt>Origin</dt>
								<dd class="hash" data-testid="status-origin">{p.origin}</dd>
								<dt>{p.signers.length === 1 ? 'Key address' : 'Key addresses'}</dt>
								<dd class="hash">{#each p.signers as s (s)}<span class="line">{s}</span>{:else}none active{/each}</dd>
								{#each p.registered as r (r.tx)}
									<dt>Registered</dt>
									<dd class="hash">block {r.block}, transaction {r.tx}</dd>
								{/each}
								{#if p.reason}
									<dt>Reason</dt>
									<dd><span class="mono">{p.reason}</span>: {REASONS[p.reason]}</dd>
								{/if}
							</dl>
							{/if}
						</li>
					{/each}
				</ul>
			{/if}
			<p class="fine" data-testid="status-source">
				{#if list.book}Registrations are in the signed book <span class="hash">{list.book}</span>{#if list.chainId !== null}&nbsp;on chain {list.chainId}{/if}. The book decides which key
					counts; this list adds the state.{/if}
				The list says nothing about who runs a publisher.
			</p>
		{/if}
	</div>
</section>

<style>
	.find {
		display: grid;
		gap: var(--sp-2);
	}
	.field {
		font-size: var(--text-sm);
		color: var(--muted);
	}
	.findrow {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-3);
	}
	.findrow input {
		flex: 1 1 16rem;
		min-width: 0;
		min-height: 44px;
		font: inherit;
		font-family: var(--mono);
		font-size: var(--text-field);
		color: var(--ink);
		background: var(--panel-solid);
		border: 1px solid var(--line-strong);
		border-radius: var(--radius-sm);
		padding: var(--sp-2) var(--sp-3);
	}
	.findrow input:hover {
		border-color: var(--line-bright);
	}
	.findrow button {
		min-height: 44px;
	}
	.result:empty {
		display: none;
	}
	.rows {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		gap: var(--sp-4);
	}
	.hit,
	.pub {
		display: grid;
		gap: var(--sp-2);
		justify-items: start;
		min-width: 0;
		padding-top: var(--sp-4);
		border-top: 1px solid var(--line);
	}
	.hit {
		padding: var(--sp-4);
		border: 1px solid var(--line-strong);
		border-radius: var(--radius-md);
		background: var(--accent-soft);
	}
	.hit .kv,
	.pub .kv {
		width: 100%;
	}
	.id {
		font-size: var(--text-base);
		font-weight: 600;
	}
	.state {
		font-family: var(--mono);
		font-size: var(--text-xs);
		font-weight: 600;
		color: var(--accent);
		background: var(--accent-soft);
		border: 1px solid var(--line-strong);
		border-radius: var(--radius-sm);
		padding: var(--sp-0) var(--sp-2);
	}
	.says {
		color: var(--muted);
	}
	.line {
		display: block;
	}
</style>
