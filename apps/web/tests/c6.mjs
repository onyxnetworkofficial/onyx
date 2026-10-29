// U1: historical "home" verification assertions now target #/how-it-works. Keep their
// names stable for THREATS citations; tests/product.mjs covers the new contributor homepage.
// ROADMAP C6 (and C9): level-5 track records on the Data page, in a real browser. Called by tests/e2e.mjs.
// No real record exists yet: the section must say so and why, and show only the example, labelled
// example everywhere and checked by the Rust verifier (pinned commitment, whole history
// recounted). A cut history or a record relabelled as real must show nothing. Every wait throws
// on timeout (wait.mjs); figures are compared with the page's own checked history, never literals.
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { settled } from './wait.mjs';

const FILE = '**/level5/track-example.json';

async function open(browser, base, doc) {
	const pg = await browser.newPage();
	if (doc) await pg.route(FILE, (r) => r.fulfill({ json: doc }));
	await pg.goto(base + '#/data');
	await settled(pg);
	const s = pg.getByTestId('track');
	await s.scrollIntoViewIfNeeded();
	// Data review: the section is folded by default.
	await s.getByTestId('track-fold').locator('> summary').click();
	return { pg, s };
}

export async function c6(browser, base, root, expect) {
	const shipped = JSON.parse(await readFile(join(root, 'level5/track-example.json'), 'utf8'));
	{
		const { pg, s } = await open(browser, base);
		// Data review: the checked example's badge is a neutral "Example" (the helper opens the fold).
		await s.locator('.pill', { hasText: /^Example$/ }).waitFor({ timeout: 120000 });
		const empty = await pg.getByTestId('track-empty').innerText();
		expect(/No real track records yet/.test(empty) && /closing-time rule/.test(empty), 'C6 track records: the section says no real record exists and why');
		expect(
			/Bitcoin entity flows/.test(empty) && /no source can be scored/.test(empty),
			'C9 track records: the empty state names the one real rule (Bitcoin flows) and says no source is scored yet'
		);
		const sources = Object.keys(shipped.records);
		const cards = s.locator('[data-testid^="track-ex-"]');
		const n = await cards.count();
		let tagged = 0;
		for (let i = 0; i < n; i++) if ((await cards.nth(i).locator('.ex-tag').innerText()).trim().toLowerCase() === 'example') tagged++;
		expect(
			n === sources.length && tagged === n && (await pg.getByTestId('track-example-tag').innerText()).toLowerCase().includes('example'),
			'C6 track records: the example checks out and every card is labelled example'
		);
		// The display rules (proof-ladder.md "Display"): a rate only when established.
		const views = [];
		for (const src of sources) {
			const card = pg.getByTestId(`track-ex-${src}`);
			const head = (await pg.getByTestId(`track-headline-${src}`).innerText()).replace(/\s+/g, ' ').trim();
			const m = /^Agreed (\d+) of (\d+) resolved(?: \((\d+\.\d)%\))?$/.exec(head);
			await card.locator('summary').click();
			const rows = await card.locator('tbody tr').evaluateAll((trs) => trs.map((tr) => [...tr.children].map((td) => td.textContent.trim())));
			views.push({ src, m, threshold: await pg.getByTestId(`track-threshold-${src}`).count(), brier: await pg.getByTestId(`track-brier-${src}`).count(), rows });
		}
		const est = views.filter((v) => v.m?.[3]);
		const below = views.filter((v) => v.m && !v.m[3]);
		expect(
			est.length === 1 && below.length === 1 && est[0].threshold === 0 && est[0].brier === 1 && below[0].threshold === 1 && below[0].brier === 0,
			'C6 track records: the established example shows a rate, the other shows counts and the threshold only'
		);
		const count = (rows, ...o) => rows.filter((r) => o.includes(r[2])).length;
		expect(
			views.every((v) => v.m && +v.m[1] === count(v.rows, 'agreed') && +v.m[2] === count(v.rows, 'agreed', 'disagreed', 'equivocated')),
			'C6 track records: the headline counts match the checked history'
		);
		// C9: skill against repeating the last known value exists only for period facts; the example's
		// metric is a point value, so no card shows one.
		expect(
			(await s.locator('[data-testid^="track-skill-"]').count()) === 0,
			'C9 track records: no skill line on the example (its metric has no period, so no persistence baseline)'
		);
		const text = await s.innerText();
		expect(!/\bverified\b|\bproven\b/i.test(text), 'C6 track records: the words verified and proven do not appear in the section');
		// Review TR-S-R2-2: the Data map does not call label track records live while no labeler is scored.
		const cells = await pg.getByTestId('data-map').locator('[role=cell]').evaluateAll((els) => els.map((e) => e.textContent ?? ''));
		const label = cells.find((c) => c.includes('Label track records')) ?? '';
		expect(/\bBuilt\b/.test(label) && !/\bLive\b/.test(label) && /no labeler scored yet/.test(label), 'C6 track records: the data map says label track records are built, none scored yet');
		await pg.close();
	}
	{
		// Review TR-S-R2-5: on a 390 px phone the example's history tables fit their boxes.
		const pg = await browser.newPage({ viewport: { width: 390, height: 844 } });
		await pg.goto(base + '#/data');
		await settled(pg);
		const s = pg.getByTestId('track');
		// Data review: the section is folded by default and the checked example's badge is a neutral "Example".
		await s.getByTestId('track-fold').locator('> summary').click();
		await s.locator('.pill', { hasText: /^Example$/ }).waitFor({ timeout: 120000 });
		await s.locator('details').evaluateAll((ds) => ds.forEach((d) => (d.open = true)));
		const boxes = await s.locator('.tablebox').evaluateAll((bs) => bs.map((b) => [b.scrollWidth, b.clientWidth]));
		expect(boxes.length === 2 && boxes.every(([sw, cw]) => cw > 0 && sw <= cw + 1), 'C6 track records: at 390 px the history tables fit without scrolling sideways');
		await pg.close();
	}
	{
		// Review TR-S-R2-2: the Home ladder's level 5 says built, no real records, and is not marked live.
		const pg = await browser.newPage();
		await pg.goto(base + '#/how-it-works');
		await settled(pg);
		const rung = pg.locator('.rung', { hasText: 'Scored when the truth arrives later' });
		await rung.waitFor({ timeout: 120000 });
		const st = (await rung.locator('.lstat').innerText()).trim();
		expect(st === 'Built; no real records yet' && !(await rung.evaluate((e) => e.classList.contains('live'))), 'C6 track records: the home ladder says level 5 is built with no real records yet');
		await pg.close();
	}
	{
		const cut = structuredClone(shipped);
		const src = Object.keys(cut.records).at(-1);
		cut.records[src].history.entries.pop();
		const { pg, s } = await open(browser, base, cut);
		await pg.getByTestId('track-failed').waitFor({ timeout: 120000 });
		expect((await s.locator('[data-testid^="track-ex-"]').count()) === 0, 'C6 track records: a cut history fails the check and nothing is shown');
		await pg.close();
	}
	{
		const real = structuredClone(shipped);
		real.records[Object.keys(real.records)[0]].example = false;
		const { pg, s } = await open(browser, base, real);
		await pg.getByTestId('track-failed').waitFor({ timeout: 120000 });
		expect((await s.locator('[data-testid^="track-ex-"]').count()) === 0, 'C6 track records: an example record relabelled as real fails the check');
		await pg.close();
	}
	{
		// ROADMAP C20 item 4: the browser remembers the commitment it saw last per record and, on the
		// next visit, asks with it (the site's since file stands in for the server's `?since=` answers).
		const ctx = await browser.newContext();
		const pg = await ctx.newPage();
		await pg.goto(base + '#/data');
		await settled(pg);
		const s = pg.getByTestId('track');
		await s.scrollIntoViewIfNeeded();
		// Data review: the section is folded by default and the checked example's badge is a neutral "Example".
		await s.getByTestId('track-fold').locator('> summary').click();
		await s.locator('.pill', { hasText: /^Example$/ }).waitFor({ timeout: 120000 });
		const first = (await pg.getByTestId('track-since').innerText()).trim();
		const stored = await pg.evaluate(() => Object.keys(localStorage).filter((k) => k.startsWith('onyx-track-seen-v1:')).map((k) => JSON.parse(localStorage.getItem(k) ?? '{}')));
		const pinned = stored[0]?.commitment;
		expect(
			/First visit/.test(first) && stored.length === 2 && stored.every((v) => v.commitment === pinned && /^0x[0-9a-f]{64}$/.test(pinned)),
			'C20 track records: a first visit checks without since and remembers the commitment per record'
		);
		const sinceLoads = [];
		pg.on('request', (r) => r.url().includes('track-example-since.json') && sinceLoads.push(r.url()));
		await pg.reload();
		await settled(pg);
		await pg.getByTestId('track').scrollIntoViewIfNeeded();
		// Data review: the section is folded by default and the checked example's badge is a neutral "Example".
		await pg.getByTestId('track').getByTestId('track-fold').locator('> summary').click();
		await pg.getByTestId('track').locator('.pill', { hasText: /^Example$/ }).waitFor({ timeout: 120000 });
		const again = (await pg.getByTestId('track-since').innerText()).trim();
		expect(
			sinceLoads.length === 1 && /asked with the commitment it saw last/.test(again) && again.includes(pinned.slice(0, 10)),
			'C20 track records: the next visit asks with the commitment it saw last and checks the chain back to it'
		);
		// A commitment this server never served (another server, or a rewritten one): nothing is shown.
		await pg.evaluate(() => {
			for (const k of Object.keys(localStorage)) if (k.startsWith('onyx-track-seen-v1:')) localStorage.setItem(k, JSON.stringify({ commitment: '0x' + '9'.repeat(64), as_of: 1 }));
		});
		await pg.reload();
		await settled(pg);
		await pg.getByTestId('track-fold').locator('> summary').click();
		await pg.getByTestId('track-unknown').waitFor({ timeout: 120000 });
		expect(
			(await pg.getByTestId('track').locator('[data-testid^="track-ex-"]').count()) === 0,
			'C20 track records: a remembered commitment the server does not know shows nothing and says so'
		);
		await pg.getByTestId('track-forget').click();
		await pg.getByTestId('track-since').waitFor({ timeout: 120000 });
		expect(/First visit/.test(await pg.getByTestId('track-since').innerText()), 'C20 track records: forgetting what the browser saw checks again from scratch');
		// ROADMAP C20 item 1: the example's logs rerun in the browser to the pinned commitment.
		await pg.getByTestId('track-rerun').click();
		const res = pg.getByTestId('track-rerun-result');
		await res.waitFor({ timeout: 120000 });
		const rerun = (await res.innerText()).replace(/\s+/g, ' ');
		expect(
			/recomputed both records from the logs \(\d+ statements, \d+ answers\)/.test(rerun) && /2 commitments checked/.test(rerun),
			'C20 track records: the browser reruns the example logs and gets the pinned commitment again'
		);
		await ctx.close();
	}
	{
		// A changed byte in the logs (the last answer's public_from) fails the browser's check: the log
		// no longer hashes to its commitment, before any rerun.
		const log = JSON.parse(await readFile(join(root, 'level5/track-example-log.json'), 'utf8'));
		const r = log.resolutions;
		log.resolutions = r.slice(0, -1) + (r.endsWith('0') ? '1' : '0');
		const pg = await browser.newPage();
		await pg.route('**/level5/track-example-log.json', (rt) => rt.fulfill({ json: log }));
		await pg.goto(base + '#/data');
		await settled(pg);
		const s = pg.getByTestId('track');
		await s.scrollIntoViewIfNeeded();
		// Data review: the section is folded by default and the checked example's badge is a neutral "Example".
		await s.getByTestId('track-fold').locator('> summary').click();
		await s.locator('.pill', { hasText: /^Example$/ }).waitFor({ timeout: 120000 });
		await pg.getByTestId('track-rerun').click();
		const res = pg.getByTestId('track-rerun-result');
		await res.waitFor({ timeout: 120000 });
		expect(/did not check/.test(await res.innerText()), 'C20 track records: a changed byte in the logs fails the browser check');
		await pg.close();
	}
	{
		// ROADMAP C23: the served registry is rebuilt and checked to the commitment's registry root
		// before the rerun. A registry naming another root is refused and nothing is recomputed.
		// The registry's bytes change (entity "example-f" becomes "example-g"), so it still decodes and
		// rebuilds, to another root.
		const reg = JSON.parse(await readFile(join(root, 'level5/track-example-registry.json'), 'utf8'));
		const f = '6578616d706c652d66';
		reg.registry = reg.registry.replace(f, '6578616d706c652d67');
		const pg = await browser.newPage();
		await pg.route('**/level5/track-example-registry.json', (rt) => rt.fulfill({ json: reg }));
		await pg.goto(base + '#/data');
		await settled(pg);
		const s = pg.getByTestId('track');
		await s.scrollIntoViewIfNeeded();
		// Data review: the section is folded by default and the checked example's badge is a neutral "Example".
		await s.getByTestId('track-fold').locator('> summary').click();
		await s.locator('.pill', { hasText: /^Example$/ }).waitFor({ timeout: 120000 });
		await pg.getByTestId('track-rerun').click();
		const res = pg.getByTestId('track-rerun-result');
		await res.waitFor({ timeout: 120000 });
		const text = await res.innerText();
		expect(
			/did not check/.test(text) && /registry/.test(text),
			'C23 track records: a served registry that does not hash to the commitment’s registry root is refused before the rerun'
		);
		await pg.close();
	}
}
