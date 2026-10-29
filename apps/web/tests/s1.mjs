// ROADMAP S1 (ADR-0038): the "Add data" page (#/submit), in a real browser. Called by tests/e2e.mjs.
// Also runs alone: node tests/s1.mjs [build dir].
// 1. The browser's classifier (src/lib/intake.ts) runs every shared vector (fixtures/intake/vectors.json),
//    the same file the Rust crate runs, and carries the same texts, hosts and caps (THREATS SUB-7).
// 2. The U.S. Treasury example walks all four steps; with no API in the build, steps 3 and 4 say so and the
//    download is exactly the shared example. The page makes no POST and no request to any API.
// 3. Refused and held answers say why and stop at step 1; a detail with an e-mail is refused.
// 4. The FRED walk-through (a public API that needs the user's own key): held, the stub proof labelled not
//    real, and the typed key in no request, no download, no storage and not in the page's HTML (SUB-9).
// 5. Phone width: no sideways scroll on any step.
// 6. axe (WCAG 2.2 AA, serious and critical) on every step of both walk-throughs, at 1280 and 390 px.
import { readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { settled } from './wait.mjs';

const I = await import(new URL('../src/lib/intake.ts', import.meta.url).href);
const V = JSON.parse(await readFile(new URL('../../../fixtures/intake/vectors.json', import.meta.url), 'utf8'));
const same = (x, y) => JSON.stringify(x) === JSON.stringify(y);
// Key order does not matter for equality of two JSON values.
const canon = (v) => (Array.isArray(v) ? v.map(canon) : v && typeof v === 'object' ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, canon(v[k])])) : v);

async function open(browser, base, viewport) {
	const ctx = await browser.newContext({ viewport: viewport ?? { width: 1280, height: 900 }, reducedMotion: 'reduce', acceptDownloads: true });
	const pg = await ctx.newPage();
	const requests = [];
	pg.on('request', (r) => requests.push({ url: r.url(), method: r.method(), body: r.postData() ?? '' }));
	await pg.goto(base + '#/submit');
	await settled(pg);
	return { ctx, pg, requests };
}

const next = (pg) => pg.getByTestId('next').click();
async function downloadJson(pg) {
	const [dl] = await Promise.all([pg.waitForEvent('download'), pg.getByTestId('download').click()]);
	return { name: dl.suggestedFilename(), text: await readFile(await dl.path(), 'utf8') };
}

export async function s1(browser, base, root, expect) {
	// 1. Shared vectors in the browser's classifier.
	{
		const bad = [];
		for (const c of V.routes) if (!same(I.classify(c.answers), c.route)) bad.push(c.name);
		for (const c of V.submissions) {
			try {
				const s = I.parseSubmissionText(JSON.stringify(c.submission));
				const r = I.routeFor(s);
				if (c.error !== null || I.fetchPolicy(s, r) !== c.fetch || r.status !== c.status || (c.reasons && !same(r.reasons, c.reasons))) bad.push(c.name);
			} catch (e) {
				if (e.code !== c.error) bad.push(`${c.name} (${e.code})`);
			}
		}
		// S1-R1-4: raw JSON text through the strict parser (duplicate keys, number spellings).
		for (const c of V.rawSubmissions) {
			let got = null;
			try {
				I.parseSubmissionText(c.raw);
			} catch (e) {
				got = e.code ?? String(e);
			}
			if (got !== c.error) bad.push(`raw ${c.name} (${got})`);
		}
		expect(bad.length === 0, `S1 vectors: the browser classifier gives all ${V.routes.length} routes, ${V.submissions.length} submission and ${V.rawSubmissions.length} raw-text verdicts of the shared file${bad.length ? ': ' + bad.join('; ') : ''}`);
		const caps = Object.entries(V.caps).every(([k, n]) => I.CAPS[k] === n);
		// S1-R1-1: the page's key guard: trimmed, any case, on the raw field values (not escaped JSON), with a minimum length.
		{
			const KEY = V.syntheticKey;
			const refused = (s, key) => {
				try {
					I.outgoing(s, key);
					return null;
				} catch (e) {
					return e.code;
				}
			};
			const withDesc = (d) => ({ ...I.FRED_EXAMPLE, description: d });
			const quoted = 'abc"def\\ghij"klmn';
			const cases = [
				[refused(withDesc(`key ${KEY.toUpperCase()} here`), KEY), 'key_in_submission', 'upper-case key in the description'],
				[refused(withDesc(`key ${KEY} here`), `  ${KEY} `), 'key_in_submission', 'key typed with spaces around it'],
				[refused(withDesc(`key ${quoted} here`), quoted), 'key_in_submission', 'a key with characters JSON escapes'],
				[refused({ ...I.FRED_EXAMPLE, unit: KEY.slice(0, 32) }, KEY.slice(0, 32)), 'key_in_submission', 'key in the unit'],
				[refused(I.FRED_EXAMPLE, 'short'), 'key_too_short', 'a key shorter than the minimum'],
				[refused(I.FRED_EXAMPLE, KEY), null, 'a clean submission passes'],
				[refused(I.FRED_EXAMPLE, ''), null, 'no key typed']
			];
			const wrong = cases.filter(([got, want]) => got !== want).map(([got, want, what]) => `${what}: ${got}, want ${want}`);
			expect(wrong.length === 0, `S1 key guard: trimmed, any case, raw field values, minimum length (${cases.length} cases)${wrong.length ? ': ' + wrong.join('; ') : ''}`);
		}
		expect(
			same(canon(I.REASONS), canon(V.reasons)) && same(canon(I.NO_FETCH), canon(V.noFetch)) && same(Object.fromEntries(Object.entries(I.LEVEL_NAMES).map(([k, v]) => [String(k), v])), V.levelNames) && same(I.CLEARED_HOSTS, V.clearedHosts) && same(I.BLOCKED_HOSTS, V.blockedHosts) && caps,
			'S1 vectors: reason texts, no-pull texts, level names, cleared and blocked hosts and byte caps match the shared file'
		);
		expect(same(canon(I.EXAMPLE), canon(V.example)) && same(canon(I.FRED_EXAMPLE), canon(V.fredExample)), 'S1 vectors: the page\'s two examples are the shared file\'s');
		const payload = (format, content) => ({ format, content, bytes: Buffer.byteLength(content), keccak256: '0x' + '0'.repeat(64) });
		const own = { answers: { what: 'own', noLogin: false, rights: 'own_data', namesPerson: false, changes: 'once' }, title: 'My data', description: 'A sample I own.', sourceName: 'My records', sourceUrl: '', dataPayload: payload('csv', 'date,value\n2026-01-01,-1.5\n') };
		let accepted = true; try { I.parseSubmission(own); } catch { accepted = false; }
		const refused = (p) => { try { I.parseSubmission({ ...own, dataPayload: p }); return false; } catch { return true; } };
		expect(accepted && refused(payload('csv', 'value\n=1+1\n')) && refused(payload('csv', 'email,value\na@example.org,1\n')) && refused(payload('csv', 'value,VALUE\n1,2\n')) && refused(payload('csv', 'name,value\n"Jane"oops,1\n')) && refused(payload('json', '{"value":1,"value":2}')) && refused(payload('json', '{"full_name":"Jane Doe"}')) && refused(payload('json', '{"api_key":"secret"}')) && refused(payload('json', '{"api\\u005fkey":"secret"}')) && refused(payload('json', '{"value":"Bearer secret"}')) && refused(payload('json', '{"value":"Bearer\\u0020secret"}')) && refused(payload('html', '<script>bad</script>')) && refused(payload('csv', 'x'.repeat(9000))), 'U3 shared rules: URL-free own-data accepts bounded samples and rejects formulas, malformed CSV, duplicate headers/JSON keys, personal fields, escaped credentials, markup and oversize content');
		let otherRouteRefused = false; try { I.parseSubmission({ ...I.EXAMPLE, sourceUrl: '', dataPayload: payload('csv', 'a,b\n1,2') }); } catch { otherRouteRefused = true; }
		expect(otherRouteRefused, 'U3 shared rules: URL-free submissions and attached samples are limited to own-data route');
	}

	// 2. The Treasury example, end to end, offline.
	{
		const { ctx, pg, requests } = await open(browser, base);
		await pg.getByTestId('try-example').click();
		const r = pg.getByTestId('route');
		const level = await r.getAttribute('data-level');
		const text = await r.innerText();
		expect(level === '2' && /Level 2: Re-observed/.test(text) && text.includes(V.reasons.public_auto_pull), `S1 example: step 1 routes it to level 2, re-observed, with the plain reason (${level})`);
		await next(pg);
		const title = await pg.getByTestId('f-title').inputValue();
		const endpoint = await pg.getByTestId('f-endpoint').inputValue();
		expect(title === V.example.title && endpoint === V.example.endpointUrl, 'S1 example: step 2 is filled with the Treasury details');
		await next(pg);
		const offline = await pg.getByTestId('api-offline').innerText();
		const fetchLine = await pg.getByTestId('check-fetch').innerText();
		expect(/API is not connected in this build/.test(offline) && /api\.fiscaldata\.treasury\.gov/.test(fetchLine), 'S1 example: step 3 says the API is not connected and that the host is rights-cleared for a pull');
		await next(pg);
		expect(/not connected in this build/.test(await pg.getByTestId('send-offline').innerText()) && (await pg.getByTestId('send').count()) === 0, 'S1 example: step 4 offers only the download, no send button');
		const dl = await downloadJson(pg);
		const got = JSON.parse(dl.text);
		expect(dl.name === 'onyx-submission.json' && same(canon(got), canon(V.example)), `S1 example: the download is exactly the shared example (${dl.name})`);
		expect(/Nothing left your browser/.test(await pg.getByTestId('downloaded').innerText()), 'S1 example: the page says nothing left the browser');
		const sentOut = requests.filter((q) => q.method !== 'GET' || /\/v1\//.test(q.url) || !q.url.startsWith(base));
		expect(sentOut.length === 0, `S1 example: no POST, no API call and nothing off the page's own origin${sentOut.length ? ': ' + sentOut.map((q) => q.method + ' ' + q.url).join(', ') : ''}`);
		await ctx.close();
	}

	// 2b. Own data with no public URL, paste preview, local JSON export, and no network send.
	{
		const { ctx, pg, requests } = await open(browser, base);
		for (const id of ['q-what-own', 'q-nologin-no', 'q-rights-own_data', 'q-person-no', 'q-changes-once']) await pg.getByTestId(id).click();
		await next(pg);
		await pg.getByTestId('f-title').fill('Quarterly metrics');
		await pg.getByTestId('f-description').fill('A small dataset I own and have permission to share.');
		await pg.getByTestId('f-source-name').fill('My records');
		const sample = 'date,value\n2026-01-01,12\n';
		await pg.getByTestId('payload-file').setInputFiles({ name: 'metrics.csv', mimeType: 'text/csv', buffer: Buffer.from(sample) });
		await pg.getByTestId('payload-ready').waitFor();
		await pg.getByTestId('payload-file').setInputFiles({ name: 'broken.csv', mimeType: 'text/csv', buffer: Buffer.from([0xff, 0xfe]) });
		expect(/not valid UTF-8/.test(await pg.getByTestId('payload-error').innerText()), 'U3 file intake: invalid UTF-8 is refused instead of silently changing the sample');
		await pg.getByTestId('payload-file').setInputFiles({ name: 'metrics.csv', mimeType: 'text/csv', buffer: Buffer.from(sample) });
		await pg.getByTestId('payload-ready').waitFor();
		await pg.getByTestId('payload-file').setInputFiles({ name: 'unsupported.txt', mimeType: 'text/plain', buffer: Buffer.from('ignored') });
		expect((await pg.getByTestId('payload-error').innerText()).includes('.csv or .json') && await pg.getByTestId('payload-checking').count() === 0, 'U3 file intake: a rejected replacement clears stale checking state and reports its own error');
		await pg.getByTestId('payload-file').setInputFiles({ name: 'metrics.csv', mimeType: 'text/csv', buffer: Buffer.from(sample) });
		await pg.getByTestId('payload-ready').waitFor();
		await next(pg);
		expect(await pg.getByTestId('api-offline').count() === 1 && await pg.getByTestId('step-3').count() === 1, 'U3 own-data: a valid uploaded CSV reaches local check with no public source URL');
		await next(pg);
		const dl = await downloadJson(pg);
		const proposal = JSON.parse(dl.text);
		expect(proposal.sourceUrl === '' && proposal.dataPayload?.content === sample && proposal.dataPayload?.bytes === Buffer.byteLength(sample) && /^0x[0-9a-f]{64}$/.test(proposal.dataPayload?.keccak256), 'U3 own-data: the exact previewed CSV, byte count and digest are in the JSON proposal');
		const leaked = requests.filter((q) => q.method !== 'GET' || /\/v1\//.test(q.url) || !q.url.startsWith(base));
		expect(leaked.length === 0, 'U3 offline path: no POST or off-origin request');
		await ctx.close();
	}

	// 3. Refused and held answers; a detail with an e-mail.
	{
		const { ctx, pg } = await open(browser, base);
		for (const id of ['q-what-own', 'q-nologin-no', 'q-rights-own_data', 'q-person-yes', 'q-changes-once']) await pg.getByTestId(id).click();
		const r = pg.getByTestId('route');
		expect((await r.getAttribute('data-outcome')) === 'refused' && (await r.innerText()).includes(V.reasons.names_person) && (await pg.getByTestId('next').isDisabled()), 'S1 refused: data naming a person is refused with the reason, and step 2 cannot open');
		await pg.getByTestId('q-person-no').click();
		await pg.getByTestId('q-rights-terms_forbid').click();
		expect((await r.getAttribute('data-outcome')) === 'refused' && (await r.innerText()).includes(V.reasons.terms_forbid), 'S1 refused: terms that forbid sharing are refused with the reason');
		await pg.getByTestId('q-rights-not_sure').click();
		expect((await r.getAttribute('data-outcome')) === 'held' && (await r.innerText()).includes(V.reasons.rights_review) && (await r.innerText()).includes(V.reasons.unverified), 'S1 held: rights "not sure" is held for review; own data is labelled unverified (level 4)');
		// S1-R1-6: level 2 is shown as possible after a rights review unless the host is already cleared.
		for (const id of ['q-what-public_api', 'q-nologin-yes', 'q-rights-public_domain', 'q-changes-daily']) await pg.getByTestId(id).click();
		const pill = await r.locator('.pill').first();
		expect(
			(await r.getAttribute('data-level')) === '2' && /Level 2 possible after rights review/.test(await pill.innerText()) && !(await pill.getAttribute('class')).includes('good'),
			'S1 honesty: level 2 without a rights-cleared host is shown as possible after rights review, not as a green level'
		);
		await pg.getByTestId('q-what-own').click();
		await pg.getByTestId('q-nologin-no').click();
		await pg.getByTestId('q-rights-own_data').click();
		await next(pg);
		expect((await pg.getByTestId('f-wallet').count()) === 0, 'S1 details: no wallet field until S3 (a wallet links submissions to one person)');
		await pg.getByTestId('f-title').fill('My numbers');
		await pg.getByTestId('f-description').fill('Ask me at someone@example.com');
		await pg.getByTestId('f-source-name').fill('Me');
		await pg.getByTestId('f-source-url').fill('https://example.org/data');
		await next(pg);
		expect(/e-mail/.test(await pg.getByTestId('details-error').innerText()) && (await pg.getByTestId('step-2').count()) === 1, 'S1 details: an e-mail address in the text is refused and the page stays on step 2');
		await ctx.close();
	}

	// 4. The FRED walk-through: keyed, held, stub proof labelled, the key never leaves the page.
	{
		const KEY = V.syntheticKey;
		const { ctx, pg, requests } = await open(browser, base);
		await pg.getByTestId('try-keyed').click();
		const r = pg.getByTestId('route');
		const rt = await r.innerText();
		expect((await r.getAttribute('data-outcome')) === 'held' && (await r.getAttribute('data-level')) === '4' && rt.includes(V.reasons.keyed_source) && rt.includes(V.reasons.source_blocked), 'S1 FRED: a keyed source is held at level 4, the blocked source named, with plain reasons');
		const steps = await pg.getByTestId('stepper').innerText();
		expect(/Key/.test(steps) && !/Check/.test(steps), 'S1 FRED: step 3 is the key step');
		await next(pg);
		expect((await pg.getByTestId('f-template').inputValue()) === V.fredExample.endpointTemplate && (await pg.getByTestId('f-series').inputValue()) === 'CPIAUCSL', 'S1 FRED: step 2 holds the series id and the request with {api_key} in place of the key');
		await next(pg);
		expect(/Proof: not yet real \(S2\)/.test(await pg.getByTestId('stub-label').innerText()), 'S1 FRED: the key step labels the prover a stub, not yet real');
		expect((await pg.getByTestId('key-input').getAttribute('autocomplete')) === 'new-password' && /makes no request/.test(await pg.getByTestId('stub-label').innerText()), 'S1 FRED: the key field asks the browser not to fill a saved password, and the stub says it makes no request');
		await pg.getByTestId('key-input').fill(KEY);
		const body = '{"observations":[{"date":"2026-09-01","value":"320.1"}]}';
		await pg.getByTestId('pasted').fill(body);
		await pg.getByTestId('make-proof').click();
		await pg.getByTestId('proof-json').waitFor({ timeout: 120000 });
		const proof = JSON.parse(await pg.getByTestId('proof-json').innerText());
		expect(proof.prover === 'stub-s2' && proof.real === false && /^0x[0-9a-f]{64}$/.test(proof.responseKeccak256) && proof.responseBytes === Buffer.byteLength(body), 'S1 FRED: the stub proof says real: false and carries only the pasted body\'s hash and length');
		await next(pg);
		expect(/FRED's terms restrict/.test(await pg.getByTestId('fred-note').innerText()) && /keyed source/.test(await pg.getByTestId('send-status').innerText()), 'S1 FRED: step 4 says in plain words that FRED stays held and is not published');
		const dl = await downloadJson(pg);
		const sub = JSON.parse(dl.text);
		const route = I.routeFor(I.parseSubmission(sub));
		expect(!dl.text.includes(KEY) && route.status === 'held_keyed_source_review' && sub.keyProof?.real === false, 'S1 FRED: the download carries no key and checks as held for keyed-source review');
		// Before the test itself types the key into the details: not in storage, not in the page's HTML.
		const store = await pg.evaluate(() => JSON.stringify({ l: { ...localStorage }, s: { ...sessionStorage } }));
		const html = await pg.evaluate(() => document.documentElement.outerHTML);
		// The key in the details is refused before anything leaves the page.
		await pg.getByTestId('back').click();
		await pg.getByTestId('key-input').fill(KEY);
		await pg.getByTestId('back').click();
		await pg.getByTestId('f-description').fill(`My key is ${KEY}`);
		await next(pg);
		await pg.getByTestId('next').click();
		await pg.getByTestId('download').click();
		expect(/Your API key appears/.test(await pg.getByTestId('send-error').innerText()), 'S1 FRED: a submission that contains the typed key is refused on the page');
		// S1-R1-1 in the page: the key typed with a trailing space, the description holding it in upper case.
		await pg.getByTestId('back').click();
		await pg.getByTestId('key-input').fill(`${KEY} `);
		await pg.getByTestId('back').click();
		await pg.getByTestId('f-description').fill(`My key is ${KEY.toUpperCase()}`);
		await next(pg);
		await pg.getByTestId('next').click();
		await pg.getByTestId('download').click();
		expect(/Your API key appears/.test(await pg.getByTestId('send-error').innerText()), 'S1 FRED: the typed key is refused in any case and with spaces around it');
		const leaked = requests.filter((q) => q.url.includes(KEY) || q.body.includes(KEY));
		expect(leaked.length === 0 && !store.includes(KEY) && !html.includes(KEY), `S1 FRED: the key is in no request URL or body (${requests.length} requests), not in local or session storage and not in the page's HTML`);
		expect(requests.every((q) => q.method === 'GET' && !/\/v1\//.test(q.url) && !/stlouisfed/.test(q.url)), 'S1 FRED: no POST, no API call and no request to FRED');
		await ctx.close();
	}

	// 5. Phone width: no sideways scroll on any step of the example.
	{
		const { ctx, pg } = await open(browser, base, { width: 390, height: 844 });
		const side = [];
		await pg.getByTestId('try-example').click();
		for (let s = 1; s <= 4; s++) {
			side.push(await pg.evaluate(() => document.documentElement.scrollWidth - innerWidth));
			if (s < 4) await next(pg);
		}
		await pg.getByTestId('try-keyed').click();
		await next(pg);
		await next(pg);
		side.push(await pg.evaluate(() => document.documentElement.scrollWidth - innerWidth));
		await pg.getByTestId('try-example').click();
		for (const id of ['q-what-own', 'q-nologin-no', 'q-rights-own_data', 'q-person-no', 'q-changes-once']) await pg.getByTestId(id).click();
		await next(pg);
		side.push(await pg.evaluate(() => document.documentElement.scrollWidth - innerWidth));
		expect(side.every((x) => x === 0), `S1 phone 390 px: no sideways scroll on steps 1-4, the key step, or own-data file/paste form (${side.join(', ')})`);
		await ctx.close();
	}

	// 6. axe on every step (the a11y suite sees only the first, empty step).
	{
		const axeSrc = await readFile(createRequire(import.meta.url).resolve('axe-core/axe.min.js'), 'utf8');
		const TAGS = ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa', 'best-practice'];
		const found = [];
		let runs = 0;
		for (const viewport of [{ width: 1280, height: 900 }, { width: 390, height: 844 }]) {
			const { ctx, pg } = await open(browser, base, viewport);
			const run = async (at) => {
				if (!(await pg.evaluate(() => 'axe' in window))) await pg.addScriptTag({ content: axeSrc });
				await pg.evaluate(() => document.querySelectorAll('details').forEach((d) => (d.open = true)));
				const v = await pg.evaluate((tags) => window.axe.run(document, { runOnly: { type: 'tag', values: tags }, resultTypes: ['violations'] }), TAGS);
				runs++;
				for (const x of v.violations) if (x.impact === 'serious' || x.impact === 'critical') found.push(`${viewport.width} ${at}: ${x.id} (${x.nodes[0]?.target.join(' ')})`);
			};
			for (const [btn, name] of [['try-example', 'example'], ['try-keyed', 'keyed']]) {
				await pg.getByTestId(btn).click();
				for (let st = 1; st <= 4; st++) {
					await run(`${name} step ${st}`);
					if (st < 4) await next(pg);
				}
			}
			await pg.getByTestId('try-example').click();
			for (const id of ['q-what-own', 'q-nologin-no', 'q-rights-own_data', 'q-person-no', 'q-changes-once']) await pg.getByTestId(id).click();
			await next(pg);
			await run('own-data file/paste form');
			await ctx.close();
		}
		expect(found.length === 0 && runs === 18, `S1 a11y: axe finds no serious or critical WCAG 2.2 AA issue on the four main steps and own-data form at 1280 and 390 px (${runs} runs)${found.length ? ': ' + found.join('; ') : ''}`);
	}
}

// Alone against a build: node tests/s1.mjs [build dir] (Chrome on this machine).
if (import.meta.url === `file://${process.argv[1]}`) {
	const { chromium } = await import('playwright-core');
	const { serve } = await import('./serve.mjs');
	const dir = process.argv[2] ?? new URL('../build/', import.meta.url).pathname;
	const server = await serve(0, dir);
	const base = `http://127.0.0.1:${server.address().port}/`;
	const browser = await chromium.launch({ channel: 'chrome' });
	const failures = [];
	const expect = (ok, what) => (ok ? console.log(`ok   ${what}`) : (failures.push(what), console.log(`FAIL ${what}`)));
	try {
		await s1(browser, base, dir, expect);
	} finally {
		await browser.close();
		server.close();
	}
	console.log(failures.length ? `${failures.length} failed` : 'S1 PASSED');
	process.exit(failures.length ? 1 : 0);
}
