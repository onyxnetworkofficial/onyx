<script lang="ts">
	// The publish page's pointer form (ADR-0066 item 7, READINESS B5): the way to apply without an
	// account. It posts the origin and the application's hash to this site's own origin
	// (/v1/publishers/pointer) and nothing else, only when the visitor presses the button. Every
	// outcome is shown in this page's own words (#lib/publisher-pointer.ts): of the node's answer
	// only a receipt whose values fit their patterns is printed, as text. This file builds no
	// markup and no link from an answer (tests/publishers.test.mjs reads it for that).
	import { POINTER_DAYS, POINTER_PATH, RECEIPT_DAYS, UNREACHED_WORDS, hashProblem, originProblem, pointerBody, pointerOutcome, receiptWords, type PointerOutcome } from '#lib/publisher-pointer.ts';

	let origin = $state('');
	let application = $state('');
	let sending = $state(false);
	let problem = $state<{ origin: string | null; application: string | null }>({ origin: null, application: null });
	let outcome = $state<PointerOutcome | null>(null);

	async function send(e: SubmitEvent) {
		e.preventDefault();
		if (sending) return;
		const o = origin.trim();
		const a = application.trim();
		problem = { origin: originProblem(o), application: hashProblem(a) };
		outcome = null;
		if (problem.origin || problem.application) return;
		sending = true;
		try {
			const r = await fetch(new URL(POINTER_PATH, document.baseURI), {
				method: 'POST',
				headers: { 'content-type': 'application/json' },
				body: pointerBody(o, a),
				credentials: 'omit',
				cache: 'no-store',
				redirect: 'error',
				signal: AbortSignal.timeout(15000)
			});
			let body: unknown = null;
			try {
				body = JSON.parse(await r.text());
			} catch {
				body = null;
			}
			outcome = pointerOutcome(r.status, body);
		} catch {
			outcome = { kind: 'failed', words: UNREACHED_WORDS };
		} finally {
			sending = false;
		}
	}
</script>

<div class="pointer" data-testid="pointer">
	<h4 class="way">First way: send the two values here, without an account</h4>
	<p>
		The form sends your origin and the hash to this site’s node and nothing else. The node keeps the two values for {POINTER_DAYS} days, never
		your address, and does not contact your origin. Your origin is public either way. A host holds three waiting pointers; if the form says
		they are taken, the issue below works at once.
	</p>
	<form class="send" onsubmit={send} novalidate data-testid="pointer-form">
		<label class="field" for="pointer-origin">Your origin</label>
		<input id="pointer-origin" type="text" bind:value={origin} maxlength="253" spellcheck="false" autocomplete="off" autocapitalize="off" placeholder="data.example.org/river-levels" aria-invalid={problem.origin ? 'true' : undefined} aria-describedby={problem.origin ? 'pointer-origin-problem' : undefined} data-testid="pointer-origin" />
		{#if problem.origin}<p class="bad" id="pointer-origin-problem" data-testid="pointer-origin-problem">{problem.origin}</p>{/if}
		<label class="field" for="pointer-hash">The hash of your application file</label>
		<input id="pointer-hash" type="text" bind:value={application} maxlength="66" spellcheck="false" autocomplete="off" autocapitalize="off" placeholder="0x and 64 digits" aria-invalid={problem.application ? 'true' : undefined} aria-describedby={problem.application ? 'pointer-hash-problem' : undefined} data-testid="pointer-hash" />
		{#if problem.application}<p class="bad" id="pointer-hash-problem" data-testid="pointer-hash-problem">{problem.application}</p>{/if}
		<div><button type="submit" disabled={sending} data-testid="pointer-send">{sending ? 'Sending' : 'Send the pointer'}</button></div>
	</form>
	<div class="outcome" role="status" aria-live="polite" data-testid="pointer-outcome" data-kind={outcome?.kind ?? 'none'}>
		{#if outcome?.kind === 'receipt'}
			<p class="got">{outcome.again ? 'The node already had this pointer. Its receipt:' : 'Your receipt:'}</p>
			<p class="hash" data-testid="pointer-receipt">{outcome.receipt.id}</p>
			<p data-testid="pointer-state">{receiptWords(outcome.receipt)}</p>
			<p class="fine" data-testid="pointer-where">
				Keep the receipt: it shows whether the node still holds your pointer, for {RECEIPT_DAYS} days, at <span class="mono">/v1/publishers/pointer/</span> and the
				receipt on this site. The decision itself is not there. It appears in “Application status” at the foot of this page: look it up
				by the first digits of your application’s hash.
			</p>
		{:else if outcome}
			<p class="bad" data-testid="pointer-problem">{outcome.words}</p>
		{/if}
	</div>
</div>

<style>
	.pointer {
		display: grid;
		gap: var(--sp-3);
		min-width: 0;
	}
	.way {
		margin: 0;
		font-size: var(--text-base);
		font-weight: 600;
	}
	.send {
		display: grid;
		gap: var(--sp-2);
		min-width: 0;
	}
	.field {
		font-size: var(--text-sm);
		color: var(--muted);
	}
	.send input {
		min-width: 0;
		width: 100%;
		box-sizing: border-box;
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
	.send input:hover {
		border-color: var(--line-bright);
	}
	.send button {
		min-height: 44px;
		margin-top: var(--sp-2);
	}
	.outcome {
		display: grid;
		gap: var(--sp-2);
		min-width: 0;
	}
	.outcome:empty {
		display: none;
	}
	.bad {
		margin: 0;
		padding: var(--sp-3) var(--sp-4);
		border-radius: var(--radius-md);
		border: 1px solid var(--line-strong);
		background: var(--btc-soft);
		max-width: none;
	}
	.got {
		font-weight: 600;
	}
	.hash {
		font-family: var(--mono);
		overflow-wrap: anywhere;
		word-break: break-all;
	}
</style>
