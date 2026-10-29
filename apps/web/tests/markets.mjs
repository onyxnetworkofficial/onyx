// e2e "Markets": the data-markets dashboard (practice credits) works end to end in a browser.
// Live prices move; a buy fills and shows its payout; a forced resolution replays the observer
// check and pays the position; a flash round takes a pick; a parlay places and reveals; quests,
// portfolio and leaderboard render; no network request leaves the site; axe finds nothing serious;
// a created market lists and trades; the Verify data tab holds the stake simulator (tests/t10.mjs
// runs its rules); the page fits a phone. Also runs alone: node tests/markets.mjs [build dir].
import { readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
const axe = await readFile(createRequire(import.meta.url).resolve('axe-core/axe.min.js'), 'utf8');

// THREATS U6-UI-1: the page must never look like live money (fixed titles the threat check cites).
const PRACTICE = 'markets: says practice credits, simulated, no real money';
const LOCAL = 'markets: no request leaves the site';

export async function markets(browser, base, expect) {
	for (const width of [1280, 390]) {
		const context = await browser.newContext({ viewport: { width, height: 900 } });
		const page = await context.newPage();
		const errors = [];
		page.on('pageerror', (e) => errors.push(e.message));
		page.on('console', (m) => m.type() === 'error' && errors.push(m.text()));
		const requests = [];
		page.on('request', (r) => requests.push(new URL(r.url()).origin));
		await page.goto(base + '#/markets');
		await page.getByTestId('arena-cards').waitFor();
		expect((await page.getByTestId('arena-practice').innerText()).includes('No real money'), `${PRACTICE} (${width})`);
		const bal = async () => Number((await page.getByTestId('arena-balance').innerText()).replace(/,/g, ''));
		expect((await bal()) === 1000, `markets ${width}: a new account starts with 1,000 credits`);
		await page.getByTestId('arena-feed').locator('li:not(.fine)').first().waitFor({ timeout: 8000 });
		expect(true, `markets ${width}: simulated trades stream into the live feed`);

		// Buy Yes on the debt market, then force Yes: the position pays 1 credit per share.
		await page.getByTestId('arena-open-debt-406').click();
		await page.getByTestId('arena-amount').fill('100');
		const towin = Number((await page.getByTestId('arena-towin').locator('b').innerText()).replace(/,/g, ''));
		expect(towin > 100 && towin < 300, `markets ${width}: the ticket shows the payout for 100 credits (${towin})`);
		await page.getByTestId('arena-buy').click();
		expect((await bal()) === 900 && (await page.getByTestId('arena-mine').count()) === 1, `markets ${width}: the buy fills, takes 100 credits and shows the position`);
		await page.getByTestId('arena-force-0').click();
		await page.getByTestId('arena-resolved').waitFor({ timeout: 8000 });
		const after = await bal();
		expect(after > 1000 && (await page.getByTestId('arena-resolution').innerText()).includes('5 of 5 agree'), `markets ${width}: forced Yes replays the observer check and pays the position (${after})`);
		await page.getByTestId('arena-reopen').click();
		await page.getByTestId('arena-amount').fill('10');
		await page.getByTestId('arena-buy').click();

		// Flash round: a pick takes the stake.
		await page.getByTestId('arena-tab-flash').click();
		const b0 = await bal();
		await page.getByTestId('flash-stake').fill('20');
		const up = page.getByTestId('flash-up');
		if (await up.isEnabled()) {
			await up.click();
			expect((await page.getByTestId('flash-pick').count()) === 1 && Math.abs((await bal()) - (b0 - 20)) < 0.01, `markets ${width}: a flash pick takes its 20-credit stake`);
		} else expect(true, `markets ${width}: flash round was locked at the check (picks reopen every 30 s)`);

		// Parlay: two legs, placed, then revealed leg by leg.
		await page.getByTestId('arena-tab-parlay').click();
		await page.getByTestId('parlay-debt-406-0').click();
		await page.getByTestId('parlay-eth-gas-0').click();
		const pays = Number((await page.getByTestId('parlay-payout').innerText()).replace(/,/g, ''));
		expect(pays > 10, `markets ${width}: a two-leg parlay multiplies the stake (${pays})`);
		await page.getByTestId('parlay-place').click();
		await page.getByTestId('parlay-latest').waitFor();
		await page.waitForFunction(() => !/Revealing/.test(document.querySelector('[data-testid="parlay-latest"]')?.textContent ?? ''), null, { timeout: 8000 });
		expect(/Hit|Missed/.test(await page.getByTestId('parlay-latest').innerText()), `markets ${width}: the parlay reveals its legs and settles`);

		await page.getByTestId('arena-tab-portfolio').click();
		expect((await page.getByTestId('arena-portfolio').innerText()).includes('Worth'), `markets ${width}: the portfolio shows the account's worth`);
		await page.getByTestId('arena-tab-board').click();
		expect((await page.getByTestId('arena-board').innerText()).includes('You'), `markets ${width}: the leaderboard ranks the viewer`);
		await page.getByTestId('arena-tab-quests').click();
		expect((await page.getByTestId('quest-trade').innerText()) === 'Claim', `markets ${width}: 3 trades make the first quest claimable`);
		await page.getByTestId('quest-trade').click();
		expect((await page.getByTestId('quest-trade').innerText()) === 'Claimed', `markets ${width}: the quest pays once`);

		// Create: a new market on the debt dataset lists, opens, and trades.
		await page.getByTestId('arena-tab-create').click();
		await page.getByTestId('create-threshold').fill('40.9');
		expect((await page.getByTestId('create-question').innerText()).startsWith('U.S. total public debt above 40.9 trillion USD on'), `markets ${width}: the question is written from the dataset, direction and threshold`);
		await page.getByTestId('create-date').fill('2020-01-01');
		await page.getByTestId('create-launch').click();
		expect((await page.getByTestId('create-error').innerText()).includes('future'), `markets ${width}: a past date is refused`);
		const soon = new Date(Date.now() + 40 * 86400000).toISOString().slice(0, 10);
		await page.getByTestId('create-date').fill(soon);
		await page.getByTestId('create-launch').click();
		await page.getByTestId('arena-detail').waitFor();
		expect((await page.locator('#mk-title').innerText()).includes('above 40.9 trillion USD'), `markets ${width}: the new market opens for trading`);
		await page.getByTestId('arena-amount').fill('5');
		await page.getByTestId('arena-buy').click();
		expect((await page.getByTestId('arena-mine').count()) === 1, `markets ${width}: the new market takes a trade`);
		await page.getByTestId('arena-tab-verify').click();
		expect((await page.getByTestId('stake-sim').count()) === 1, `markets ${width}: Verify data holds the stake simulator`);

		await page.getByTestId('arena-tab-markets').click();
		expect(/your rank/i.test(await page.getByTestId('arena-overview').innerText()), `markets ${width}: the overview shows open markets, volume, traders and rank`);
		await page.addScriptTag({ content: axe });
		const violations = await page.evaluate(async () => (await window.axe.run(document, { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21aa', 'best-practice'] } })).violations.filter((v) => ['serious', 'critical'].includes(v.impact)).map((v) => v.id));
		expect(violations.length === 0, `markets ${width}: no serious accessibility issue (${violations.join(',')})`);
		expect(!(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1)), `markets ${width}: page fits the viewport`);
		expect(requests.every((o) => o === new URL(base).origin), `${LOCAL} (${width})`);
		expect(errors.length === 0, `markets ${width}: no console error (${errors.slice(0, 2).join(' | ')})`);
		await context.close();
	}
}

if (import.meta.url === `file://${process.argv[1]}`) {
	const { chromium } = await import('playwright-core');
	const { serve } = await import('./serve.mjs');
	const { resolve } = await import('node:path');
	const server = await serve(0, resolve(process.argv[2] ?? new URL('../build/', import.meta.url).pathname));
	const browser = await chromium.launch({ channel: 'chrome' });
	const failures = [];
	const expect = (ok, what) => (ok ? console.log(`ok   ${what}`) : (failures.push(what), console.log(`FAIL ${what}`)));
	try {
		await markets(browser, `http://127.0.0.1:${server.address().port}/`, expect);
	} finally {
		await browser.close();
		server.close();
	}
	if (failures.length) process.exit(1);
	console.log('MARKETS PASSED');
}
