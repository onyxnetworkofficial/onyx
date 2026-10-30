<script lang="ts">
	// ROADMAP S1 (ADR-0038): "Add data". Four steps: 1 Questions (five closed answers decide the proof level),
	// 2 Details, 3 Check (or Key, for a source behind the submitter's own key), 4 Send. The rules are
	// #lib/intake.ts, the same as the API's (crates/onyx-intake; both run fixtures/intake/vectors.json).
	// Offline builds have no intake API: steps 3 and 4 then say so and offer the submission as a JSON file; the page
	// sends nothing anywhere. A build made with VITE_ONYX_SUBMIT_API=1 (local dev with the submit-dev
	// server or configured node) posts to the same origin's /v1/submit. A typed API key never leaves this page: it is not part
	// of the submission object, never sent, never stored, and the page refuses a submission that contains it.
	import { onDestroy, onMount, tick } from 'svelte';
	import { probeSubmitApi, submitApi } from '#lib/submit-api.svelte.ts';
	import SubmissionReceipt from '#lib/components/SubmissionReceipt.svelte';
	import WantedData from '#lib/components/WantedData.svelte';
	import AddWithAgents from '#lib/components/AddWithAgents.svelte';
	import Downloads from '#lib/components/Downloads.svelte';
	import Bot from '@lucide/svelte/icons/bot';
	import { parseReceipt } from '#lib/submission-receipt.ts';
	import OnyxSlab from '#lib/components/OnyxSlab.svelte';
	import Pill, { type PillState } from '#lib/components/Pill.svelte';
	import {
		CLEARED_HOSTS,
		EXAMPLE,
		FRED_EXAMPLE,
		IntakeError,
		KEY_PLACEHOLDER,
		LEVEL_NAMES,
		NO_FETCH,
		REASONS,
		fetchPolicy,
		outgoing,
		parseSubmission,
		routeFor,
		stubProve,
		validateDataPayload,
		type Answers,
		type Changes,
		type Kind,
		type KeyProof,
		type Rights,
		type Route,
		type Submission,
		type DataPayload
	} from '#lib/intake.ts';

	/** Same-origin API only: a build that asks for it, or a release whose intake.json says so (MP2). */
	const API = $derived(submitApi.on);
	onMount(() => void probeSubmitApi());

	const STEP_NAMES = ['Questions', 'Details', 'Check', 'Send'];
	let step = $state(1);
	let reached = $state(1);

	type Draft = { what?: Kind; noLogin?: boolean; rights?: Rights; licenseId: string; namesPerson?: boolean; changes?: Changes };
	let a = $state<Draft>({ licenseId: '' });
	const blank = { title: '', description: '', sourceName: '', sourceUrl: '', endpointUrl: '', jsonPointer: '', unit: '', endpointTemplate: '', seriesId: '' };
	let d = $state({ ...blank });
	// The key: in this component's memory only, never in `a`, `d` or any submission object.
	let key = $state('');
	let pasted = $state('');
	let payloadPaste = $state('');
	let payloadFormat = $state<'csv' | 'json'>('csv');
	let dataPayload = $state.raw<DataPayload | null>(null);
	let payloadError = $state('');
	let payloadBusy = $state(false);
	let payloadRevision = 0;
	let proof = $state.raw<KeyProof | null>(null);
	let proofNote = $state('');
	let detailsError = $state('');
	let sendError = $state('');
	let sending = $state(false);
	let checkPill = $state<{ state: PillState; text: string }>({ state: 'idle', text: 'Not checked' });
	type ApiCheck = { preview: { value: string | null; valueStatus: string; bodyKeccak256: string; bytes: number } | null; fetchError: { code: string; message: string } | null; fetch: { pull: boolean; reason?: string; text?: string } };
	let apiCheck = $state.raw<ApiCheck | null>(null);
	let sent = $state.raw<{ id: string; status: string } | null>(null);
	let downloaded = $state('');
	let stepHead = $state<HTMLElement>();

	const keyed = $derived(a.what === 'keyed_api');
	const ownDataRoute = $derived(a.what === 'own' && a.rights === 'own_data');
	const names = $derived(STEP_NAMES.map((n, i) => (i === 2 && keyed ? 'Key' : n)));
	const complete = $derived(a.what !== undefined && a.noLogin !== undefined && a.rights !== undefined && a.namesPerson !== undefined && a.changes !== undefined);

	function answers(): Answers {
		const out: Answers = { what: a.what!, noLogin: a.what === 'keyed_api' ? false : a.noLogin!, rights: a.rights!, namesPerson: a.namesPerson!, changes: a.changes! };
		if (a.rights === 'open_license') out.licenseId = a.licenseId.trim();
		return out;
	}
	/** The submission: the answers and every non-empty field; for a keyed source the template and the proof. */
	function build(withProof = true): Submission {
		const s: Submission = { answers: answers(), title: d.title, description: d.description, sourceName: d.sourceName.trim(), sourceUrl: d.sourceUrl.trim() };
		if (ownDataRoute && dataPayload) s.dataPayload = dataPayload;
		const opt = (k: 'endpointUrl' | 'jsonPointer' | 'unit' | 'endpointTemplate' | 'seriesId', v: string) => {
			if (v.trim() !== '') s[k] = v.trim();
		};
		if (keyed) {
			opt('endpointTemplate', d.endpointTemplate);
			opt('seriesId', d.seriesId);
			if (withProof && proof) s.keyProof = proof;
		} else opt('endpointUrl', d.endpointUrl);
		opt('jsonPointer', d.jsonPointer);
		opt('unit', d.unit);
		return s;
	}
	const route = $derived<Route | null>(complete && (a.rights !== 'open_license' || a.licenseId.trim() !== '') ? routeFor(safeBuild()) : null);
	/** Whether the endpoint is on a rights-cleared host on an accepted level-2 route (a check pull is allowed). */
	const cleared = $derived(route !== null && fetchPolicy(safeBuild(), route) === 'pull');
	function safeBuild(): Submission {
		// The route of the answers plus any URL typed so far (a blocked host holds it).
		return build(false);
	}

	const KIND_OPTS: [Kind, string][] = [
		['public_api', 'A public web page or API'],
		['keyed_api', 'A public API that needs my own key'],
		['chain', 'Data from a public blockchain'],
		['signed', 'Data the source signs'],
		['own', 'My own or private data, or something else'],
		['forecast', 'A forecast']
	];
	const RIGHTS_OPTS: [Rights, string][] = [
		['public_domain', 'Public domain'],
		['open_license', 'An open licence'],
		['own_data', 'My data; I have permission to share it'],
		['not_sure', 'Not sure'],
		['terms_forbid', 'The terms forbid sharing it']
	];
	const CHANGE_OPTS: [Changes, string][] = [
		['once', 'Once'],
		['daily', 'About daily'],
		['live', 'All the time (live)']
	];

	async function go(n: number) {
		if (sending) return;
		step = n;
		reached = Math.max(reached, n);
		await tick();
		stepHead?.focus();
	}

	function fill(s: Submission) {
		if (sending) return;
		const x = s.answers;
		a = { what: x.what, noLogin: x.noLogin, rights: x.rights, licenseId: x.licenseId ?? '', namesPerson: x.namesPerson, changes: x.changes };
		d = { ...blank };
		for (const k of Object.keys(blank) as (keyof typeof blank)[]) if (typeof s[k] === 'string') d[k] = s[k] as string;
		key = '';
		pasted = '';
		payloadPaste = '';
		dataPayload = null;
		payloadError = '';
		proof = null;
		proofNote = '';
		apiCheck = null;
		sent = null;
		downloaded = '';
		detailsError = '';
		sendError = '';
		checkPill = { state: 'idle', text: 'Not checked' };
		reached = 1;
		go(1);
	}

	async function preparePayload(content: string, format: 'csv' | 'json') {
		const revision = ++payloadRevision;
		dataPayload = null;
		payloadError = '';
		payloadBusy = true;
		try {
			const utf8 = new TextEncoder().encode(content);
			if (new TextDecoder('utf-8', { fatal: true }).decode(utf8) !== content) throw new Error('The sample contains text that is not valid UTF-8.');
			const bytes = utf8.length;
			if (bytes > 8192) throw new Error('The sample is over 8,192 bytes. Choose a smaller sample.');
			const { verifier } = await import('#lib/verifier.ts');
			const hash = (await verifier.keccak(utf8)).toLowerCase();
			if (revision !== payloadRevision) return;
			const candidate: DataPayload = { format, content, bytes, keccak256: hash };
			validateDataPayload(candidate);
			dataPayload = candidate;
			payloadError = '';
		} catch (e) {
			if (revision !== payloadRevision) return;
			dataPayload = null;
			payloadError = e instanceof Error ? e.message : String(e);
		} finally {
			if (revision === payloadRevision) payloadBusy = false;
		}
	}

	function invalidatePayload() {
		payloadRevision++;
		dataPayload = null;
		payloadError = '';
		payloadBusy = false;
	}

	async function readPayloadFile(event: Event) {
		const input = event.currentTarget as HTMLInputElement;
		const revision = ++payloadRevision;
		payloadBusy = false;
		dataPayload = null;
		payloadError = '';
		payloadPaste = '';
		const file = input.files?.[0];
		if (!file) return;
		const ext = file.name.toLowerCase().split('.').pop();
		if (ext !== 'csv' && ext !== 'json') { payloadError = 'Choose a .csv or .json file.'; return; }
		if (file.size > 8192) { payloadError = 'The sample is over 8,192 bytes. Choose a smaller sample.'; input.value = ''; return; }
		payloadBusy = true;
		payloadFormat = ext;
		try {
			const content = new TextDecoder('utf-8', { fatal: true }).decode(await file.arrayBuffer());
			if (revision !== payloadRevision) return;
			payloadPaste = content;
		} catch {
			if (revision !== payloadRevision) return;
			payloadBusy = false;
			payloadError = 'This file is not valid UTF-8 text.';
			input.value = '';
			return;
		}
		await preparePayload(payloadPaste, ext);
	}

	function toDetails() {
		if (sending) return;
		if (route?.outcome === 'refused') return;
		go(2);
	}

	function checkDetails() {
		if (sending) return;
		detailsError = '';
		if (ownDataRoute && (payloadBusy || payloadError !== '' || (payloadPaste !== '' && dataPayload === null))) {
			detailsError = 'Wait for the sample check to finish, or remove the sample to continue without it.';
			return;
		}
		try {
			parseSubmission(build(false));
			proof = null;
			apiCheck = null;
			sent = null;
			go(3);
		} catch (e) {
			detailsError = e instanceof IntakeError ? e.message : String(e);
		}
	}

	async function apiCheckRun() {
		if (sending) return;
		checkPill = { state: 'run', text: 'Checking' };
		apiCheck = null;
		try {
			const r = await fetch(new URL('v1/submit/check', document.baseURI), { method: 'POST', headers: { 'content-type': 'application/json' }, body: outgoing(build(), key) });
			const b = await r.json();
			if (!r.ok) throw new Error(b?.error?.message ?? `HTTP ${r.status}`);
			apiCheck = b;
			checkPill = b.fetchError ? { state: 'warn', text: 'Valid; the pull failed' } : { state: 'good', text: 'Valid' };
		} catch (e) {
			checkPill = { state: 'bad', text: e instanceof Error ? e.message : String(e) };
		}
	}

	async function makeProof() {
		if (sending) return;
		proofNote = '';
		sendError = '';
		try {
			let hash: { keccak: string; bytes: number } | undefined;
			if (pasted.trim() !== '') {
				const bytes = new TextEncoder().encode(pasted);
				const { verifier } = await import('#lib/verifier.ts');
				hash = { keccak: (await verifier.keccak(bytes)).toLowerCase(), bytes: bytes.length };
			}
			const p = stubProve(key, d.endpointTemplate, hash);
			const s = build(false);
			s.keyProof = p;
			parseSubmission(s);
			outgoing(s, key);
			proof = p;
			pasted = '';
			proofNote = key
				? 'Made. The stub did not use your key. The key stays only in this page until you download, send or leave, so the page can refuse a submission that contains it.'
				: 'Made, with no key typed: the stub never uses one.';
		} catch (e) {
			proofNote = e instanceof Error ? e.message : String(e);
		}
	}

	function body(): string {
		const s = build();
		parseSubmission(s);
		return outgoing(s, key);
	}

	function download() {
		if (sending) return;
		sendError = '';
		try {
			const text = body() + '\n';
			const url = URL.createObjectURL(new Blob([text], { type: 'application/json' }));
			const link = document.createElement('a');
			link.href = url;
			link.download = 'onyx-submission.json';
			document.body.append(link);
			link.click();
			link.remove();
			setTimeout(() => URL.revokeObjectURL(url), 0);
			downloaded = 'Saved as onyx-submission.json. Nothing left your browser.';
			key = '';
		} catch (e) {
			sendError = e instanceof Error ? e.message : String(e);
		}
	}

	async function send() {
		if (sending || sent || !API) return;
		sendError = '';
		let payload: string;
		try { payload = body(); }
		catch (e) { sendError = e instanceof Error ? e.message : String(e); return; }
		sending = true;
		try {
			const r = await fetch(new URL('v1/submit', document.baseURI), { method: 'POST', headers: { 'content-type': 'application/json' }, body: payload, signal: AbortSignal.timeout(15000) });
			const b = await r.json();
			if (!r.ok) throw new Error(b?.error?.message ?? `HTTP ${r.status}`);
			if (r.status !== 201) throw new Error('Unexpected receipt response.');
			sent = parseReceipt(b);
			key = '';
		} catch (e) {
			sendError = `${e instanceof Error ? e.message : String(e)} Delivery could not be confirmed. Keep any receipt you received; sending again may create another proposal.`;
		} finally { sending = false; }
	}

	const STATUS_TEXT: Record<string, string> = {
		pending_review: 'Pending review: a reviewer reads it before anything is published.',
		held_rights_review: 'Held for a rights review: not published until a reviewer clears the rights.',
		held_keyed_source_review: 'Held: a keyed source, waiting for a rights review of its terms. Not published.',
		refused: 'Refused: it cannot be taken.'
	};
	const preview = $derived.by(() => {
		if (step < 3 || !route) return null;
		try {
			const s = build();
			parseSubmission(s);
			return { json: JSON.stringify(s, null, 2), fetch: fetchPolicy(s, route) };
		} catch {
			return null;
		}
	});
	const blockedNote = $derived(route?.reasons.includes('source_blocked') ?? false);
	const fred = $derived(blockedNote && /stlouisfed\.org/.test(`${d.sourceUrl} ${d.endpointTemplate} ${d.endpointUrl}`));

	onDestroy(() => {
		key = '';
	});

	// MP1: the AI and API steps open under the page title and take the focus.
	let agentsOpen = $state(false);
	async function openAgents() {
		agentsOpen = !agentsOpen;
		if (!agentsOpen) return;
		await tick();
		const h = document.getElementById('agents-title');
		h?.scrollIntoView({ behavior: matchMedia('(prefers-reduced-motion: reduce)').matches ? 'auto' : 'smooth', block: 'start' });
		h?.focus({ preventScroll: true });
	}
</script>

<svelte:head><title>Add data · Onyx</title></svelte:head>

<OnyxSlab>
	<h1>Add data</h1>
	<div class="slabside" data-testid="slabside">
		<p class="slablede">Tell Onyx about data you know. Five questions decide how strongly it can be checked.</p>
		<p class="slablede">Nothing is published without a review. Data about people is never taken.</p>
		<p class="slabactions">
			<button type="button" class="agents-btn" aria-expanded={agentsOpen} aria-controls="add-with-agents" onclick={openAgents} data-testid="open-agents"
				><Bot size={18} /> Add data with AI or the API</button
			>
		</p>
	</div>
</OnyxSlab>

<div class="wrap">
	<AddWithAgents bind:open={agentsOpen} />
	<section class="section" data-testid="submit" inert={sending} aria-busy={sending}>
		<h2>Four steps</h2>
		<p class="lede">
			Answer the questions, describe the data, check it, then send it for review. Not sure where to start? Try the U.S. Treasury
			example: public data anyone can fetch, so Onyx can pull it itself.
		</p>
		<div class="row">
			<button type="button" onclick={() => fill(EXAMPLE)} disabled={sending} data-testid="try-example">Try an example</button>
			<button type="button" class="ghost" onclick={() => fill(FRED_EXAMPLE)} disabled={sending} data-testid="try-keyed">Try a source that needs a key (FRED)</button>
		</div>

		<ol class="steps" aria-label="Steps" data-testid="stepper">
			{#each names as n, i (i)}
				<li class:on={step === i + 1} class:done={reached > i + 1 || step > i + 1}>
					<button type="button" class="ghost stepbtn" aria-current={step === i + 1 ? 'step' : undefined} disabled={i + 1 > reached || sending} onclick={() => go(i + 1)}
						><span class="num" aria-hidden="true">{i + 1}</span>{n}</button
					>
				</li>
			{/each}
		</ol>

		<div class="panel stepbox" data-testid="step-{step}">
			<h3 tabindex="-1" bind:this={stepHead} class="ititle">{step}. {names[step - 1]}</h3>

			{#if step === 1}
				<div class="q" role="group" aria-labelledby="q1">
					<p id="q1" class="qt">What is it?</p>
					<div class="choices">
						{#each KIND_OPTS as [k, label] (k)}
							<button type="button" class="ghost opt" aria-pressed={a.what === k} onclick={() => (a.what = k)} data-testid="q-what-{k}">{label}</button>
						{/each}
					</div>
				</div>
				<div class="q" role="group" aria-labelledby="q2">
					<p id="q2" class="qt">Can anyone fetch it with no login, key or payment?</p>
					{#if keyed}
						<p class="fine">No: it needs your own key. Your key will stay in your browser (step 3).</p>
					{:else}
						<div class="choices">
							<button type="button" class="ghost opt" aria-pressed={a.noLogin === true} onclick={() => (a.noLogin = true)} data-testid="q-nologin-yes">Yes</button>
							<button type="button" class="ghost opt" aria-pressed={a.noLogin === false} onclick={() => (a.noLogin = false)} data-testid="q-nologin-no">No</button>
						</div>
					{/if}
				</div>
				<div class="q" role="group" aria-labelledby="q3">
					<p id="q3" class="qt">What are the rights?</p>
					<div class="choices">
						{#each RIGHTS_OPTS as [r, label] (r)}
							<button type="button" class="ghost opt" aria-pressed={a.rights === r} onclick={() => (a.rights = r)} data-testid="q-rights-{r}">{label}</button>
						{/each}
					</div>
					{#if a.rights === 'open_license'}
						<label class="field"
							><span>Licence id (for example CC-BY-4.0)</span><input type="text" bind:value={a.licenseId} maxlength="64" spellcheck="false" autocomplete="off" data-testid="q-license" /></label
						>
					{/if}
				</div>
				<div class="q" role="group" aria-labelledby="q4">
					<p id="q4" class="qt">Does it name a person?</p>
					<div class="choices">
						<button type="button" class="ghost opt" aria-pressed={a.namesPerson === false} onclick={() => (a.namesPerson = false)} data-testid="q-person-no">No, companies or things only</button>
						<button type="button" class="ghost opt" aria-pressed={a.namesPerson === true} onclick={() => (a.namesPerson = true)} data-testid="q-person-yes">Yes</button>
					</div>
				</div>
				<div class="q" role="group" aria-labelledby="q5">
					<p id="q5" class="qt">How often does it change?</p>
					<div class="choices">
						{#each CHANGE_OPTS as [c, label] (c)}
							<button type="button" class="ghost opt" aria-pressed={a.changes === c} onclick={() => (a.changes = c)} data-testid="q-changes-{c}">{label}</button>
						{/each}
					</div>
				</div>
			{:else if step === 2}
				<div class="fields">
					<label class="field"><span>Title</span><input type="text" bind:value={d.title} data-testid="f-title" /></label>
					<label class="field"><span>What it is and what it means</span><textarea rows="4" bind:value={d.description} data-testid="f-description"></textarea></label>
					<label class="field"><span>Who publishes it</span><input type="text" bind:value={d.sourceName} data-testid="f-source-name" /></label>
					<label class="field"><span>{ownDataRoute ? 'A public page about it (optional)' : 'Where a person can read about it (https)'}</span><input type="url" bind:value={d.sourceUrl} spellcheck="false" required={!ownDataRoute} data-testid="f-source-url" /></label>
					{#if ownDataRoute}
						<section class="payload" aria-labelledby="payload-heading" data-testid="own-data-payload">
							<h2 id="payload-heading">Add a small data sample (optional)</h2>
							<p>Only add data you have permission to share. This private proposal is reviewed before use. Choose a CSV or JSON file up to 8 KiB, or paste a sample. Nothing runs in your browser.</p>
							<label class="field"><span>Choose a .csv or .json file (8 KiB maximum)</span><input type="file" accept=".csv,.json,text/csv,application/json" onchange={readPayloadFile} data-testid="payload-file" /></label>
							<label class="field"><span>Or paste a sample</span><textarea rows="5" bind:value={payloadPaste} oninput={invalidatePayload} spellcheck="false" data-testid="payload-paste"></textarea></label>
							<label class="field"><span>Paste format</span><select bind:value={payloadFormat} onchange={invalidatePayload} data-testid="payload-format"><option value="csv">CSV</option><option value="json">JSON</option></select></label>
							<div class="row"><button type="button" onclick={() => preparePayload(payloadPaste, payloadFormat)} data-testid="payload-preview">Validate and preview</button><button type="button" class="ghost" onclick={() => { invalidatePayload(); payloadPaste = ''; }} data-testid="payload-clear">Remove sample</button></div>
							{#if payloadBusy}<p role="status" data-testid="payload-checking">Checking sample…</p>{/if}
							{#if payloadError}<p class="err" role="alert" data-testid="payload-error">{payloadError}</p>{/if}
							{#if dataPayload}<p role="status" data-testid="payload-ready">Sample checked: {dataPayload.format.toUpperCase()}, {dataPayload.bytes.toLocaleString('en-US')} bytes. Digest: <code>{dataPayload.keccak256}</code></p><details class="more"><summary>Preview the exact sample</summary><pre class="code">{dataPayload.content}</pre></details>{/if}
						</section>
					{/if}
					{#if keyed}
						<label class="field"
							><span>Series id (for example CPIAUCSL)</span><input type="text" bind:value={d.seriesId} spellcheck="false" autocomplete="off" data-testid="f-series" /></label
						>
						<label class="field"
							><span>Request with the key left out: write <code>{KEY_PLACEHOLDER}</code> where your key goes</span><input
								type="text"
								bind:value={d.endpointTemplate}
								spellcheck="false"
								autocomplete="off"
								data-testid="f-template"
							/></label
						>
					{:else}
						<label class="field"
							><span>Where a machine can fetch it (https, optional)</span><input type="url" bind:value={d.endpointUrl} spellcheck="false" data-testid="f-endpoint" /></label
						>
					{/if}
					<label class="field"><span>Where the value sits in the JSON (a JSON pointer, optional)</span><input type="text" bind:value={d.jsonPointer} spellcheck="false" data-testid="f-pointer" /></label>
					<label class="field"><span>Unit (optional)</span><input type="text" bind:value={d.unit} data-testid="f-unit" /></label>

				</div>
				<p class="fine">No e-mail, phone or name is asked for or taken. Text holding an e-mail address is refused.</p>
				{#if detailsError}<p class="err" role="alert" data-testid="details-error">{detailsError}</p>{/if}
			{:else if step === 3 && keyed}
				<div class="keybox" data-testid="key-step">
					<p>
						Your key stays in this page. It is never sent to Onyx, never written into the submission or the file, and never stored. A real
						prover (ROADMAP S2) would make the request from your side and send Onyx only a proof of the response.
					</p>
					<p class="stubnote" data-testid="stub-label"><strong>Proof: not yet real (S2).</strong> This build's prover is a stub. It makes no request and proves nothing.</p>
					<p class="fine" data-testid="tlsn-progress">Progress: a real TLSNotary proof of the U.S. Treasury API, with one request header hidden, now verifies offline in the repository (experiments/tlsn-fiscaldata). Running the prover in your browser is the next step.</p>
					<label class="field"><span>Your API key (stays in this page)</span><input type="password" bind:value={key} autocomplete="new-password" spellcheck="false" data-testid="key-input" /></label>
					<p class="fine">Your browser may offer to save this field as a password; there is no need to.</p>
					<label class="field"
						><span>Optional: a response you fetched yourself. Only its Keccak-256 and length go into the submission.</span><textarea rows="3" bind:value={pasted} spellcheck="false" data-testid="pasted"></textarea></label
					>
					<div class="row">
						<button type="button" onclick={makeProof} data-testid="make-proof">Make the proof (stub)</button>
						{#if proofNote}<span class="fine" role="status" data-testid="proof-note">{proofNote}</span>{/if}
					</div>
					{#if proof}
						<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
						<pre class="code" data-testid="proof-json" tabindex="0" role="region" aria-label="The stub proof">{JSON.stringify(proof, null, 2)}</pre>
					{/if}
				</div>
			{:else if step === 3}
				{#if route}
					<p data-testid="check-local">Your browser ran the checks: the details are valid. These are the same rules the server runs.</p>
					{#if preview}
						<p class="fine" data-testid="check-fetch">
							{#if preview.fetch === 'pull'}Onyx may pull this endpoint once to check it: {CLEARED_HOSTS.join(', ')} is a source whose rights are already reviewed.{:else}{NO_FETCH[preview.fetch]}{/if}
						</p>
					{/if}
				{/if}
				{#if API}
					<div class="row">
						<button type="button" onclick={apiCheckRun} data-testid="api-check">Check with the API</button>
						<Pill live state={checkPill.state} text={checkPill.text} />
					</div>
					{#if apiCheck?.preview}
						<dl class="kv" data-testid="api-preview">
							<dt>Value at the pointer</dt><dd class="mono">{apiCheck.preview.value ?? apiCheck.preview.valueStatus}</dd>
							<dt>Body Keccak-256</dt><dd class="mono hash">{apiCheck.preview.bodyKeccak256}</dd>
							<dt>Body size</dt><dd class="mono">{apiCheck.preview.bytes.toLocaleString('en-US')} bytes</dd>
						</dl>
					{:else if apiCheck?.fetchError}
						<p class="fine">The pull failed: {apiCheck.fetchError.message}</p>
					{/if}
				{:else}
					<p class="offline" data-testid="api-offline">The API is not connected in this build, so nothing was fetched or sent. Step 4 saves your submission as a file instead.</p>
				{/if}
			{:else if step === 4}
				{#if route}
					<p data-testid="send-status"><strong>{STATUS_TEXT[route.status]}</strong></p>
				{/if}
				{#if fred}
					<p class="offline" data-testid="fred-note">
						FRED's terms restrict redistributing its data, and Onyx's rights review blocks FRED as a source. This submission stays held for a
						rights review and is not published.
					</p>
				{:else if blockedNote}
					<p class="offline">Onyx's rights review blocks this source. It stays held and is not published.</p>
				{/if}
				{#if keyed && !proof}
					<p class="fine" data-testid="no-proof">No proof was made (step 3), so the submission carries none.</p>
				{/if}
				{#if preview}
					<details class="more">
						<summary>Show the exact JSON</summary>
						<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
						<pre class="code" data-testid="send-json" tabindex="0" role="region" aria-label="The submission as JSON">{preview.json}</pre>
					</details>
				{/if}
				{#if API}
					<div class="row">
						<button type="button" onclick={send} disabled={sent !== null || sending} data-testid="send">{sending ? 'Sending…' : 'Send for review'}</button>
						<button type="button" class="ghost" onclick={download} data-testid="download">Download submission (JSON)</button>
					</div>
					{#if sent}<p role="status" data-testid="sent">Received: <span class="mono">{sent.id}</span>. {STATUS_TEXT[sent.status] ?? sent.status}</p>{/if}
				{:else}
					<p class="offline" data-testid="send-offline">The API is not connected in this build, so this page cannot send anything. Download the submission instead; nothing leaves your browser.</p>
					<div class="row">
						<button type="button" onclick={download} data-testid="download">Download submission (JSON)</button>
					</div>
				{/if}
				{#if downloaded}<p class="fine" role="status" data-testid="downloaded">{downloaded}</p>{/if}
				{#if sendError}<p class="err" role="alert" data-testid="send-error">{sendError}</p>{/if}
			{/if}

			{#if route && step < 4}
				<div class="route" data-testid="route" data-outcome={route.outcome} data-level={route.level ?? ''}>
					<p class="rhead">
						{#if route.outcome === 'refused'}
							<Pill state="bad" text="Refused" />
						{:else}
							{#if route.level === 2 && !cleared}
								<!-- S1-R1-6: level 2 needs a rights-cleared source and observer rounds (S4): not shown as reached. -->
								<Pill state="warn" text="Level 2 possible after rights review" />
							{:else}
								<Pill state={route.outcome === 'held' ? 'warn' : 'good'} text="Level {route.level}: {LEVEL_NAMES[route.level ?? 0]}" />
							{/if}
							{#if route.outcome === 'held'}<Pill state="warn" text="Held" />{/if}
						{/if}
					</p>
					<ul class="reasons" data-testid="reasons">
						{#each route.reasons as r (r)}<li data-reason={r}>{REASONS[r]}</li>{/each}
					</ul>
				</div>
			{/if}

			<div class="nav row">
				{#if step > 1}<button type="button" class="ghost" onclick={() => go(step - 1)} disabled={sending} data-testid="back">Back</button>{/if}
				{#if step === 1}
					<button type="button" onclick={toDetails} disabled={!route || route.outcome === 'refused' || sending} data-testid="next">Next: details</button>
				{:else if step === 2}
					<button type="button" onclick={checkDetails} disabled={sending} data-testid="next">Next: {keyed ? 'your key' : 'check'}</button>
				{:else if step === 3}
					<button type="button" onclick={() => go(4)} disabled={sending} data-testid="next">Next: send</button>
				{/if}
			</div>
		</div>
	</section>

	<SubmissionReceipt received={sent} />

	<WantedData />

	<Downloads />

	<section class="section side" data-testid="submit-levels">
		<h2>What the levels mean</h2>
		<div class="levelsbox">
		<ol class="levels">
			<li><b>Level 1</b><span>Anyone recomputes it from public chain data.</span></li>
			<li><b>Level 2</b><span>Several bonded observers fetch it at the same time and must agree.</span></li>
			<li><b>Level 3</b><span>The source signs it.</span></li>
			<li><b>Level 4</b><span>A claim nobody has checked, labelled unverified.</span></li>
			<li><b>Level 5</b><span>A forecast, scored later.</span></li>
		</ol>
		<p class="planned">Staking on your data being right is planned, not built.</p>
		</div>
	</section>
</div>

<style>
	.slabactions {
		margin: var(--sp-5) 0 0;
	}
	.agents-btn {
		gap: var(--sp-2);
	}
	.steps {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-2);
	}
	.stepbtn {
		gap: var(--sp-2);
	}
	.steps li.on .stepbtn {
		border-color: var(--accent);
		color: var(--ink);
	}
	.num {
		font-family: var(--mono);
		font-size: var(--text-xs);
		color: var(--muted);
	}
	.steps li.done .num {
		color: var(--good);
	}
	.stepbox h3 {
		margin: 0;
	}
	.stepbox h3:focus {
		outline: none;
	}
	.q {
		display: grid;
		gap: var(--sp-2);
	}
	.qt {
		margin: 0;
		font-weight: 600;
	}
	.choices {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-2);
	}
	.opt[aria-pressed='true'],
	.opt[aria-pressed='true']:hover:not(:disabled) {
		background: var(--accent);
		border-color: var(--accent);
		color: var(--accent-ink);
	}
	.fields {
		display: grid;
		gap: var(--sp-3);
	}
	.field {
		display: grid;
		gap: var(--sp-1);
		font-size: var(--text-sm);
		color: var(--muted);
	}
	.field input,
	.field textarea,
	.field select {
		font: inherit;
		font-size: var(--text-field);
		line-height: var(--lh-snug);
		color: var(--ink);
		background: var(--panel-solid);
		border: 1px solid var(--line-strong);
		border-radius: var(--radius-sm);
		padding: var(--sp-2) var(--sp-3);
		margin: 0;
		min-width: 0;
		width: 100%;
		transition: border-color var(--dur) var(--ease);
	}
	.field input:hover,
	.field textarea:hover,
	.field select:hover {
		border-color: var(--line-bright);
	}
	.field textarea {
		resize: vertical;
	}
	.payload {
		display: grid;
		gap: var(--sp-3);
		border: 1px solid var(--line-strong);
		border-radius: var(--radius-md);
		padding: var(--sp-4);
		background: var(--panel);
	}
	.payload h2, .payload p { margin: 0; }
	.payload p { max-width: 70ch; line-height: var(--lh-body); }
	.payload code { overflow-wrap: anywhere; }
	.field code {
		overflow-wrap: anywhere;
	}
	@media (max-width: 760px) {
		.field input {
			min-height: 44px;
		}
		.field select { min-height: 44px; }
		.field textarea {
			min-height: 88px;
		}
	}
	.route {
		display: grid;
		gap: var(--sp-2);
		padding-top: var(--sp-4);
		border-top: 1px solid var(--line);
	}
	.rhead {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-2);
		margin: 0;
	}
	.reasons {
		margin: 0;
		padding-left: var(--sp-5);
		display: grid;
		gap: var(--sp-1);
		line-height: var(--lh-body);
		max-width: 68ch;
	}
	.err {
		color: var(--bad);
		margin: 0;
	}
	.offline,
	.stubnote {
		margin: 0;
		padding: var(--sp-3) var(--sp-4);
		border-radius: var(--radius-md);
		border: 1px solid var(--line-strong);
		background: var(--btc-soft);
		line-height: var(--lh-body);
	}
	.keybox {
		display: grid;
		gap: var(--sp-3);
	}
	.keybox > p {
		margin: 0;
		line-height: var(--lh-body);
		max-width: 68ch;
	}
	pre.code {
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		overflow-x: hidden;
	}
	.nav {
		padding-top: var(--sp-2);
	}
	.levels {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		gap: var(--sp-2);
		max-width: 68ch;
	}
	.levels li {
		display: grid;
		grid-template-columns: 6rem minmax(0, 1fr);
		gap: var(--sp-3);
		padding-block: var(--sp-2);
		border-top: 1px solid var(--line);
		line-height: var(--lh-body);
	}
	.levelsbox {
		display: grid;
		gap: var(--sp-3);
	}
	.levelsbox .planned {
		color: var(--muted);
		font-size: var(--text-sm);
	}
	.levels b {
		font: 500 var(--text-sm) / var(--lh-body) var(--mono);
		color: var(--accent);
	}
	@media (max-width: 520px) {
		.steps {
			display: grid;
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
		.stepbtn {
			width: 100%;
			justify-content: flex-start;
		}
	}
	.side p {
		margin: 0;
		line-height: var(--lh-body);
		max-width: 68ch;
	}
</style>
