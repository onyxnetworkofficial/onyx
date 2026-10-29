// Pre-public loop PP3 (ROADMAP D30): the Labels page checks one salted label row from
// fixtures/dated/salted-vectors.json against the pin compiled into the verifier, refuses a row
// whose salt was changed, and shows that a guessed neighbour does not match its sibling hash.
// Called by tests/e2e.mjs with a built site.
export async function pp3(browser, base, expect) {
	for (const width of [1280, 390]) {
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
		const pg = await ctx.newPage();
		await pg.goto(base + '#/labels');
		const panel = pg.getByTestId('salted-demo');
		await panel.waitFor({ timeout: 60000 });
		const result = async (id, want) => {
			await pg.getByTestId(id).click();
			await pg.getByTestId('salted-result').filter({ hasText: want }).waitFor({ timeout: 120000 }).catch(() => {});
			return (await pg.getByTestId('salted-result').innerText()).replace(/\s+/g, ' ');
		};
		let r = await result('salted-check', 'belonged to');
		expect(r.includes('test:addr-2 belonged to entity:example-b from block 200 to 499') && r.includes('pin in this site'), `PP3 ${width}: a bought salted row checks against the compiled pin (${r.slice(0, 160)})`);
		r = await result('salted-flip', 'Refused');
		expect(r.includes('Refused: NotInRoot'), `PP3 ${width}: a row whose salt changed is refused (${r.slice(0, 160)})`);
		r = await result('salted-guess', 'neighbour');
		expect(r.includes('does not match the sibling hash'), `PP3 ${width}: a guessed neighbour is not confirmed by the proof (${r.slice(0, 160)})`);
		const over = await pg.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1);
		expect(!over, `PP3 ${width}: the salted-row panel does not scroll the page sideways`);
		await ctx.close();
	}
}
