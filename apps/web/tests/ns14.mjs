// e2e "NS14 estimates" (ADR-0050): an estimate is shown apart and never in a total. The data
// page's estimate is the mean of the checked window's whole days with a 95% interval; it says it
// is not checked, shows its method and band, carries no level badge, and no dataset card or
// checked total includes it. Called by tests/e2e.mjs with a built site; checks 1280 and 390 px.
const E = await import(new URL('../src/lib/estimates.ts', import.meta.url).href);
const C = await import(new URL('../src/lib/catalog.ts', import.meta.url).href);

export async function ns14(browser, base, root, expect) {
	// The method on known numbers: mean 5, s = 2.138 (n = 8), t(7) = 2.365, half = 1.788.
	const ci = E.meanCi95([2n, 4n, 4n, 4n, 5n, 5n, 7n, 9n].map((x) => x * 1000n));
	expect(ci && ci.mean === 5000n && ci.hi - ci.mean === 1787n && ci.mean - ci.lo === 1787n, `NS14 estimates: mean ± t·s/√n on known numbers (${ci && ci.lo}..${ci && ci.hi})`);
	expect(E.meanCi95([1n]) === null, 'NS14 estimates: one input is no estimate');
	expect(E.isqrt(10n ** 30n) === 10n ** 15n && E.isqrt(99n) === 9n, 'NS14 estimates: integer square root');
	// R-EST-1: no dataset card is an estimate, and every estimate metric is in its namespace.
	expect(C.DATASETS.every((d) => !d.id.startsWith('estimate.')), 'NS14 estimates: no checked dataset card is an estimate');

	for (const width of [1280, 390]) {
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
		const pg = await ctx.newPage();
		await pg.goto(base + '#/data');
		const sec = pg.getByTestId('estimates');
		await sec.waitFor({ timeout: 90000 });
		// Data review: the section and its method are folded by default; open both.
		await pg.getByTestId('estimates-fold').locator('> summary').click();
		await pg.getByTestId('estimate-method').locator('> summary').click();
		const tag = await pg.getByTestId('estimate-tag').innerText();
		expect(tag === 'Estimate, not checked', `NS14 ${width}: the estimate says it is not checked (${tag})`);
		const band = await pg.getByTestId('estimate-band').innerText();
		expect(/^95% range: [\d,]+\.\d{2} to [\d,]+\.\d{2} BTC$/.test(band), `NS14 ${width}: the estimate shows its error band (${band})`);
		const text = await sec.innerText();
		expect(text.includes('onyx.mean-ci95.v1') && text.includes('estimate.btc.fees_per_day.mean'), `NS14 ${width}: the estimate names its method and metric`);
		expect((await sec.locator('.lvl').count()) === 0 && !/Level [1-5] · /.test(text), `NS14 ${width}: the estimate carries no proof-level badge`);
		const inputs = await pg.getByTestId('estimate-inputs').innerText();
		expect(/^\d+ checked whole days \(\d{1,2} [A-Z][a-z]{2} \d{4} to \d{1,2} [A-Z][a-z]{2} \d{4}\)$/.test(inputs), `NS14 ${width}: the estimate names its checked inputs (${inputs})`);
		// Never in a total: the checked stats sit in the on-chain section, the estimate in its own.
		const inOnchain = await pg.getByTestId('onchain').getByTestId('estimate').count();
		const value = await pg.getByTestId('estimate-value').innerText();
		const stats = await pg.getByTestId('onchain-stats').innerText();
		expect(inOnchain === 0 && !stats.includes(value), `NS14 ${width}: the estimate is outside the checked section and its totals`);
		const cards = await pg.getByTestId('catalog-item').evaluateAll((els) => els.map((e) => e.innerText));
		expect(cards.every((t) => !t.includes('Estimate, not checked')), `NS14 ${width}: no dataset card holds the estimate`);
		const over = await pg.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1);
		expect(!over, `NS14 ${width}: the estimate does not scroll the page sideways`);
		await ctx.close();
	}
}
