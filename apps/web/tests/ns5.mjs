// e2e "NS5 ladder" (ADR-0048): each dataset card shows its proof level as a badge, and the two
// Base Sepolia values (ROADMAP B6) show their level history as the client check accepted it.
// A ladder that falls or states a level its steps do not reach is refused and shows no history.
// Called by tests/e2e.mjs with a built site; checks the page at 1280 and 390 px.
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
const L = await import(new URL('../src/lib/ladder.ts', import.meta.url).href);

export async function ns5(browser, base, root, expect) {
	const file = JSON.parse(await readFile(join(root, 'level2/ladders-base-sepolia.json'), 'utf8'));
	expect(file.values.length === 2 && file.values.every((v) => L.checkLadder(v.ladder, 2).length === 0), 'NS5 ladder: the shipped ladders pass the check');
	// Free public-source values: their ladders ship whole in every build (THREATS NS5-8).
	const sealed = !!file.values[0].ladder.sealed;
	expect(!sealed, 'NS5 ladder: the ladders of free public-source values ship unsealed');
	const src = JSON.parse(await readFile(new URL('../static/level2/ladders-base-sepolia.json', import.meta.url), 'utf8'));
	expect(src.values.every((v) => !v.ladder.sealed && v.ladder.value && v.ladder.steps.every((s) => s.evidence)), 'NS5 ladder: the source file holds the full ladders');
	if (sealed) expect(!JSON.stringify(file).includes(src.values[0].ladder.steps[0].evidence), 'NS5 ladder: a public build ships no result hash in the ladders');
	// The check itself: a rise passes; a fall, a lifted level and a repeat are refused.
	const one = src.values[0].ladder;
	const step = (level, at, ev = '0x' + '1'.repeat(64)) => ({ level, name: L.LEVEL_SLUG[level], evidence: ev, at, record: one.steps[0].record });
	const rise = { value: one.value, level: 2, steps: [step(4, 1), step(2, 2)] };
	const fall = { value: one.value, level: 4, steps: [step(2, 1), step(4, 2)] };
	const repeat = { value: one.value, level: 2, steps: [step(2, 1), step(2, 2)] };
	const lifted = structuredClone(one);
	lifted.level = 1;
	lifted.steps[0].level = 1;
	lifted.steps[0].name = 'recomputed';
	expect(L.checkLadder(rise, 2).length === 0, 'NS5 ladder: a rise from level 4 to 2 passes');
	expect(L.checkLadder(fall).length > 0 && L.checkLadder(repeat).length > 0, 'NS5 ladder: a fall and a repeat are refused');
	expect(L.checkLadder(lifted, 2).length > 0 && L.checkLadder({ ...one, level: 1 }, 2).length > 0, 'NS5 ladder: a lifted level is refused');
	// C64 (ADR-0048 amendment 2): contradictions and the memory of an earlier visit.
	const contra = (level) => ({ ...step(level, 3, '0x' + '2'.repeat(64)), value: '0x' + '3'.repeat(64) });
	expect(L.checkLadder({ ...one, contradictions: [contra(1)] }, 2).length === 0, 'NS5 ladder: a stronger contradiction passes and keeps the level');
	expect(L.checkLadder({ ...one, contradictions: [contra(3)] }, 2).length > 0, 'NS5 ladder: a weaker "contradiction" is refused');
	const seen = { value: one.value, level: 2, steps: [step(4, 1), one.steps[0]] };
	expect(L.checkExtends(seen, seen).length === 0 && L.checkExtends(seen, one).length > 0, 'NS5 ladder: a ladder that drops a step seen before is refused');
	const found = { value: one.value, level: 1, steps: [step(4, 1), step(1, one.steps[0].at)] };
	const late = { value: one.value, level: 1, steps: [step(4, 1), step(1, one.steps[0].at + 1)] };
	expect(L.checkExtends(seen, found).length === 0 && L.checkExtends(seen, late).length > 0, 'NS5 ladder: a stronger step no later than a dropped one supersedes it; a later one does not');

	for (const width of [1280, 390]) {
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
		const pg = await ctx.newPage();
		await pg.goto(base + '#/data');
		await pg.getByTestId('ladders').waitFor({ timeout: 60000 });
		for (const v of file.values) {
			const badge = await pg.getByTestId(`ladder-${v.id}-badge`).innerText();
			expect(badge === 'Level 2 · Re-observed', `NS5 ${width}: the ${v.id} value shows its level badge (${badge})`);
			const hist = pg.getByTestId(`ladder-${v.id}-history`);
			await hist.locator('summary').click();
			const steps = await pg.getByTestId(`ladder-${v.id}-step`).allInnerTexts();
			const s = v.ladder.steps[0];
			const iso = new Date(s.at * 1000).toISOString();
			const day = `${+iso.slice(8, 10)} ${['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'][+iso.slice(5, 7) - 1]} ${iso.slice(0, 4)}`;
			expect(
				steps.length === 1 && steps[0].includes(day) && steps[0].includes('observation recorded in the registry'),
				`NS5 ${width}: the ${v.id} history shows its one step, ${day}, recorded in the registry`
			);
			if (sealed) {
				expect(await pg.getByTestId(`ladder-${v.id}-sealed`).isVisible(), `NS5 ${width}: the sealed ${v.id} step says the paid API carries its evidence`);
			} else {
				const ev = await hist.locator('.hash').getAttribute('title');
				expect(ev === s.evidence, `NS5 ${width}: the ${v.id} step names its round's result hash`);
			}
		}
		const cards = await pg.getByTestId('catalog-item').evaluateAll((els) => els.map((e) => e.innerText));
		expect(cards.length > 0 && cards.every((t) => /Level [1-5] · /.test(t)), `NS5 ${width}: every dataset card shows its level badge`);
		// THREATS NS5-13: nothing on the card checks the catalog's level, so the badge says so.
		// Owner review: the cards' lede says the levels are as stated; each badge keeps it in data-stated and its tooltip.
		const stated = await pg.getByTestId('catalog-item').evaluateAll((els) => els.map((e) => e.querySelector('[data-stated]')?.getAttribute('title') ?? ''));
		expect(stated.every((t) => t.startsWith('The level this dataset states')) && (await pg.getByTestId('catalog-lede').innerText()).includes('the proof level it states'), 'NS5 ladder: a badge without a check says stated');
		const over = await pg.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1);
		expect(!over, `NS5 ${width}: the level history does not scroll the page sideways`);
		await ctx.close();
	}

	// A served ladder whose level is lifted: refused, no history.
	const forged = await browser.newPage();
	const bad = structuredClone(file);
	bad.values[0].ladder = lifted;
	await forged.route('**/level2/ladders-base-sepolia.json', (r) => r.fulfill({ json: bad }));
	await forged.goto(base + '#/data');
	const id = file.values[0].id;
	expect(
		await forged.getByTestId(`ladder-${id}-refused`).waitFor({ timeout: 60000 }).then(() => true, () => false),
		'NS5 ladder: a lifted level is refused on the page'
	);
	expect((await forged.getByTestId(`ladder-${id}-history`).count()) === 0, 'NS5 ladder: a refused ladder shows no history');
	await forged.close();

	// A stronger answer with another value: shown next to the value, the level unchanged (R-LAD2-3).
	const cpage = await browser.newPage();
	const withC = structuredClone(file);
	const c0 = withC.values[0].ladder;
	c0.contradictions = [{ level: 1, name: L.LEVEL_SLUG[1], evidence: '0x' + '2'.repeat(64), at: c0.steps[0].at + 60, record: 'OnyxRegistry.ShardAttested', value: '0x' + '3'.repeat(64) }];
	await cpage.route('**/level2/ladders-base-sepolia.json', (r) => r.fulfill({ json: withC }));
	await cpage.goto(base + '#/data');
	const shown = await cpage.getByTestId(`ladder-${id}-contradiction`).waitFor({ timeout: 60000 }).then(() => true, () => false);
	expect(shown, 'NS5 ladder: a contradiction is shown next to the value');
	expect((await cpage.getByTestId(`ladder-${id}-badge`).innerText()) === 'Level 2 · Re-observed', 'NS5 ladder: a contradiction leaves the level as it is');
	await cpage.close();

	// THREATS NS5-9: the page remembers the ladder it accepted; a later visit that drops a step is refused.
	const ctx = await browser.newContext();
	const vpage = await ctx.newPage();
	const risen = structuredClone(file);
	const r0 = risen.values[0].ladder;
	r0.steps = [{ ...r0.steps[0], level: 4, name: L.LEVEL_SLUG[4], evidence: '0x' + '4'.repeat(64), at: r0.steps[0].at - 60 }, r0.steps[0]];
	await vpage.route('**/level2/ladders-base-sepolia.json', (r) => r.fulfill({ json: risen }));
	await vpage.goto(base + '#/data');
	await vpage.getByTestId(`ladder-${id}-history`).waitFor({ timeout: 60000 });
	await vpage.unroute('**/level2/ladders-base-sepolia.json');
	const again = await ctx.newPage();
	await again.goto(base + '#/data');
	const fell = await again.getByTestId(`ladder-${id}-refused`).waitFor({ timeout: 60000 }).then(() => true, () => false);
	expect(fell && (await again.getByTestId(`ladder-${id}-refused`).innerText()).includes('earlier visit'), 'NS5 ladder: a fallen ladder is refused on the second visit');
	await ctx.close();
}
