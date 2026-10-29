<script lang="ts">
	import { checkSummary, groupedValue, parseReceipt, RECEIPT_STATUS, validReceiptId, type Receipt } from '#lib/submission-receipt.ts';
	let { received = null }: { received?: Receipt | null } = $props();
	import { submitApi } from '#lib/submit-api.svelte.ts';
	// MP2: the Add data page reads the site's intake.json; this follows it.
	const API = $derived(submitApi.on);
	let id = $state('');
	let receipt = $state<Receipt | null>(null);
	let busy = $state(false);
	let error = $state('');
	let notice = $state('');
	$effect(() => {
		if (received) { id = received.id; receipt = received; error = ''; notice = ''; }
	});
	async function refresh() {
		if (busy || !API) return;
		const target = id.trim();
		error = ''; notice = '';
		if (!validReceiptId(target)) { error = 'Enter the full receipt ID, starting with sub_.'; return; }
		busy = true;
		try {
			const r = await fetch(new URL(`v1/submit/${target}`, document.baseURI), { cache: 'no-store', signal: AbortSignal.timeout(15000) });
			if (r.status === 404) throw new Error('Receipt not found on this server. Check the ID and try again.');
			if (!r.ok) throw new Error('Status is unavailable right now. Keep your receipt and try again later.');
			const next = parseReceipt(await r.json());
			if (next.id !== target) throw new Error('The server returned a different receipt.');
			receipt = next; notice = 'Status checked just now.';
		} catch (e) { error = e instanceof Error ? e.message : 'Could not check the receipt.'; }
		finally { busy = false; }
	}
	function save() {
		if (!receipt) return;
		const url = URL.createObjectURL(new Blob([JSON.stringify({ format: 'onyx-submission-receipt-v1', ...receipt }, null, 2)], { type: 'application/json' }));
		const a = document.createElement('a'); a.href = url; a.download = 'onyx-receipt.json'; a.click();
		setTimeout(() => URL.revokeObjectURL(url), 1000);
		notice = 'Receipt downloaded. Return to Add data and enter its ID to check progress.';
	}
</script>

<section class="section" aria-labelledby="receipt-heading" data-testid="receipt-section">
	<h2 id="receipt-heading">{received ? 'Keep your receipt' : 'Already submitted data?'}</h2>
	<p>{received ? 'Your proposal is saved for review. Keep this receipt to check progress; it does not contain your data.' : API ? 'Enter your receipt ID to see where your proposal stands.' : 'Your receipt ID (it starts with sub_) shows where your proposal stands.'}</p>
	{#if API}
		<form onsubmit={(e) => { e.preventDefault(); refresh(); }}>
			<label class="field"><span>Receipt ID</span><input bind:value={id} oninput={() => { receipt = null; error = ''; notice = ''; }} disabled={busy} placeholder="sub_…" spellcheck="false" autocomplete="off" maxlength="36" data-testid="receipt-id" /></label>
			<button type="submit" disabled={busy} data-testid="receipt-refresh">{busy ? 'Checking status…' : 'Check status'}</button>
		</form>
	{:else}
		<p class="fine">Ask your AI assistant for its status, or call <code>GET /v1/submit/sub_…</code> on the API. The steps are under “Add data with AI or the API” at the top of this page.</p>
	{/if}
	{#if receipt}
		<div role="status" data-testid="receipt-status">
			<p class="mono receipt-id">{receipt.id}</p>
			<p>{RECEIPT_STATUS[receipt.status]}</p>
		</div>
		{#if receipt.check}
			{@const c = checkSummary(receipt.check)}
			<div class="check" data-testid="receipt-check">
				<h3>{c.title}</h3>
				<p>{c.meaning}</p>
				<dl>
					<dt>Record date</dt><dd class="mono">{receipt.check.recordDate}</dd>
					<dt>Value the observers agreed on</dt><dd class="mono" data-testid="receipt-value">{groupedValue(receipt.check.value)}</dd>
					<dt>Round</dt><dd class="mono">{receipt.check.roundId}</dd>
					<dt>Result hash</dt><dd class="mono">{receipt.check.result}</dd>
				</dl>
				<p class="fine">{c.limit} {c.where}</p>
			</div>
		{/if}
		<button type="button" class="ghost" onclick={save} data-testid="receipt-download">Download receipt</button>
	{/if}
	{#if notice}<p role="status" class="fine">{notice}</p>{/if}
	{#if error}<p role="alert" data-testid="receipt-error">{error}</p>{/if}
</section>

<style>
	form { display: grid; gap: var(--sp-3); max-width: 42rem; }
	button { justify-self: start; }
	.field { display: grid; gap: var(--sp-2); }
	.receipt-id { overflow-wrap: anywhere; }
	.check { border-left: 2px solid var(--accent-line); padding-left: var(--sp-4); margin-block: var(--sp-4); max-width: 42rem; }
	.check h3 { margin: 0 0 var(--sp-2); }
	.check dl { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: var(--sp-1) var(--sp-3); margin: var(--sp-2) 0; }
	.check dt { font-weight: 600; }
	.check dd { margin: 0; overflow-wrap: anywhere; }
</style>
