// Pre-public loop PP4 (PRODUCT-PLAN.md U5): Browse data opens with the datasets first: one card
// per entry of src/lib/catalog.ts, each with its proof level, source, unit and rights; a card's
// button brings its section into view and focus; a rights-fallback build marks the label-derived
// datasets as held back; PP11: each card shows how fresh its data is. The section opens with the way
// to every dataset (a search and one button to All datasets, with the node's count), and a card whose
// values only the hosted node serves reads one from it (a fake node here: page.route). Called by
// tests/e2e.mjs with a built site.
import { readFile } from 'node:fs/promises';
const C = await import(new URL('../src/lib/catalog.ts', import.meta.url).href);

export async function pp4(browser, base, expect, FB) {
	for (const width of [1280, 390]) {
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
		const pg = await ctx.newPage();
		// The node, faked: its dataset count, one settled value, and one route that does not answer.
		const TGA = 'us.treasury.tga_closing_balance';
		const RAIN = 'noaa.ghcnd.usw00094728.prcp';
		const asked = [];
		let nodeUp = false;
		await pg.route('**/v1/bulk?*', (route) => (nodeUp ? route.fulfill({ json: { datasets: [], total: 9054, next: null } }) : route.fulfill({ status: 404, body: '' })));
		await pg.route('**/v1/observations/*', (route) => {
			const u = new URL(route.request().url());
			asked.push(u.pathname + u.search);
			if (u.pathname.endsWith('/' + TGA)) return route.fulfill({ json: { metric: TGA, subjectText: u.searchParams.get('subject'), value: '1052530', proofLevel: 2 } });
			return route.fulfill({ status: 503, json: { error: { code: 'unavailable' } } });
		});
		await pg.goto(base + '#/data');
		const cat = pg.getByTestId('catalog');
		await cat.waitFor({ timeout: 60000 });
		// The full list comes first: a search and one button to All datasets, then the worked examples.
		const top = async () =>
			pg.evaluate(() => {
				const q = (id) => document.querySelector(`[data-testid="${id}"]`);
				const before = (a, b) => !!a && !!b && !!(a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING);
				const sub = document.getElementById('catalog-examples');
				const card = q('catalog-item');
				return {
					all: q('catalog-all')?.textContent.trim(),
					href: q('catalog-all')?.getAttribute('href'),
					hint: q('catalog-q')?.getAttribute('placeholder'),
					order: before(q('catalog-q'), q('catalog-all')) && before(q('catalog-all'), sub) && before(sub, card),
					sub: sub?.textContent.trim(),
					lede: q('catalog-lede')?.textContent.replace(/\s+/g, ' ').trim(),
					tall: q('catalog-all')?.getBoundingClientRect().height >= 44 && q('catalog-go')?.getBoundingClientRect().height >= 44
				};
			});
		const T0 = await top();
		expect(T0.all === 'Browse all datasets' && T0.href === '#/datasets' && T0.hint === 'Search datasets', `PP4 ${width}: without the node's count the button still leads to All datasets (${JSON.stringify(T0)})`);
		expect(T0.order && T0.sub === 'Worked examples' && T0.lede.startsWith(`${C.DATASETS.length} datasets worked through on this page, each with the proof level it states.`) && (width > 1000 || T0.tall), `PP4 ${width}: the search and the button come first (phone targets 44 px), the cards under "Worked examples" (${JSON.stringify(T0)})`);
		nodeUp = true;
		await pg.reload();
		await cat.waitFor({ timeout: 60000 });
		await pg.waitForFunction(() => /9,054/.test(document.querySelector('[data-testid="catalog-all"]')?.textContent ?? ''), null, { timeout: 20000 }).catch(() => {});
		const T1 = await top();
		expect(T1.all === 'Browse all 9,054 datasets' && T1.hint === 'Search 9,054 datasets', `PP4 ${width}: the button and the search say the node's own count (${T1.all}; ${T1.hint})`);
		const cards = await pg.getByTestId('catalog-item').evaluateAll((els) => els.map((e) => ({ id: e.dataset.id, text: e.innerText.replace(/\s+/g, ' ') })));
		const want = C.DATASETS.map((d) => d.id);
		expect(JSON.stringify(cards.map((c) => c.id)) === JSON.stringify(want), `PP4 ${width}: Browse data lists all ${want.length} datasets first (${cards.length} cards)`);
		const bad = cards.filter((c) => {
			const d = C.DATASETS.find((x) => x.id === c.id);
			return !d || !c.text.includes(`Level ${d.level} · ${C.LEVEL_NAME[d.level]}`) || !c.text.includes(d.source) || !c.text.includes(d.unit) || !c.text.includes(d.rights);
		});
		expect(bad.length === 0, `PP4 ${width}: every card shows its level, source, unit and rights (${bad.map((b) => b.id).join(', ') || 'all'})`);
		// PP11: each card shows the newest date its shipped data holds, read from data/freshness.json
		// (scripts/gen-freshness.py; check-all keeps it current); the reserves card has none.
		const fresh = C.parseFreshness(JSON.parse(await readFile(new URL('../static/data/freshness.json', import.meta.url), 'utf8')));
		await pg.waitForFunction(() => [...document.querySelectorAll('[data-testid="catalog-asof"]')].every((e) => !e.querySelector('.sr-only')), null, { timeout: 20000 }).catch(() => {});
		const dates = await pg.getByTestId('catalog-item').evaluateAll((els) => els.map((e) => ({ id: e.dataset.id, asof: e.querySelector('[data-testid="catalog-asof"]')?.innerText.trim() ?? null })));
		const wrongDate = dates.filter((x) => {
			const d = C.DATASETS.find((y) => y.id === x.id);
			if (d.noDate) return x.asof !== null;
			const f = fresh[x.id];
			// Two records of one series: the page's day and the hosted node's, each saying which.
			const want = f?.nodeAsOf ? `${C.dayText(f.asOf)} (${f.what}, the round on this page)\n${C.dayText(f.nodeAsOf)} (the round the hosted test node serves)` : f && `${C.dayText(f.asOf)} (${f.what})`;
			return !f || x.asof.replace(/\s*\n\s*/g, '\n') !== want;
		});
		// Newcomer test 2026-11-18: the earthquake card said 8 Jul 2026 while the node serves the round
		// of 19 Jul 2026. Both are true (two rounds), so the card names both; the node's day is the
		// subject in the shipped ladders, the same one "Show the value" cards and the AI connector use.
		const nodeDays = C.nodeSubjects(JSON.parse(await readFile(new URL('../static/level2/ladders-base-sepolia.json', import.meta.url), 'utf8')));
		const QUAKES = 'usgs.earthquakes.m5_count';
		const two = Object.entries(fresh).filter(([, f]) => f.nodeAsOf);
		expect(fresh[QUAKES]?.nodeAsOf === nodeDays[QUAKES].split(':').pop() && fresh[QUAKES].nodeAsOf !== fresh[QUAKES].asOf && two.every(([id, f]) => f.nodeAsOf === nodeDays[id]?.split(':').pop()) && (await pg.getByTestId('catalog-asof-node').count()) === two.length, `PP11 ${width}: a card whose node round is of another day names both days (${two.map(([id, f]) => `${id}: ${f.asOf} here, ${f.nodeAsOf} on the node`).join('; ') || 'none'})`);
		expect(wrongDate.length === 0 && Object.keys(fresh).length === C.DATASETS.filter((d) => !d.noDate).length, `PP11 ${width}: every card but the reserves one shows its data's date (${wrongDate.map((w) => `${w.id}=${w.asof}`).join(', ') || dates.filter((d) => d.asof).length + ' dated'})`);
		const firstSection = await pg.evaluate(() => document.querySelector('main section.section')?.dataset.testid);
		expect(firstSection === 'catalog', `PP4 ${width}: the datasets come before the charts (first section: ${firstSection})`);
		const held = await pg.getByTestId('catalog-held').count();
		const wantHeld = FB ? C.DATASETS.filter((d) => d.labelDerived).length : 0;
		expect(held === wantHeld, `PP4 ${width}: ${wantHeld} label-derived datasets are marked held back in this build (${held})`);
		// A section high on the page in every build (a short fallback page cannot scroll a section near
		// its end up to the top half of the screen).
		await pg.locator('[data-id="btc.chain-stats"] button').click();
		const landed = await pg.waitForFunction(() => {
			const s = document.querySelector('[data-testid="onchain"]');
			const r = s?.getBoundingClientRect();
			return !!r && r.top >= -2 && r.top < innerHeight / 2 && document.activeElement === s.querySelector('h2');
		}, null, { timeout: 10000 }).then(() => true, () => false);
		expect(landed, `PP4 ${width}: a card's button brings its section into view and moves focus to its heading`);
		// No card is a dead end: each has a button, a link or, when held back, the note that says so.
		const ends = await pg.getByTestId('catalog-item').evaluateAll((els) => els.filter((e) => !e.querySelector('button, a.open, [data-testid="catalog-held"]')).map((e) => e.dataset.id));
		const fromNode = C.DATASETS.filter(C.readsFromNode);
		expect(ends.length === 0 && (await pg.getByTestId('catalog-node-read').count()) === fromNode.length, `PP4 ${width}: every card leads to its values; the ${fromNode.length} served by the hosted node have "Show the value" (${ends.join(', ') || 'none without'})`);
		// Opening the page asks the node for its count only: no card reads a value before its button.
		expect(asked.length === 0, `PP4 ${width}: no card asks the node before its button is pressed (${asked.join(', ') || 'none asked'})`);
		const subjects = C.nodeSubjects(JSON.parse(await readFile(new URL('../static/level2/ladders-base-sepolia.json', import.meta.url), 'utf8')));
		const day = (m) => C.dayText(subjects[m].split(':').pop());
		const tga = pg.locator(`[data-id="${TGA}"]`);
		await tga.getByTestId('catalog-node-read').click();
		await tga.getByTestId('catalog-node-value').waitFor({ timeout: 15000 }).catch(() => {});
		const V = {
			value: (await tga.getByTestId('catalog-node-value').allInnerTexts()).join(' ').replace(/\s+/g, ' ').trim(),
			full: await tga.getByTestId('catalog-node-full').getAttribute('href').catch(() => null),
			note: (await tga.locator('.node .fine').allInnerTexts()).join(' ')
		};
		expect(V.value === `1,052,530 USD millions · ${day(TGA)}` && V.full === `/v1/observations/${TGA}?subject=${subjects[TGA]}` && asked.includes(V.full), `PP4 ${width}: "Show the value" reads the card's settled round from the node and shows it with its unit and day (${JSON.stringify(V)})`);
		expect(/Your browser did not check it/.test(V.note), `PP4 ${width}: the value is said to be the node's answer, not a checked one`);
		const rain = pg.locator(`[data-id="${RAIN}"]`);
		await rain.getByTestId('catalog-node-read').click();
		await rain.getByTestId('catalog-node-curl').waitFor({ timeout: 15000 }).catch(() => {});
		const curl = (await rain.getByTestId('catalog-node-curl').allInnerTexts()).join(' ').trim();
		expect(curl === `curl -s "${new URL(base).origin}/v1/observations/${RAIN}?subject=${subjects[RAIN]}"` && (await rain.getByTestId('catalog-node-value').count()) === 0, `PP4 ${width}: a node that does not answer shows no value, and the command that reads it (${curl})`);
		const over = await pg.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1);
		expect(!over, `PP4 ${width}: the dataset cards do not scroll the page sideways`);
		// The search sends the visitor to All datasets with the words as the node takes them.
		await pg.getByTestId('catalog-q').fill('  ETH   Supply ');
		await pg.getByTestId('catalog-q').press('Enter');
		await pg.getByTestId('datasets-q').waitFor({ timeout: 30000 }).catch(() => {});
		// The page fills the box from the address after the box is drawn: wait for the words.
		await pg.waitForFunction(() => (document.querySelector('[data-testid="datasets-q"]')?.value ?? '') !== '', null, { timeout: 15000 }).catch(() => {});
		const G = { hash: await pg.evaluate(() => location.hash), q: await pg.getByTestId('datasets-q').inputValue().catch(() => null) };
		expect(G.hash === '#/datasets?q=eth+supply' && G.q === 'eth supply', `PP4 ${width}: the search opens All datasets with the words searched (${JSON.stringify(G)})`);
		await ctx.close();
	}
}
