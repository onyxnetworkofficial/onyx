// e2e "publish" (ROADMAP S27, READINESS B1, ADR-0066 slice 4): becoming a publisher, on the site.
// The ADR's four named checks, at 1280 and 390 px:
//   "the Add data page names both paths"
//   "the publish page shows every step with its command"
//   "a status lookup shows the state and the reason"
//   "a refused application shows how to apply again"
// With the pointer form (READINESS B5, ADR-0066 item 7) also:
//   "the pointer form sends two values to this site and shows the receipt"
//   "a node without the route is said plainly, with the issue as the way"
//   "a hostile answer is never shown"
// After review S4-R1 also:
//   "the jump list opens the page and the words are explained"
//   "a publisher from before the record shows its id and state only"
//   "every value of the list is text, never markup or a link"
// and around them: the committed status list is shown word for word, a missing or broken list is a
// plain message (never a guess), a hostile list is refused whole and runs nothing, the page asks
// only this site (its one data request by itself is the static publishers.json; the form's POST goes
// to this site's own /v1/publishers/pointer and only on a press of its button), the policy refuses nothing
// and allows no inline script, no visitor text carries an internal code, and on a phone nothing
// scrolls sideways or stands past the right edge. Called by tests/e2e.mjs; also runs alone against
// a build: node tests/publish.mjs [build dir].
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

const P = await import(new URL('../src/lib/publishers.ts', import.meta.url).href);
const S = await import(new URL('../src/lib/publish-steps.ts', import.meta.url).href);
const R = await import(new URL('../src/lib/release.ts', import.meta.url).href);
const PTR = await import(new URL('../src/lib/publisher-pointer.ts', import.meta.url).href);

// A made-up list (no real id, origin or key): one publisher per kind of state, one application
// per state that is not an approval.
const H = { refused: '0x' + 'ab'.repeat(32), bare: '0x' + 'cd'.repeat(32), expired: '0x' + 'ef'.repeat(32), received: '0x' + '12'.repeat(32), withdrawn: '0x' + '9f'.repeat(32) };
const UNTIL = 1795478700;
const PLANTED = {
	version: 1,
	chain_id: 84532,
	book: '0x' + '04'.repeat(20),
	publishers: [
		{ publisher: 'river-levels.open', state: 'approved (record)', date: '2026-11-17', origin: 'data.example.org/river-levels', signers: ['0x' + '11'.repeat(20)], registered: [{ tx: '0x' + '22'.repeat(32), block: 60000001 }] },
		{ publisher: 'tide-tables.open', state: 'paused', date: '2026-11-18', origin: 'tides.example.net', signers: ['0x' + '33'.repeat(20)], registered: [{ tx: '0x' + '44'.repeat(32), block: 60000002 }], reason: 'no-capacity' },
		// From before the record: the list carries no origin for it, and the page shows id and state only.
		{ publisher: 'old-gauges.open', state: 'registered-before-record', date: '2026-10-30', signers: ['0x' + '55'.repeat(20)], registered: [{ tx: '0x' + '66'.repeat(32), block: 52000000 }] }
	],
	applications: [
		{ application: H.refused, state: 'refused', date: '2026-11-17', reason: 'rights-unclear', blocks_signer_until: UNTIL },
		{ application: H.bare, state: 'refused', date: '2026-11-17', reason: 'bad-signature' },
		{ application: H.expired, state: 'expired', date: '2026-11-16' },
		{ application: H.received, state: 'received', date: '2026-11-17' },
		{ application: H.withdrawn, state: 'withdrawn', date: '2026-11-15' }
	]
};
// The same list with one id that carries markup: out of shape, so nothing of it may be shown.
const HOSTILE = { ...PLANTED, publishers: [{ ...PLANTED.publishers[0], publisher: '<img src=x onerror=__xss()>' }, ...PLANTED.publishers.slice(1)] };
const INTERNAL = /\bADR\b|ADR-\d|THREATS|PUBON|READINESS|ROADMAP|\bS27\b|\bB1\b|SUB-\d|slice \d/;
const flat = (t) => t.replace(/\s+/g, ' ').trim();
const text0 = flat;

/** Sideways scroll, and what stands past the right edge without a clipping box around it. */
const edges = (pg) =>
	pg.evaluate(() => {
		const W = innerWidth;
		const clipped = (e) => {
			for (let n = e.parentElement; n; n = n.parentElement) if (/auto|scroll|hidden|clip/.test(getComputedStyle(n).overflowX)) return true;
			return false;
		};
		const all = [...document.querySelectorAll('main *')].filter((e) => e.getBoundingClientRect().width > 0);
		const past = all.filter((e) => e.getBoundingClientRect().right > W + 1 && !clipped(e)).map((e) => `${e.tagName.toLowerCase()} "${(e.textContent || '').trim().slice(0, 30)}"`);
		const inner = all.filter((e) => /auto|scroll/.test(getComputedStyle(e).overflowX) && e.scrollWidth > e.clientWidth + 1).map((e) => `${e.tagName.toLowerCase()} "${(e.textContent || '').trim().slice(0, 30)}"`);
		return { side: document.documentElement.scrollWidth - W, past: past.slice(0, 4), inner: inner.slice(0, 4) };
	});

export async function publish(browser, base, root, expect) {
	const committed = JSON.parse(await readFile(join(root, 'publishers.json'), 'utf8'));
	const origin = new URL(base).origin;
	for (const width of [1280, 390]) {
		const at = `publish ${width}`;
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
		const errors = [];
		const asked = [];
		let serve = 'real';
		await ctx.exposeFunction('__xss', () => errors.push('a hostile status list ran script'));
		await ctx.addInitScript(() => document.addEventListener('securitypolicyviolation', (e) => console.error(`PUBLISH-CSP ${e.violatedDirective} ${e.blockedURI}`)));
		ctx.on('request', (r) => asked.push(r.url()));
		// The pointer route of this site's own origin: answered here as a node would (or would not).
		const posts = [];
		let node = 'none';
		const RECEIPT = { id: 'ptr_' + '3f9a1c5e'.repeat(4), status: 'received', expiresUnix: 1795392000, decision: 'see_status_list' };
		const XSS = '<img src=x onerror=__xss()><script>__xss()</script>';
		await ctx.route('**/v1/publishers/pointer', (route) => {
			const rq = route.request();
			posts.push({ method: rq.method(), url: rq.url(), body: rq.postData(), type: rq.headers()['content-type'], cookie: rq.headers()['cookie'] ?? null });
			const json = (status, body) => route.fulfill({ status, contentType: 'application/json', headers: { 'cache-control': 'no-store' }, body: JSON.stringify(body) });
			if (node === 'none') return route.continue();
			if (node === 'created') return json(201, RECEIPT);
			if (node === 'again') return json(200, RECEIPT);
			if (node === 'expired') return json(200, { ...RECEIPT, status: 'expired', reason: 'pointer_lifetime_over' });
			if (node === 'dropped') return json(200, { ...RECEIPT, status: 'dropped', reason: 'application_hash_mismatch' });
			if (node === 'busy') return json(409, { error: { code: 'pointer_origin_busy', message: XSS } });
			if (node === 'cross') return json(403, { error: { code: 'cross_site_refused', message: XSS } });
			if (node === 'off') return json(404, { error: { code: 'not_found', message: 'no such route' } });
			if (node === 'forbidden') return route.fulfill({ status: 403, contentType: 'text/html', body: `<!doctype html><title>403</title>${XSS}` });
			if (node === 'shared') return json(422, { error: { code: 'origin_shared_host', message: XSS } });
			if (node === 'quota') return json(429, { error: { code: 'daily_quota', message: XSS } });
			if (node === 'hostile-receipt') return json(201, { ...RECEIPT, id: XSS });
			if (node === 'hostile-extra') return json(201, { ...RECEIPT, note: XSS });
			if (node === 'hostile-error') return json(500, { error: { code: XSS, message: XSS } });
			if (node === 'hostile-html') return route.fulfill({ status: 200, contentType: 'text/html', body: `<!doctype html>${XSS}` });
			if (node === 'redirect') return route.fulfill({ status: 307, headers: { location: 'https://elsewhere.example/v1/publishers/pointer' }, body: '' });
			return route.abort('connectionrefused');
		});
		await ctx.route('**/publishers.json', (route) => {
			if (serve === 'real') return route.continue();
			if (serve === 'missing') return route.fulfill({ status: 404, body: 'not found' });
			if (serve === 'html') return route.fulfill({ status: 200, contentType: 'text/html', body: '<!doctype html><title>x</title><img src=x onerror=__xss()>' });
			if (serve === 'broken') return route.fulfill({ status: 200, contentType: 'application/json', body: '{"version": 1, "publishers": [' });
			return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(serve === 'hostile' ? HOSTILE : PLANTED) });
		});
		const open = async (hash, testid) => {
			const pg = await ctx.newPage();
			pg.on('pageerror', (e) => errors.push(String(e)));
			pg.on('console', (m) => m.type() === 'error' && m.text().startsWith('PUBLISH-CSP') && errors.push(m.text()));
			await pg.goto(base + hash);
			await pg.getByTestId(testid).waitFor({ timeout: 90000 });
			return pg;
		};
		const statusDrawn = (pg) => pg.waitForFunction(() => document.querySelector('[data-testid="publish-status"] .pill') && !document.querySelector('[data-testid="publish-status"] .pill.run'), null, { timeout: 90000 });

		// 1. "the Add data page names both paths"
		{
			const pg = await open('#/submit', 'add-paths');
			const cards = pg.getByTestId('add-paths').locator('.path');
			const suggest = flat(await pg.getByTestId('path-suggest').innerText());
			const own = flat(await pg.getByTestId('path-publish').innerText());
			expect(
				(await cards.count()) === 2 && (await pg.getByTestId('path-suggest').locator('h3').innerText()).trim() === 'Suggest a dataset' && (await pg.getByTestId('path-publish').locator('h3').innerText()).trim() === 'Publish your own data',
				`${at}: the Add data page names both paths: two cards, "Suggest a dataset" and "Publish your own data"`
			);
			expect(
				suggest.includes('review queue') && suggest.includes('nothing is published by itself') && /For anyone who knows/.test(suggest) && suggest.includes('No key'),
				`${at}: the suggest card says what happens (a review queue, nothing published by itself) and who it is for`
			);
			expect(
				own.includes('sign your data with your own key') && own.includes('a reviewer checks your application') && /For whoever holds a dataset/.test(own) && own.includes(`up to ${P.DECIDE_DAYS} days`),
				`${at}: the publish card says what happens (your own key, a reviewer checks the application) and who it is for`
			);
			expect(!INTERNAL.test(suggest + own), `${at}: the two cards carry no internal code`);
			const boxes = await cards.evaluateAll((es) => es.map((e) => e.getBoundingClientRect()).map((r) => [Math.round(r.left), Math.round(r.top), Math.round(r.right)]));
			expect(
				width === 1280 ? boxes[0][1] === boxes[1][1] && boxes[0][0] < boxes[1][0] : boxes[0][0] === boxes[1][0] && boxes[0][1] < boxes[1][1] && boxes.every((b) => b[2] <= width - 16),
				`${at}: the two cards stand ${width === 1280 ? 'side by side' : 'one under the other, inside the screen'} (${JSON.stringify(boxes)})`
			);
			const E = await edges(pg);
			expect(E.side === 0 && E.past.length === 0, `${at}: the Add data page has no sideways scroll and nothing past the right edge${E.past.length ? ': ' + E.past.join('; ') : ''}`);
			// The first card leads to the questions (the focus on the step's title), the second to the publish page.
			await pg.getByTestId('path-suggest-go').click();
			await pg.waitForFunction(() => document.activeElement?.closest('[data-testid="step-1"]') && document.activeElement.tagName === 'H3', null, { timeout: 30000 }).catch(() => {});
			expect(await pg.evaluate(() => document.activeElement?.tagName === 'H3' && !!document.activeElement.closest('[data-testid="step-1"]')), `${at}: "Start with the questions" moves to the first question`);
			const go = pg.getByTestId('path-publish-go');
			const gb = await go.boundingBox();
			await go.click();
			await pg.getByTestId('publish-steps').waitFor({ timeout: 90000 });
			await statusDrawn(pg);
			expect((await pg.evaluate(() => location.hash)) === '#/publish' && gb.height >= 44, `${at}: "See the steps" opens #/publish (a ${Math.round(gb.height)} px link)`);
			await pg.close();
		}

		// 2. "the publish page shows every step with its command"
		{
			const pg = await open('#/publish', 'publish-steps');
			await statusDrawn(pg);
			expect((await pg.locator('h1').count()) === 1 && (await pg.locator('h1').innerText()).trim() === 'Publish your own data', `${at}: one h1, "Publish your own data"`);
			// The commands name the hosted test node, never this server's own origin (review S4-R1 L4).
			const want = S.steps();
			const got = await pg.getByTestId('publish-step').evaluateAll((lis) =>
				lis.map((li) => ({ title: li.querySelector('[data-testid="publish-step-title"]').textContent.trim(), n: li.querySelector('.sn').textContent.trim(), cmds: [...li.querySelectorAll('[data-testid="publish-cmd"]')].map((c) => c.textContent) }))
			);
			expect(
				got.length === 10 && got.every((g, i) => g.n === String(i + 1) && g.title === want[i].title && JSON.stringify(g.cmds) === JSON.stringify(want[i].commands)),
				`${at}: the publish page shows every step with its command: ten numbered steps, each with the guide's exact command (${got.map((g) => g.cmds.length).join(',')})`
			);
			expect(got.slice(8).every((g) => g.cmds.every((c) => c.includes(S.HOSTED_NODE) && !c.includes(origin))) && got[7].cmds[0].startsWith(`curl -s -o publishers.json ${S.STATUS_URL}\n`), `${at}: the upload and the check name the hosted test node, not the address this page is served from, and step 8 says where the status list is fetched from`);
			const all = got.flatMap((g) => g.cmds).join('\n');
			expect(
				['onyx-publish keygen --out publisher.key', 'onyx-publish keys-doc ', 'onyx-publish build ', 'onyx-publish sign ', 'onyx-publish apply ', 'onyx-publish doctor --live', 'origin: ', 'application keccak: ', 'onyx-publish upload ', 'onyx-check --bulk '].every((c) => all.includes(c)) &&
					all.includes(`--chain-id ${S.CHAIN_ID} --book ${S.BOOK}`),
				`${at}: keygen, the keys document, the first dataset, apply, doctor, the issue's two lines, the upload and the check are all there, for chain ${S.CHAIN_ID} and the signed book`
			);
			// The tools come from the release the site links (the release helper), once that release's
			// tools have the commands of this page (review S4-R1 H1); until then no download is linked.
			const tools = flat(await pg.getByTestId('publish-tools').innerText());
			const ready = R.hasOnboarding(R.RELEASE);
			if (ready) {
				expect(
					(await pg.getByTestId('publish-download').getAttribute('href')) === R.BASE + S.TOOLS_FILE && tools.includes(R.RELEASE) && (await pg.getByTestId('publish-unpack').textContent()) === S.UNPACK.join('\n') && (await pg.getByTestId('publish-build').textContent()) === S.buildFromSource(R.RELEASE).join('\n') && (await pg.getByTestId('publish-soon').count()) === 0,
					`${at}: the tools are the publisher file of ${R.RELEASE}, with the unpack and path lines and the build of the same release from the source`
				);
				expect(text0(await pg.getByTestId('publish-steps').innerText()).includes('onyx-publish/README.md'), `${at}: the page says the guide is in the download as onyx-publish/README.md`);
			} else {
				const soon = flat(await pg.getByTestId('publish-soon').innerText().catch(() => ''));
				expect(
					soon.includes(R.ONBOARDING_SINCE) && soon.includes(R.RELEASE) && (await pg.getByTestId('publish-download').count()) === 0 && (await pg.getByTestId('publish-tools').locator('pre, a').count()) === 0 && !flat(await pg.getByTestId('publish-steps').innerText()).includes('README.md'),
					`${at}: ${R.RELEASE} is from before the page's commands, so no download and no build is offered; a note names ${R.ONBOARDING_SINCE}`
				);
			}
			// The jump list at the top (review S4-R1 L5): every section, and the focus lands on its heading.
			const jumps = await pg.getByTestId('publish-jump-to').evaluateAll((bs) => bs.map((b) => [b.textContent.trim(), Math.round(b.getBoundingClientRect().height)]));
			const h2s = await pg.locator('main section.section > h2').evaluateAll((hs) => hs.map((h) => h.textContent.trim()));
			await pg.getByTestId('publish-jump-to').last().click();
			await pg.waitForFunction(() => document.activeElement?.tagName === 'H2', null, { timeout: 30000 }).catch(() => {});
			const landed = await pg.evaluate(() => ({ text: document.activeElement?.textContent?.trim(), top: document.activeElement?.getBoundingClientRect().top, vh: innerHeight }));
			expect(jumps.length === 7 && h2s.length === 7 && jumps[0][0] === h2s[0] && jumps[6][0] === 'Application status' && (width > 760 || jumps.every((j) => j[1] >= 44)), `${at}: "On this page" lists the 7 sections (${jumps.map((j) => j[0]).join(', ')})`);
			expect(landed.text === 'Application status' && landed.top >= -1 && landed.top < landed.vh, `${at}: the last entry moves to the status box, the focus on its heading (${landed.text}, ${Math.round(landed.top ?? -1)} px)`);
			await pg.evaluate(() => scrollTo(0, 0));
			const words = flat(await pg.getByTestId('publish-words').innerText());
			const names = flat(await pg.getByTestId('publish-names').innerText());
			expect(S.WORDS.every((w) => words.includes(w.word) && words.includes(w.means)) && S.WORDS.length === 4, `${at}: signed book, hash, SPDX id and test ETH are each explained in a sentence before the steps`);
			expect(names.includes(S.EXAMPLE_ID) && names.includes(S.EXAMPLE_ORIGIN) && names.includes('bound together for good'), `${at}: the id and the origin are picked before the steps, as in the guide, with made-up examples`);
			const text = flat(await pg.locator('main').innerText());
			const says = (testid, words) => pg.getByTestId(testid).innerText().then((t) => words.every((w) => flat(t).includes(w)));
			expect(await says('publish-testnet', ['This is a test network', 'No real money', 'One person reviews', 'cannot be undone', 'No application from outside has been reviewed yet']), `${at}: the page says plainly that this is a test network with one reviewer, and that no outside application was reviewed yet`);
			expect(await says('publish-review', ['What the reviewer checks', `within ${P.DECIDE_DAYS} days`, 'Checked by a program', 'Read by a person', 'No row is about one person']), `${at}: what the reviewer checks, and a decision within ${P.DECIDE_DAYS} days`);
			expect(await says('publish-grant', ['1 GiB', '16 datasets']), `${at}: what a new publisher gets: 1 GiB and 16 datasets by default`);
			expect(await says('publish-public', ['publisher id', 'origin', "key's address", 'only its hash']) && (await says('publish-never', ['No name', 'No contact detail', 'No key file'])), `${at}: what is public (the record) and what is never asked (no name, no contact, no key)`);
			const step7 = flat(await pg.getByTestId('publish-step').nth(6).innerText());
			const ways = await pg.getByTestId('publish-step').nth(6).evaluate((li) => {
				const top = (sel) => li.querySelector(sel)?.getBoundingClientRect().top ?? -1;
				return { form: top('[data-testid="pointer-form"]'), issue: top('[data-testid="publish-issue-way"]'), lines: top('[data-testid="publish-cmd"]'), link: top('[data-testid="publish-issues"]'), forms: document.querySelectorAll('[data-testid="pointer-form"]').length };
			});
			expect(
				step7.includes('First way: send the two values here, without an account') && step7.includes('Second way: a public issue on the source repository') && step7.includes(`Title it "${S.ISSUE_TITLE}"`) && ways.forms === 1 && ways.form > 0 && ways.form < ways.issue && ways.issue < ways.lines && ways.lines < ways.link && (await pg.getByTestId('publish-issues').getAttribute('href')) === S.ISSUES,
				`${at}: step 7 offers the pointer form first and the public issue second, with the issue's title and two lines`
			);
			expect(
				step7.includes('The form needs no account, and the node keeps no address of yours') && step7.includes('does not contact your origin') && step7.includes('The issue is public and shows the account that opens it') && !/planned, not built|Today an application is filed/.test(step7),
				`${at}: step 7 says what each way shows of the applicant, and no longer that the way without an account is not built`
			);
			expect(!INTERNAL.test(text), `${at}: no internal code in the page's text${INTERNAL.test(text) ? ': ' + INTERNAL.exec(text)[0] : ''}`);
			expect(!/[^\s@]+@[^\s@]+\.[a-z]{2,}/i.test(text), `${at}: no mail address anywhere on the page`);
			const hrefs = await pg.locator('main a[href]').evaluateAll((as) => as.map((a) => a.getAttribute('href')));
			const outside = hrefs.filter((h) => !h.startsWith('#') && !h.startsWith(R.REPO));
			expect(outside.length === 0, `${at}: every link is a page of this site or the project's repository${outside.length ? ': ' + outside.join(' ') : ''}`);
			// Commands: a region a keyboard can reach; scrolled inside its box on a wide screen, wrapped on a phone.
			const cmd = await pg.locator('main pre.cmd').evaluateAll((ps) => ps.map((p) => ({ tab: p.tabIndex, role: p.getAttribute('role'), name: p.getAttribute('aria-label'), wrap: getComputedStyle(p).whiteSpace, over: p.scrollWidth - p.clientWidth, right: p.getBoundingClientRect().right })));
			expect(cmd.length === (ready ? 14 : 12) && cmd.every((c) => c.tab === 0 && c.role === 'region' && c.name), `${at}: each of the ${cmd.length} command blocks is a named region in the Tab order`);
			if (width === 390) expect(cmd.every((c) => c.wrap === 'pre-wrap' && c.over <= 1 && c.right <= width - 16 + 1), `${at}: on a phone every command wraps inside its box (none scrolls sideways)`);
			else expect(cmd.every((c) => c.right <= width) && cmd.some((c) => c.wrap === 'pre'), `${at}: on a wide screen a long command stays one line and scrolls inside its box`);
			const E = await edges(pg);
			expect(E.side === 0, `${at}: no sideways scroll on the publish page`);
			expect(E.past.length === 0, `${at}: nothing past the right edge on the publish page${E.past.length ? ': ' + E.past.join('; ') : ''}`);
			if (width === 390) expect(E.inner.length === 0, `${at}: no box scrolls sideways inside the publish page${E.inner.length ? ': ' + E.inner.join('; ') : ''}`);
			// The policy: no inline script allowed, and the only inline script is SvelteKit's hashed start.
			const csp = (await pg.locator('meta[http-equiv="content-security-policy"]').getAttribute('content')) ?? '';
			const script = /script-src ([^;]+)/.exec(csp)?.[1] ?? '';
			const handlers = await pg.evaluate(() => [...document.querySelectorAll('*')].filter((e) => [...e.attributes].some((a) => /^on/i.test(a.name))).length);
			expect(script.startsWith("'self' 'wasm-unsafe-eval' 'sha256-") && !/unsafe-inline|(?<!wasm-)unsafe-eval|\*|https:|data:/.test(script) && handlers === 0, `${at}: the page's policy allows no inline script, and the page has no inline handler (${script.slice(0, 40)}…)`);

			// The committed list, word for word: every publisher with the state the file has.
			const rows = await pg.getByTestId('status-row').evaluateAll((lis) => lis.map((li) => [li.querySelector('[data-testid="status-id"]').textContent.trim(), li.querySelector('[data-testid="status-row-state"]').textContent.trim(), li.innerText]));
			const wantRows = committed.publishers.map((p) => [p.publisher, p.state]);
			expect(JSON.stringify(rows.map((r) => r.slice(0, 2))) === JSON.stringify(wantRows), `${at}: the status box lists the ${wantRows.length} publishers of the committed list with their states as written (${[...new Set(wantRows.map((r) => r[1]))].join(', ')})`);
			const shown = (p, i) =>
				p.state === P.BEFORE_RECORD
					? flat(rows[i][2]) === `${p.publisher} ${p.state} ${P.PUBLISHER_STATES[p.state]}`
					: rows[i][2].includes(p.origin) && rows[i][2].includes(p.date) && p.signers.every((s) => rows[i][2].includes(s)) && p.registered.every((r) => rows[i][2].includes(r.tx) && rows[i][2].includes(String(r.block))) && flat(rows[i][2]).includes(P.PUBLISHER_STATES[p.state]);
			expect(committed.publishers.every(shown), `${at}: a publisher from before the record shows its id, its state and "Registered before this record began." and nothing else; any other shows its date, origin, key and registration from the file`);
			const boxText = flat(await pg.getByTestId('publish-status').innerText());
			expect(
				committed.publishers.every((p) => p.state !== P.BEFORE_RECORD || !('origin' in p)) && !/[a-z0-9-]+\.[a-z]{2,}\/[a-z]/.test(boxText) && !boxText.includes(new URL(S.HOSTED_NODE).host) && (await pg.getByTestId('status-origin').count()) === committed.publishers.filter((p) => p.state !== P.BEFORE_RECORD).length,
				`${at}: no origin of a publisher from before the record is in the list or on the page (review S4-R1 M2)`
			);
			const body = flat(await pg.getByTestId('publish-status').innerText());
			expect(
				committed.publishers.some((p) => p.state.startsWith('approved')) || !/\bapproved\b/i.test(rows.map((r) => r[2]).join(' ')),
				`${at}: no publisher is called approved unless the file says so`
			);
			expect(body.includes('says nothing about who runs a publisher') && body.includes('not sent anywhere'), `${at}: the box says the list names no one behind a publisher, and that a lookup is not sent anywhere`);
			// A lookup of a committed publisher by its id; nothing leaves the page for it.
			const before = asked.length;
			const first = committed.publishers[0];
			await pg.getByTestId('status-query').fill(first.publisher);
			await pg.getByTestId('status-find').click();
			await pg.getByTestId('status-publisher').waitFor({ timeout: 30000 });
			const hit = flat(await pg.getByTestId('status-result').innerText());
			await pg.getByTestId('status-query').fill('no-such-publisher.example');
			await pg.keyboard.press('Enter');
			await pg.waitForFunction(() => document.querySelector('[data-testid="status-result"]')?.dataset.kind === 'none', null, { timeout: 30000 }).catch(() => {});
			const none = flat(await pg.getByTestId('status-result').innerText());
			await pg.getByTestId('status-query').fill('<script>alert(1)</script>');
			await pg.keyboard.press('Enter');
			await pg.waitForFunction(() => document.querySelector('[data-testid="status-result"]')?.dataset.kind === 'invalid', null, { timeout: 30000 }).catch(() => {});
			const invalid = flat(await pg.getByTestId('status-result').innerText());
			expect(hit.startsWith(first.state) && hit.includes(P.PUBLISHER_STATES[first.state]) && hit.includes(first.publisher), `${at}: a lookup by a publisher id shows that publisher's state as the file has it`);
			expect(none.includes('Nothing in the list matches') && invalid.includes('neither a publisher id nor') && !invalid.includes('script'), `${at}: an unknown id and a query that is neither an id nor a hash each get a plain answer, and the query is not shown back`);
			expect(asked.length === before, `${at}: a lookup makes no request (${asked.length - before})`);
			await pg.close();
		}

		// 3. "a status lookup shows the state and the reason"; 4. "a refused application shows how to apply again"
		{
			serve = 'planted';
			const pg = await open('#/publish', 'publish-steps');
			await statusDrawn(pg);
			const look = async (q, kind = 'found') => {
				await pg.getByTestId('status-query').fill(q);
				await pg.getByTestId('status-find').click();
				await pg.waitForFunction(([k, q]) => document.querySelector('[data-testid="status-result"]')?.dataset.kind === k && (k !== 'found' || [...document.querySelectorAll('[data-testid="status-result"] .hash')].some((h) => h.textContent.includes(q.replace(/^0x/, '').toLowerCase()))), [kind, q], { timeout: 30000 }).catch(() => {});
				return {
					states: (await pg.getByTestId('status-result').getByTestId('status-state').allInnerTexts()).map((t) => t.trim()),
					reasons: (await pg.getByTestId('status-result').getByTestId('status-reason').allInnerTexts()).map(flat),
					next: (await pg.getByTestId('status-result').getByTestId('status-next').allInnerTexts()).map(flat),
					text: flat(await pg.getByTestId('status-result').innerText())
				};
			};
			const refused = await look(H.refused.slice(0, 18));
			expect(
				JSON.stringify(refused.states) === '["refused"]' && refused.reasons.length === 1 && refused.reasons[0] === `rights-unclear: ${P.REASONS['rights-unclear']}` && refused.text.includes(H.refused) && refused.text.includes('2026-11-17'),
				`${at}: a status lookup shows the state and the reason: "refused", "rights-unclear" and its plain words, by the first 16 digits of the hash`
			);
			expect(
				refused.next.length === 1 && refused.next[0].includes('fix the reason named above') && refused.next[0].includes('apply with a new key') && refused.next[0].includes(`blocked until ${P.utcMinute(UNTIL)}`),
				`${at}: a refused application shows how to apply again: fix the reason, a new key, and the time the refused key is blocked until (${P.utcMinute(UNTIL)})`
			);
			const bare = await look(H.bare.slice(2, 12).toUpperCase());
			expect(
				JSON.stringify(bare.states) === '["refused"]' && bare.reasons[0]?.startsWith('bad-signature: ') && bare.next[0]?.includes('fix the reason named above') && !/blocked|new key/.test(bare.next[0] ?? 'blocked'),
				`${at}: a refusal that blocks no key names no key and no time (found without 0x, in capitals)`
			);
			const expired = await look(H.expired);
			const received = await look(H.received.slice(0, 10));
			const withdrawn = await look(H.withdrawn.slice(0, 10));
			expect(
				expired.states[0] === 'expired' && expired.next[0].includes('--reapply') && received.states[0] === 'received' && received.next[0].includes(`within ${P.DECIDE_DAYS} days`) && withdrawn.states[0] === 'withdrawn' && withdrawn.next[0].includes('closed'),
				`${at}: an expired application says to apply again with --reapply, a received one that the decision comes within ${P.DECIDE_DAYS} days, a withdrawn one that it is closed`
			);
			const paused = await look('tide-tables.open');
			expect(
				paused.states[0] === 'paused' && paused.reasons[0] === `no-capacity: ${P.REASONS['no-capacity']}` && paused.text.includes(P.PUBLISHER_STATES.paused),
				`${at}: a publisher's lookup shows its state and reason too ("paused", "no-capacity")`
			);
			const record = await look('river-levels.open');
			expect(record.states[0] === 'approved (record)' && record.text.includes('has not run'), `${at}: an approval only the record shows reads "approved (record)" and says the chain check has not run for it`);
			// An application that is not approved is under its hash only: its id finds nothing, and the list of publishers does not name it.
			const pill = flat(await pg.getByTestId('publish-status').locator('.pill').innerText());
			expect(pill === '3 publishers · 5 applications not approved' && (await pg.getByTestId('status-row').count()) === 3, `${at}: the box counts 3 publishers and 5 applications that are not approved (${pill})`);
			// Review S4-R1 M3: every value of the list is a text node, never markup or a link.
			const drawn = await pg.evaluate(() => {
				const box = document.querySelector('[data-testid="publish-status"]');
				const cell = (sel) => [...box.querySelectorAll(sel)].map((e) => ({ text: e.textContent, kids: e.childElementCount, tag: e.tagName, inLink: !!e.closest('a') }));
				return { links: box.querySelectorAll('a, [href], [src], img, iframe, script').length, origins: cell('[data-testid="status-origin"]'), ids: cell('[data-testid="status-id"]'), states: cell('[data-testid="status-row-state"]'), rows: [...box.querySelectorAll('[data-testid="status-row"]')].map((r) => r.innerText.replace(/\s+/g, ' ').trim()) };
			});
			const plain = (cells, values) => JSON.stringify(cells.map((c) => c.text)) === JSON.stringify(values) && cells.every((c) => c.kids === 0 && !c.inLink && c.tag !== 'A');
			expect(
				drawn.links === 0 && plain(drawn.origins, ['data.example.org/river-levels', 'tides.example.net']) && plain(drawn.ids, PLANTED.publishers.map((p) => p.publisher)) && plain(drawn.states, PLANTED.publishers.map((p) => p.state)),
				`${at}: each id, state and origin of the list is the element's own text, with no element inside it; the origin is never a link, and the status box holds no link or source at all (${drawn.links})`
			);
			expect(
				drawn.rows[2] === `old-gauges.open registered-before-record ${P.PUBLISHER_STATES[P.BEFORE_RECORD]}` && !drawn.rows[2].includes('0x'),
				`${at}: a planted publisher from before the record shows its id and state only (no date, key or transaction): "${drawn.rows[2]}"`
			);
			const old = await look('old-gauges.open');
			expect(old.states[0] === P.BEFORE_RECORD && old.text === `${P.BEFORE_RECORD} ${P.PUBLISHER_STATES[P.BEFORE_RECORD]} Publisher id old-gauges.open`, `${at}: its lookup shows the same and no more ("${old.text}")`);
			if (width === 390) {
				await look(H.refused);
				const E = await edges(pg);
				expect(E.side === 0 && E.past.length === 0 && E.inner.length === 0, `${at}: a lookup result with a full hash fits the phone (no sideways scroll, nothing past the right edge)${E.past.length ? ': ' + E.past.join('; ') : ''}`);
			}
			await pg.close();
		}

		// The pointer form (step 7): two values to this site's own node, the receipt, and every
		// other answer in the page's own words.
		{
			serve = 'planted';
			const pg = await open('#/publish', 'publish-steps');
			await statusDrawn(pg);
			const form = pg.getByTestId('pointer-form');
			const sent = () => posts.length;
			const press = async (kind) => {
				const n = sent();
				await pg.getByTestId('pointer-send').click();
				if (kind) await pg.waitForFunction((k) => document.querySelector('[data-testid="pointer-outcome"]')?.dataset.kind === k && !document.querySelector('[data-testid="pointer-send"]')?.disabled, kind, { timeout: 30000 }).catch(() => {});
				return {
					posted: sent() - n,
					kind: await pg.getByTestId('pointer-outcome').getAttribute('data-kind'),
					text: flat(await pg.getByTestId('pointer-outcome').innerText()),
					kids: await pg.getByTestId('pointer-outcome').evaluate((e) => e.querySelectorAll('a, [href], [src], img, iframe, script, b').length)
				};
			};
			const fill = async (origin, hash) => {
				await pg.getByTestId('pointer-origin').fill(origin);
				await pg.getByTestId('pointer-hash').fill(hash);
			};
			// Nothing was asked of a node before a press, and a value out of shape is not sent at all.
			expect(sent() === 0 && (await pg.getByTestId('pointer-outcome').getAttribute('data-kind')) === 'none', `${at}: the pointer form asks nothing of the node before its button is pressed`);
			await fill('https://Data.example.org/', 'not a hash');
			const unsent = await press(null);
			const problems = [flat(await pg.getByTestId('pointer-origin-problem').innerText()), flat(await pg.getByTestId('pointer-hash-problem').innerText())];
			expect(
				unsent.posted === 0 && problems[0] === PTR.originProblem('https://Data.example.org/') && problems[1] === PTR.hashProblem('not a hash') && (await pg.getByTestId('pointer-origin').getAttribute('aria-invalid')) === 'true',
				`${at}: an origin with https:// and a hash out of shape are named under their fields and nothing is sent`
			);
			// "the pointer form sends two values to this site and shows the receipt"
			node = 'created';
			await fill(` ${S.EXAMPLE_ORIGIN} `, H.received);
			const made = await press('receipt');
			const last = posts[posts.length - 1];
			expect(
				made.posted === 1 && last.method === 'POST' && last.url === origin + '/' + PTR.POINTER_PATH && last.body === JSON.stringify({ origin: S.EXAMPLE_ORIGIN, application: H.received }) && last.type === 'application/json' && last.cookie === null,
				`${at}: the pointer form sends two values to this site and shows the receipt: one POST to ${PTR.POINTER_PATH} of this origin, exactly {origin, application}, no cookie`
			);
			const receipt = await pg.getByTestId('pointer-receipt').evaluate((e) => ({ text: e.textContent, kids: e.childElementCount }));
			expect(
				made.kind === 'receipt' && receipt.text === RECEIPT.id && receipt.kids === 0 && made.text.includes('Your receipt:') && made.text.includes(PTR.receiptWords(PTR.parsePointerReceipt(RECEIPT))) && made.text.includes(`waits until ${PTR.utcMinute(RECEIPT.expiresUnix)}`),
				`${at}: the receipt id is shown as text (${receipt.text}), with the time the pointer waits until`
			);
			const where = flat(await pg.getByTestId('pointer-where').innerText());
			expect(
				where.includes('Application status') && where.includes('first digits of your application’s hash') && where.includes('The decision itself is not there') && where.includes('/v1/publishers/pointer/') && made.kids === 0,
				`${at}: the receipt says where the status is (the status box, by the application's hash) and that the node knows no decision; no link is built`
			);
			if (width === 390) {
				const E = await edges(pg);
				const fields = await form.locator('input, button').evaluateAll((es) => es.map((e) => e.getBoundingClientRect()).map((r) => [Math.round(r.height), Math.round(r.right)]));
				expect(E.side === 0 && E.past.length === 0 && E.inner.length === 0 && fields.every((f) => f[0] >= 44 && f[1] <= width - 16), `${at}: on a phone the form and its receipt fit: no sideways scroll, fields and button at least 44 px high (${JSON.stringify(fields)})${E.past.length ? ': ' + E.past.join('; ') : ''}`);
			}
			node = 'again';
			const again = await press('receipt');
			expect(again.posted === 1 && again.text.includes('The node already had this pointer') && again.text.includes(RECEIPT.id), `${at}: the same two values again show the same receipt, said as already held`);
			node = 'expired';
			const old = await press('receipt');
			expect(old.text.includes('no longer waits') && !old.text.includes('Received.'), `${at}: an expired pointer is said as expired, with what to do`);
			// "a node without the route is said plainly, with the issue as the way"
			for (const off of ['off', 'forbidden', 'none']) {
				node = off;
				const out = await press('off');
				expect(
					out.posted === 1 && out.kind === 'off' && out.text === PTR.OFF_WORDS && out.kids === 0 && (await pg.getByTestId('pointer-receipt').count()) === 0,
					`${at}: a node without the route is said plainly, with the issue as the way (${off === 'none' ? 'this static copy of the site, which has no node' : off === 'off' ? 'the node answers 404' : 'a proxy answers 403 with a page'})`
				);
			}
			expect((await pg.getByTestId('publish-issues').isVisible()) && (await pg.getByTestId('publish-issue-way').isVisible()), `${at}: the issue path stands right under the form`);
			// A refusal by its code: the page's own words, never the node's text.
			node = 'dropped';
			const gone = await press('receipt');
			expect(gone.text.includes('Dropped: the review found that the file at this origin does not have this hash') && !gone.text.includes('Received.'), `${at}: a pointer the reviewer dropped says so, with why and what to do`);
			node = 'busy';
			const busy = await press('refused');
			expect(
				busy.text === PTR.REFUSALS.pointer_origin_busy && busy.text.includes('You do not have to wait: file your application as a public issue') && (await pg.getByTestId('publish-issue-way').isVisible()),
				`${at}: a host whose three places are taken is said plainly, with the issue as the way that works at once`
			);
			for (const [mode, code] of [['shared', 'origin_shared_host'], ['quota', 'daily_quota'], ['cross', 'cross_site_refused']]) {
				node = mode;
				const out = await press('refused');
				expect(out.kind === 'refused' && out.text === PTR.REFUSALS[code] && out.kids === 0, `${at}: a refusal (${code}) is shown in the page's own words`);
			}
			// "a hostile answer is never shown"
			for (const [mode, want, words] of [
				['hostile-receipt', 'failed', PTR.BAD_ANSWER_WORDS],
				['hostile-extra', 'failed', PTR.BAD_ANSWER_WORDS],
				['hostile-html', 'failed', PTR.BAD_ANSWER_WORDS],
				['hostile-error', 'failed', PTR.otherWords(500)],
				['redirect', 'failed', PTR.UNREACHED_WORDS],
				['down', 'failed', PTR.UNREACHED_WORDS]
			]) {
				node = mode;
				const out = await press(want);
				const dom = await pg.evaluate(() => ({ img: document.querySelectorAll('main img, main script, main iframe').length, html: document.querySelector('[data-testid="pointer"]').innerHTML }));
				expect(
					out.kind === want && out.text === words && out.kids === 0 && dom.img === 0 && !/onerror|__xss|<img|<script/.test(dom.html) && (await pg.getByTestId('pointer-receipt').count()) === 0,
					`${at}: a hostile answer is never shown (${mode}): the page's own words, no receipt, no markup`
				);
			}
			const typed = [await pg.getByTestId('pointer-origin').inputValue(), await pg.getByTestId('pointer-hash').inputValue()];
			expect(typed[0] === ` ${S.EXAMPLE_ORIGIN} ` && typed[1] === H.received, `${at}: what was typed stays in the form after a failure, to send again`);
			const stored = await pg.evaluate(() => ({ local: localStorage.length, session: sessionStorage.length, cookie: document.cookie }));
			expect(stored.local === 0 && stored.session === 0 && stored.cookie === '', `${at}: the form keeps nothing in the browser (no storage, no cookie)`);
			node = 'none';
			await pg.close();
		}

		// A missing, broken or hostile list: a plain message, no state, no lookup, nothing run.
		for (const kind of ['missing', 'html', 'broken', 'hostile']) {
			serve = kind;
			const pg = await open('#/publish', 'publish-steps');
			await statusDrawn(pg);
			const box = flat(await pg.getByTestId('publish-status').innerText());
			const drawn = await pg.evaluate(() => ({ img: document.querySelectorAll('main img').length, rows: document.querySelectorAll('[data-testid="status-row"]').length, form: document.querySelectorAll('[data-testid="status-form"]').length, steps: document.querySelectorAll('[data-testid="publish-step"]').length }));
			expect(
				(await pg.getByTestId('status-failed').count()) === 1 && box.includes('The status list could not be read') && box.includes('says nothing about any application') && box.includes('Status list not available') && drawn.rows === 0 && drawn.form === 0 && drawn.img === 0 && drawn.steps === 10 && !box.includes('onerror') && !box.includes('river-levels'),
				`${at}: a ${kind} status list is a plain message: no publisher, no state and no lookup shown, the ten steps still there`
			);
			await pg.close();
		}
		serve = 'real';

		// The page asks only this site, and of its data only the static status list.
		const away = [...new Set(asked.filter((u) => !u.startsWith(origin + '/') && !u.startsWith('data:') && !u.startsWith('blob:')))];
		const toNode = asked.filter((u) => new URL(u).pathname.startsWith('/v1/'));
		const lists = asked.filter((u) => u.endsWith('/publishers.json'));
		expect(away.length === 0, `${at}: no request leaves this site${away.length ? ': ' + away.slice(0, 3).join(' ') : ''}`);
		expect(
			toNode.length === posts.length && posts.length === 17 && toNode.every((u) => u === origin + '/' + PTR.POINTER_PATH) && posts.every((p) => p.method === 'POST'),
			`${at}: the only requests to a node are the form's own POSTs to this site's ${PTR.POINTER_PATH}, one per press (${posts.length}); no visit asks a node by itself`
		);
		expect(lists.length === 8 && lists.every((u) => u === origin + '/publishers.json'), `${at}: each visit of the publish page reads the static publishers.json once (${lists.length} in 8 visits)`);
		expect(errors.length === 0, `${at}: no page error, no policy refusal, no script from a status list${errors.length ? ': ' + errors.join('; ') : ''}`);
		await ctx.close();
	}
}

if (import.meta.url === `file://${process.argv[1]}`) {
	const { chromium } = await import('playwright-core');
	const { serve } = await import('./serve.mjs');
	const root = process.argv[2] ?? new URL('../build/', import.meta.url).pathname;
	const server = await serve(0, root);
	const browser = await chromium.launch({ channel: 'chrome' });
	const failures = [];
	try {
		await publish(browser, `http://127.0.0.1:${server.address().port}/`, root, (ok, msg) => {
			console.log(`${ok ? 'ok  ' : 'FAIL'} ${msg}`);
			if (!ok) failures.push(msg);
		});
	} finally {
		await browser.close();
		server.close();
	}
	process.exitCode = failures.length ? 1 : 0;
}
