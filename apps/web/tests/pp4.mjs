// Pre-public loop PP4 (PRODUCT-PLAN.md U5): Browse data opens with the datasets first: one card
// per entry of src/lib/catalog.ts, each with its proof level, source, unit and rights; a card's
// button brings its section into view and focus; a rights-fallback build marks the label-derived
// datasets as held back; PP11: each card shows how fresh its data is. Called by tests/e2e.mjs with a built site.
import { readFile } from 'node:fs/promises';
const C = await import(new URL('../src/lib/catalog.ts', import.meta.url).href);

export async function pp4(browser, base, expect, FB) {
	for (const width of [1280, 390]) {
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
		const pg = await ctx.newPage();
		await pg.goto(base + '#/data');
		const cat = pg.getByTestId('catalog');
		await cat.waitFor({ timeout: 60000 });
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
			return !f || x.asof !== `${C.dayText(f.asOf)} (${f.what})`;
		});
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
		const over = await pg.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1);
		expect(!over, `PP4 ${width}: the dataset cards do not scroll the page sideways`);
		await ctx.close();
	}
}
