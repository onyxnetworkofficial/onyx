// The contributor app's page (CL10). Plain JavaScript, no outside files. Every text from a
// provider or from Onyx is put on the page with textContent (never as HTML). A key typed here is
// sent once to this computer's onyx-contrib, which stores it in the keychain; the field is then
// emptied and the key is never shown again (the server never sends it back).
'use strict';

const $ = (id) => document.getElementById(id);
let token = '';
try {
	const m = /^#t=([0-9a-f]{64})$/.exec(location.hash);
	if (m) {
		token = m[1];
		sessionStorage.setItem('onyx-t', token);
		history.replaceState(null, '', location.pathname);
	} else token = sessionStorage.getItem('onyx-t') || '';
} catch {}

function el(tag, attrs = {}, ...kids) {
	const e = document.createElement(tag);
	for (const [k, v] of Object.entries(attrs)) {
		if (k === 'class') e.className = v;
		else if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
		else if (v === true) e.setAttribute(k, '');
		else if (v !== false && v !== undefined && v !== null) e.setAttribute(k, String(v));
	}
	for (const k of kids.flat()) if (k !== null && k !== undefined && k !== false) e.append(k instanceof Node ? k : document.createTextNode(String(k)));
	return e;
}

function say(text, kind = '') {
	const s = $('status');
	s.textContent = text;
	s.className = `banner ${kind}`;
	s.hidden = !text;
}

async function api(method, path, body) {
	const init = { method, headers: { 'x-onyx-token': token }, credentials: 'omit', cache: 'no-store' };
	if (body !== undefined) {
		init.headers['content-type'] = 'application/json';
		init.body = JSON.stringify(body);
	}
	const res = await fetch(path, init);
	let v;
	try {
		v = await res.json();
	} catch {
		v = { ok: false, error: `the app answered ${res.status}` };
	}
	if (v.ok === false) {
		const e = new Error(typeof v.error === 'string' ? v.error : JSON.stringify(v.error ?? v.check ?? v));
		e.body = v;
		throw e;
	}
	return v;
}

const fail = (e) => say(`Not done: ${e.message}`, 'bad');

// 1. Health.
async function loadDoctor() {
	const d = await api('GET', '/api/doctor');
	const ul = $('checks');
	ul.replaceChildren(...d.checks.map((c) => el('li', { class: c.ok ? '' : 'bad' }, el('span', { class: c.ok ? 'okw' : 'badw' }, c.ok ? 'OK ' : 'To fix: '), c.say, c.fix ? el('span', { class: 'fix' }, 'What to do: ', c.fix) : null)));
	const vault = d.checks.find((c) => c.id === 'vault');
	if (vault && !vault.ok) $('filevault').open = true;
}

// 2. Providers.
let current = null;
async function loadState() {
	const s = await api('GET', '/api/state');
	$('ver').textContent = `onyx-contrib ${s.version} · ${s.network} · Onyx: ${s.onyx} · keys in: ${s.vault}`;
	const ul = $('providers');
	if (!s.providers.length) ul.replaceChildren(el('li', { class: 'muted' }, 'No provider yet. Add the API you have a key for below.'));
	else ul.replaceChildren(...s.providers.map(providerCard));
	$('addbox').open = !s.providers.length;
	const c = s.claude;
	$('claude-state').textContent = c.current ? `Connected: Claude Desktop's settings (${c.path}) start this program.` : c.connected ? `Claude Desktop has an older Onyx entry (${c.path}); connect again to update it.` : c.invalid ? `Claude Desktop's settings file (${c.path}) is not valid JSON; fix it in Claude Desktop first.` : c.found ? `Not connected yet (${c.path}).` : `Claude Desktop's settings file was not found (${c.path}). Install Claude Desktop first, or skip this step.`;
}

function providerCard(p) {
	const keyForm = el(
		'form',
		{ class: 'row', onsubmit: (ev) => storeKey(ev, p.name) },
		el('label', { class: 'grow' }, `${p.keyStored ? 'Replace the key' : 'Paste your key'} (stored in your keychain, never shown again)`, el('input', { name: 'key', type: 'password', autocomplete: 'off', spellcheck: 'false', required: true, minlength: 12 })),
		el('button', { type: 'submit' }, 'Store key'),
	);
	return el(
		'li',
		{},
		el('b', {}, p.name),
		el('span', { class: 'muted' }, `${p.baseUrl} · key sent as ${p.keySentAs}, only to this host`),
		el('span', { class: p.keyStored ? 'okw' : 'badw' }, p.keyStored ? 'Key stored' : 'No key stored yet'),
		keyForm,
		el(
			'div',
			{ class: 'actions' },
			el('button', { type: 'button', class: 'quiet', disabled: !p.openapi || !p.keyStored, onclick: () => explore(p.name) }, 'Endpoints'),
			p.keyStored ? el('button', { type: 'button', class: 'quiet', onclick: (ev) => twice(ev.target, () => removeKey(p.name)) }, 'Remove key') : null,
			el('button', { type: 'button', class: 'quiet', onclick: (ev) => twice(ev.target, () => removeProvider(p.name)) }, 'Remove provider'),
		),
		!p.openapi ? el('span', { class: 'muted' }, 'No OpenAPI file: remove and add the provider again with one to list its endpoints.') : null,
	);
}

async function storeKey(ev, name) {
	ev.preventDefault();
	const input = ev.target.elements.key;
	const key = input.value;
	input.value = '';
	try {
		const r = await api('POST', '/api/key', { provider: name, key });
		say(`Key stored for ${r.provider} in ${r.vault}. It will be sent only to ${r.sentTo}.`, 'ok');
		await loadState();
	} catch (e) {
		fail(e);
	}
}

/** A removal needs a second press on the same button (no pop-up dialogs: desktop web views differ). */
function twice(btn, action) {
	if (btn.dataset.armed === '1') return action();
	btn.dataset.armed = '1';
	const label = btn.textContent;
	btn.textContent = `${label}: press again to confirm`;
	setTimeout(() => {
		btn.dataset.armed = '';
		btn.textContent = label;
	}, 5000);
}

async function removeKey(name) {
	try {
		await api('POST', '/api/key/remove', { provider: name });
		say(`Key for ${name} removed.`);
		await loadState();
	} catch (e) {
		fail(e);
	}
}

async function removeProvider(name) {
	try {
		await api('POST', '/api/providers/remove', { name });
		say(`${name} removed.`);
		if (current?.provider === name) $('explore').hidden = true;
		await loadState();
	} catch (e) {
		fail(e);
	}
}

$('f-add').addEventListener('submit', async (ev) => {
	ev.preventDefault();
	const f = ev.target.elements;
	try {
		const r = await api('POST', '/api/providers', { name: f.name.value.trim(), baseUrl: f.baseUrl.value.trim(), authIn: f.authIn.value, authName: f.authName.value.trim(), bearer: f.bearer.checked, openapi: f.openapi.value.trim(), docsUrl: f.docsUrl.value.trim() });
		ev.target.reset();
		say(`Added ${r.provider.name}. Now paste its key in its card.`, 'ok');
		await loadState();
	} catch (e) {
		fail(e);
	}
});

$('f-file').addEventListener('submit', async (ev) => {
	ev.preventDefault();
	const input = ev.target.elements.passphrase;
	const passphrase = input.value;
	input.value = '';
	try {
		const r = await api('POST', '/api/vault/file', { passphrase });
		say(`Keys now go in ${r.vault} for as long as the app runs.`, 'ok');
		await Promise.all([loadDoctor(), loadState()]);
	} catch (e) {
		fail(e);
	}
});

// 3. Endpoints, preview, check, confirm.
async function explore(name, search = '') {
	current = { provider: name };
	$('explore').hidden = false;
	$('ex-provider').textContent = name;
	for (const id of ['f-preview', 'preview', 'f-check', 'confirm']) $(id).hidden = true;
	try {
		const r = await api('GET', `/api/endpoints?provider=${encodeURIComponent(name)}${search ? `&search=${encodeURIComponent(search)}` : ''}`);
		$('endpoints').replaceChildren(...(r.endpoints.length ? r.endpoints.map((e) => el('li', {}, el('code', {}, e.path), el('span', { class: 'muted' }, e.summary || ''), el('button', { type: 'button', class: 'quiet', onclick: () => pickEndpoint(e) }, 'Use this endpoint'))) : [el('li', { class: 'muted' }, 'No GET endpoint matches.')]), r.more ? el('li', { class: 'muted' }, `${r.endpoints.length} of ${r.total} shown: search to narrow.`) : []);
		$('explore').scrollIntoView({ block: 'start' });
	} catch (e) {
		fail(e);
	}
}

$('f-search').addEventListener('submit', (ev) => {
	ev.preventDefault();
	if (current) explore(current.provider, ev.target.elements.search.value.trim());
});

function pickEndpoint(e) {
	current = { provider: current.provider, path: e.path, params: e.params };
	$('ep-title').textContent = e.path;
	$('ep-params').replaceChildren(...e.params.map((p, i) => el('label', {}, `${p.name}${p.required ? ' (required)' : ''}${p.description ? `: ${p.description}` : ''}`, el('input', { 'data-param': p.name, 'data-in': p.in, required: p.required, placeholder: p.example ?? (p.enum ? p.enum.join(', ') : ''), autocomplete: 'off', id: `prm-${i}` }))));
	$('f-preview').hidden = false;
	for (const id of ['preview', 'f-check', 'confirm']) $(id).hidden = true;
	$('f-preview').scrollIntoView({ block: 'start' });
}

function readParams() {
	const params = {};
	for (const i of $('ep-params').querySelectorAll('input')) if (i.value.trim() !== '') params[i.dataset.param] = i.value.trim();
	return params;
}

$('f-preview').addEventListener('submit', async (ev) => {
	ev.preventDefault();
	const pointer = ev.target.elements.pointer.value.trim();
	current.params = readParams();
	current.pointer = pointer || undefined;
	try {
		const r = await api('POST', '/api/preview', { provider: current.provider, path: current.path, params: current.params, pointer: current.pointer });
		$('preview').hidden = false;
		$('pv-value').textContent = JSON.stringify(r.missing ? r.missing : r.value !== undefined ? r.value : r.answer, null, 1);
		$('pv-note').textContent = `${r.endpoint} · ${r.bytes} bytes${r.keyRemovedFromAnswer ? ' · the provider echoed your key: it was removed' : ''}. Pick where the value is:`;
		$('pv-pointers').replaceChildren(...r.pointers.map((p) => el('button', { type: 'button', class: 'quiet', onclick: () => { ev.target.elements.pointer.value = p; ev.target.requestSubmit(); } }, p)));
		$('f-check').hidden = !(r.value !== undefined && current.pointer);
		$('confirm').hidden = true;
	} catch (e) {
		fail(e);
	}
});

let checkId = null;
$('f-check').addEventListener('submit', async (ev) => {
	ev.preventDefault();
	const f = ev.target.elements;
	const a = { provider: current.provider, path: current.path, params: current.params, pointer: current.pointer, title: f.title.value.trim(), description: f.description.value.trim(), rights: f.rights.value, changes: f.changes.value, namesPerson: f.namesPerson.checked, unit: f.unit.value.trim(), licenseId: f.rights.value === 'open_license' ? f.licenseId.value.trim() : '' };
	try {
		const r = await api('POST', '/api/contribute', a);
		checkId = r.checkId;
		$('willsend').textContent = JSON.stringify(r.willSend, null, 1);
		$('agree').checked = false;
		$('send').disabled = true;
		$('confirm').hidden = false;
		say('Onyx checked it and recorded nothing. Read what will be sent below.');
		$('confirm').scrollIntoView({ block: 'start' });
	} catch (e) {
		const c = e.body?.check;
		fail(c ? new Error(`Onyx's check: ${JSON.stringify(c.route?.reasons ?? c.errors ?? c)}`) : e);
	}
});

$('agree').addEventListener('change', (ev) => {
	$('send').disabled = !ev.target.checked || !checkId;
});

$('send').addEventListener('click', async () => {
	if (!$('agree').checked || !checkId) return;
	$('send').disabled = true;
	try {
		const r = await api('POST', '/api/contribute', { checkId, confirm: true });
		checkId = null;
		$('confirm').hidden = true;
		say(`Sent. Receipt ${r.receipt.id}: ${r.receipt.status ?? 'received'}. It is held for a review of the provider's terms before anything is published.`, 'ok');
		await loadHistory();
	} catch (e) {
		fail(e);
	}
});

// 4. Past contributions.
async function loadHistory() {
	const r = await api('GET', '/api/contributions');
	$('history').replaceChildren(...(r.contributions.length ? r.contributions.map(historyItem) : [el('li', { class: 'muted' }, 'Nothing sent from this computer yet.')]));
}

function historyItem(c) {
	const st = el('span', { class: 'muted' }, c.status ? `Status when sent: ${c.status}` : '');
	return el(
		'li',
		{},
		el('b', {}, c.title || c.id),
		el('span', { class: 'muted' }, `${c.sentAt} · ${c.provider} · ${c.endpoint ?? ''} ${c.pointer ?? ''}`),
		el('code', {}, c.id),
		st,
		el(
			'button',
			{
				type: 'button',
				class: 'quiet',
				onclick: async () => {
					try {
						const r = await api('GET', `/api/contributions/status?id=${encodeURIComponent(c.id)}`);
						st.textContent = `Status now: ${r.status?.status ?? JSON.stringify(r.status)}`;
					} catch (e) {
						st.textContent = `Status: ${e.message}`;
					}
				},
			},
			'Check status',
		),
	);
}

// 5. Claude Desktop.
$('claude').addEventListener('click', async () => {
	try {
		const r = await api('POST', '/api/claude/connect', {});
		say(r.changed ? `Done: Onyx was added to ${r.path}${r.backup ? ` (old file kept as ${r.backup})` : ''}. ${r.next}` : `Already connected (${r.path}). ${r.next}`, 'ok');
		await loadState();
	} catch (e) {
		fail(e);
	}
});

if (!token) $('nokey').hidden = false;
else {
	loadDoctor().catch(fail);
	loadState().catch(fail);
	loadHistory().catch(fail);
}
