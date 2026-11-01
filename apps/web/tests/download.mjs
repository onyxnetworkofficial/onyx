// e2e "download" (CL10, ADR-0061): the #/download page at 1280 and 390 px for a Windows, macOS,
// Linux and phone visitor. The release and file names come from src/lib/release.ts (never a literal
// here). While RELEASE predates the desktop installers (DESKTOP_SINCE) no installer is linked and a
// note names DESKTOP_SINCE; once it carries them, one big button links the visitor's installer, the
// other systems and SHA256SUMS sit under it. Either way: one h1, only links to the project's own
// releases, the command-line downloads under "For developers", no sideways scroll, and the header
// menu and the Add data page lead here. Called by tests/e2e.mjs; also runs alone against a build:
// node tests/download.mjs [build dir].
const R = await import(new URL('../src/lib/release.ts', import.meta.url).href);
const RELEASES = 'https://github.com/onyxnetworkofficial/onyx/releases/';

const VISITORS = [
	['windows', { userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 Edg/130.0.0.0' }],
	['macos', { userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Safari/605.1.15' }],
	['linux', { userAgent: 'Mozilla/5.0 (X11; Ubuntu; Linux x86_64; rv:132.0) Gecko/20100101 Firefox/132.0' }],
	['other', { userAgent: 'Mozilla/5.0 (iPhone; CPU iPhone OS 18_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Mobile/15E148 Safari/604.1', isMobile: true, hasTouch: true }]
];

export async function download(browser, base, root, expect) {
	const ready = R.hasDesktop(R.RELEASE);
	const desktopFiles = R.DESKTOP.flatMap((d) => d.files.map((f) => f.file));
	for (const width of [1280, 390]) {
		for (const [sys, opts] of VISITORS) {
			if (sys === 'other' && width === 1280) continue;
			const at = `download ${width} ${sys}`;
			const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce', ...opts });
			const pg = await ctx.newPage();
			const errors = [];
			pg.on('pageerror', (e) => errors.push(String(e)));
			await pg.goto(base + '#/download');
			const get = pg.getByTestId('download-get');
			await get.waitFor({ timeout: 90000 });
			await pg.waitForFunction(() => document.querySelector('[data-testid="download-get"]')?.dataset.system, null, { timeout: 30000 }).catch(() => {});
			const detected = await get.getAttribute('data-system');
			expect(detected === sys, `${at}: the page detects the visitor's system (${detected})`);
			expect((await pg.locator('h1').count()) === 1 && (await pg.locator('h1').innerText()).trim() === 'Download Onyx', `${at}: one h1, "Download Onyx"`);
			const hrefs = await pg.locator('main a[href]').evaluateAll((as) => as.map((a) => a.getAttribute('href')));
			const outside = hrefs.filter((h) => !h.startsWith('#') && !h.startsWith(RELEASES));
			expect(outside.length === 0, `${at}: every link is a page of this site or a file of the project's releases${outside.length ? ': ' + outside.join(' ') : ''}`);
			const linkedDesktop = hrefs.filter((h) => desktopFiles.some((f) => h.endsWith('/' + f)));
			if (!ready) {
				expect(linkedDesktop.length === 0 && (await pg.getByTestId('download-button').count()) === 0, `${at}: ${R.RELEASE} predates the installers, so none is linked`);
				const soon = (await pg.getByTestId('download-soon').innerText().catch(() => '')).replace(/\s+/g, ' ');
				expect(soon.includes(R.DESKTOP_SINCE), `${at}: a note says the installers arrive with ${R.DESKTOP_SINCE}`);
				expect((await pg.getByTestId('download-warnings').count()) === 0, `${at}: no installer warnings without installers`);
			} else {
				const want = R.DESKTOP.find((d) => d.id === sys);
				const button = pg.getByTestId('download-button');
				if (want) {
					const href = await button.getAttribute('href');
					const box = await button.boundingBox();
					expect((await button.count()) === 1 && href === R.BASE + want.files[0].file && box.height >= 44, `${at}: one big button downloads ${want.files[0].file} (${href})`);
					expect((await button.innerText()).includes(want.name), `${at}: the button names ${want.name}`);
				} else expect((await button.count()) === 0 && (await pg.getByTestId('download-pick').count()) === 1, `${at}: no button on a phone; the page asks to pick a system`);
				const rest = desktopFiles.filter((f) => !want || f !== want.files[0].file);
				const listed = await pg.locator('[data-testid="download-desktop-file"]').evaluateAll((as) => as.map((a) => a.getAttribute('href')));
				expect(listed.length === rest.length && rest.every((f) => listed.includes(R.BASE + f)), `${at}: the other ${rest.length} installers are listed under it`);
				expect((await pg.getByTestId('download-sums').getAttribute('href')) === R.BASE + R.SUMS, `${at}: SHA256SUMS is linked`);
				const warn = (await pg.getByTestId('download-warnings').innerText()).replace(/\s+/g, ' ');
				expect(['More info', 'Run anyway', 'Open Anyway', 'Applications', 'chmod +x', 'sudo apt install'].every((w) => warn.includes(w)), `${at}: plain steps for the unsigned-installer warnings on each system`);
				const cli = await pg.locator('[data-testid="download-cli-file"]').evaluateAll((as) => as.map((a) => a.getAttribute('href')));
				expect(cli.length === R.CLI_FILES.length && R.CLI_FILES.every((f) => cli.includes(R.BASE + f)), `${at}: the single-file programs are under For developers`);
			}
			// For developers: the command-line downloads (the Add data page's list, without its app link).
			const dev = pg.getByTestId('downloads');
			const devFiles = await dev.locator('[data-testid="download-file"]').evaluateAll((as) => as.map((a) => a.getAttribute('href')));
			const want = R.programs(R.RELEASE).flatMap((p) => p.files);
			expect(
				(await dev.locator('h2').innerText()).trim() === 'For developers' && devFiles.length === want.length && want.every((f) => devFiles.includes(R.BASE + f)) && (await dev.getByTestId('download-app-link').count()) === 0,
				`${at}: "For developers" lists the ${want.length} command-line files of ${R.RELEASE}`
			);
			const over = await pg.evaluate(() => document.documentElement.scrollWidth - innerWidth);
			expect(over === 0, `${at}: no sideways scroll`);
			expect(errors.length === 0, `${at}: no page errors${errors.length ? ': ' + errors.join('; ') : ''}`);
			await ctx.close();
		}
	}
	// The header menu (inline at 1280 px, the menu button at 390 px) and the Add data page lead here.
	for (const width of [1280, 390]) {
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
		const pg = await ctx.newPage();
		await pg.goto(base + '#/data');
		await pg.locator('header.top').waitFor();
		if (width < 761) await pg.getByTestId('menu-button').click();
		await pg.getByTestId('site-nav').getByRole('link', { name: 'Download', exact: true }).click();
		await pg.waitForFunction(() => location.hash === '#/download', null, { timeout: 30000 }).catch(() => {});
		await pg.getByTestId('download-get').waitFor({ timeout: 30000 }).catch(() => {});
		expect((await pg.evaluate(() => location.hash)) === '#/download' && (await pg.getByTestId('download-get').count()) === 1, `download ${width}: the main menu's Download leads to #/download`);
		await pg.goto(base + '#/submit');
		const link = pg.getByTestId('download-app-link');
		await link.waitFor({ timeout: 90000 });
		const box = await link.boundingBox();
		await link.click();
		await pg.waitForFunction(() => location.hash === '#/download', null, { timeout: 30000 }).catch(() => {});
		expect((await pg.evaluate(() => location.hash)) === '#/download' && box.height >= 44, `download ${width}: the Add data page's downloads link the app's Download page (a ${Math.round(box.height)} px link)`);
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
		await download(browser, `http://127.0.0.1:${server.address().port}/`, root, (ok, msg) => {
			console.log(`${ok ? 'ok  ' : 'FAIL'} ${msg}`);
			if (!ok) failures.push(msg);
		});
	} finally {
		await browser.close();
		server.close();
	}
	process.exitCode = failures.length ? 1 : 0;
}
