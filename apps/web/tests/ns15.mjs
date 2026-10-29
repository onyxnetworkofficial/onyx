// e2e "NS15 wanted data" (north-star loop NL9, docs/spec/bounty-board.md): the Add data page asks
// the node's bounty board on request. With no node it says so; with a board it lists each shown
// category in plain words with its bucket; a value outside the closed lists (an exact query slipped
// into an answer) is dropped, never printed. Called by tests/e2e.mjs; checks 1280 and 390 px.
const ROUTE = '**/v1/wants';
const BOARD = {
	minAccounts: 5,
	week: 2964,
	nextCut: 1793318400,
	categories: [
		{ kind: 'public_api', domain: 'weather_climate', region: 'europe', cadence: 'daily', bucket: '5-19' },
		{ kind: 'forecast', domain: 'energy', region: 'global', cadence: 'monthly', bucket: '20+' },
		{ kind: 'public_api', domain: 'finance', region: 'rue de la Paix 3, Lyon', cadence: 'daily', bucket: '5-19' }
	]
};

export async function ns15(browser, base, root, expect) {
	for (const width of [1280, 390]) {
		for (const mode of ['none', 'board']) {
			const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
			const pg = await ctx.newPage();
			if (mode === 'board') {
				await pg.route(ROUTE, (r) => r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(BOARD) }));
			}
			await pg.goto(base + '#/submit');
			const sec = pg.getByTestId('wanted');
			await sec.waitFor({ timeout: 90000 });
			expect((await pg.getByTestId('wanted-list').count()) === 0, `NS15 ${width} ${mode}: the board is asked only on request`);
			await pg.getByTestId('wanted-ask').click();
			if (mode === 'none') {
				await pg.getByTestId('wanted-none').waitFor({ timeout: 30000 });
				expect(true, `NS15 ${width}: with no node the page says there is no board`);
			} else {
				await pg.getByTestId('wanted-list').waitFor({ timeout: 30000 });
				const text = await pg.getByTestId('wanted-list').innerText();
				expect(text.includes('A public page or API: weather and climate, Europe, daily') && text.includes('5-19 buyers'), `NS15 ${width}: a category in plain words with its bucket`);
				expect(text.includes('A forecast: energy, the whole world, monthly') && text.includes('20+ buyers'), `NS15 ${width}: a second category`);
				expect(!text.includes('Lyon') && (await pg.locator('[data-testid="wanted-list"] li').count()) === 2, `NS15 ${width}: a value outside the closed lists is dropped, never shown`);
			}
			const over = await pg.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1);
			expect(!over, `NS15 ${width} ${mode}: no sideways scroll`);
			await ctx.close();
		}
	}
}
