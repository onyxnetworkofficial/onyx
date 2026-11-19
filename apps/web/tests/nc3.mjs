// e2e "nc3" (newcomer test 3, 2026-11-18: an analyst, a sceptic journalist, a data owner and a
// developer on the live site), against a fake node (page.route), at 1280 and 390 px:
// Home says how many datasets come from one publisher, from the node's own catalogs; the Markets
// button and page call no market open and say who bet; the Run page is a recorded run and its
// prices are planned; the footer claims only the demo checks; the header search sends what is
// typed to the dataset search and shows Ctrl K off Apple machines; a metric-only search opens on
// Bitcoin and Ethereum; a typo offers "Did you mean"; the About page says what Onyx is, that no
// money is real, that it does not say who runs anything, the licences, how to reach the project
// and that no terms of service exist. Called by tests/e2e.mjs; also runs alone against a build:
// node tests/nc3.mjs [build dir].
import { settled, WAIT_CAP } from './wait.mjs';

const REPO = 'https://github.com/onyxnetworkofficial/onyx';
const DAY = 86400;
const ds = (name, age) => ({
	publisher: 'onchain-archive',
	dataset: name,
	version: 1,
	root: '0x' + 'cd'.repeat(32),
	rows: 25,
	bytes: 2_500_000,
	shards: 1,
	stored_shards: 1,
	key_min: String(1_700_006_400),
	key_max: String(1_700_006_400 + 24 * DAY),
	license: 'CC-BY-4.0',
	forked: false,
	created_unix: 1_793_727_822,
	newest: '2026-11-13',
	newest_age_days: age
});
// In the node's order (by id): the metric-only search must not open at 1inch.
const LIST = [ds('1inch.mvrv', 2), ds('aave.mvrv', 2), ds('btc.mvrv', 80), ds('btc.sth-mvrv', 2), ds('eth.mvrv', 2)];
// The node's catalogs, as the live one answered on 2026-11-18: one publisher holds nearly all.
const CATALOGS = [
	{ publisher: 'early-record', leaves: 2, removed: 0, moved: 0, number: 22 },
	{ publisher: 'onchain-archive', leaves: 9059, removed: 8, moved: 8, number: 15 }
];
// The node's `any` search: groups by ';', terms by ',', a last '*' for a word start.
const matches = (any, id) => {
	const words = id.split(/[^a-z0-9]+/).filter(Boolean);
	return any.split(';').filter(Boolean).every((g) => g.split(',').some((t) => (t.endsWith('*') ? words.some((w) => w.startsWith(t.slice(0, -1))) : words.includes(t))));
};
const flat = (s) => s.replace(/\s+/g, ' ').trim();

export async function nc3(browser, base, expect) {
	for (const width of [1280, 390]) {
		const at = `nc3 ${width}`;
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
		const pg = await ctx.newPage();
		const errors = [];
		const sent = [];
		pg.on('pageerror', (e) => errors.push(String(e)));
		pg.on('request', (r) => r.method() !== 'GET' && sent.push(`${r.method()} ${r.url()}`));
		await pg.route('**/v1/bulk?*', (route) => {
			const u = new URL(route.request().url());
			const any = u.searchParams.get('any') ?? '';
			// The opening list and the two counts say the live totals; a search lists its matches.
			if (!any) return route.fulfill({ json: { catalogs: CATALOGS, datasets: [], total: u.searchParams.get('fresh') ? 6302 : 9054, next: null } });
			const hit = LIST.filter((d) => matches(any, `${d.publisher}/${d.dataset}`));
			return route.fulfill({ json: { catalogs: CATALOGS, datasets: hit.slice(0, Number(u.searchParams.get('limit') ?? 50)), total: hit.length, next: null } });
		});
		const fits = () => pg.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1);
		const text = async (id) => flat((await pg.getByTestId(id).allInnerTexts()).join(' '));

		// 1. Home: the share of one publisher, from the node's catalogs; no market is called open.
		await pg.goto(base);
		await settled(pg);
		await pg.waitForFunction(() => /9,051/.test(document.querySelector('[data-testid="home-publishers"]')?.textContent ?? ''), null, { timeout: WAIT_CAP }).catch(() => {});
		const pubs = await text('home-publishers');
		expect(pubs.startsWith('Today 9,051 of 9,054 datasets come from one publisher, onchain-archive.') && /Onyx does not check its numbers/.test(pubs), `${at}: Home says how many datasets come from one publisher, from the node's catalogs ("${pubs}")`);
		const what = await text('home-what');
		expect(/built to hold data from many publishers/.test(what) && !/from many sources/.test(what), `${at}: Home no longer says the data comes from many sources ("${what.slice(0, 80)}")`);
		const mk = flat(await pg.locator('.market-preview').innerText());
		expect((await text('home-markets')) === 'See the test markets' && !/Open markets|Live on/.test(mk) && /Past test markets/.test(mk) && /placed by the project’s own test accounts/.test(mk) && /nothing is traded there/.test(mk), `${at}: Home calls no market open and says who bet ("${mk.slice(0, 90)}")`);
		const foot = await text('footer-checks');
		expect(/^The demo checks on How it works run in your browser, on the network's own code\./.test(foot) && /A dataset's proof needs the onyx-check program: a Linux download, or a build from source elsewhere\.$/.test(foot) && !/Every check runs/.test(foot), `${at}: the footer claims only the demo checks for the browser ("${foot}")`);
		expect(await fits(), `${at}: Home fits the viewport`);

		// 2. The header search: Ctrl K off Apple machines; what is typed goes to the dataset search.
		const key = await pg.getByTestId('search-button').locator('kbd').evaluate((k) => k.textContent);
		expect(key === 'Ctrl K', `${at}: the search shortcut reads Ctrl K on this (Linux) browser, not the Apple key ("${key}")`);
		await pg.getByTestId('search-button').click();
		await pg.keyboard.type('mvrv');
		const offered = await pg.locator('#pal-list [role=option]').allInnerTexts();
		expect(offered.length === 1 && /Search datasets for “mvrv”/.test(offered[0]) && (await pg.locator('.pal .none').count()) === 0, `${at}: the header search offers the dataset search for "mvrv", never "No match" (${offered.map(flat).join(' | ')})`);
		await pg.keyboard.press('Enter');
		await pg.waitForFunction(() => location.hash === '#/datasets?q=mvrv', null, { timeout: WAIT_CAP }).catch(() => {});
		await pg.getByTestId('datasets-list').waitFor({ timeout: WAIT_CAP }).catch(() => {});
		const ids = async () => (await pg.getByTestId('datasets-id').allInnerTexts()).map((t) => t.trim());
		expect((await pg.getByTestId('datasets-q').inputValue()) === 'mvrv' && (await pg.evaluate(() => location.hash)) === '#/datasets?q=mvrv', `${at}: Enter opens All datasets with the search filled in`);
		// 3. A search that names no asset: Bitcoin, Ethereum, then by asset; fresh before stale in one asset.
		expect(JSON.stringify(await ids()) === JSON.stringify(['btc.sth-mvrv', 'btc.mvrv', 'eth.mvrv', '1inch.mvrv', 'aave.mvrv']), `${at}: "mvrv" opens on Bitcoin (fresh first), then Ethereum, then the rest by name (${await ids()})`);
		// From the datasets page itself (the same route): a typo, and "Did you mean".
		await pg.getByTestId('search-button').click();
		await pg.keyboard.type('bitcon');
		await pg.keyboard.press('Enter');
		await pg.getByTestId('datasets-suggest').waitFor({ timeout: WAIT_CAP }).catch(() => {});
		const mean = await text('datasets-suggest');
		expect((await pg.getByTestId('datasets-q').inputValue()) === 'bitcon' && (await pg.getByTestId('datasets-row').count()) === 0 && mean === 'Did you mean bitcoin? (2 datasets)', `${at}: a search with no match offers the nearest known word with the node's count ("${mean}")`);
		const box = await pg.getByTestId('datasets-suggest-go').boundingBox();
		expect(!!box && box.height >= 44, `${at}: the suggestion is a 44 px tap target (${Math.round(box?.height ?? 0)} px)`);
		await pg.getByTestId('datasets-suggest-go').click();
		await pg.getByTestId('datasets-list').waitFor({ timeout: WAIT_CAP }).catch(() => {});
		expect((await pg.getByTestId('datasets-q').inputValue()) === 'bitcoin' && JSON.stringify(await ids()) === JSON.stringify(['btc.sth-mvrv', 'btc.mvrv']) && (await pg.getByTestId('datasets-suggest').count()) === 0 && (await pg.evaluate(() => location.hash)) === '#/datasets?q=bitcoin', `${at}: one click runs the suggested search (${await ids()})`);
		const where = await text('datasets-where');
		expect(where === 'Where these numbers come from. 9,051 of 9,054 datasets here come from one publisher, onchain-archive. What Onyx knows about these numbers is only what their publisher states. The project’s public notes describe this publisher in one line: “on-chain metrics from its own nodes”. The publisher has not published its method. The proof shows these are the rows it signed.', `${at}: All datasets says where the numbers come from and that the method is not published ("${where}")`);
		expect(await fits(), `${at}: All datasets fits the viewport`);

		// 4. Markets: nothing is called open by the page's own words; who bet; the practice figures say simulated.
		await pg.goto(base + '#/markets');
		await settled(pg);
		const slab = await text('slabside');
		const who = await text('live-who');
		const live = flat(await pg.getByTestId('live-markets').innerText());
		const over = await text('arena-overview');
		expect(!/\bTrade on\b/.test(slab) && /This page reads the past test markets from the chain/.test(slab) && /nothing is traded from this page/.test(slab), `${at}: the Markets page says what it does: it reads the chain ("${slab.slice(0, 80)}")`);
		expect(/These are past test markets\./.test(who) && /placed by the project's own test accounts \(the operator's test keys\), not by outside traders/.test(who) && /Nothing can be traded or bought from this page/.test(who), `${at}: the test markets say who bet and that the page cannot trade ("${who.slice(0, 80)}")`);
		expect(!/Live on Base Sepolia/.test(live) && /this page only reads/i.test(live), `${at}: the test markets' heading no longer says "Live"`);
		expect(/Open practice markets/i.test(over) && /Simulated volume/i.test(over) && /Simulated traders/i.test(over) && !/^Open markets/i.test(over), `${at}: the practice figures say simulated beside each number ("${over.slice(0, 80)}")`);
		expect(await fits(), `${at}: Markets fits the viewport`);

		// 5. Run: a recorded run, dated; every price planned; payments switched off.
		await pg.goto(base + '#/run');
		await settled(pg);
		await pg.getByTestId('payments-off').waitFor({ timeout: WAIT_CAP }).catch(() => {});
		const runSlab = await text('slabside');
		const run = flat(await pg.locator('main').innerText());
		expect((await pg.locator('h1').innerText()).trim() === 'Recorded run' && (await pg.title()) === 'Recorded run · Onyx' && /^Recorded runs on Base Sepolia, a public testnet: \d full runs, the newest on 2026-\d\d-\d\d/.test(runSlab) && /Nothing on this page is running now\.$/.test(runSlab), `${at}: the Run page is a recorded run with its date ("${runSlab.slice(0, 90)}")`);
		expect(/Payments are switched off on the hosted test node: everything is free to read today\./.test(await text('payments-off')) && /Planned: agents pay per call/.test(run) && /Planned prices, switched off today: /.test(run) && /Full exports would stay paid\./.test(run) && !/Full exports stay paid/.test(run) && /one paid answer in the recorded run/.test(run), `${at}: every price on the Run page is planned, and payments are switched off`);
		expect(await fits(), `${at}: Run fits the viewport`);

		// 6. How it works: recorded blocks with their day; no "buyers pay"; the four status words.
		await pg.goto(base + '#/how-it-works');
		await settled(pg);
		await pg.waitForFunction(() => /\d{4}/.test(document.querySelector('[data-testid="stat-blocks"]')?.textContent ?? ''), null, { timeout: WAIT_CAP }).catch(() => {});
		const stat = await text('stat-blocks');
		const hero = await text('home-what');
		expect(/^recorded blocks from our node, \d{1,2} [A-Z][a-z]{2} 20\d\d$/.test(stat), `${at}: the block count is a recorded figure with its day ("${stat}")`);
		expect(!/buyers pay/.test(hero) && /Paying per row is planned and switched off: everything is free to read today\./.test(hero) && !/Pay per call, check the proof|buyers pay/.test(flat(await pg.locator('main').innerText())), `${at}: How it works does not say anyone pays today`);
		expect(await fits(), `${at}: How it works fits the viewport`);

		// 7. Add data: the CSV owner's sentence, the levels in the same four words. Publish: six words, both ways.
		await pg.goto(base + '#/submit');
		await settled(pg);
		const csv = await text('csv-owner');
		expect(/^I have a CSV file\. Publishing it yourself needs three things today: an HTTPS host you control/.test(csv) && /a Linux machine for the ready-made tools \(or a Rust build/.test(csv) && /a schema file you write by hand/.test(csv) && /After a review of up to 5 days/.test(csv) && /describe the file in the form below instead/.test(csv) && (await pg.getByTestId('csv-owner-example').getAttribute('href')) === '#/publish', `${at}: Add data says what a CSV owner needs today and what happens after review ("${csv.slice(0, 80)}")`);
		const levels = await pg.getByTestId('submit-levels').locator('.lmark').allInnerTexts();
		expect(levels.length === 5 && levels.every((l) => /^(Live now|Recorded|Planned|Switched off): /.test(l.trim())) && !levels.some((l) => /^Built/.test(l.trim())) && /Live now: Served by the hosted test node today\./.test(await text('status-words')), `${at}: Add data's levels use the same four status words as How it works (${levels.map((l) => l.split(':')[0]).join(', ')})`);
		expect(await fits(), `${at}: Add data fits the viewport`);
		await pg.goto(base + '#/publish');
		await settled(pg);
		const words = await pg.getByTestId('publish-words');
		expect((await words.locator('.ititle').innerText()).trim() === 'Words used below' && (await words.locator('dt').count()) === 6, `${at}: the Publish page's word list no longer counts "four" over six words`);
		expect(/may name only the issue: that file was packed before the form existed\. Both ways reach the same reviewer/.test(await text('publish-readme-note')), `${at}: the Publish page says why the released README names one way and the page two`);

		// 8. About: in the footer of every page, in the header search, and what it says.
		const about = pg.getByTestId('footer-nav').locator('a[href="#/about"]');
		expect((await about.count()) === 1 && (await about.innerText()).trim() === 'About', `${at}: the footer links About`);
		await about.click();
		await pg.waitForFunction(() => location.hash === '#/about', null, { timeout: WAIT_CAP }).catch(() => {});
		await settled(pg);
		const A = {
			h1: (await pg.locator('h1').innerText()).trim(),
			title: await pg.title(),
			lines: await pg.getByTestId('about-what').locator('li').count(),
			testnet: await text('about-testnet'),
			who: await text('about-who'),
			terms: await text('about-terms'),
			security: await text('about-contact'),
			links: await pg.locator('main a[target="_blank"]').evaluateAll((as) => as.map((a) => a.getAttribute('href'))),
			main: flat(await pg.locator('main').innerText())
		};
		expect(A.h1 === 'About Onyx' && A.title === 'About · Onyx' && A.lines === 4, `${at}: About says what Onyx is in four lines (${A.lines})`);
		expect(/test chain/.test(A.testnet) && /have no value/.test(A.testnet) && /Payments are switched off on the hosted test node: everything is free to read today\./.test(A.testnet), `${at}: About says it is a test network with no real money`);
		expect(/is run by one operator\. This site does not say who that is, and it does not say who any publisher is/.test(A.who) && /almost all datasets come from one publisher/.test(A.who), `${at}: About says one operator runs the node and that the site names nobody`);
		expect(JSON.stringify(A.links) === JSON.stringify([`${REPO}/blob/main/LICENSE`, `${REPO}/blob/main/DATA-LICENSE.md`, `${REPO}/issues`, `${REPO}/blob/main/SECURITY.md`]), `${at}: About links the two licence files, the issues and SECURITY.md of the public repository (${A.links.join(', ')})`);
		expect(/Use GitHub private vulnerability reporting: open the repository's Security tab and choose Report a vulnerability\. Only the maintainers see the report\. Onyx has no e-mail contact\. Do not open a public issue for a vulnerability\./.test(A.security), `${at}: About quotes the security route of SECURITY.md word for word`);
		expect(/^Terms of service There are none\. No terms of service exist for this test network/.test(A.terms), `${at}: About says plainly that no terms of service exist`);
		expect(!/[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}/.test(A.main), `${at}: About prints no e-mail address`);
		expect(await fits(), `${at}: About fits the viewport`);
		await pg.getByTestId('search-button').click();
		await pg.keyboard.type('about');
		const first = flat((await pg.locator('#pal-list [role=option]').first().innerText()) ?? '');
		expect(/^About Onyx/.test(first), `${at}: the header search lists the About page ("${first}")`);
		await pg.keyboard.press('Escape');

		expect(sent.length === 0, `${at}: nothing but GET is sent (${sent.join(', ')})`);
		expect(errors.length === 0, `${at}: no page errors${errors.length ? ': ' + errors.join('; ') : ''}`);
		await ctx.close();
	}
}

if (import.meta.url === `file://${process.argv[1]}`) {
	const { chromium } = await import('playwright-core');
	const { serve } = await import('./serve.mjs');
	const { resolve } = await import('node:path');
	const server = await serve(0, resolve(process.argv[2] ?? new URL('../build/', import.meta.url).pathname));
	const browser = await chromium.launch({ channel: 'chrome' });
	let failed = 0;
	await nc3(browser, `http://127.0.0.1:${server.address().port}/`, (ok, what) => {
		console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`);
		if (!ok) failed++;
	});
	await browser.close();
	server.close();
	process.exit(failed ? 1 : 0);
}
