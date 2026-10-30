// e2e "downloads" (scripts/release-build.sh): the Add data page lists the three programs of the
// test-network release, each linking only to files of that release on the project's public
// repository, with the SHA-256 page. Called by tests/e2e.mjs; checks 1280 and 390 px.
const RELEASE = 'https://github.com/onyxnetworkofficial/onyx/releases/';

export async function downloads(browser, base, root, expect) {
	for (const width of [1280, 390]) {
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
		const pg = await ctx.newPage();
		await pg.goto(base + '#/submit');
		await pg.getByTestId('downloads').waitFor({ timeout: 90000 });
		for (const id of ['contrib', 'join', 'publish']) {
			expect((await pg.getByTestId(`download-${id}`).count()) === 1, `downloads ${width}: the ${id} program is listed`);
		}
		const hrefs = await pg.locator('[data-testid="download-file"]').evaluateAll((as) => as.map((a) => a.getAttribute('href')));
		expect(
			hrefs.length === 4 && hrefs.every((h) => h.startsWith(RELEASE + 'download/v') && /-testnet\/onyx-[a-z0-9._-]+\.(tar\.gz|zip)$/.test(h)),
			`downloads ${width}: every file is a release asset of the public repository (${hrefs.join(' ')})`
		);
		const page = await pg.getByTestId('download-release').getAttribute('href');
		expect(page.startsWith(RELEASE + 'tag/v'), `downloads ${width}: the release page with the SHA-256 sums is linked`);
		const over = await pg.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1);
		expect(!over, `downloads ${width}: no sideways scroll`);
		await ctx.close();
	}
}
