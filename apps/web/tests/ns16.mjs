// e2e "NS16 live track record" (north-star loop NL4, ADR-0036 amendment 8): the data page asks the
// node for the NWS forecaster's record on request and checks it in the browser. With no node behind the page
// it says no record is served yet; with the node's body (tests/fixtures/track-nws-record.json, made
// by adapters/source-ncei/tests/track_nws.rs) it shows the checked counts and "too few to score";
// a body with a changed as-of time fails the check and shows nothing. The testnet limits are stated.
// Called by tests/e2e.mjs with a built site; checks 1280 and 390 px.
import { readFileSync } from 'node:fs';

const ROUTE = '**/v1/track/noaa.nws.forecast.bou/noaa.ghcnd.usw00003017.tmax';

export async function ns16(browser, base, root, expect) {
	const body = readFileSync(new URL('./fixtures/track-nws-record.json', import.meta.url), 'utf8');
	const forged = JSON.parse(body);
	forged.asOf += 1;
	for (const width of [1280, 390]) {
		for (const mode of ['none', 'served', 'forged']) {
			const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
			const pg = await ctx.newPage();
			if (mode !== 'none') {
				await pg.route(ROUTE, (r) =>
					r.fulfill({ status: 200, contentType: 'application/json', body: mode === 'served' ? body : JSON.stringify(forged) })
				);
			}
			await pg.goto(base + '#/data');
			// Data review: the track records section is folded by default.
			await pg.getByTestId('track-fold').locator('> summary').click();
			const panel = pg.getByTestId('track-live');
			await panel.waitFor({ timeout: 90000 });
			// Nothing is asked before the reader presses the button.
			expect((await pg.getByTestId('track-live-none').count()) === 0, `NS16 ${width} ${mode}: the node is asked only on request`);
			await pg.getByTestId('track-live-ask').click();
			const limits = await pg.getByTestId('track-live-limits').innerText();
			expect(
				limits.includes('not an independent track record') && limits.includes('NOAA runs both'),
				`NS16 ${width} ${mode}: the testnet limits are stated`
			);
			if (mode === 'none') {
				await pg.getByTestId('track-live-none').waitFor({ timeout: 30000 });
				expect(true, `NS16 ${width}: with no node the page says no record is served yet`);
			} else if (mode === 'served') {
				await pg.getByTestId('track-live-headline').waitFor({ timeout: 30000 });
				const head = await pg.getByTestId('track-live-headline').innerText();
				const few = await pg.getByTestId('track-live-too-few').innerText();
				expect(head.startsWith('6 forecasts stated, 2 graded'), `NS16 ${width}: the checked record shows its counts (${head})`);
				expect(few.startsWith('Too few to score: 2 graded days'), `NS16 ${width}: too few to score (${few})`);
			} else {
				await pg.getByTestId('track-live-error').waitFor({ timeout: 30000 });
				const shown = await pg.getByTestId('track-live-headline').count();
				expect(shown === 0, `NS16 ${width}: a changed record fails the check and shows nothing`);
			}
			const over = await pg.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1);
			expect(!over, `NS16 ${width} ${mode}: no sideways scroll`);
			await ctx.close();
		}
	}
}
