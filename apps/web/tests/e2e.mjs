// U1: historical "home" verification assertions now target #/how-it-works. Keep their
// names stable for THREATS citations; tests/product.mjs covers the new contributor homepage.
// End-to-end check of the built site in a real browser (Chrome on this
// machine): every check on every page must pass, and the honest/tampered
// demos must give the right verdicts. Run: pnpm build && pnpm test:e2e
import { createServer } from 'node:http';
import { readFile, readdir } from 'node:fs/promises';
import { extname, join, normalize } from 'node:path';
import { createRequire } from 'node:module';
import { chromium } from 'playwright-core';
import { w10 } from './w10.mjs';
import { w4 } from './w4.mjs';
import { w12 } from './w12.mjs';
import { c6 } from './c6.mjs';
import { c61 } from './c61.mjs';
import { s1 } from './s1.mjs';
import { product } from './product.mjs';
import { markets } from './markets.mjs';
import { daily } from './daily.mjs';
import { ns5 } from './ns5.mjs';
import { ns11 } from './ns11.mjs';
import { ns14 } from './ns14.mjs';
import { ns15 } from './ns15.mjs';
import { downloads } from './downloads.mjs';
import { ns16 } from './ns16.mjs';
import { t10 } from './t10.mjs';
import { pp1Parser } from './pp1.mjs';
import { pp6 } from './pp6.mjs';
import { pp3 } from './pp3.mjs';
import { pp4 } from './pp4.mjs';
import { attr, capWaits, checksDone, quietDom, settled, WAIT_CAP } from './wait.mjs';

const root = new URL('../build/', import.meta.url).pathname;
// The numbers the site shows, read from the same files it loads (AUD-1-F6): never a literal here.
const demo0 = JSON.parse(await readFile(join(root, 'data/demo.json'), 'utf8'));
const labelsV2 = JSON.parse(await readFile(new URL('../../../fixtures/labels/test.onyx.entities.btc.v2.json', import.meta.url), 'utf8'));
const liveBlocks = demo0.live.blocks.length;
const nLabels = labelsV2.labels.length;
const byEntity = {};
for (const l of labelsV2.labels) byEntity[l.entity] = (byEntity[l.entity] ?? 0) + 1;
const nVectors = demo0.bip322_vectors.length;
// AUD-2-F1: a public build (ONYX_PUBLIC=1) ships no label rows; its checks differ where they need them.
const PUB = demo0.public === true;
// ROADMAP L14: a rights-fallback build (ONYX_RIGHTS=fallback) ships no label-derived total.
const FB = demo0.rights === 'fallback';
// The in-browser checks: 7, or 5 in a fallback build (no run or live rows to check; pools instead).
const NCHK = FB ? 5 : 7;
const fullFuel = demo0.runs.find((r) => r.id === 'full').fuel;
const n = (x) => x.toLocaleString('en-US');
// Satoshis (decimal string) as the site prints BTC (format.ts `btc`).
const btcFmt = (s) => { const v = BigInt(s); const a = v < 0n ? -v : v; return `${v < 0n ? '−' : ''}${(a / 100000000n).toLocaleString('en-US')}.${(a % 100000000n).toString().padStart(8, '0')}`; };
const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.json': 'application/json', '.wasm': 'application/wasm', '.txt': 'text/plain' };
const server = createServer(async (req, res) => {
	const path = normalize(decodeURIComponent(new URL(req.url, 'http://x').pathname)).replace(/^(\.\.[/\\])+/, '');
	try {
		const file = path === '/' ? 'index.html' : path;
		const body = await readFile(join(root, file));
		res.writeHead(200, { 'content-type': types[extname(file)] ?? 'application/octet-stream' }).end(body);
	} catch {
		res.writeHead(404).end();
	}
}).listen(0, '127.0.0.1');
await new Promise((r) => server.once('listening', r));
const base = `http://127.0.0.1:${server.address().port}/`;

// E12: every wait without its own timeout is capped by wait.mjs, not Playwright's 30 s.
const browser = capWaits(await chromium.launch({ channel: 'chrome' }));
const failures = [];
const expect = (ok, what) => (ok ? console.log(`ok   ${what}`) : (failures.push(what), console.log(`FAIL ${what}`)));
try {
	const p = await browser.newPage();
	const errors = [];
	p.on('pageerror', (e) => errors.push(String(e)));
	p.on('console', (m) => m.type() === 'error' && errors.push(m.text()));

	await p.goto(base + '#/how-it-works');
	await p.locator('header.top').waitFor();
	expect((await p.getByTestId('preview-banner').count()) === (PUB ? 0 : 1), PUB ? 'public: no preview banner' : 'preview: the private preview banner shows');
	{
		// ROADMAP L14: a fallback build says on every page why the company totals are missing.
		const rb = p.getByTestId('rights-banner');
		expect(FB ? (await rb.innerText()) === 'In development · testnet preview. Data and features may change; some company data is not shown yet.' : (await rb.count()) === 0, FB ? 'fallback: the banner says the site is in development and some company data is not shown yet' : 'no rights banner outside a fallback build');
	}
	// E10: click only once the page has drawn and stopped moving, then wait for every verdict.
	await settled(p);
	await p.getByRole('button', { name: new RegExp(`Run all ${NCHK} checks`) }).click();
	await checksDone(p);
	expect((await p.locator('.tally').innerText()).startsWith(`${NCHK} passed`), `overview: ${NCHK} of ${NCHK} checks pass`);
	if (FB) {
		const titles = await p.locator('.checks li .ct > span').allInnerTexts();
		expect(titles.includes('Pool shares hash to the pinned commitment') && !titles.some((t) => /Run rows|Live rows|delivered row/.test(t)), 'fallback overview: no check over company rows; the pool check instead');
		expect((await p.getByText(/This one is level 4 today/).count()) === 0 && (await p.getByText('OKX net flow, live range').count()) === 0, 'fallback overview: no OKX market and no OKX flow figure');
	}
	// L2R3-F2: the demo market's level is stated honestly (unsigned labels make it level 4).
	else expect(await p.getByText(/This one is level 4 today/).isVisible(), 'overview: demo market says level 4, not level 1');
	expect((await p.getByText(/settles at level 1/).count()) === 0, 'overview: no "settles at level 1" claim');
	// AUD-1-F6: the overview's figures come from the export.
	// The figures count up (Ticker); wait for them to reach the value (the public build's checks
	// finish before they do). E10: the cap was 10 s of wall time, which a loaded machine spent
	// before the count-up ended; the wait is for the value, the cap only stops a wrong one.
	const settles = (loc, want) => loc.evaluate((e, [w, cap]) => new Promise((ok) => { const t0 = Date.now(); const f = () => (e.textContent.replace(/\D/g, '') === w || Date.now() - t0 > cap ? ok(e.textContent.replace(/\D/g, '') === w) : setTimeout(f, 100)); f(); }), [want, WAIT_CAP]);
	expect(await settles(p.locator('.fuel b'), String(fullFuel)), `overview: fuel figure is the export's (${n(fullFuel)})`);
	expect(await settles(p.locator('.stats .stat').nth(2).locator('b'), String(nVectors)), `overview: vector count is the export's (${nVectors})`);
	if (!FB) expect((await p.locator('.mq .q').innerText()).includes(`blocks ${n(demo0.live.from_height)}–${n(demo0.live.to_height)}`), 'overview: market block range is the live range');
	// ADR-0028: the ladder states level 3 honestly (signed data feeds built, none connected).
	expect((await p.getByText(/signed data feeds built, none connected yet/).count()) >= 1, 'overview: level-3 ladder line says no signed feed is connected');
	// AUD-2-F11: level 2 names the recorded sources from their files.
	expect(await p.getByText(/recorded rounds on U\.S\. Treasury Fiscal Data and SEC EDGAR/).first().waitFor({ timeout: WAIT_CAP }).then(() => true, () => false), 'overview: level-2 line names both recorded sources');

	await p.goto(base + '#/explorer');
	if (PUB) {
		const pi = p.getByTestId('public-input');
		expect(await pi.getByText('Pinned root', { exact: true }).waitFor({ timeout: WAIT_CAP }).then(() => true, () => false), 'public explorer: the run root is the pinned one');
		expect((await pi.innerText()).includes('does not ship it'), 'public explorer: says the recipe input is not shipped');
		expect((await p.getByRole('button', { name: 'Rerun the recipe' }).count()) === 0, 'public explorer: no rerun over label rows');
	} else {
		await p.getByRole('button', { name: 'Rerun the recipe' }).click();
		expect(await p.getByText('Same root as the pinned one').waitFor({ timeout: 120000 }).then(() => true, () => false), 'explorer: rerun matches');
		await p.selectOption('#scenario', 'labels');
		await p.getByRole('button', { name: 'Validate' }).click();
		expect(await p.getByText('Mismatch: the miner loses its bond').waitFor({ timeout: 120000 }).then(() => true, () => false), 'explorer: lazy miner caught');
		await p.selectOption('#scenario', 'blocks');
		await p.getByRole('button', { name: 'Validate' }).click();
		expect(await p.getByText(/Refused before running/).waitFor().then(() => true, () => false), 'explorer: changed blocks refused');
	}
	if (FB) {
		expect((await p.getByTestId('explorer-held-back').innerText()).includes('Company totals are held back until data rights are cleared.'), 'fallback explorer: says the shard rows are held back and why');
		expect((await p.getByRole('heading', { name: 'Row proof' }).count()) === 0, 'fallback explorer: no row proof (no company row shipped)');
	} else {
		await p.getByRole('button', { name: 'Change 1 sat, then check' }).click();
		expect(await p.getByText('Fails: the row was changed').waitFor().then(() => true, () => false), 'explorer: changed row fails its proof');
	}
	await p.getByRole('button', { name: 'Check all headers' }).click();
	expect(await p.getByText(/All \d+ headers pass/).waitFor({ timeout: WAIT_CAP }).then(() => true, () => false), 'explorer: live headers pass');
	// NL15 review: after a header check the blocks' screen-reader text escaped the still strip
	// (reduced motion) and the page scrolled sideways by the whole row (3,450 px at 390).
	{
		const rc = await browser.newContext({ viewport: { width: 390, height: 800 }, reducedMotion: 'reduce' });
		const rp = await rc.newPage();
		await rp.goto(base + '#/explorer');
		await rp.getByRole('button', { name: 'Check all headers' }).click();
		await rp.getByText(/All \d+ headers pass/).waitFor({ timeout: WAIT_CAP }).catch(() => {});
		const sw = await rp.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
		expect(sw <= 0, 'explorer: no sideways scroll after the header check (390 px, still)');
		if (sw > 0) console.log(`  explorer overflow ${sw} px`);
		await rc.close();
	}

	await p.goto(base + '#/data');
	{
		// AUD-2-F6/F11: pool shares sit at level 4; SEC filings are a live level-2 item.
		const map = p.getByTestId('data-map');
		await map.waitFor({ timeout: WAIT_CAP });
		// textContent: the level suffix is hidden where the column already shows it.
		const cells = await map.locator('[role=cell]').evaluateAll((els) => els.map((e) => e.textContent ?? ''));
		expect(cells.some((c) => c.includes('Bitcoin pool shares') && c.includes('L4')), 'data map: pool shares at level 4');
		expect(cells.some((c) => c.includes('Company filings') && c.includes('Live') && c.includes('L2')), 'data map: SEC filings live at level 2');
	}
	if (FB) {
		const hb = p.getByTestId('flows-held-back');
		expect((await hb.innerText()).includes('Company totals are held back until data rights are cleared.'), 'fallback data: the company flows section says they are held back and why');
		expect(await p.getByText(new RegExp(`All ${liveBlocks} headers pass proof of work`)).waitFor({ timeout: WAIT_CAP }).then(() => true, () => false), 'fallback data: live headers pass');
		expect((await p.getByTestId('sums').count()) === 0 && (await p.getByTestId('series').count()) === 0 && (await p.getByTestId('history').count()) === 0, 'fallback data: no company flows, series or history');
	} else {
		const sums = p.getByTestId('sums');
		expect(await sums.getByText('Adds up').waitFor({ timeout: 120000 }).then(() => true, () => false), `data: ${liveBlocks} block rows add up to the range row`);
		expect(await p.getByText(new RegExp(`All ${liveBlocks} headers pass proof of work`)).waitFor({ timeout: WAIT_CAP }).then(() => true, () => false), 'data: live headers pass');
		// E12: the charts draw after the sums; wait for the marks the check counts.
		const drawn = await p.waitForFunction(() => document.querySelectorAll('.chart svg path').length >= 48, null, { timeout: WAIT_CAP }).then(() => true, () => false);
		expect(drawn && (await p.locator('.chart svg path').count()) >= 48, 'data: block and flow charts drawn');
		await p.getByRole('button', { name: /Leave out block/ }).click();
		expect(await sums.getByText('Does not add up').waitFor().then(() => true, () => false), 'data: a left-out block is caught');
		await p.getByRole('button', { name: 'Put it back' }).click();
		expect(await sums.getByText('Adds up').waitFor().then(() => true, () => false), 'data: restored blocks add up again');
		// Flows over time: the btc-entity-flow-series rows, checked against the range rows
		// under both pinned roots; one small chart per active company, with the export's counts.
		const demoJson = JSON.parse(await readFile(join(root, 'data/demo.json'), 'utf8'));
		const seriesCheck = p.getByTestId('series-check');
		expect(await seriesCheck.getByText('Adds up', { exact: true }).waitFor({ timeout: 120000 }).then(() => true, () => false), 'data: series rows add up to the range rows');
		const perEntity = {};
		for (const r of demoJson.live.series.rows) perEntity[r.entity] = (perEntity[r.entity] ?? 0) + 1;
		const seriesRowsShown = p.getByTestId('series-row');
		expect((await seriesRowsShown.count()) === Object.keys(perEntity).length, `data: one series chart per active company (${Object.keys(perEntity).length})`);
		const seriesTexts = await seriesRowsShown.allInnerTexts();
		expect(Object.values(perEntity).every((n) => seriesTexts.some((t) => t.includes(`${n} active blocks`))), 'data: active-block counts match the series rows');
		expect(await p.getByText(`${liveBlocks} blocks today; longer history needs more blocks fetched from our node.`).isVisible(), 'data: series says how far it reaches');
		await p.getByRole('button', { name: /Hide one row/ }).click();
		expect(await seriesCheck.getByText('Does not add up', { exact: true }).waitFor().then(() => true, () => false), 'data: a hidden series row is caught');
		await seriesCheck.getByRole('button', { name: 'Put it back' }).click();
		expect(await seriesCheck.getByText('Adds up', { exact: true }).waitFor().then(() => true, () => false), 'data: restored series adds up again');
	}
	// Beyond Bitcoin: the recorded level-2 round on U.S. Treasury Fiscal Data,
	// rechecked in the browser (fetcher rerun, sealed commits recomputed).
	const l2Json = JSON.parse(await readFile(join(root, 'level2/debt-to-penny.json'), 'utf8'));
	const l2 = p.getByTestId('level2');
	expect(await l2.getByText('Checks out', { exact: true }).waitFor({ timeout: 120000 }).then(() => true, () => false), 'data: the level-2 round checks out');
	if (l2Json.sealed) {
		// ATT-R2-F1: a public build ships the round sealed; the paid API sells the value.
		expect(PUB, 'data: only a public build seals the level-2 round');
		expect((await p.getByTestId('level2-sealed').innerText()) === 'Available via the paid API', 'data: sealed level-2 round says the value is sold by the paid API');
		expect((await p.getByTestId('level2-value').count()) === 0, 'data: sealed level-2 round shows no value');
		// SIG-R1-F2/F3: no result hash shipped or shown; the copy claims no rerun fetcher and no value.
		expect(!('result_hash' in l2Json) && (await p.getByTestId('level2-observers').count()) === 0 && (await p.getByTestId('level2-draw').count()) === 0, 'data: sealed level-2 round ships and shows no result hash, observers or pool');
		const c = await p.getByTestId('level2-copy').innerText();
		expect(c.includes('ships neither the value nor its result hash') && !c.includes('reran') && !c.includes('demo data') && !/result hash, which/.test(c), 'data: sealed level-2 copy claims only the checks it ran');
	} else {
		const l2Value = await p.getByTestId('level2-value').innerText();
		const [l2i, l2f] = l2Json.value.split('.');
		expect(l2Value === `$${l2i.replace(/\B(?=(\d{3})+(?!\d))/g, ',')}.${l2f}`, `data: level-2 value equals the export (${l2Value})`);
		// W8: the value is the source's real figure; only the round (observers, draw) was a test.
		expect((await p.getByTestId('level2-tag').innerText()).toLowerCase() === 'real value · test round', 'data: the level-2 value is tagged real value, test round (W8)');
		// Owner review: prose dates read "8 Oct 2026".
		const words = (iso) => `${+iso.slice(8, 10)} ${['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'][+iso.slice(5, 7) - 1]} ${iso.slice(0, 4)}`;
		const l2Day = words(new Date(1000 * l2Json.feed.window_start).toISOString().slice(0, 10));
		const l2Real = await p.getByTestId('level2-real').innerText();
		expect(l2Real.includes(`real published figure for ${words(l2Json.feed.record_date)}`) && l2Real.includes(`fetched from fiscaldata.treasury.gov on ${l2Day} (UTC)`) && l2Real.includes('local rehearsal'), `data: level-2 copy says real value fetched on ${l2Day}, rehearsal round (W8)`);
		expect(!(await l2.innerText()).toLowerCase().includes('demo data'), 'data: the level-2 card no longer says demo data (W8)');
		expect((await p.getByTestId('level2-observers').innerText()) === `${l2Json.agreed} of ${l2Json.observers} agreed`, 'data: level-2 observer count equals the export');
		expect((await p.getByTestId('level2-draw').innerText()) === `simulated pool of ${l2Json.pool.length}, beacon from a local test chain` && l2Json.pool.length > l2Json.observers, 'data: level-2 card says the pool is simulated and the beacon came from a local test chain');
		expect((await p.getByTestId('level2-copy').innerText()).includes('reran the fetch code, fixed by its hash, on the recorded response'), 'data: preview level-2 copy says the fetcher was rerun (SIG-R1-F3)');
	}
	// THREATS NS5-8, ADR-0048 amendment 1: a free public source's value is free to read, in every build.
	expect(l2Json.sealed !== true, 'data: the Treasury round ships unsealed in every build (free public source)');
	const draws = (d) => `${d} ${d === 1 ? 'draw' : 'draws'}, ${d - 1} without a value`;
	expect((await p.getByTestId('level2-draws').innerText()) === draws(l2Json.draw.announcement.draw_count), 'data: level-2 card shows the fact\'s draws from the checked announcement (SEC-R2-F1)');
	expect((await p.locator('body').innerText()).includes('checked the draw against its recorded announcement (consistency only)'), 'data: level-2 card says the browser checks the recorded draw for consistency only');
	const l2Text = await l2.innerText();
	expect(l2Text.includes('local rehearsal') && l2Text.includes('re-observed') && l2Text.includes('fiscaldata.treasury.gov'), 'data: level-2 card says it is a rehearsal and names the source');
	expect((await l2.getByRole('link', { name: /Fiscal Data/ }).getAttribute('href')) === 'https://fiscaldata.treasury.gov/about-us/', 'data: level-2 card links the source terms');
	// Company holdings (SEC filings): the recorded level-2 rounds on EDGAR,
	// rechecked in the browser; every shown number equals the export.
	const secJson = JSON.parse(await readFile(join(root, 'level2/sec-holdings.json'), 'utf8'));
	const sec = p.getByTestId('sec');
	expect(await sec.getByText('Checks out', { exact: true }).waitFor({ timeout: 120000 }).then(() => true, () => false), 'data: the SEC filing rounds check out');
	for (const co of secJson.companies) {
		const k = co.company.key;
		if (co.sealed) {
			expect(PUB && (await p.getByTestId(`sec-sealed-${k}`).innerText()) === 'Available via the paid API', `data: ${k} sealed in a public build`);
			expect(!('result_hash' in co) && (await p.getByTestId(`sec-observers-${k}`).count()) === 0 && (await p.getByTestId(`sec-draw-${k}`).count()) === 0, `data: ${k} sealed ships and shows no result hash, observers or pool (SIG-R1-F2)`);
		} else {
			const shown = await p.getByTestId(`sec-value-${k}`).innerText();
			expect(shown === co.value.replace(/\B(?=(\d{3})+(?!\d))/g, ','), `data: ${k} holdings equal the export (${shown})`);
			expect((await p.getByTestId(`sec-observers-${k}`).innerText()) === `${co.agreed} of ${co.observers} agreed`, `data: ${k} observer count equals the export`);
			expect((await p.getByTestId(`sec-draw-${k}`).innerText()) === `simulated pool of ${co.pool.length}, beacon from a local test chain` && co.pool.length > co.observers, `data: ${k} card says the pool is simulated and the beacon came from a local test chain`);
		}
		expect(PUB === (co.sealed === true), `data: ${k} is sealed exactly in a public build`);
		expect((await p.getByTestId(`sec-draws-${k}`).innerText()) === draws(co.draw.announcement.draw_count), `data: ${k} card shows the fact's draws from the checked announcement`);
	}
	// ROADMAP P2c: spot bitcoin ETF holdings from SEC filings, recomputed by the verifier;
	// shown in every build (public filing data, not a sold round); every number equals the file.
	{
		const etfJson = JSON.parse(await readFile(join(root, 'level2/etf-holdings.json'), 'utf8'));
		const etf = p.getByTestId('etf');
		expect(await etf.getByText('Checks out', { exact: true }).waitFor({ timeout: 120000 }).then(() => true, () => false), 'data: the ETF filings check out');
		const dec = (v) => {
			const sign = /^[-+]/.test(v) ? v[0] : '';
			const [i, f] = v.slice(sign.length).split('.');
			const t = f ? f.replace(/0+$/, '') : '';
			return sign + i.replace(/\B(?=(\d{3})+(?!\d))/g, ',') + (t ? '.' + t : '');
		};
		expect(etfJson.funds.length >= 7, 'data: ETF file covers the funds');
		for (const f of etfJson.funds) {
			const h = f.holdings[f.holdings.length - 1];
			expect((await p.getByTestId(`etf-value-${f.key}`).innerText()) === dec(h.as_filed), `data: ETF ${f.key} holding equals the file`);
			expect((await p.getByTestId(`etf-date-${f.key}`).innerText()) === h.period_end, `data: ETF ${f.key} period equals the file`);
			expect((await p.getByTestId(`etf-filing-${f.key}`).getAttribute('href')) === h.filing_url && h.filing_url.startsWith('https://www.sec.gov/Archives/edgar/data/'), `data: ETF ${f.key} links its filing`);
		}
		expect((await p.getByTestId('etf-flows-note').innerText()).includes('Filings are quarterly') && (await p.getByTestId('etf-flows-note').innerText()).includes('Daily flows need the issuers\' own daily holdings files'), 'data: ETF section says filings are quarterly and daily flows need another source (ETF-3)');
		// Data review: the rerun command sits in a closed "Reproduce it yourself" fold (textContent reads it).
		const etfRerun = await p.getByTestId('etf-rerun').textContent();
		expect(etfRerun.includes('cargo run -p onyx-source-edgar --example etf-holdings') && etfRerun.includes('no bonded observer round has run on these feeds yet'), 'data: ETF section says how to rerun and that no observer round ran');
		expect((await p.getByTestId('etf-lede').innerText()).includes('not an on-chain fact'), 'data: ETF section says level 2 from filings, not on chain');
		// Data review: both lists sit in one closed "Not covered yet" fold.
		expect((await p.getByTestId('etf-not-covered').textContent()).includes('BITB') && (await p.getByTestId('etf-outside-us').textContent()).includes('Purpose'), 'data: ETF section names the funds not covered');
	}
	// SIG-R1-F3: the SEC copy depends on the sealed flag.
	const secCopy = await p.getByTestId('sec-copy').innerText();
	expect(PUB ? secCopy.includes('ships neither the values nor their result hashes') && !secCopy.includes('reran') && !secCopy.includes('Values shown') : secCopy.includes('reran the fetch code') && secCopy.includes('Values shown are the companies\' real filed figures, fetched from data.sec.gov on ') && secCopy.includes('local rehearsal'), 'data: SEC copy claims only the checks this build ran');
	if (!PUB) {
		// W8: real filed figures, fetched on the recorded window's date; only the rounds were a test.
		const words = (iso) => `${+iso.slice(8, 10)} ${['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'][+iso.slice(5, 7) - 1]} ${iso.slice(0, 4)}`;
		const secDays = [...new Set(secJson.companies.map((co) => words(new Date(1000 * co.feed.window_start).toISOString().slice(0, 10))))];
		expect((await p.getByTestId('sec-real').innerText()).includes(`on ${secDays.join(' and ')} (UTC)`), `data: SEC copy names the fetch date ${secDays.join(', ')} (W8)`);
		for (const co of secJson.companies) expect((await p.getByTestId(`sec-tag-${co.company.key}`).innerText()).toLowerCase() === 'real value · test round', `data: ${co.company.key} value tagged real value, test round (W8)`);
	}
	expect(!(await sec.innerText()).toLowerCase().includes('demo data'), 'data: the SEC cards no longer say demo data (W8)');
	const secText = await sec.innerText();
	expect(secText.includes('not an on-chain fact') && secText.includes('Local rehearsal') && (await p.getByTestId('sec-repro').textContent()).includes('CryptoAssetNumberOfUnits') && secText.includes('company-wide total') && secText.includes('omits XBRL precision') && secText.includes('within rounding, for the periods checked') && !secText.includes('matches their own'), 'data: SEC card says level 2 from filings, a company-wide crypto-asset total, as tagged, not on chain, and a rehearsal');
	expect((await p.getByTestId('sec-terms').getAttribute('href')) === 'https://www.sec.gov/privacy', 'data: SEC card links the SEC terms');
	// One week of history: the index (headers, shards, pinned commitment), then
	// each day's leaves, checked against the pinned commitment. A fallback build ships none.
	const weekDir = join(root, 'history/week');
	const idxJson = FB ? null : JSON.parse(await readFile(join(weekDir, 'index.json'), 'utf8'));
	const lastDay = FB ? null : idxJson.days[idxJson.days.length - 1];
	if (FB) {
		const gone = await Promise.all(['history/week/index.json', 'history/reserves-week.json'].map((f) => readFile(join(root, f)).then(() => false, () => true)));
		expect(gone.every(Boolean), 'fallback: the build ships no per-company history and no reserve history');
	} else {
		const hist = p.getByTestId('history');
		const histCheck = p.getByTestId('history-check');
		expect(await histCheck.getByText('Checks out', { exact: true }).waitFor({ timeout: 240000 }).then(() => true, () => false), 'data: history checks out against the pinned commitment');
		const histTitle = await hist.getByTestId('history-title').innerText();
		expect(histTitle.startsWith(`One week: ${(idxJson.to - idxJson.from + 1).toLocaleString('en-US')} blocks`), 'data: history title names the week and its block count');
		const histTrust = await hist.getByTestId('history-trust').innerText();
		expect(histTrust.includes('Onyx sets') && histTrust.includes('not the amounts') && (await hist.getByTestId('history-repro').textContent()).includes('fetch-btc-node.sh'), 'data: history says who sets the pin and what headers prove (HIST-R1-F1)');
		expect((await hist.locator('.lede').first().innerText()).includes('Test labels, sharing rights not cleared'), 'data: history says the labels are test labels (HIST-R1-F2)');
		const weekRows = idxJson.days.reduce((a, d) => a + d.rows, 0);
		const histEq = await histCheck.locator('.eq').innerText();
		expect(
			histEq.includes(`${idxJson.headers.length.toLocaleString('en-US')} headers`) &&
				histEq.includes(`${idxJson.shards.length} shards`) &&
				histEq.includes(`${idxJson.days.length} of ${idxJson.days.length} days · ${weekRows.toLocaleString('en-US')} rows`) &&
				histEq.includes('pinned'),
			'data: history counts equal the export (headers, shards, days, rows)'
		);
		// 7-day view: one chart per active company; its active six-block steps equal the export's buckets.
		const bucketCounts = {};
		for (const d of idxJson.days) for (const k of d.buckets) for (const e of k.entities) bucketCounts[e.entity] = (bucketCounts[e.entity] ?? 0) + 1;
		// Top companies first; "Show all" draws every active company (IDX-4).
		if ((await hist.getByTestId('history-show-all').count()) > 0) await hist.getByTestId('history-show-all').click();
		const weekTexts = await hist.getByTestId('history-row').allInnerTexts();
		{
			// "All labelled exchanges" (IDX-4): its net equals the sum of the exchange rows' nets,
			// and it never shows a transaction count (one transaction can touch two exchanges).
			const agg = hist.getByTestId('history-all-exchanges');
			const rows = await hist.getByTestId('history-row').evaluateAll((els) => els.map((e) => ({ kind: e.dataset.kind, net: e.dataset.net })));
			const ex = rows.filter((r) => r.kind === 'Exchange');
			if (ex.length >= 2) {
				const sum = ex.reduce((a, r) => a + BigInt(r.net), 0n);
				const aggNet = await agg.getAttribute('data-net');
				expect((await agg.count()) === 1 && (await agg.innerText()).includes('All labelled exchanges'), 'data: an "All labelled exchanges" line (IDX-4)');
				expect(aggNet !== null && BigInt(aggNet) === sum, `data: the exchanges line sums the exchange rows (${aggNet} vs ${sum})`);
				expect(rows.every((r) => r.kind !== 'Custodian' && r.kind !== 'Other'), 'data: no custodian or unknown company in the week');
			} else expect((await agg.count()) === 0, 'data: no exchanges line with fewer than two exchanges');
		}
		expect(
			weekTexts.length === Object.keys(bucketCounts).length && Object.values(bucketCounts).every((n) => weekTexts.some((t) => t.includes(`${n} active six-block steps`))),
			'data: 7 days: one chart per active company, active steps equal the export'
		);
		expect((await hist.getByTestId('history-span').innerText()).startsWith(`Blocks ${idxJson.from.toLocaleString('en-US')}–${idxJson.to.toLocaleString('en-US')}`), 'data: 7-day span is the whole week');
		expect((await hist.locator('.chart svg text.axis.sub').count()) >= 4, 'data: history charts have a block and time axis');
		// 1-day view: the newest day; its active blocks add up to the day's rows.
		await hist.getByTestId('history-range-day').click();
		expect((await hist.getByTestId('history-span').innerText()).startsWith(`Blocks ${lastDay.from.toLocaleString('en-US')}–${lastDay.to.toLocaleString('en-US')}`), 'data: 1 day: the newest day is shown');
		const activeBlocks = (texts) => texts.reduce((a, t) => a + Number((t.match(/(\d+) active blocks/) ?? [0, 0])[1]), 0);
		expect(activeBlocks(await hist.getByTestId('history-row').allInnerTexts()) === lastDay.rows, 'data: 1 day: active blocks add up to the day\'s rows');
		const firstDay = idxJson.days[0];
		await hist.getByTestId('history-days').locator('button').first().click();
		expect((await hist.getByTestId('history-span').innerText()).startsWith(`Blocks ${firstDay.from.toLocaleString('en-US')}`), 'data: 1 day: picking the first day shows it');
		expect(activeBlocks(await hist.getByTestId('history-row').allInnerTexts()) === firstDay.rows, 'data: 1 day: first day active blocks add up to its rows');
		await hist.getByTestId('history-range-week').click();
		await histCheck.getByRole('button', { name: 'Change one number' }).click();
		expect(await histCheck.getByText('Does not check out', { exact: true }).waitFor().then(() => true, () => false), 'data: one changed number in a day is caught');
		expect(await histCheck.getByTestId('history-problems').isVisible(), 'data: the changed day lists its problems');
		await histCheck.getByRole('button', { name: 'Put it back' }).click();
		expect(await histCheck.getByText('Checks out', { exact: true }).waitFor().then(() => true, () => false), 'data: restored history checks out again');
	}

	// Who mined the blocks (ADR-0032): drawn from the checked leaves; numbers equal the export's
	// leaves; never a coinbase string or an excluded list name.
	{
		const poolsDoc = JSON.parse(await readFile(join(root, 'history/pools-week.json'), 'utf8'));
		const sec = p.getByTestId('pools');
		expect(await sec.getByText('Checks out', { exact: true }).waitFor({ timeout: 120000 }).then(() => true, () => false), 'pools: the week checks out');
		expect((await sec.locator('.day').count()) === poolsDoc.days.length, `pools: one bar per day (${poolsDoc.days.length})`);
		const ranked = Object.entries(poolsDoc.totals).filter(([k]) => k !== 'unknown').sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
		const first = await sec.getByTestId('pool-share-0').innerText();
		expect(first.includes(ranked[0][0]) && first.includes(`${ranked[0][1]} ·`), `pools: largest pool is ${ranked[0][0]} with ${ranked[0][1]} blocks`);
		const note = await sec.getByTestId('pool-note').innerText();
		const total = Object.values(poolsDoc.totals).reduce((a, b) => a + b, 0);
		expect(note.startsWith(`${total.toLocaleString('en-US')} blocks, ${poolsDoc.days.length} days`), `pools: ${total} blocks over ${poolsDoc.days.length} days`);
		expect(note.includes('not proof of ownership'), 'pools: the note says the table is a claim, not proof of ownership');
		if (poolsDoc.totals.unknown) expect((await sec.getByTestId('pool-share-unknown').innerText()).includes(`${poolsDoc.totals.unknown} ·`), 'pools: unknown blocks shown honestly');
		const t = await sec.innerText();
		expect(!/Mined by/.test(t), 'pools: no coinbase text on the page');
		const allowed = new Set([...poolsDoc.pools.map((x) => x.name), 'unknown']);
		expect(Object.keys(poolsDoc.totals).every((n) => allowed.has(n)), 'pools: every shown pool is a reviewed allowlist name');
		expect((await sec.getByTestId('pool-legend').innerText()).includes('not credited to a reviewed pool') || !poolsDoc.totals.unknown, 'pools: unknown is labelled honestly');
		// L8-F2: the pool list's MIT notice ships with the site and is linked.
		const lic = await (await fetch(new URL(await p.getByTestId('pool-license').getAttribute('href'), base))).text();
		expect(lic.includes('MIT License') && lic.includes('0xB10C and contributors'), 'pools: the MIT notice of the pool list ships and is linked');
	}

	// Bitcoin on chain (ADR-0043): in every build; the figures are the checked leaves' (here read
	// back from the shipped leaves, never a literal); the rerun of the shipped block gives the
	// window's root; one changed number is caught.
	{
		const chainDoc = JSON.parse(await readFile(join(root, 'onchain/btc-chain-stats.json'), 'utf8'));
		const leaves = chainDoc.days.flatMap((d) => d.leaves);
		// ChainStatsRow.txs: after the key (4 + 8 bytes), the value length (4), height (8), hash (32),
		// time (4), interval (8) and bits (4): byte 72 of the leaf, u64 little endian.
		const txsOf = (hex) => { const b = Buffer.from(hex.slice(2), 'hex'); return b.readBigUInt64LE(72); };
		const totalTxs = leaves.reduce((a, l) => a + txsOf(l), 0n);
		const blocks = chainDoc.to - chainDoc.from + 1;
		const sec = p.getByTestId('onchain');
		expect(await sec.getByTestId('onchain-stats').waitFor({ timeout: 120000 }).then(() => true, () => false), 'onchain: the window checks out and its figures show');
		const stats = await sec.getByTestId('onchain-stats').innerText();
		expect(leaves.length === blocks && stats.includes(n(blocks)) && stats.includes(`${n(chainDoc.from)}–${n(chainDoc.to)}`), `onchain: ${n(blocks)} blocks, ${chainDoc.from}–${chainDoc.to}`);
		expect(stats.includes(totalTxs.toLocaleString('en-US')), `onchain: ${totalTxs.toLocaleString('en-US')} transactions, the sum of the shipped leaves`);
		expect((await sec.locator('.mrow').count()) === 11, 'onchain: eleven metric charts');
		const src = await sec.getByTestId('onchain-source').innerText();
		expect((await sec.getByTestId('onchain-repro').textContent()).includes('btc-chain-stats') && src.includes(`${n(chainDoc.from)}–${n(chainDoc.to)}`) && (await sec.getByTestId('onchain-rerun-cmd').textContent()).includes('golden_chain'), 'onchain: the charts name the recipe, the blocks and the rerun command');
		expect(src.includes('the height that created it') && src.includes('your own node checks them'), 'onchain: the page says prevouts (value, script, creation height) come from the node, checked only by a rerun on your own node');
		expect((await sec.getByTestId('onchain-feerate').innerText()).includes('unweighted by size'), 'onchain: the median fee rate says it counts each transaction once');
		expect((await sec.getByTestId('onchain-planned').innerText()).includes('MVRV'), 'onchain: the planned metrics and what they need are named');
		const check = sec.getByTestId('onchain-check');
		expect((await check.getByTestId('onchain-eq').innerText()).includes('pinned'), 'onchain: the commitment is the pinned one');
		await check.getByTestId('onchain-rerun-btn').click();
		expect(await check.getByText('Same root', { exact: true }).waitFor({ timeout: 180000 }).then(() => true, () => false), `onchain: block ${chainDoc.rerun.height} rerun here gives the window's root`);
		expect((await check.getByTestId('onchain-rerun').innerText()).includes(n(chainDoc.rerun.height)), 'onchain: the rerun names its block');
		await check.getByRole('button', { name: 'Change one number' }).click();
		expect(await check.getByText('Does not check out', { exact: true }).first().waitFor().then(() => true, () => false), 'onchain: one changed number is caught');
		await check.getByRole('button', { name: 'Put it back' }).click();
		expect(await check.getByText('Checks out', { exact: true }).first().waitFor().then(() => true, () => false), 'onchain: restored window checks out again');
	}

	// OSS-12: the bundled Lucide icons' ISC notice and the Feather MIT notice ship with the site.
	{
		const lucide = await (await fetch(new URL('licenses/LICENSE-lucide.txt', base))).text();
		expect(lucide.includes('ISC License') && lucide.includes('Lucide Icons and Contributors') && lucide.includes('Copyright (c) 2013-present Cole Bemis'), 'licences: the Lucide ISC notice and the Feather MIT notice ship with the site');
	}

	// Ethereum on chain (ADR-0044): in every build; the figures are the checked leaves' (read back
	// from the shipped leaves here, never a literal); every chart names the recipe, its blocks and
	// the rerun command; the window's dates show; the rerun of the shipped block gives the window's
	// root; one changed number is caught.
	{
		const ethDoc = JSON.parse(await readFile(join(root, 'onchain/eth-chain-stats.json'), 'utf8'));
		const rows = ethDoc.days.flatMap((d) => d.leaves).map((l) => Buffer.from(l.slice(2), 'hex'));
		// EthChainStatsRow after the key (4 + 8 bytes) and the value length (4): number 16, hash 24,
		// time 56, interval 64, txs 72, ..., fullness 112 (u32), base fee 116 and burned 132 (u128 LE).
		const u128 = (b, o) => b.readBigUInt64LE(o) + (b.readBigUInt64LE(o + 8) << 64n);
		const totalTxs = rows.reduce((a, b) => a + b.readBigUInt64LE(72), 0n);
		const burned = rows.reduce((a, b) => a + u128(b, 132), 0n);
		const E = 10n ** 18n;
		// Data review: the headline tile shows two decimals (the chart's summary keeps four).
		const ethFmt = (w) => `${(w / E).toLocaleString('en-US')}.${(((w % E) * 100n) / E).toString().padStart(2, '0')}`;
		// Owner review: the span reads "19 Oct 2026, 07:54 UTC".
		const words = (iso) => `${+iso.slice(8, 10)} ${['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'][+iso.slice(5, 7) - 1]} ${iso.slice(0, 4)}`;
		const stampOf = (t) => { const iso = new Date(t * 1000).toISOString(); return `${words(iso.slice(0, 10))}, ${iso.slice(11, 16)} UTC`; };
		const blocks = ethDoc.to - ethDoc.from + 1;
		const sec = p.getByTestId('eth-onchain');
		await sec.scrollIntoViewIfNeeded();
		expect(await sec.getByTestId('eth-stats').waitFor({ timeout: 120000 }).then(() => true, () => false), 'eth: the window checks out and its figures show');
		const stats = await sec.getByTestId('eth-stats').innerText();
		expect(rows.length === blocks && stats.includes(n(blocks)) && stats.includes(`${n(ethDoc.from)}–${n(ethDoc.to)}`), `eth: ${n(blocks)} blocks, ${ethDoc.from}–${ethDoc.to}`);
		expect(stats.includes(totalTxs.toLocaleString('en-US')), `eth: ${totalTxs.toLocaleString('en-US')} transactions, the sum of the shipped leaves`);
		expect(stats.includes(ethFmt(burned)), `eth: ${ethFmt(burned)} ETH burned, the sum of the shipped leaves`);
		expect((await sec.locator('.mrow').count()) === 9, 'eth: nine metric charts');
		const by = await sec.getByTestId('eth-by').allInnerTexts();
		// Data review: said once for all the charts (it repeated under each), the command in the section's fold.
		expect(by.length === 1 && (await sec.getByTestId('eth-repro').textContent()).includes('eth-chain-stats') && by.every((t) => t.includes(`${n(ethDoc.from)}–${n(ethDoc.to)}`)) && (await sec.getByTestId('eth-rerun-cmd').textContent()).includes('golden_eth_chain'), 'eth: the charts name the recipe, their blocks and the rerun command');
		const span = await sec.getByTestId('eth-span').innerText();
		expect(span.includes(stampOf(ethDoc.first_time)) && span.includes(stampOf(ethDoc.last_time)), `eth: the window's dates show (${stampOf(ethDoc.first_time)} to ${stampOf(ethDoc.last_time)})`);
		expect((await sec.getByTestId('eth-planned').innerText()).includes('USDT'), 'eth: what is not here yet is named');
		// P4A-R2: the one thing taken on trust is named (ETH-2), and the own-node rerun command runs as printed
		// (a window fetched with --no-cross-check exports only under ONYX_ETH_UNCHECKED=1).
		const trust = await sec.getByTestId('eth-trust').innerText();
		expect(trust.includes('Trusted, not checked: finality'), 'eth: finality is named as trusted (ETH-2)');
		const cmd = await sec.getByTestId('eth-rerun-cmd').textContent();
		expect(cmd.includes('--no-cross-check') && cmd.includes('ONYX_ETH_UNCHECKED=1'), 'eth: the own-node rerun command exports an unchecked window');
		const check = sec.getByTestId('eth-check');
		expect((await check.getByTestId('eth-eq').innerText()).includes('pinned'), 'eth: the commitment is the pinned one');
		await check.getByTestId('eth-rerun-btn').click();
		expect(await check.getByText('Same root', { exact: true }).waitFor({ timeout: 180000 }).then(() => true, () => false), `eth: block ${ethDoc.rerun.number} rerun here gives the window's root`);
		expect((await check.getByTestId('eth-rerun').innerText()).includes(n(ethDoc.rerun.number)), 'eth: the rerun names its block');
		await check.getByRole('button', { name: 'Change one number' }).click();
		expect(await check.getByText('Does not check out', { exact: true }).first().waitFor().then(() => true, () => false), 'eth: one changed number is caught');
		await check.getByRole('button', { name: 'Put it back' }).click();
		expect(await check.getByText('Checks out', { exact: true }).first().waitFor().then(() => true, () => false), 'eth: the restored window checks out again');
	}

	// Reserves (ADR-0034): balances drawn from the checked chain equal the export's rows; the page
	// carries no label address in either build; the recorded live checks are shown.
	if (FB) {
		await p.goto(base + '#/reserves');
		const hb = p.getByTestId('reserves-held-back');
		expect((await hb.innerText()).includes('Company totals are held back until data rights are cleared.'), 'fallback reserves: the page says the reserves are held back and why');
		const body = await p.locator('main').innerText();
		expect(!body.includes('146,358.26346743') && !body.includes('37,290.75352194') && (await p.getByTestId('reserve-cards').count()) === 0 && (await p.getByTestId('reserve-live').count()) === 0, 'fallback reserves: no balance, no card, no published-figure table');
		expect((await p.getByTestId('rosters').count()) === 0 && (await readFile(join(root, 'history/rosters-week.json')).then(() => false, () => true)), 'fallback reserves: no roster section and no roster export (label set v3)');
	} else {
		const rdoc = JSON.parse(await readFile(join(root, 'history/reserves-week.json'), 'utf8'));
		await p.goto(base + '#/reserves');
		const sec = p.getByTestId('reserves');
		expect(await sec.getByText('Checks out', { exact: true }).waitFor({ timeout: 120000 }).then(() => true, () => false), 'reserves: the chain checks out');
		const weekShards = rdoc.shards.filter((s) => s.week);
		const lastWeek = weekShards[weekShards.length - 1];
		expect(lastWeek.to === rdoc.week.to, `reserves: the week's last shard ends at ${rdoc.week.to}`);
		expect((await sec.locator('[data-testid^="reserve-end-"]').count()) === lastWeek.rows.length, `reserves: one card per company (${lastWeek.rows.length})`);
		for (const r of lastWeek.rows) {
			const slug = r.entity.replace(/^entity:/, '');
			const shown = await sec.getByTestId(`reserve-end-${slug}`).innerText();
			expect(shown === `${btcFmt(r.balance_sats)} BTC`, `reserves: ${slug} week-end balance is the export's (${btcFmt(r.balance_sats)})`);
			const scanRow = rdoc.snapshot.totals.find((x) => x.entity === r.entity);
			expect((await sec.getByTestId(`reserve-change-${slug}`).innerText()).includes(btcFmt(scanRow.balance_sats)), `reserves: ${slug} shows the scan total`);
			// THREATS RES-1: only OKX and BITB have an independent cross-check of the base.
			const xc = await sec.getByTestId(`reserve-xcheck-${slug}`).innerText();
			const crossed = slug === 'okx' || slug === 'bitwise-bitb';
			expect(crossed ? xc.startsWith('Cross-checked') : xc.startsWith('Not independently cross-checked'), `reserves: ${slug} cross-check badge (${crossed ? 'cross-checked' : 'not cross-checked'})`);
		}
		const note = await sec.getByTestId('reserves-note').innerText();
		expect(note.startsWith(`${weekShards.length} week shards and ${rdoc.shards.length - weekShards.length} more`), `reserves: ${weekShards.length} week shards and ${rdoc.shards.length - weekShards.length} to the scan`);
		// ADR-0034 Amendment 1: the page says how the numbers are served (endpoint, price, level 4,
		// cross-check flag); OKX and BITB served, Binance and BitMEX refused.
		const api = p.getByTestId('reserve-api');
		expect((await api.getByTestId('reserve-api-endpoint').innerText()).includes('/v1/metrics/btc.entity-reserve'), 'reserves: the API endpoint is shown');
		expect((await api.getByTestId('reserve-api-price').innerText()).includes('2,500 micro-USDC'), 'reserves: the API price is shown');
		expect((await api.getByTestId('reserve-api-level').innerText()).startsWith('4 overall'), 'reserves: the API proof level is 4');
		// ROADMAP D3: the client serving check, in the browser (the note) and for API clients.
		expect(note.includes("derived each company's starting balance from the pinned scan"), 'reserves: the browser derives each base from the pinned scan');
		expect((await api.getByTestId('reserve-api-answer').innerText()).includes('/v1/served/btc.entity-reserve'), 'reserves: the page says how a client re-runs the serving check');
		for (const slug of ['okx', 'bitwise-bitb', 'binance', 'bitmex']) {
			const row = await api.getByTestId(`reserve-api-${slug}`).innerText();
			const served = slug === 'okx' || slug === 'bitwise-bitb';
			expect(served ? row.includes('crossChecked: true') : row.includes('not_cross_checked'), `reserves: ${slug} is ${served ? 'served cross-checked' : 'refused'} by the API`);
		}
		const live = await p.getByTestId('reserve-live').innerText();
		expect(live.includes('146,358.26346743') && live.includes('37,290.75352194'), 'reserves: the recorded OKX and BITB checks are shown');
		const body = await p.locator('main').innerText();
		expect(labelsV2.labels.every((l) => !body.includes(l.address.slice(4))), 'reserves: no label address on the page');
		const shipped = await readFile(join(root, 'history/reserves-week.json'), 'utf8');
		expect(labelsV2.labels.every((l) => !shipped.includes(l.address.slice(4))), 'reserves: the export ships no label address');
		// Label set v3 (ROADMAP P2a): every roster company with its source and grade; no address.
		const ros = JSON.parse(await readFile(join(root, 'history/rosters-week.json'), 'utf8'));
		const rsec = p.getByTestId('rosters');
		expect(await rsec.locator('[data-testid^="roster-grade-"]').first().waitFor({ timeout: 60000 }).then(() => true, () => false), 'reserves: the roster table loads');
		expect(ros.entities.length === 11 && (await rsec.locator('tbody tr').count()) === ros.entities.length, `reserves: one roster row per company (${ros.entities.length})`);
		for (const e of ros.entities) {
			const slug = e.entity.replace(/^entity:/, '');
			expect((await rsec.getByTestId(`roster-grade-${slug}`).innerText()) === e.grade, `reserves: ${slug} roster grade is ${e.grade}`);
			expect((await rsec.getByTestId(`roster-${slug}`).locator('a').getAttribute('href')) === e.source_url, `reserves: ${slug} links its issuer's own roster`);
		}
		expect((await rsec.getByTestId('roster-grade-okx').innerText()) === 'signed', 'reserves: OKX rosters are signed');
		const v3 = await readFile(new URL('../../../fixtures/labels/test.onyx.entities.btc.v3.json', import.meta.url), 'utf8');
		const v3Addrs = JSON.parse(v3).members.map((m) => m.address.slice(4));
		const rtext = await readFile(join(root, 'history/rosters-week.json'), 'utf8');
		const rbody = await rsec.innerText();
		expect(v3Addrs.every((a) => !rtext.includes(a) && !rbody.includes(a)), 'reserves: the roster export and section carry no address');
	}

	await p.goto(base + '#/labels');
	// E10: the verifier worker first decodes the label set (one job at a time); click once that
	// is done, then wait for the verdict, whichever it is, and check it.
	await settled(p);
	const attPill = p.locator('.panel', { has: p.getByRole('heading', { name: 'Attestation', exact: true }) }).locator('.pill');
	await p.getByRole('button', { name: 'Check both signatures' }).click();
	await attPill.evaluate((e) => new Promise((ok) => { const t0 = Date.now(); const f = () => (/\b(idle|run)\b/.test(e.className) && Date.now() - t0 < 120000 ? setTimeout(f, 100) : ok()); f(); }));
	const attText = await attPill.innerText();
	expect(attText === 'Valid: both signatures check out', `labels: attestation valid (the pill says "${attText}")`);
	await p.getByRole('button', { name: 'Change the text, then check' }).click();
	expect(await p.getByText(/Refused: entity signature invalid/).waitFor().then(() => true, () => false), 'labels: changed text refused');
	await p.getByRole('button', { name: 'Run the vectors' }).click();
	expect(await p.getByText(new RegExp(`All ${nVectors} match`)).waitFor().then(() => true, () => false), `labels: ${nVectors} BIP-322 vectors match`);
	// W12 round 8 review: each result square shows its result as a shape too (a tick for a match, a
	// cross for a difference; not colour alone, W3) and names its vector and result in words.
	{
		const V = await p.evaluate(() => {
			const glyph = (e) => getComputedStyle(e, '::before').maskImage;
			const sq = [...document.querySelectorAll('ul.dots > li')];
			const probe = Object.assign(document.createElement('li'), { className: sq[0]?.className.replace(/\bok\b/, 'no') ?? '' });
			document.querySelector('ul.dots').append(probe);
			const cross = glyph(probe);
			probe.remove();
			const tick = sq.map(glyph);
			return { n: sq.length, ok: sq.filter((e) => e.classList.contains('ok')).length, named: sq.filter((e) => /^\S+ .*: \w+, matches$/.test(e.textContent.trim())).length, tick: tick.every((t) => t.includes('svg') && t.includes('M20 6')), cross: cross.includes('svg') && cross.includes('M18 6') };
		});
		expect(V.n === nVectors && V.ok === nVectors && V.named === nVectors && V.tick && V.cross, `labels: ${V.n} vector squares, ${V.ok} ticked and ${V.named} named "variant body: result, matches"; a differing result draws a cross (${V.cross})`);
	}
	if (PUB) {
		const ls = p.getByTestId('public-label-set');
		expect(await ls.getByText('Pinned label set', { exact: true }).waitFor({ timeout: WAIT_CAP }).then(() => true, () => false), 'public labels: the set hash is the pinned one');
		expect((await ls.innerText()).includes(`${n(nLabels)} labels`), 'public labels: count from the export');
		expect((await p.locator('#filter').count()) === 0, 'public labels: no label table');
	} else {
		// Label set v2, decoded from the pinned input; counts from the fixture file; the table shows
		// the first 200 and the filter narrows to one company.
		await p.getByText(`Browse all ${n(nLabels)} labels`).waitFor({ timeout: 120000 });
		expect(await p.getByText(`${Object.keys(byEntity).length} companies`).isVisible(), `labels: v2 names ${Object.keys(byEntity).length} companies`);
		{
			const groups = await p.getByTestId('label-groups').innerText();
			expect(new RegExp(`OKX\\s+(?:Exchanges\\s+)?${n(byEntity['entity:okx'])}\\s+Level 3`).test(groups), 'labels: OKX labels are signed, level 3');
			expect(new RegExp(`Bitwise BITB\\s+(?:ETFs\\s+)?${n(byEntity['entity:bitwise-bitb'])}\\s+Level 4`).test(groups), 'labels: published lists are claims, level 4');
			expect((await p.getByTestId('label-groups-caption').innerText()).includes('every flow is a'), 'labels: caption says flows are level 4 (IDX-R1-F5)');
		}
		await p.getByText(`Browse all ${n(nLabels)} labels`).click();
		expect(await p.getByText(`${n(nLabels)} match · first 200 shown`).waitFor().then(() => true, () => false), 'labels: table capped at 200 rows');
		expect((await p.locator('details.more .tablebox tbody tr').count()) === 200, 'labels: 200 rows rendered');
		await p.fill('#filter', 'entity:bitwise-bitb');
		expect(await p.getByText(`${n(byEntity['entity:bitwise-bitb'])} match`, { exact: true }).waitFor().then(() => true, () => false), `labels: filter finds the ${byEntity['entity:bitwise-bitb']} Bitwise BITB labels`);

	}

	await p.goto(base + '#/api');
	if (FB) {
		await p.getByTestId('api-scope').waitFor();
		expect((await p.getByTestId('api-held-back').innerText()).includes('held back until data rights clear'), 'fallback api: says a fallback node holds back label-derived metrics until data rights clear');
		expect((await p.getByRole('button', { name: 'Check the proof against the pinned root' }).count()) === 0, 'fallback api: no sample company row');
	} else {
		await p.getByRole('button', { name: 'Check the proof against the pinned root' }).click();
		expect(await p.getByText('Valid against the pinned root').waitFor().then(() => true, () => false), 'api: sample proof checks against the pinned root');
	}
	{
		const note = await p.getByTestId('api-verify-default').innerText();
		expect(note.includes('ClientNode::verify') && note.includes('NOT VERIFIED ON CHAIN'), 'api: the client checks roots on its own node by default');
		// M7L3-R1-5: the real state, without overclaiming; the button says what it checks.
		const page = await p.locator('body').innerText();
		for (const stale of ['Planned for milestone M7', 'draft endpoints', 'Root on Base (M6)', 'until M6']) {
			expect(!page.includes(stale), `api: no stale "${stale}"`);
		}
		const status = await p.getByTestId('api-status').innerText();
		expect(status.includes('testnet only') && status.includes('onyx.cryptagraph.com/v1'), 'api: testnet only, the hosted test node named');
		expect(page.includes('Roots on Base Sepolia since 2026-10-12'), 'api: roots on Base Sepolia since 2026-10-12');
		const scope = await p.getByTestId('api-scope').textContent();
		for (const m of ['btc.entity-flow', 'btc.entity-reserve', 'Mining pools', 'level-2 observations', 'x402']) {
			expect(scope.includes(m), `api: endpoints listed for ${m}`);
		}
		// M7L3-R2-4: per dataset, what served it, where, and how it was paid.
		expect(scope.includes('onyx-api') && !scope.includes('onyx-node'), 'api: served by onyx-api, not onyx-node');
		// The per-run detail sits in the folded "Run notes": read its text, not what is drawn.
		const flow = (await p.getByTestId('api-scope-flow').textContent()).replace(/\s+/g, ' ');
		expect(
			flow.includes('mock facilitator') && flow.includes('no token moves') && flow.includes('Base Sepolia') && flow.includes('2026-10-14') && flow.includes('self-settle') && flow.includes('OnyxRevenue'),
			'api: entity-flow local (mock, no token moves) and Base Sepolia (real testnet USDC, self-settle into OnyxRevenue)'
		);
		const reserve = (await p.getByTestId('api-scope-reserve').textContent()).replace(/\s+/g, ' ');
		// W7: the 2026-10-14 run sold a reserve row on Base Sepolia (ROADMAP A2).
		expect(
			reserve.includes('local rehearsal') && reserve.includes('no token moves') && reserve.includes('Base Sepolia run of 2026-10-14') && reserve.includes('serving check') && reserve.includes('not_cross_checked'),
			'api: entity-reserve in the local rehearsal (mock payments) and sold on Base Sepolia 2026-10-14 behind its serving check'
		);
		// M7L3-R3-1: the 2026-10-12 run's second sale (standard facilitator, receipt credited) and the local level-2 serve.
		expect(
			flow.includes('twice') && flow.includes('standard x402 facilitator') && flow.includes('transferWithAuthorization') && flow.includes('unallocated') && flow.includes('reporter credited'),
			'api: entity-flow sold twice on Base Sepolia, the second through the standard-facilitator receipt path'
		);
		const level2 = (await p.getByTestId('api-scope-level2').textContent()).replace(/\s+/g, ' ');
		expect(
			level2.includes('/v1/observations') && level2.includes('local rehearsal only') && level2.includes('mock facilitator') && level2.includes('no token moves') && !level2.includes('Base Sepolia'),
			'api: level-2 observations served in the local rehearsal, paid through the mock facilitator'
		);
		const tested = (await p.getByTestId('api-scope-tests').textContent()).replace(/\s+/g, ' ');
		expect(tested.includes('Mining pools') && tested.includes('test suites') && tested.includes('no rehearsal') && !tested.includes('level-2'), 'api: only pools are served in tests only');
		expect(!status.includes('M7 rehearsals'), 'api: no "M7 rehearsals" claim');
		if (!FB) expect((await p.getByTestId('api-check-scope').innerText()).includes('does not read the chain'), 'api: the button says it checks a pinned root, not the chain');
	}

	await p.goto(base + '#/how-it-works');
	await p.getByRole('button', { name: /Search pages and actions/ }).waitFor();
	await p.keyboard.press('Control+k');
	await p.keyboard.type('expl');
	await p.keyboard.press('Enter');
	expect(await p.waitForURL(/#\/explorer$/).then(() => true, () => false), 'command palette: Ctrl+K, type, Enter navigates');

	await p.goto(base + '#/does-not-exist');
	expect(await p.getByRole('heading', { name: 'No page here' }).waitFor().then(() => true, () => false), 'unknown route: styled not-found page');

	// A forger edits the data next to the page. Checks against pinned values must fail.
	const forged = await browser.newPage();
	if (FB) {
		// A fallback build has no company rows to edit: a swapped label set and an edited pool
		// leaf must each fail their check.
		await forged.route('**/data/demo.json', async (route) => {
			const d = await (await route.fetch()).json();
			d.label_set.content_hash = '0x' + 'ab'.repeat(32);
			await route.fulfill({ json: d });
		});
		await forged.route('**/history/pools-week.json', async (route) => {
			const j = await (await route.fetch()).json();
			const leaf = j.days[3].leaves[0];
			j.days[3].leaves[0] = leaf.slice(0, -2) + (leaf.endsWith('00') ? '01' : '00');
			await route.fulfill({ json: j });
		});
		await forged.goto(base + '#/how-it-works');
		await settled(forged);
		await forged.getByRole('button', { name: new RegExp(`Run all ${NCHK} checks`) }).click();
		await checksDone(forged);
		const failedTitles = (await forged.locator('.checks li').allInnerTexts()).filter((t) => t.includes('Failed')).map((t) => t.split('\n')[0]);
		expect(failedTitles.includes('Label set is the pinned one'), 'fallback forged: swapped label set refused');
		expect(failedTitles.includes('Pool shares hash to the pinned commitment'), 'fallback forged: edited pool leaf refused');
	} else {
		await forged.route('**/data/demo.json', async (route) => {
			const d = await (await route.fetch()).json();
			const leaf = d.live.rows[1].leaf;
			d.live.rows[1].leaf = leaf.slice(0, -52) + (leaf.at(-52) === '0' ? '1' : '0') + leaf.slice(-51);
			// A company row of block 970,545: flip a byte of one of its amounts.
			const pb = d.live.per_block[2].rows[1];
			pb.leaf = pb.leaf.slice(0, -52) + (pb.leaf.at(-52) === '0' ? '1' : '0') + pb.leaf.slice(-51);
			// One series row: flip a byte of one of its amounts.
			const sr = d.live.series.rows[0];
			sr.leaf = sr.leaf.slice(0, -52) + (sr.leaf.at(-52) === '0' ? '1' : '0') + sr.leaf.slice(-51);
			if (PUB) {
				const r = d.runs[0].rows[0];
				r.leaf = r.leaf.slice(0, -52) + (r.leaf.at(-52) === '0' ? '1' : '0') + r.leaf.slice(-51);
				d.label_set.content_hash = '0x' + 'ab'.repeat(32);
			} else d.runs[0].input = 'data/input-half.b64.txt';
			await route.fulfill({ json: d });
		});
		await forged.goto(base + '#/how-it-works');
		await settled(forged);
		await forged.getByRole('button', { name: /Run all 7 checks/ }).click();
		await checksDone(forged);
		const status = await forged.locator('.checks li').allInnerTexts();
		const failedTitles = status.filter((t) => t.includes('Failed')).map((t) => t.split('\n')[0]);
		if (PUB) {
			expect(failedTitles.includes('Label set is the pinned one'), 'public forged: swapped label set refused');
			expect(failedTitles.includes('Run rows hash to the pinned root'), 'public forged: edited run row refused');
		} else {
			expect(failedTitles.includes('Blocks and labels are the pinned ones'), 'forged: swapped input refused');
			expect(failedTitles.includes('Recipe rerun gives the pinned root'), 'forged: swapped input gives the wrong root');
		}
		expect(failedTitles.includes('Live rows hash to the pinned root'), 'forged: edited live row refused');

		await forged.route(`**/history/week/${lastDay.file}`, async (route) => {
			const d = await (await route.fetch()).json();
			// One amount in one history row: flip a hex digit of a leaf.
			const sh = d.shards.find((s) => s.series_leaves.length > 1);
			const leaf = sh.series_leaves[1];
			sh.series_leaves[1] = leaf.slice(0, -52) + (leaf.at(-52) === '0' ? '1' : '0') + leaf.slice(-51);
			await route.fulfill({ json: d });
		});
		await forged.goto(base + '#/data');
		expect(await forged.getByTestId('history-check').getByText('Does not check out', { exact: true }).waitFor({ timeout: 240000 }).then(() => true, () => false), 'forged: edited history row does not check out');
		expect((await forged.getByTestId('history-failed').innerText()).includes(lastDay.date), 'forged: the edited day is named and not drawn');
		// E9: the sections above finish their own checks after this one and grow, which moved the
		// button under a click taken too early; click once the page is still, and see it pressed.
		await settled(forged);
		const rangeDay = forged.getByTestId('history-range-day');
		await rangeDay.click();
		await attr(rangeDay, 'aria-pressed', 'true');
		expect(await forged.getByTestId('history-days').locator('button').last().isDisabled(), 'forged: the edited day cannot be shown');
	}

	// HIST-W1-F1: an honest day served at another day's URL, with that other
	// day's six-block buckets edited in the index (buckets are not bound by the
	// pinned commitment). The swapped slot is named as failed, not drawn.
	if (!FB) {
		const swap = await browser.newPage();
		const X = idxJson.days[idxJson.days.length - 2];
		const Y = idxJson.days[idxJson.days.length - 1];
		const forgedIdx = JSON.parse(JSON.stringify(idxJson));
		const fx = forgedIdx.days[forgedIdx.days.length - 2];
		const k = fx.buckets.find((b) => b.entities.length > 0);
		k.entities[0].net_sats = '-500000000000';
		k.entities[0].sent_sats = (BigInt(k.entities[0].received_sats) + 500000000000n).toString();
		await swap.route('**/history/week/index.json', (r) => r.fulfill({ json: forgedIdx }));
		await swap.route(`**/history/week/${X.file}`, async (r) => r.fulfill({ body: await readFile(join(weekDir, Y.file), 'utf8'), contentType: 'application/json' }));
		await swap.goto(base + '#/data');
		expect(await swap.getByTestId('history-check').getByText('Does not check out', { exact: true }).waitFor({ timeout: 240000 }).then(() => true, () => false), 'swap: a day served in another day\'s slot does not check out (HIST-W1-F1)');
		expect((await swap.getByTestId('history-failed').innerText()).includes(X.date), 'swap: the swapped slot is named as failed');
		await swap.close();
	}

	// A forged level-2 value (the response and every hash left as they were)
	// fails the browser check and is not shown.
	{
		const l2Forged = await browser.newPage();
		const forgedL2 = JSON.parse(await readFile(join(root, 'level2/debt-to-penny.json'), 'utf8'));
		// Sealed (public build), the result hash is what a forger edits.
		if (forgedL2.sealed) forgedL2.result_hash = '0x' + '11'.repeat(32);
		else forgedL2.value = '30305316210829.72';
		await l2Forged.route('**/level2/debt-to-penny.json', (r) => r.fulfill({ json: forgedL2 }));
		await l2Forged.goto(base + '#/data');
		const c = l2Forged.getByTestId('level2');
		expect(await c.getByText('did not pass the check').waitFor({ timeout: 120000 }).then(() => true, () => false), 'forged level-2 value: the round does not check out');
		expect((await l2Forged.getByTestId('level2-value').count()) === 0, 'forged level-2 value: no value is shown');
		await l2Forged.close();
	}

	// L2-F2: a hand-picked observer set (a drawn observer swapped for an
	// undrawn pool member) fails the browser's draw check and is not shown.
	{
		const pg = await browser.newPage();
		const j = JSON.parse(await readFile(join(root, 'level2/debt-to-penny.json'), 'utf8'));
		const drawn = new Set(j.bonds.map((b) => b.observer));
		const outsider = j.pool.find((m) => !drawn.has(m.observer));
		j.bonds[0] = outsider;
		(j.reveals ?? j.commits)[0].observer = outsider.observer;
		j.bonds.sort((a, b) => (a.observer < b.observer ? -1 : 1));
		await pg.route('**/level2/debt-to-penny.json', (r) => r.fulfill({ json: j }));
		await pg.goto(base + '#/data');
		const c = pg.getByTestId('level2');
		expect(await c.getByText('did not pass the check').waitFor({ timeout: 120000 }).then(() => true, () => false), 'hand-picked level-2 observers: the round does not check out');
		expect((await pg.getByTestId('level2-value').count()) === 0, 'hand-picked level-2 observers: no value is shown');
		await pg.close();
	}

	// ATT-R1-F3: a commit carrying another observer's signature fails the
	// browser check: every commit is signed by its own observer's key.
	{
		const pg = await browser.newPage();
		const j = JSON.parse(await readFile(join(root, 'level2/debt-to-penny.json'), 'utf8'));
		const entries = j.reveals ?? j.commits;
		entries[0].commit_sig = entries[1].commit_sig;
		await pg.route('**/level2/debt-to-penny.json', (r) => r.fulfill({ json: j }));
		await pg.goto(base + '#/data');
		const c = pg.getByTestId('level2');
		expect(await c.getByText('did not pass the check').waitFor({ timeout: 120000 }).then(() => true, () => false), 'moved observer signature: the round does not check out');
		expect((await pg.getByTestId('level2-value').count()) === 0, 'moved observer signature: no value is shown');
		await pg.close();
	}

	// A leaf of the Ethereum window edited (still decodable) fails: no chart is drawn, and the
	// rerun input and recipe load only on the button (ADR-0044).
	{
		const pg = await browser.newPage();
		const urls = [];
		pg.on('request', (r) => urls.push(new URL(r.url()).pathname));
		const j = JSON.parse(await readFile(join(root, 'onchain/eth-chain-stats.json'), 'utf8'));
		const leaf = j.days[0].leaves[10];
		j.days[0].leaves[10] = leaf.slice(0, -1) + (parseInt(leaf.slice(-1), 16) ^ 1).toString(16);
		await pg.route('**/onchain/eth-chain-stats.json', (r) => r.fulfill({ json: j }));
		await pg.goto(base + '#/data');
		expect(await pg.getByTestId('eth-failed').waitFor({ timeout: 120000 }).then(() => true, () => false), 'forged eth: the window does not check out');
		expect((await pg.getByTestId('eth-onchain').locator('.mrow').count()) === 0, 'forged eth: no chart is drawn');
		expect(!urls.some((u) => /eth-rerun-input|eth-chain-stats\.wasm/.test(u)), 'eth: the rerun input and recipe load only when asked');
		await pg.close();
	}
	// A leaf edited in the pool history (even with its day's root rebuilt) fails: nothing is drawn.
	{
		const pg = await browser.newPage();
		const j = JSON.parse(await readFile(join(root, 'history/pools-week.json'), 'utf8'));
		const leaf = j.days[3].leaves[0];
		j.days[3].leaves[0] = leaf.slice(0, -2) + (leaf.endsWith('00') ? '01' : '00');
		await pg.route('**/history/pools-week.json', (r) => r.fulfill({ json: j }));
		await pg.goto(base + '#/data');
		expect(await pg.getByTestId('pools-failed').waitFor({ timeout: 120000 }).then(() => true, () => false), 'forged pools: the week does not check out');
		expect((await pg.getByTestId('pool-shares').count()) === 0, 'forged pools: no bars are drawn');
		await pg.close();
	}
	// A leaf of the on-chain window edited (still decodable) fails: no chart is drawn, and the
	// rerun input and recipe load only on the button.
	{
		const pg = await browser.newPage();
		const urls = [];
		pg.on('request', (r) => urls.push(new URL(r.url()).pathname));
		const j = JSON.parse(await readFile(join(root, 'onchain/btc-chain-stats.json'), 'utf8'));
		const leaf = j.days[2].leaves[1];
		j.days[2].leaves[1] = leaf.slice(0, -1) + (parseInt(leaf.slice(-1), 16) ^ 1).toString(16);
		await pg.route('**/onchain/btc-chain-stats.json', (r) => r.fulfill({ json: j }));
		await pg.goto(base + '#/data');
		expect(await pg.getByTestId('onchain-failed').waitFor({ timeout: 120000 }).then(() => true, () => false), 'forged onchain: the window does not check out');
		expect((await pg.getByTestId('onchain').locator('.mrow').count()) === 0, 'forged onchain: no chart is drawn');
		expect(!urls.some((u) => /rerun-input|chain-stats\.wasm/.test(u)), 'onchain: the rerun input and recipe load only when asked');
		await pg.close();
	}
	// A reserve leaf edited, or a base moved by one satoshi, fails: no balance is drawn.
	if (!FB) for (const [name, forge] of [
		['leaf', (j) => { const l = j.shards[5].leaves[1]; j.shards[5].leaves[1] = l.slice(0, -2) + (l.endsWith('00') ? '01' : '00'); }],
		['base', (j) => { j.base.rows[0].balance_sats = String(BigInt(j.base.rows[0].balance_sats) + 1n); }]
	]) {
		const pg = await browser.newPage();
		const j = JSON.parse(await readFile(join(root, 'history/reserves-week.json'), 'utf8'));
		forge(j);
		await pg.route('**/history/reserves-week.json', (r) => r.fulfill({ json: j }));
		await pg.goto(base + '#/reserves');
		expect(await pg.getByTestId('reserves-failed').waitFor({ timeout: 120000 }).then(() => true, () => false), `forged reserves (${name}): the chain does not check out`);
		expect((await pg.getByTestId('reserve-cards').count()) === 0, `forged reserves (${name}): no balance is drawn`);
		await pg.close();
	}
	// SEC-5: a forged filing value, or a forged source link, fails the browser
	// check; no value is shown and no forged text appears.
	for (const [name, forge] of [
		['value', (j) => (j.companies[0].sealed ? (j.companies[0].result_hash = '0x' + '22'.repeat(32)) : (j.companies[0].value = '862099'))],
		['terms', (j) => (j.source.terms_url = 'https://evil.example/terms')]
	]) {
		const pg = await browser.newPage();
		const j = JSON.parse(await readFile(join(root, 'level2/sec-holdings.json'), 'utf8'));
		forge(j);
		await pg.route('**/level2/sec-holdings.json', (r) => r.fulfill({ json: j }));
		await pg.goto(base + '#/data');
		const c = pg.getByTestId('sec');
		expect(await pg.getByTestId('sec-failed').waitFor({ timeout: 120000 }).then(() => true, () => false), `forged SEC ${name}: the rounds do not check out`);
		expect((await pg.getByTestId('sec-value-mstr').count()) === 0, `forged SEC ${name}: no value is shown`);
		const t = await c.innerText();
		expect(!t.includes('862,099') && !t.includes('evil.example'), `forged SEC ${name}: none of the forged text is shown`);
		await pg.close();
	}

	// ETF-4: an edited ETF file (value or terms link) fails the check and nothing from it is shown.
	for (const [name, forge] of [
		['value', (j) => (j.funds[0].holdings[j.funds[0].holdings.length - 1].as_filed = '934261')],
		['terms', (j) => (j.source.terms_url = 'https://evil.example/terms')]
	]) {
		const pg = await browser.newPage();
		const j = JSON.parse(await readFile(join(root, 'level2/etf-holdings.json'), 'utf8'));
		forge(j);
		await pg.route('**/level2/etf-holdings.json', (r) => r.fulfill({ json: j }));
		await pg.goto(base + '#/data');
		expect(await pg.getByTestId('etf-failed').waitFor({ timeout: 120000 }).then(() => true, () => false), `forged ETF ${name}: the filings do not check out`);
		const t = await pg.getByTestId('etf').innerText();
		expect((await pg.getByTestId('etf-table').count()) === 0 && !t.includes('934,261') && !t.includes('evil.example'), `forged ETF ${name}: nothing forged is shown`);
		await pg.close();
	}

	// L2SRC-R1-F3: a self-consistent forgery (another date and value, the page
	// hash and every commit recomputed from the revealed salts; generated by
	// the verifier's native test) fails on the pinned spec, response and result.
	{
		const l2Consistent = await browser.newPage();
		const forgedC = JSON.parse(await readFile(new URL('./fixtures/level2-forged.json', import.meta.url), 'utf8'));
		await l2Consistent.route('**/level2/debt-to-penny.json', (r) => r.fulfill({ json: forgedC }));
		await l2Consistent.goto(base + '#/data');
		const c = l2Consistent.getByTestId('level2');
		expect(await c.getByText('did not pass the check').waitFor({ timeout: 120000 }).then(() => true, () => false), 'consistent level-2 forgery: the round does not check out');
		expect((await l2Consistent.getByTestId('level2-value').count()) === 0, 'consistent level-2 forgery: no value is shown');
		await l2Consistent.close();
	}

	// L2SRC-R2-F3: a summary that keeps every hash but swaps what the card
	// shows (source name and link, licence quote, attribution, observer count)
	// does not check out, and none of the forged text is displayed.
	{
		const l2Shown = await browser.newPage();
		const forgedS = JSON.parse(await readFile(join(root, 'level2/debt-to-penny.json'), 'utf8'));
		forgedS.source.name = 'U.S. Treasury (official, certified)';
		forgedS.source.terms_url = 'https://evil.example/login';
		forgedS.source.terms_quote = 'certified by the U.S. Treasury';
		forgedS.source.attribution = 'Certified by Onyx and the Treasury';
		forgedS.observers = 50;
		await l2Shown.route('**/level2/debt-to-penny.json', (r) => r.fulfill({ json: forgedS }));
		await l2Shown.goto(base + '#/data');
		const c = l2Shown.getByTestId('level2');
		expect(await c.getByText('did not pass the check').waitFor({ timeout: 120000 }).then(() => true, () => false), 'forged level-2 card fields: the round does not check out');
		expect((await c.getByText('Checks out', { exact: true }).count()) === 0, 'forged level-2 card fields: never "Checks out"');
		const t = await c.innerText();
		expect(!t.includes('evil.example') && !t.includes('certified') && !t.includes('of 50'), 'forged level-2 card fields: none of the forged text is shown');
		await l2Shown.close();
	}

	// HIST-W2-F1/F3: a truncated day file and a missing (404) one. Each names
	// that day as failed, the other days are still drawn, nothing stays
	// "still being checked", and the week view says it covers fewer days.
	if (!FB) for (const [name, serve] of [
		['partial', async (r, D) => r.fulfill({ body: (await readFile(join(weekDir, D.file), 'utf8')).slice(0, 4000), contentType: 'application/json' })],
		['404', async (r) => r.fulfill({ status: 404, body: '' })]
	]) {
		const pg = await browser.newPage();
		const D = idxJson.days[idxJson.days.length - 4];
		await pg.route(`**/history/week/${D.file}`, (r) => serve(r, D));
		await pg.goto(base + '#/data');
		const hc = pg.getByTestId('history-check');
		expect(await hc.getByText('Does not check out', { exact: true }).waitFor({ timeout: 240000 }).then(() => true, () => false), `${name}: a broken day file does not check out (HIST-W2-F1)`);
		expect((await pg.getByTestId('history-failed').innerText()).includes(D.date), `${name}: the broken day is named as failed`);
		expect((await pg.getByTestId('history-pending').count()) === 0, `${name}: nothing stays "still being checked"`);
		expect((await pg.getByTestId('history-row').count()) > 0, `${name}: the other days are still drawn`);
		expect((await pg.getByTestId('history-title').innerText()).includes(`(${idxJson.days.length - 1} of ${idxJson.days.length} days checked)`), `${name}: the title says how many days are drawn (HIST-W2-F3)`);
		expect(!(await pg.getByTestId('history-partial').innerText()).includes(D.date), `${name}: the week totals list only the drawn days`);
		// HIST-W2-F2: with a failed day, the demo cannot turn the check green.
		expect(await hc.getByRole('button', { name: 'Change one number' }).isDisabled(), `${name}: the demo is off while a day has failed (HIST-W2-F2)`);
		await pg.close();
	}
	if (!FB) {
		expect(await forged.getByTestId('sums').getByText('Does not add up').waitFor({ timeout: 120000 }).then(() => true, () => false), 'forged: edited block row does not add up');
		expect(await forged.getByTestId('series-check').getByText('Does not add up', { exact: true }).waitFor({ timeout: 120000 }).then(() => true, () => false), 'forged: edited series row does not add up');
	}

	// A quiet range (SER-R2-F4): the real block 970,551, where no labelled company
	// moved coins. Its series is the recipe's NO_ACTIVITY leaf; the page checks it
	// against the pinned quiet roots and shows an all-zero series, no charts.
	if (!FB) {
		const quietLive = JSON.parse(await readFile(new URL('./fixtures/live-quiet.json', import.meta.url), 'utf8'));
		const quiet = await browser.newPage();
		await quiet.route('**/data/demo.json', async (route) => {
			const d = await (await route.fetch()).json();
			d.live = quietLive;
			await route.fulfill({ json: d });
		});
		await quiet.goto(base + '#/data');
		expect(await quiet.getByTestId('series-check').getByText('Adds up', { exact: true }).waitFor({ timeout: 120000 }).then(() => true, () => false), 'quiet: the no-activity series checks against the pinned quiet roots');
		expect(await quiet.getByTestId('series-no-activity').isVisible(), 'quiet: the page says no labelled company moved coins');
		expect((await quiet.getByTestId('series-row').count()) === 0, 'quiet: no company charts for a quiet range');
		await quiet.close();
	}

	// Live run: the page shows the recorded run (run/live-run.json) and the
	// shared fee-split vectors, with the numbers they hold.
	const runJson = JSON.parse(await readFile(join(root, 'run/live-run.json'), 'utf8'));
	const cases = JSON.parse(await readFile(join(root, 'data/revenue-vectors.json'), 'utf8')).cases;
	await p.goto(base + '#/run');
	expect(await p.getByTestId('honest').waitFor({ timeout: WAIT_CAP }).then(() => true, () => false), 'run: honest round drawn');
	expect((await p.getByTestId('run-txs').innerText()) === runJson.totals.txs.toLocaleString('en-US'), 'run: total transactions match the recorded run');
	const honestTx = (await p.getByTestId('honest').getByTestId('txs').allInnerTexts()).map(Number);
	const wantTx = runJson.steps.filter((s) => !s.step.startsWith('r2.')).map((s) => s.txs);
	expect(JSON.stringify(honestTx) === JSON.stringify(wantTx), 'run: each honest step shows its recorded transactions');
	expect((await p.getByTestId('dispute').locator('li').count()) === runJson.steps.filter((s) => s.step.startsWith('r2.')).length, 'run: every dispute step drawn');
	expect((await p.getByTestId('cheater').innerText()).includes('bond 4 taken'), 'run: the cheater lost its bond');
	const v = cases.find((c) => c.amount === runJson.round.served.price_micro_usdc && c.miners === 1 && c.validators === 4 && c.labelSet && c.minersBps === 5000);
	const share = async (k) => Number((await p.getByTestId(`share-${k}`).innerText()).replace(/,/g, ''));
	expect(
		v &&
			(await share('miners')) === Number(v.perMiner) &&
			(await share('validators')) === 4 * Number(v.perValidator) &&
			(await share('labels')) === Number(v.label) &&
			(await share('treasury')) === Number(v.treasury),
		'run: fee split equals the shared vector'
	);
	// P5: the hardware section names every role, and each number in it is in docs/ops/hardware.md
	// (measured or linked there), so the page cannot carry a figure the doc does not back.
	const hw = p.getByTestId('hardware');
	const hwText = await hw.innerText();
	const hwDoc = await readFile(new URL('../../../docs/ops/hardware.md', import.meta.url), 'utf8');
	const hwNums = [...new Set(hwText.match(/\d[\d,]*(\.\d+)?/g) ?? [])];
	expect((await hw.getByTestId('hw-row').count()) === 6 && ['Contributor', 'Validator', 'indexer', 'Node operator', 'Buyer', 'Coordinator'].every((r) => hwText.includes(r)), 'run: the hardware table has a row per role');
	const hwAll = await hw.textContent();
	expect(hwAll.includes('onyx-node doctor') && hwAll.includes('docs/ops/hardware.md'), 'run: the hardware section names the preflight and the doc (folded under "How this was built")');
	const hwMissing = hwNums.filter((n) => !hwDoc.includes(n));
	expect(hwNums.length >= 15 && hwMissing.length === 0, `run: all ${hwNums.length} hardware figures are in docs/ops/hardware.md${hwMissing.length ? ': missing ' + hwMissing.join(', ') : ''}`);
	const policy = JSON.parse(await readFile(join(root, 'data/contributor-policy.json'), 'utf8'));
	const rule = await p.getByTestId('contrib-rule').innerText().catch(() => '');
	expect(
		rule.includes(`last ${policy.windowDays} days`) &&
			rule.includes(`${policy.readsPerMinute.toLocaleString('en-US')} reads a minute`) &&
			rule.includes(`${policy.rowsPerDay.toLocaleString('en-US')} rows a day`),
		'run: the free-read rule shows the API defaults (contributor-policy.json)'
	);
	const prices = JSON.parse(await readFile(join(root, 'data/pricing-defaults.json'), 'utf8'));
	const priceRule = await p.getByTestId('price-rule').innerText().catch(() => '');
	expect(
		priceRule.includes(`${prices.answerMicroUsdc / 10000}¢ an answer`) &&
			priceRule.includes(`${prices.datasetMicroUsdc / 10000}¢ a fresh dataset`) &&
			priceRule.includes(`less than ${prices.datasetFloorRows} rows`),
		'run: the prices show the API defaults (pricing-defaults.json, AUD-2-F11)'
	);

	// The full runs on Base Sepolia (run/base-sepolia-<date>.json, each from its run's own logs),
	// found in the build, latest first: for each, the verdict, every step and every transaction
	// linked to basescan, apart from the anvil rehearsal.
	const netFiles = (await readdir(join(root, 'run'))).filter((f) => /^base-sepolia-\d{4}-\d{2}-\d{2}\.json$/.test(f)).sort().reverse();
	const netRuns = await Promise.all(netFiles.map(async (f) => JSON.parse(await readFile(join(root, 'run', f), 'utf8'))));
	expect(netRuns.length >= 3 && netRuns.every((r, i) => `base-sepolia-${r.date}.json` === netFiles[i]), `testnet: ${netRuns.length} recorded runs, each file named by its date (${netFiles.join(', ')})`);
	const MIN_TX_LINKS = 150;
	const tn = p.getByTestId('testnet-run');
	await tn.getByTestId('net-pick').first().waitFor({ timeout: WAIT_CAP }).catch(() => {});
	const picks = tn.getByTestId('net-pick');
	// W7: buttons for the newest three runs; with more, a dropdown of every run beside them.
	const CHIPS = 3;
	const sel = tn.getByTestId('net-select');
	expect((await picks.count()) === Math.min(netRuns.length, CHIPS), `run: the picker has a button for each of the newest ${Math.min(netRuns.length, CHIPS)} runs`);
	expect(
		(await sel.locator('option').count()) === netRuns.length && (await sel.isVisible()) === netRuns.length > CHIPS,
		`run: the dropdown lists every recorded run (${netRuns.length}), shown on a wide screen only past ${CHIPS} runs`
	);
	expect((await picks.first().getAttribute('aria-pressed')) === 'true' && (await picks.first().innerText()).includes(netRuns[0].date), `run: the latest run (${netRuns[0].date}) is shown first`);
	const allNetUrls = [];
	for (const [i, netJson] of netRuns.entries()) {
		const d = netJson.date;
		const netUrls = netJson.steps.flatMap((s) => s.txs.map((t) => t.url));
		allNetUrls.push(...netUrls);
		expect(netJson.verdict === 'REHEARSAL PASSED' && netJson.chain_id === 84532 && netUrls.length >= MIN_TX_LINKS, `testnet ${d}: the recorded run passed on chain 84532 with ≥ ${MIN_TX_LINKS} tx links (${netUrls.length})`);
		if (i < CHIPS) await picks.nth(i).click();
		else await sel.selectOption({ index: i });
		expect(await tn.locator(`[data-testid="net-body"][data-date="${d}"]`).waitFor({ timeout: WAIT_CAP }).then(() => true, () => false), `run ${d}: picked and drawn`);
		expect(i >= CHIPS || (await picks.nth(i).getAttribute('aria-pressed')) === 'true', `run ${d}: its picker button is pressed`);
		expect((await sel.inputValue()) === String(i), `run ${d}: the dropdown names the drawn run`);
		// W8: the recorded verdict is the driver's "REHEARSAL PASSED"; the page calls a public testnet run a run (the rehearsal is the local anvil one).
		expect(await tn.getByText('Run passed', { exact: true }).waitFor({ timeout: WAIT_CAP }).then(() => true, () => false), `run ${d}: Base Sepolia run says Run passed`);
		// W8: no words glued together where an {#if} block starts (Svelte trims a block's leading space).
		const tnText = await tn.innerText();
		expect(tnText.includes(`Public testnet · chain ${netJson.chain_id}`) && !/page\)\.\S/.test(tnText) && (!tnText.includes('is the same as the local rehearsal') || tnText.includes("page). Root ")), `run ${d}: spaces kept around the chain id and the root note`);
		expect((await tn.getByTestId('net-sent').innerText()) === netJson.totals.sent_txs.toLocaleString('en-US'), `run ${d}: transactions sent match the record (${netJson.totals.sent_txs})`);
		expect((await tn.getByTestId('net-step').count()) === netJson.steps.length, `run ${d}: every Base Sepolia step drawn (${netJson.steps.length})`);
		const txHrefs = await tn.locator('a[href^="https://sepolia.basescan.org/tx/0x"]').evaluateAll((as) => as.map((a) => a.getAttribute('href')));
		expect(txHrefs.length >= MIN_TX_LINKS && JSON.stringify(txHrefs) === JSON.stringify(netUrls), `run ${d}: ${txHrefs.length} basescan tx links, each a recorded transaction in order`);
		expect(txHrefs.every((h) => /^https:\/\/sepolia\.basescan\.org\/tx\/0x[0-9a-f]{64}$/.test(h)) && new Set(txHrefs).size === txHrefs.length, `run ${d}: tx links are distinct well-formed basescan links`);
		const addrHrefs = await tn.getByTestId('net-contract').evaluateAll((as) => as.map((a) => a.getAttribute('href')));
		expect(addrHrefs.length === netJson.contracts.length && addrHrefs.every((h, k) => h === `https://sepolia.basescan.org/address/${netJson.contracts[k].address}`), `run ${d}: ${addrHrefs.length} contract addresses link to basescan`);
		// Runs from 2026-10-13 on record the API's chain indexer (step 6a) and the floor registrations.
		if (netJson.indexer) {
			const ixStep = netJson.steps.find((s) => s.id === 'r1.index');
			expect(
				// Step 5 prints the head read after finalizeShard, the indexer the Finalized log's own block:
				// equal, or the head a few blocks ahead on a live chain (2026-10-14: one block).
				ixStep &&
					netJson.round.finalized_block - netJson.indexer.finalized_block >= 0 &&
					netJson.round.finalized_block - netJson.indexer.finalized_block <= 5 &&
					(await tn.getByTestId('net-step').filter({ hasText: 'The API finds the root on chain' }).count()) === 1,
				`run ${d}: the indexer step is drawn (Finalized at block ${netJson.indexer.finalized_block}, ${netJson.indexer.get_logs_calls} eth_getLogs calls)`
			);
		}
		// Runs from 2026-10-14 on (ROADMAP A2): a reserve round settles (5b) and one of its rows is sold (8).
		const r = netJson.reserve;
		// ROADMAP L14: a fallback build ships the run with its OKX figure held back.
		const okxShown = FB ? 'the same company total as the scan (held back until data rights are cleared)' : `${r?.okx_btc} BTC = the scan`;
		if (FB && r) expect(r.okx_btc === null && netJson.round.okx_net_btc == null, `fallback run ${d}: the record carries no OKX figure`);
		if (r) {
			const sold = tn.getByTestId('net-step').filter({ hasText: 'A reserve row sold behind its serving check' });
			const settled = tn.getByTestId('net-step').filter({ hasText: 'A reserve round settles on chain' });
			const soldText = (await sold.count()) === 1 ? await sold.innerText() : '';
			expect(
				(await settled.count()) === 1 && soldText.includes(okxShown) && soldText.includes(`${n(Number(r.price_micro_usdc))} micro-USDC booked to the shard`) && soldText.includes(`${r.gate} cleared it`),
				`run ${d}: the reserve round settles and its OKX row is sold (${r.okx_btc} BTC, ${r.price_micro_usdc} micro-USDC, ${r.gate})`
			);
			expect(soldText.includes('Binance refused (no independent cross-check)') && r.binance_status === 403, `run ${d}: the reserve step shows Binance refused with 403`);
			const settleTx = await sold.locator('a[href^="https://sepolia.basescan.org/tx/"]').evaluateAll((as) => as.map((a) => a.getAttribute('href')));
			expect(settleTx.length === 1 && settleTx[0].endsWith(r.settle_tx), `run ${d}: the reserve sale links its settle transaction`);
			const line = await tn.getByTestId('net-reserve').innerText().catch(() => '');
			expect(line.includes(`Reserve round ${r.round}`) && line.includes(FB ? 'its value held back until data rights are cleared' : `${r.okx_btc} BTC`) && line.includes('Binance refused before any payment'), `run ${d}: the reserve summary line names the round, the OKX total and the Binance refusal`);
		} else {
			expect((await tn.getByTestId('net-reserve').count()) === 0, `run ${d}: no reserve line for a run without a reserve round`);
		}
	}
	expect(new Set(allNetUrls).size === allNetUrls.length, 'run: no transaction is linked in two runs');
	expect(netRuns[0].reserve && netRuns[0].steps.some((s) => s.id === 'r3.serve'), `run ${netRuns[0].date}: the latest run sold a reserve row`);
	// On a phone the picker is one dropdown of every run (no row of wrapping buttons).
	{
		const ph = await browser.newPage({ viewport: { width: 390, height: 844 } });
		await ph.goto(base + '#/run');
		const psel = ph.getByTestId('testnet-run').getByTestId('net-select');
		await psel.waitFor({ timeout: WAIT_CAP }).catch(() => {});
		const box = await psel.boundingBox();
		expect(
			(await psel.isVisible()) && (await ph.getByTestId('testnet-run').getByTestId('net-pick').first().isVisible()) === false && box && box.height >= 44,
			'phone run: the picker is a dropdown at least 44 px tall, the buttons are hidden'
		);
		await psel.selectOption({ index: netRuns.length - 1 });
		const last = netRuns.at(-1).date;
		expect(await ph.locator(`[data-testid="net-body"][data-date="${last}"]`).waitFor({ timeout: WAIT_CAP }).then(() => true, () => false), `phone run: choosing ${last} in the dropdown draws it`);
		expect((await ph.evaluate(() => document.documentElement.scrollWidth - innerWidth)) === 0, 'phone run: no sideways scroll with the dropdown');
		await ph.close();
	}
	// W9 follow-up (CLS): until the latest run's file loads, the heading shows the date in the
	// file's name (each file is named by the date it records, checked above) and no status pill;
	// once it loads, the date the file records is the one shown, even if the name said otherwise.
	// The picker does not move when the date and the pill arrive (Lighthouse's phone viewport).
	{
		const pg = await browser.newPage({ viewport: { width: 412, height: 823 } });
		let release;
		const held = new Promise((r) => (release = r));
		const other = '2099-01-01';
		await pg.route(`**/run/${netFiles[0]}`, async (route) => {
			await held;
			await route.fulfill({ json: { ...netRuns[0], date: other } });
		});
		await pg.goto(base + '#/run');
		const sec = pg.getByTestId('testnet-run');
		const h = sec.locator('.sh h2').first();
		const picker = sec.getByTestId('net-picker');
		await picker.waitFor({ timeout: WAIT_CAP }).catch(() => {});
		await pg.evaluate(() => document.fonts.ready);
		const before = await picker.boundingBox();
		expect(
			(await h.innerText()) === `On Base Sepolia, ${netRuns[0].date}` && (await sec.locator('.badges .pill').count()) === 0 && (await sec.locator('.tag').first().innerText()) === 'Public testnet',
			`run (loading): the heading shows the file name's date (${netRuns[0].date}), no status pill and no chain yet`
		);
		release();
		const drawn = await sec.locator(`[data-testid="net-body"][data-date="${other}"]`).waitFor({ timeout: WAIT_CAP }).then(() => true, () => false);
		const after = await picker.boundingBox();
		expect(drawn && (await h.innerText()) === `On Base Sepolia, ${other}`, `run (loaded): the heading shows the date the file records (${other}), not the one in its name`);
		expect(before && after && before.y === after.y && before.height === after.height, `run: the picker stays put when the run loads (y ${before?.y} -> ${after?.y})`);
		await pg.close();
	}
	// W9 follow-up (CLS): private Labels' placeholder carries the panel heading and no pill while
	// the label set decodes; the slot does not move when the decoded table replaces it.
	if (!PUB) {
		const pg = await browser.newPage({ viewport: { width: 412, height: 823 } });
		let release;
		const held = new Promise((r) => (release = r));
		await pg.route('**/data/input-*.b64.txt', async (route) => {
			await held;
			await route.continue();
		});
		await pg.goto(base + '#/labels');
		const slot = pg.locator('.setslot');
		await slot.getByRole('status', { name: 'Decoding the label set' }).waitFor({ timeout: WAIT_CAP }).catch(() => {});
		await pg.evaluate(() => document.fonts.ready);
		const before = await slot.boundingBox();
		expect(
			(await slot.locator('h2').allInnerTexts()).join('|') === 'Label set' && (await slot.locator('.pill').count()) === 0 && (await slot.getByRole('status', { name: 'Decoding the label set' }).count()) === 1,
			'labels (decoding): the placeholder shows the Label set heading, no pill, and says it is decoding'
		);
		release();
		const decoded = await slot.locator('.pill').first().waitFor({ timeout: WAIT_CAP }).then(() => true, () => false);
		const after = await slot.boundingBox();
		expect(decoded && before && after && before.y === after.y, `labels: the label set slot stays put when the set decodes (y ${before?.y} -> ${after?.y})`);
		await pg.close();
	}
	// W9 follow-up (LCP): app.html preloads data/demo.json on the pages that read it at once; the
	// page's fetch reuses that request (one request), and pages that never read it ask for none.
	for (const [hash, want] of [['#/data', 1], ['#/labels', 1], ['#/run', 0], ['#/reserves', 0], ['#/no-such-page', 0]]) {
		const pg = await browser.newPage();
		let asked = 0;
		pg.on('request', (r) => new URL(r.url()).pathname === '/data/demo.json' && asked++);
		await pg.goto(base + hash, { waitUntil: 'load' });
		// E12: a request made late would be counted after the page has stopped changing for 1.5 s (it
		// was a fixed 1.5 s sleep, which a loaded machine could spend before the page drew).
		await quietDom(pg, { quiet: 1500, timeout: WAIT_CAP });
		const via = await pg.evaluate(() => performance.getEntriesByType('resource').filter((e) => e.name.endsWith('/data/demo.json')).map((e) => e.initiatorType));
		expect(asked === want && (want === 0 || via.join() === 'link'), `${hash}: data/demo.json requested ${asked} time(s)${want ? ', by the preload' : ''} (want ${want})`);
		await pg.close();
	}
	expect(netRuns[0].indexer && netRuns[0].registry?.some((r) => r.metric === 'crypto.btc.entity_reserve'), `run ${netRuns[0].date}: the latest run records its indexer step and the reserve metric's registration`);
	expect((await p.getByTestId('anvil-run').innerText()).includes('Local test chain') && !(await tn.innerText()).includes('Local test chain'), 'run: the local test chain rehearsal is a separate, labelled section');
	// ADR-0033: what the chain indexer finds on Base Sepolia (run/indexer-base-sepolia-2026-10-12.json,
	// a read-only read by scripts/read-indexer-sepolia.py), cross-checked with the recorded run.
	const chainIdx = JSON.parse(await readFile(join(root, 'run/indexer-base-sepolia-2026-10-12.json'), 'utf8'));
	// The run whose settlement contract the read is of.
	const netJson = netRuns.find((r) => r.contracts.some((c) => c.address === chainIdx.settlement));
	expect(netJson, `indexer: the read is of a recorded run's settlement contract (${netJson?.date})`);
	const netUrls = netJson ? netJson.steps.flatMap((s) => s.txs.map((t) => t.url)) : [];
	const ix = p.getByTestId('indexer-run');
	await ix.getByTestId('idx-event').first().waitFor({ timeout: WAIT_CAP }).catch(() => {});
	const ixEvents = ix.getByTestId('idx-event');
	expect(
		chainIdx.chain_id === 84532 && chainIdx.deploy.ok && chainIdx.deploy.code_hex_chars_before === 0 && chainIdx.deploy.code_hex_chars_at > 0 && chainIdx.all_agree && chainIdx.events.length === 2,
		'indexer: the read checked the deploy block and found 2 Finalized events whose roots the contract confirms'
	);
	expect(
		(await ixEvents.count()) === chainIdx.events.length && (await ix.getByTestId('idx-agrees').count()) === chainIdx.events.length && (await ix.getByTestId('idx-agree').innerText()) === `${chainIdx.events.length}/${chainIdx.events.length}`,
		`indexer: every event drawn, each confirmed by getFinalizedRoot (${chainIdx.events.length})`
	);
	const r1 = chainIdx.events.find((e) => e.round === 1);
	expect(
		r1 && netJson && r1.block === netJson.round.finalized_block && r1.root === netJson.round.root && r1.shard === netJson.round.shard && r1.view.root === r1.root,
		`indexer: round 1's event is the recorded run's root, shard and block (${netJson?.round.finalized_block})`
	);
	const ixTx = await ix.getByTestId('idx-tx').evaluateAll((as) => as.map((a) => a.getAttribute('href')));
	expect(
		JSON.stringify(ixTx) === JSON.stringify(chainIdx.events.map((e) => e.tx_url)) && ixTx.every((h) => netUrls.includes(h)),
		'indexer: each event links to its transaction on basescan, a transaction of the recorded run'
	);
	expect(
		(await ix.getByTestId('idx-deploy').innerText()).includes(`Deploy block ${n(chainIdx.deploy.block)}`) && (await ix.getByTestId('idx-calls').innerText()) === String(chainIdx.scan.get_logs_calls),
		`indexer: deploy block ${chainIdx.deploy.block} and ${chainIdx.scan.get_logs_calls} eth_getLogs calls shown from the read`
	);
	expect(
		netJson && (await ix.getByTestId('idx-scope').innerText()).includes(`settlement contract the ${netJson.date} run deployed.`) && !/deployed\.\S/.test(await ix.getByTestId('idx-scope').innerText()) && (await ix.innerText()).includes(`the ${netJson.date} run's contract`),
		`indexer: the section names the run whose contract it read (${netJson?.date})`
	);
	// W5: the home page says what Onyx is in one sentence, shows three proof points that each
	// open their record (Run, Reserves, the in-browser checks), one primary and one secondary
	// next step, and a calm testnet note; the public build shows no label row.
	const homeReqs = [];
	const onReq = (r) => homeReqs.push(new URL(r.url()).pathname);
	p.on('request', onReq);
	await p.goto(base + '#/how-it-works');
	const head = p.getByTestId('testnet-headline');
	expect(await head.waitFor({ timeout: WAIT_CAP }).then(() => true, () => false), 'home: Base Sepolia proof card shows');
	const times = netRuns.length === 2 ? ' twice' : ` ${netRuns.length} times`;
	const runCard = p.getByTestId('proof-run');
	const runText = await runCard.innerText();
	expect(
		(await head.innerText()) === `Full run passed live on Base Sepolia${times}` &&
			runText.includes(`Latest ${netRuns[0].date}: ${n(netRuns[0].totals.sent_txs)} transactions${netRuns[0].reserve ? ', reserve data sold on chain' : ''}`) &&
			netRuns.slice(1).every((r) => runText.includes(r.date)),
		`home: proof 1 says the full run passed live${times} and names the latest (${netRuns[0].date}, ${netRuns[0].totals.sent_txs} transactions) and earlier runs, from the run files`
	);
	const what = await p.getByTestId('home-what').innerText();
	expect(
		(await p.locator('h1').innerText()) === 'How Onyx checks data' &&
			/^Onyx is a contributor-run network for any kind of data: anyone can submit data, others check it by the strongest method the data allows, and buyers pay per row and check every answer against a root posted on Base Sepolia \(testnet\)\.$/.test(what.replace(/\s+/g, ' ').trim()),
		'home: the headline and one sentence say what Onyx is'
	);
	// NORTH-STAR NS1: the page speaks about any data; Bitcoin is named as one example.
	const kinds = p.getByTestId('data-kinds');
	expect(
		(await kinds.locator('h2').innerText()) === 'One network for any data' &&
			(await kinds.locator('li.kind').count()) === 6 &&
			(await kinds.innerText()).includes('It is one example, not the limit.'),
		'home: six kinds of data, each with its level, and Bitcoin named as one example'
	);
	// W5 review: the roots are on the testnet, and the sentence says so (no bare "on Base").
	expect(what.includes('Base Sepolia (testnet)') && !/on Base\.|on Base\b(?! Sepolia)/.test(what), 'home: the hero sentence names Base Sepolia (testnet), not Base');
	expect(
		(await p.locator('meta[name="description"]').getAttribute('content')).includes('every submission needs review'),
		'site: metadata invites contributions without promising publication'
	);
	const note = p.getByTestId('home-note');
	const noteText = (await note.innerText()).replace(/\s+/g, ' ');
	expect(
		(await note.isVisible()) && noteText.startsWith(PUB ? 'Testnet only · early preview' : 'Testnet only · private preview') && noteText.includes('test USDC on Base Sepolia') && noteText.includes('not switched on yet'),
		`home: the testnet note shows above the fold ("${noteText}")`
	);
	const noteBox = await note.boundingBox();
	const vp = p.viewportSize();
	expect(noteBox && noteBox.y + noteBox.height <= vp.height, `home: the testnet note is inside the first screen at ${vp.width} x ${vp.height}`);
	const primary = p.getByTestId('home-primary');
	const secondary = p.getByTestId('home-secondary');
	expect(
		(await primary.evaluate((e) => e.tagName)) === 'BUTTON' && (await primary.innerText()).trim() === 'Verify it in your browser' && (await secondary.getAttribute('href')) === '#/run' && (await secondary.innerText()).trim() === 'See the live run',
		'home: one primary step (verify in your browser) and one secondary (see the live run)'
	);
	const ctaStyle = await p.evaluate(() => [...document.querySelectorAll('.pitch button, .pitch a')].map((e) => e.className));
	expect(ctaStyle.length === 2 && ctaStyle.filter((c) => /ghost/.test(c)).length === 1, 'home: the hero has exactly two calls to action, one filled and one ghost');
	// Proof 2: the figures are the committed cross-checks' (fixtures/reserves/cross-checks.json),
	// so src/lib/reserves.ts cannot drift from them; the link opens the Reserves page.
	// ROADMAP L14: in a fallback build card 2 says the company totals are held back, and links the same page.
	const resCard = p.getByTestId(FB ? 'proof-held-back' : 'proof-reserves');
	const resText = (await resCard.innerText()).replace(/\s+/g, ' ');
	// Each card's link is its title (a stretched link): one link, a short spoken name.
	const runLink = runCard.getByRole('link');
	const resLink = resCard.getByRole('link');
	const linkNames = await Promise.all([runLink, resLink].map((l) => l.evaluate((a) => [a.closest('h3') !== null, a.textContent.trim().replace(/\s+/g, ' '), a.getAttribute('aria-label')])));
	expect(
		(await runLink.count()) === 1 && (await resLink.count()) === 1 && (await resLink.getAttribute('href')) === '#/reserves' && (await runLink.getAttribute('href')) === '#/run',
		'home: proof links point at the Run and Reserves pages'
	);
	expect(
		linkNames.every(([inH3, name, label]) => inH3 && label === null && name.length > 20 && name.length < 90) && linkNames[0][1] === `Full run passed live on Base Sepolia${netRuns.length === 2 ? ' twice' : ` ${netRuns.length} times`}`,
		`home: each proof link is named by its card title only (${linkNames.map((l) => l[1].length).join(', ')} characters)`
	);
	const crossDoc = JSON.parse(await readFile(new URL('../../../fixtures/reserves/cross-checks.json', import.meta.url), 'utf8'));
	const xc = (e) => crossDoc.checks.find((c) => c.entity === e);
	const okxX = xc('entity:okx');
	const bitbX = xc('entity:bitwise-bitb');
	expect(
		[okxX, bitbX].every((c) => c && c.theirs_sats && c.theirs_sats === c.onyx_sats && c.addresses_exact === c.addresses_compared),
		'reserve cross-checks: OKX and BITB record their published and Onyx totals, equal, every address exact'
	);
	if (FB)
		expect(
			resText.includes('Company totals are held back until data rights are cleared') && [okxX, bitbX].every((c) => !resText.includes(btcFmt(c.onyx_sats))) && (await p.getByTestId('proof-reserves').count()) === 0,
			'fallback home: proof 2 says the company totals are held back and why, with no reserve figure'
		);
	else expect(
		resText.includes('Reserves for OKX and BITB matched their own published figures exactly') &&
			resText.includes(`OKX: ${btcFmt(okxX.onyx_sats)} BTC over its ${n(okxX.addresses_compared)} addresses at its 2026-09-08 snapshot (block 965,949), against OKX's own proof-of-reserves address list.`) &&
			resText.includes(`Bitwise BITB: ${btcFmt(bitbX.onyx_sats)} BTC over its ${n(bitbX.addresses_compared)} wallets`),
		`home: proof 2 shows the committed OKX (${btcFmt(okxX.onyx_sats)}, at its 2026-09-08 snapshot) and BITB (${btcFmt(bitbX.onyx_sats)}) totals`
	);
	await runCard.click();
	expect(await p.locator('h1', { hasText: 'Live run' }).waitFor({ timeout: WAIT_CAP }).then(() => true, () => false) && (await p.evaluate(() => location.hash)) === '#/run', 'home: proof 1 opens the Run page');
	await p.goto(base + '#/how-it-works');
	await resCard.click();
	const resLive = p.getByTestId(FB ? 'reserves-held-back' : 'reserve-live');
	expect(await resLive.waitFor({ timeout: WAIT_CAP }).then(() => true, () => false) && (await p.evaluate(() => location.hash)) === '#/reserves', 'home: proof 2 opens the Reserves page');
	const resLiveText = await resLive.innerText();
	if (!FB) expect(
		[okxX, bitbX].every((c) => resLiveText.split(btcFmt(c.theirs_sats)).length === 3) &&
			resLiveText.includes(`${n(okxX.addresses_exact)} of ${n(okxX.addresses_compared)} addresses exact`) &&
			resLiveText.includes(`${bitbX.addresses_exact} of ${bitbX.addresses_compared} wallets exact`),
		"reserves: the published-figures table shows the committed cross-checks' totals (theirs = Onyx) and address counts"
	);
	await p.goto(base + '#/how-it-works');
	await p.getByTestId('home-secondary').click();
	expect(await p.locator('h1', { hasText: 'Live run' }).waitFor({ timeout: WAIT_CAP }).then(() => true, () => false), 'home: the secondary step opens the Run page');
	// Proof 3 runs the in-browser checks and says the result on the card.
	await p.goto(base + '#/how-it-works');
	const verify = p.getByTestId('proof-verify');
	const vBtn = verify.getByRole('button');
	await p.waitForFunction(() => !document.querySelector('[data-testid="proof-verify"] button')?.disabled, null, { timeout: WAIT_CAP });
	expect((await vBtn.innerText()).trim() === `Run the ${NCHK} checks`, `home: proof 3 offers to run the ${NCHK} checks here`);
	await settled(p);
	await vBtn.click();
	// Focus moves a frame after the click: wait for that frame, not a fixed 150 ms. E10 review:
	// it must land while the checks still run (focus that came only once they ended would pass
	// a plain wait).
	const focusAt = await p
		.waitForFunction(
			() => {
				const here = document.activeElement?.id === 'checks-h';
				const running = !!document.querySelector('.console button')?.disabled;
				return here ? (running ? 'while the checks run' : 'after the checks ended') : !running && 'never (the checks ended)';
			},
			null,
			{ timeout: WAIT_CAP, polling: 'raf' }
		)
		.then((h) => h.jsonValue(), () => 'not in 60 s');
	expect(focusAt === 'while the checks run', `home: running the checks moves focus to the checks (their heading): ${focusAt}`);
	await checksDone(p);
	expect((await p.locator('.tally').innerText()).startsWith(`${NCHK} passed`) && (await vBtn.innerText()).includes(`${NCHK} of ${NCHK} passed`), `home: proof 3 runs the in-browser checks, ${NCHK} of ${NCHK} pass, and the card says so`);
	// Review of round 14: focus reaches the checks before their first result even when the next frame comes late
	// (at load 56 the checks once ended in their worker before the frame that moved focus). Frames are held back
	// 8 s after the click; the heading must take focus with the checks running and none of them finished.
	{
		const lc = await browser.newContext({ viewport: { width: 1280, height: 800 }, reducedMotion: 'reduce' });
		await lc.addInitScript(() => {
			const raf = window.requestAnimationFrame.bind(window);
			window.requestAnimationFrame = (cb) => (window.__lateFrames ? raf(() => setTimeout(() => raf(cb), window.__lateFrames)) : raf(cb));
		});
		const lp = await lc.newPage();
		await lp.goto(base + '#/how-it-works');
		await lp.waitForFunction(() => !document.querySelector('[data-testid="proof-verify"] button')?.disabled, null, { timeout: 120000 });
		await settled(lp);
		await lp.evaluate(() => {
			document.getElementById('checks-h').addEventListener('focus', () => (window.__atFocus = { done: document.querySelectorAll('.checks li.good, .checks li.bad').length, running: !!document.querySelector('.console button')?.disabled }), { once: true });
			window.__lateFrames = 8000;
		});
		await lp.getByTestId('proof-verify').getByRole('button').click();
		const A = await lp.waitForFunction(() => window.__atFocus, null, { timeout: 120000, polling: 100 }).then((h) => h.jsonValue(), () => null);
		await checksDone(lp);
		expect(!!A && A.running && A.done === 0, `home: with the next frame 8 s late, focus reaches the checks while they run, before any result (${A ? `${A.done} finished` : 'no focus in 120 s'})`);
		await lc.close();
	}
	// No label row on the home page (either build); the public build fetches no recipe input.
	const homeBody = await p.locator('main').innerText();
	expect(labelsV2.labels.every((l) => !homeBody.includes(l.address.slice(4))), 'home: no label address on the page');
	p.off('request', onReq);
	if (PUB) expect(!homeReqs.some((u) => /input-.*\.b64\.txt$/.test(u)), `public home: no recipe input (label rows) fetched (${homeReqs.length} requests)`);
	// Layout: three proof cards side by side at 1280 px, one column at 390 px.
	const cardBoxes = async (pg) => pg.locator('.pgrid > li').evaluateAll((ls) => ls.map((l) => { const r = l.getBoundingClientRect(); return [Math.round(r.x), Math.round(r.y), Math.round(r.width)]; }));
	{
		const wide = await browser.newPage({ viewport: { width: 1280, height: 800 } });
		await wide.goto(base + '#/how-it-works');
		await wide.getByTestId('proof-run').waitFor();
		const w = await cardBoxes(wide);
		expect(w.length === 3 && new Set(w.map((b) => b[1])).size === 1 && w[0][0] < w[1][0] && w[1][0] < w[2][0], `home 1280 px: three proof cards in one row (${JSON.stringify(w)})`);
		await wide.close();
		const narrow = await browser.newPage({ viewport: { width: 390, height: 844 } });
		await narrow.goto(base + '#/how-it-works');
		await narrow.getByTestId('proof-run').waitFor();
		const nb = await cardBoxes(narrow);
		expect(nb.length === 3 && new Set(nb.map((b) => b[0])).size === 1 && nb[0][1] < nb[1][1] && nb[1][1] < nb[2][1] && nb.every((b) => b[2] <= 390 - 32), `home 390 px: proof cards stack in one column inside the screen (${JSON.stringify(nb)})`);
		const h1Size = await narrow.locator('h1').evaluate((e) => parseFloat(getComputedStyle(e).fontSize));
		expect(h1Size >= 40 && h1Size <= 48 && (await narrow.evaluate(() => document.documentElement.scrollWidth - innerWidth)) === 0, `home 390 px: headline ${h1Size} px, no sideways scroll`);
		await narrow.close();
	}

	// W2: phone layout at 390 px on every page: no sideways scroll and nothing past the
	// right edge, the menu reaches every page, primary controls are 44 x 44 px tap targets,
	// no text under 12 px, and a skip link comes first.
	const phone = await browser.newPage({ viewport: { width: 390, height: 844 } });
	const ROUTES = [['', 'Home'], ['#/how-it-works', 'How it works'], ['#/data', 'Browse data'], ['#/reserves', 'Reserves'], ['#/run', 'Run'], ['#/explorer', 'Explorer'], ['#/labels', 'Labels'], ['#/api', 'API'], ['#/submit', 'Add data'], ['#/markets','Markets']];
	const PRIMARY = [['#/data', 'Browse data'], ['#/markets', 'Markets'], ['#/how-it-works', 'How it works'], ['#/submit', 'Add data']];
	// Each page is measured once settled (tests/wait.mjs): drawn, no placeholder or running
	// check left (the private Explorer decodes the recipe input first), and the DOM still.
	const layout = (pg) =>
		pg.evaluate(() => {
			const W = innerWidth;
			const vis = (e) => {
				const r = e.getBoundingClientRect();
				return r.width > 0 && r.height > 0 && getComputedStyle(e).visibility !== 'hidden';
			};
			const desc = (e) => {
				const r = e.getBoundingClientRect();
				return `${e.tagName.toLowerCase()} "${(e.getAttribute('aria-label') || e.textContent || '').trim().replace(/\s+/g, ' ').slice(0, 30)}" ${Math.round(r.width)}x${Math.round(r.height)}`;
			};
			const clipped = (e) => {
				for (let n = e.parentElement; n; n = n.parentElement) if (/auto|scroll|hidden|clip/.test(getComputedStyle(n).overflowX)) return true;
				return false;
			};
			const all = [...document.querySelectorAll('body *')].filter(vis);
			const past = all.filter((e) => e.getBoundingClientRect().right > W + 1 && !clipped(e));
			// A link inside running text (a sentence around it) is exempt (WCAG 2.5.8 inline exception).
			const inline = (e) => e.tagName === 'A' && getComputedStyle(e).display === 'inline' && [...e.parentElement.childNodes].some((n) => n !== e && n.nodeType === 3 && n.textContent.trim());
			const taps = all.filter((e) => e.matches('a[href], button, input, select, textarea, summary, [role=button]') && !inline(e));
			const small = taps.filter((e) => {
				const r = e.getBoundingClientRect();
				return r.width < 44 - 0.5 || r.height < 44 - 0.5;
			});
			const inner = all.filter((e) => /auto|scroll/.test(getComputedStyle(e).overflowX) && e.scrollWidth > e.clientWidth + 1);
			const tinyText = all.filter((e) => [...e.childNodes].some((n) => n.nodeType === 3 && n.textContent.trim()) && parseFloat(getComputedStyle(e).fontSize) < 12);
			return { side: document.documentElement.scrollWidth - W, past: past.map(desc).slice(0, 4), inner: inner.map(desc).slice(0, 4), taps: taps.length, small: small.map(desc).slice(0, 4), tinyText: tinyText.map(desc).slice(0, 4) };
		});
	for (const [route, name] of ROUTES) {
		await phone.goto(base + route);
		await settled(phone);
		const L = await layout(phone);
		expect(L.side === 0, `phone: no sideways scroll on /${route}`);
		expect(L.past.length === 0, `phone: nothing past the right edge on /${route}${L.past.length ? ': ' + L.past.join('; ') : ''}`);
		expect(L.inner.length === 0, `phone: no box scrolls sideways inside /${route}${L.inner.length ? ': ' + L.inner.join('; ') : ''}`);
		expect(L.taps > 0 && L.small.length === 0, `phone: all ${L.taps} primary controls and standalone links on /${route} are at least 44 x 44 px${L.small.length ? ': ' + L.small.join('; ') : ''}`);
		expect(L.tinyText.length === 0, `phone: no text under 12 px on /${route}${L.tinyText.length ? ': ' + L.tinyText.join('; ') : ''}`);
		// Skip link: the first Tab stop, shown on focus; it moves focus to the page content.
		await phone.evaluate(() => document.activeElement?.blur());
		await phone.keyboard.press('Tab');
		const skipOk = await phone.evaluate(() => {
			const a = document.activeElement;
			const r = a?.getBoundingClientRect();
			return a?.dataset.testid === 'skip-link' && /skip to content/i.test(a.textContent) && r.top >= 0 && r.height >= 44;
		});
		await phone.keyboard.press('Enter');
		const toMain = await phone.evaluate(() => document.activeElement?.id === 'main');
		expect(skipOk && toMain, `phone: /${route} starts with a visible "Skip to content" link that moves focus to the content`);
		// The menu: one 44 px button; it lists every page as a 44 px row and opens the next one.
		const menu = phone.getByTestId('menu-button');
		const nav = phone.getByTestId('site-nav');
		const mb = await menu.boundingBox();
		const closed = !(await nav.isVisible()) && (await menu.getAttribute('aria-expanded')) === 'false';
		await menu.click();
		const links = nav.getByRole('link');
		const rows = await links.evaluateAll((as) => as.map((a) => ({ t: a.textContent.trim(), h: a.getBoundingClientRect().height, w: a.getBoundingClientRect().width, cur: a.getAttribute('aria-current') })));
		expect(
			closed && mb && mb.width >= 44 && mb.height >= 44 && (await menu.getAttribute('aria-expanded')) === 'true' && JSON.stringify(rows.map((r) => r.t)) === JSON.stringify(PRIMARY.map((r) => r[1])) && rows.every((r) => r.h >= 44 && r.w >= 44) && rows.find((r) => r.cur === 'page')?.t === PRIMARY.find((r) => r[0] === route)?.[1],
			`phone: on /${route} the menu button opens the four primary pages as 44 px rows with correct current state`
		);
		const [nextRoute, nextName] = PRIMARY[(PRIMARY.findIndex((r) => r[0] === route) + 1) % PRIMARY.length];
		await links.filter({ hasText: new RegExp(`^${nextName}$`) }).click();
		await phone.waitForFunction((r) => location.hash === r || (r === '' && (location.hash === '' || location.hash === '#/')), nextRoute, { timeout: WAIT_CAP }).catch(() => {});
		const at = await phone.evaluate(() => location.hash);
		expect((at === nextRoute || (nextRoute === '' && (at === '' || at === '#/'))) && !(await nav.isVisible()) && (await menu.getAttribute('aria-expanded')) === 'false', `phone: the menu on /${route} opens ${nextName} and closes`);
	}
	{
		// Esc closes the menu and gives focus back to its button.
		await phone.goto(base + '#/api');
		await phone.getByTestId('menu-button').click();
		await phone.keyboard.press('Escape');
		const escOk = !(await phone.getByTestId('site-nav').isVisible()) && (await phone.evaluate(() => document.activeElement?.dataset.testid === 'menu-button'));
		expect(escOk, 'phone: Esc closes the menu and returns focus to the menu button');
		// A wide screen keeps the inline nav and no menu button.
		const wide = await browser.newPage({ viewport: { width: 1280, height: 800 } });
		await wide.goto(base + '#/api');
		await wide.getByTestId('site-nav').getByRole('link').nth(PRIMARY.length - 1).waitFor({ timeout: WAIT_CAP }).catch(() => {});
		expect((await wide.getByTestId('site-nav').getByRole('link').count()) === PRIMARY.length && (await wide.getByTestId('site-nav').isVisible()) && !(await wide.getByTestId('menu-button').isVisible()), 'desktop: inline nav with four primary destinations, no menu button');
		await wide.close();
	}
	{
		// Run page on a phone: each step's transactions sit behind one "Show N transactions"
		// control (steps with more than 2), so the closed page has few Tab stops; opened, every
		// transaction is a 44 px row linking its basescan page.
		await phone.goto(base + '#/run');
		const tn = phone.getByTestId('testnet-run');
		await tn.getByTestId('net-step').first().waitFor({ timeout: WAIT_CAP });
		const steps = netRuns[0].steps;
		const big = steps.filter((s) => s.txs.length > 2);
		const sets = tn.getByTestId('net-txset');
		const summaries = await sets.locator('summary').allInnerTexts();
		expect((await sets.count()) === big.length && big.every((s, i) => summaries[i]?.includes(`Show ${s.txs.length} transactions`)), `phone run: ${big.length} steps fold their transactions behind "Show N transactions"`);
		const focusable = await tn.evaluate((el) => [...el.querySelectorAll('a[href], button, select, summary')].filter((e) => e.checkVisibility() && e.getBoundingClientRect().height > 0).length);
		const shownTx = steps.filter((s) => s.txs.length <= 2).reduce((a, s) => a + s.txs.length, 0);
		expect(focusable <= steps.length + big.length + shownTx + netRuns[0].contracts.length + 4, `phone run: the latest run has ${focusable} Tab stops with the lists closed (was one per transaction)`);
		const first = sets.first();
		await first.locator('summary').click();
		const rowsOpen = await first.getByTestId('net-tx').evaluateAll((as) => as.map((a) => [a.getAttribute('href'), a.getBoundingClientRect().height, a.getBoundingClientRect().width]));
		expect(
			rowsOpen.length === big[0].txs.length && rowsOpen.every(([h, ht, w], k) => h === big[0].txs[k].url && ht >= 44 && w >= 44) && (await first.locator('summary').innerText()).includes(`Hide ${big[0].txs.length} transactions`),
			`phone run: "Show ${big[0].txs.length} transactions" opens ${big[0].txs.length} rows of at least 44 px, each its recorded basescan link`
		);
		expect((await phone.evaluate(() => document.documentElement.scrollWidth - innerWidth)) === 0, 'phone run: no sideways scroll with a transaction list open');
		// Visible text names the transaction (no label-content mismatch).
		const nameOk = await first.getByTestId('net-tx').first().evaluate((a) => !a.hasAttribute('aria-label') && new RegExp(`^\\S.* ${a.title.slice(0, 8)}…${a.title.slice(-4)}↗$`).test(a.textContent.trim()));
		expect(nameOk, 'phone run: a transaction row is named by its visible function and hash');
	}
	// The open label table scrolls inside its panel, not the page.
	if (!PUB) {
		await phone.goto(base + '#/labels');
		await phone.getByText(`Browse all ${n(nLabels)} labels`).click({ timeout: 120000 });
		await quietDom(phone);
		expect((await phone.evaluate(() => document.documentElement.scrollWidth - innerWidth)) === 0, 'phone: open label table has no sideways scroll');
	}
	// W3: accessibility, WCAG 2.2 AA. axe-core on every page at 1280 and 390 px, as loaded and
	// with every fold open, plus the open menu and command palette: no serious or critical
	// violation. Then what axe cannot see: headings and landmarks, a focus ring on every Tab
	// stop with no trap, charts reachable by keyboard with a data table of the same numbers,
	// the palette's contrast, and motion that stops.
	{
		const axeSrc = await readFile(createRequire(import.meta.url).resolve('axe-core/axe.min.js'), 'utf8');
		const AXE_TAGS = ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa', 'best-practice'];
		// `only`: a CSS selector to limit the run to (axe then skips its page-level rules).
		const axeRun = async (pg, only) => {
			if (!(await pg.evaluate(() => 'axe' in window))) await pg.addScriptTag({ content: axeSrc });
			const r = await pg.evaluate(([tags, only]) => window.axe.run(only ? { include: [only] } : document, { runOnly: { type: 'tag', values: tags }, resultTypes: ['violations'] }), [AXE_TAGS, only]);
			return r.violations.filter((v) => v.impact === 'serious' || v.impact === 'critical');
		};
		const fmtV = (vs) => vs.map((v) => `${v.id} (${v.impact}, ${v.nodes.length}: ${v.nodes[0]?.target.join(' ')})`).join('; ');
		for (const [view, viewport] of [['desktop', { width: 1280, height: 800 }], ['phone', { width: 390, height: 844 }]]) {
			const ctx = await browser.newContext({ viewport });
			const pg = await ctx.newPage();
			pg.on('pageerror', (e) => errors.push(String(e)));
			for (const [route] of ROUTES) {
				await pg.goto(base + route);
				await settled(pg);
				const loaded = await axeRun(pg);
				// W12 round 13: sections below the first screen wait at opacity 0 until they scroll into view
				// (axe skips them as hidden), so the second pass reads the page after a reader's scroll through
				// it, every section faded in (no section left waiting or part-way through its fade). Round 14 review:
				// the steps are not paced on frames, so a loaded machine may draw none while a section is in view
				// (the fade must still show one scrolled past, w12); the scroll ends at the bottom, drawn, as a
				// reader's does, before going back up.
				await pg.evaluate(async () => {
					for (let y = 0; y < document.body.scrollHeight; y += 400) (scrollTo(0, y), await new Promise((r) => setTimeout(r, 60)));
					scrollTo(0, document.body.scrollHeight);
					await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
					scrollTo(0, 0);
				});
				await settled(pg);
				await pg.evaluate(() => Promise.all(document.getAnimations().filter((a) => a instanceof CSSTransition).map((a) => a.finished.catch(() => {}))));
				const dimmed = await pg.evaluate(() => [...document.querySelectorAll('main section.section')].filter((s) => s.dataset.fade === 'wait' || getComputedStyle(s).opacity !== '1').map((s) => s.dataset.testid ?? s.querySelector('h2')?.textContent?.trim().slice(0, 30) ?? '?'));
				const dim = dimmed.length;
				// Review of round 13: the sections axe skipped as loaded (they waited unshown) are read once with
				// their folds still closed too, so a closed fold's own text (Run's "show N transactions") below
				// the first screen is still measured, as before the fade.
				const FADED = 'main section.section[data-fade="in"]';
				const faded = (await pg.locator(FADED).count()) ? await axeRun(pg, FADED) : [];
				await pg.evaluate(() => document.querySelectorAll('details').forEach((d) => (d.open = true)));
				await quietDom(pg);
				const open = await axeRun(pg);
				const all = [...loaded, ...faded, ...open];
				expect(dim === 0, `a11y ${view}: on /${route} every section is fully shown after a scroll through it, before axe's second pass (${dim} dimmed${dim ? ': ' + dimmed.join(', ') : ''})`);
				expect(all.length === 0, `a11y ${view}: axe finds no serious or critical WCAG 2.2 AA issue on /${route}, as loaded, below the first screen once shown with its folds closed, and with every fold open${all.length ? ': ' + fmtV(all) : ''}`);
			}
			await pg.goto(base + '#/api');
			await settled(pg);
			if (view === 'phone') {
				await pg.getByTestId('menu-button').click();
				const v = await axeRun(pg);
				expect(v.length === 0, `a11y phone: no serious or critical axe issue with the menu open${v.length ? ': ' + fmtV(v) : ''}`);
				await pg.keyboard.press('Escape');
			} else {
				await pg.keyboard.press('Control+k');
				await pg.locator('dialog[open]').waitFor();
				const v = await axeRun(pg);
				expect(v.length === 0, `a11y desktop: no serious or critical axe issue with the command palette open${v.length ? ': ' + fmtV(v) : ''}`);
				await pg.keyboard.press('Escape');
			}
			await ctx.close();
		}

		const desk = await browser.newPage({ viewport: { width: 1280, height: 800 } });
		for (const [route, name] of ROUTES) {
			await desk.goto(base + route);
			await settled(desk);
			// Language, landmarks, one h1 first and no skipped heading level, named graphics and controls.
			const S = await desk.evaluate(() => {
				const hs = [...document.querySelectorAll('h1,h2,h3,h4,h5,h6')].filter((e) => e.checkVisibility()).map((e) => +e.tagName[1]);
				const named = (e) => (e.textContent ?? '').trim() || e.getAttribute('aria-label') || e.getAttribute('aria-labelledby');
				return {
					lang: document.documentElement.lang,
					h: hs,
					marks: ['header.top', 'nav', 'main#main', 'footer'].map((q) => document.querySelectorAll(q).length),
					svg: [...document.querySelectorAll('svg')].filter((v) => !v.closest('[aria-hidden="true"]') && !(v.getAttribute('role') === 'img' && v.getAttribute('aria-label'))).length,
					unlabelled: [...document.querySelectorAll('input, select, textarea')].filter((e) => !(e.labels?.length || e.getAttribute('aria-label') || e.getAttribute('aria-labelledby'))).length,
					nameless: [...document.querySelectorAll('button, a[href]')].filter((e) => e.checkVisibility() && !named(e)).length
				};
			});
			expect(S.lang === 'en', `a11y: /${route} declares its language (en)`);
			expect(JSON.stringify(S.marks) === '[1,1,1,1]', `a11y: /${route} has one header, nav, main and footer landmark (${S.marks})`);
			expect(S.h[0] === 1 && S.h.filter((x) => x === 1).length === 1 && S.h.every((x, i) => i === 0 || x <= S.h[i - 1] + 1), `a11y: /${route} has one h1, first, and no skipped heading level (${S.h.join('')})`);
			expect(S.svg === 0 && S.unlabelled === 0 && S.nameless === 0, `a11y: /${route} has no unnamed drawing, unlabelled field or nameless control (${S.svg}/${S.unlabelled}/${S.nameless})`);
			// Tab through the page: every stop shows a 2 px ring and is on screen; focus leaves at the end.
			await desk.evaluate(() => {
				document.activeElement?.blur();
				scrollTo(0, 0);
			});
			const seen = new Set();
			let stops = 0;
			let ringless = [];
			let left = false;
			for (let k = 0; k < 250; k++) {
				await desk.keyboard.press('Tab');
				const f = await desk.evaluate(() => {
					const e = document.activeElement;
					if (!e || e === document.body) return null;
					const cs = getComputedStyle(e);
					const r = e.getBoundingClientRect();
					const ring = (cs.outlineStyle !== 'none' && parseFloat(cs.outlineWidth) >= 2) || cs.boxShadow !== 'none';
					const key = e.dataset.k ?? (e.dataset.k = String(Math.random()));
					return { key, ring, onScreen: r.width > 0 && r.height > 0 && r.bottom > 0 && r.top < innerHeight, what: `${e.tagName.toLowerCase()} "${(e.getAttribute('aria-label') || e.textContent || '').trim().slice(0, 30)}"` };
				});
				if (!f || seen.has(f.key)) {
					left = true;
					break;
				}
				seen.add(f.key);
				stops++;
				if (!f.ring || !f.onScreen) ringless.push(f.what);
			}
			expect(left && stops > 5 && ringless.length === 0, `a11y keyboard: all ${stops} Tab stops on /${route} show a focus ring on screen, and focus leaves the page at the end${ringless.length ? ': ' + ringless.slice(0, 4).join('; ') : ''}${left ? '' : ' (no exit after 250 stops)'}`);
			void name;
		}

		// Charts: one Tab stop each; the arrow keys move the tooltip; a data table holds the numbers.
		await desk.goto(base + '#/data');
		await settled(desk);
		const blockChart = desk.locator('.chart[tabindex="0"]').first();
		const lb = demo0.live.blocks;
		// E12: each key's tooltip is read once it shows that key's block (or is gone), never at once.
		const tipShows = (t) => desk.waitForFunction((t) => document.querySelector('.chart[tabindex="0"] .tip')?.textContent.includes(t), t, { timeout: WAIT_CAP }).catch(() => {});
		await blockChart.focus();
		await desk.keyboard.press('ArrowRight');
		await tipShows(`#${n(lb[0].height)}`);
		const tip1 = await blockChart.locator('.tip').innerText().catch(() => '');
		await desk.keyboard.press('End');
		await tipShows(`#${n(lb[lb.length - 1].height)}`);
		const tipN = await blockChart.locator('.tip').innerText().catch(() => '');
		await desk.keyboard.press('Escape');
		await blockChart.locator('.tip').waitFor({ state: 'detached', timeout: WAIT_CAP }).catch(() => {});
		const tipGone = (await blockChart.locator('.tip').count()) === 0;
		expect(tip1.includes(`#${n(lb[0].height)}`) && tip1.includes(`${n(lb[0].txs)}`) && tipN.includes(`#${n(lb[lb.length - 1].height)}`) && tipGone, 'a11y charts: arrow keys, End and Esc move and hide the block chart tooltip');
		const charts = await desk.evaluate(() => [...document.querySelectorAll('.chart')].map((c) => ({ compact: c.classList.contains('compact'), tab: c.getAttribute('tabindex'), label: c.getAttribute('aria-label') ?? '', table: c.nextElementSibling?.matches('details.datatable') ?? false })));
		expect(charts.length >= (FB ? 1 : 3) && charts.every((c) => c.compact || (c.tab === '0' && c.label.length > 20 && c.table)), `a11y charts: all ${charts.length} charts on /data are named, take focus and have a data table`);
		const bt = desk.getByTestId('blocks-table').first();
		await bt.locator('summary').click();
		// Review of W12 round 12: the long cap of tests/wait.mjs, not the 30 s default (at load 58 the pool
		// table's first row took longer to draw and the gate failed with nothing wrong on the page).
		await bt.locator('tbody tr').first().waitFor({ timeout: 240000 });
		const btRows = await bt.locator('tbody tr').evaluateAll((trs) => trs.map((tr) => [...tr.children].map((c) => c.textContent.trim())));
		expect(btRows.length === liveBlocks && btRows.every((r, i) => r[0] === n(lb[i].height) && r[2] === n(lb[i].txs)), `a11y charts: the block chart's table lists all ${liveBlocks} blocks with the export's heights and transaction counts`);
		{
			const poolsDoc = JSON.parse(await readFile(join(root, 'history/pools-week.json'), 'utf8'));
			const pt = desk.getByTestId('pool-table');
			await pt.locator('summary').click();
			await pt.locator('tbody tr').first().waitFor({ timeout: 240000 });
			const totals = await pt.locator('tbody tr').evaluateAll((trs) => trs.map((tr) => Number(tr.lastElementChild.textContent.trim())));
			const total = Object.values(poolsDoc.totals).reduce((a, b) => a + b, 0);
			expect(totals.length === poolsDoc.days.length && totals.reduce((a, b) => a + b, 0) === total, `a11y charts: the pool table has ${poolsDoc.days.length} days adding up to the export's ${total} blocks`);
		}
		if (!FB) {
			const rdoc = JSON.parse(await readFile(join(root, 'history/reserves-week.json'), 'utf8'));
			const weekShards = rdoc.shards.filter((s) => s.week);
			await desk.goto(base + '#/reserves');
			await desk.getByTestId('reserve-cards').waitFor({ timeout: 120000 });
			let ok = true;
			// W12 round 5: one "Data tables" switch opens every card's table (no fold per card).
			const rsw = desk.getByTestId('reserve-tables');
			await rsw.click();
			await attr(rsw, 'aria-expanded', 'true');
			for (const r of weekShards[weekShards.length - 1].rows) {
				const card = desk.getByTestId(`reserve-${r.entity.replace(/^entity:/, '')}`);
				await card.getByTestId('reserve-table').locator('tbody tr').last().waitFor({ timeout: WAIT_CAP });
				const last = await card.getByTestId('reserve-table').locator('tbody tr').last().locator('td').innerText();
				ok &&= last === btcFmt(r.balance_sats);
			}
			expect(ok, 'a11y charts: each reserve table ends on the export\'s week-end balance');
		}

		// Contrast (WCAG 1.4.3 and 1.4.11), read from the live stylesheet: text >= 4.5:1 and chart
		// fills >= 3:1 on the page and the darkest panel. W1 found --pool-other at 2.78:1.
		const C = await desk.evaluate(() => {
			const css = getComputedStyle(document.documentElement);
			const hex = (v) => {
				const h = css.getPropertyValue(v).trim();
				return [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16));
			};
			const lum = (rgb) => {
				const [r, g, b] = rgb.map((x) => (x /= 255) <= 0.03928 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4);
				return 0.2126 * r + 0.7152 * g + 0.0722 * b;
			};
			const ratio = (a, b) => {
				const [hi, lo] = [lum(hex(a)), lum(hex(b))].sort((x, y) => y - x);
				return (hi + 0.05) / (lo + 0.05);
			};
			const out = [];
			for (const bg of ['--bg', '--panel-solid']) {
				for (const t of ['--ink', '--muted', '--accent', '--accent-2', '--btc', '--good', '--bad', '--line-net', '--slab-muted']) out.push([t, bg, ratio(t, bg), 4.5]);
				for (const f of ['--pool-1', '--pool-2', '--pool-3', '--pool-4', '--pool-5', '--pool-6', '--pool-other']) out.push([f, bg, ratio(f, bg), 3]);
			}
			return out;
		});
		const low = C.filter(([, , r, need]) => r < need);
		const other = C.find(([f, bg]) => f === '--pool-other' && bg === '--panel-solid');
		expect(low.length === 0, `a11y contrast: ${C.length} palette pairs pass AA (text 4.5:1, chart fills 3:1); --pool-other on --panel-solid ${other[2].toFixed(2)}:1${low.length ? ': ' + low.map(([a, b, r]) => `${a} on ${b} ${r.toFixed(2)}`).join('; ') : ''}`);

		// PP6: one chart style on Data, Reserves and Run: one tooltip, one axis font, one set of tick
		// formats, colours from the app.css tokens only (each mark >= 3:1 on the panel), one legend
		// swatch set, a folded data table under every chart, and named loading states.
		{
			const lo = demo0.live.from_height - 5000;
			const hi = demo0.live.to_height + 5000;
			const TICK = [/^−?(0|\d{1,3}(,\d{3})*|\d{1,2}\.\d|0\.\d\d)$/, /^#\d{1,3}(,\d{3})*$/, /^\d{2}:\d{2}$/, /^\d{1,2} [A-Z][a-z]{2}$/, /^(In ↑|Out ↓)$/, /^−?[\d,]+\.\d{2} BTC$/, /^$/];
			const style = async (pg) =>
				pg.evaluate(([lo, hi, TICKS]) => {
					const re = TICKS.map((t) => new RegExp(t));
					const probe = document.createElement('i');
					document.body.append(probe);
					const tok = (v) => {
						probe.style.color = `var(${v})`;
						return getComputedStyle(probe).color;
					};
					const marks = Object.fromEntries(['--accent', '--btc', '--line-net', '--pool-1', '--pool-2', '--pool-3', '--pool-4', '--pool-5', '--pool-6', '--pool-other', '--good', '--bad'].map((v) => [tok(v), v]));
					const frame = new Set([tok('--panel-solid'), tok('--line-strong'), tok('--muted')]);
					const panel = tok('--panel-solid');
					probe.remove();
					const rgb = (c) => c.match(/\d+(\.\d+)?/g).slice(0, 3).map(Number);
					const lum = (c) => {
						const [r, g, b] = rgb(c).map((x) => ((x /= 255) <= 0.03928 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4));
						return 0.2126 * r + 0.7152 * g + 0.0722 * b;
					};
					const ratio = (a, b) => {
						const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p);
						return (x + 0.05) / (y + 0.05);
					};
					const cs = (e) => getComputedStyle(e);
					const axis = [...document.querySelectorAll('main .chart svg text.axis')];
					const used = [];
					for (const e of document.querySelectorAll('main .chart svg path, main .chart svg rect, main .chart svg circle')) {
						if ((e.tagName === 'path' && !e.getAttribute('d')) || e.classList.contains('cut')) continue;
						const s = cs(e);
						if (s.fill !== 'none') used.push(s.fill);
						if (s.stroke !== 'none' && !e.classList.contains('dot')) used.push(s.stroke);
					}
					for (const e of document.querySelectorAll('main .chart .seg:not(.hatch), main .track i, main .split .bar i')) used.push(cs(e).backgroundColor);
					const foreign = [...new Set(used)].filter((c) => !(c in marks) && !frame.has(c));
					const weak = [...new Set(used)].filter((c) => c in marks && ratio(c, panel) < 3).map((c) => marks[c]);
					const charts = [...document.querySelectorAll('main .chart')].filter((c) => !c.classList.contains('compact'));
					return {
						fonts: [...new Set(axis.map((t) => `${cs(t).fontFamily.split(',')[0]} ${cs(t).fontSize}`))],
						badTicks: axis.map((t) => t.textContent.trim()).filter((x) => !re.some((r) => r.test(x))),
						bareHeights: axis.map((t) => t.textContent.trim()).filter((x) => /^\d{3},\d{3}$/.test(x) && +x.replace(',', '') >= lo && +x.replace(',', '') <= hi),
						foreign,
						weak,
						swatches: [...new Set([...document.querySelectorAll('main .key')].map((e) => `${cs(e).width}x${cs(e).height}`))],
						legends: document.querySelectorAll('main .chart-legend, main .poollegend, main .split .shares').length,
						charts: charts.length,
						tabled: charts.filter((c) => c.nextElementSibling?.matches('details.datatable')).length
					};
				}, [lo, hi, TICK.map((r) => r.source)]);
			const tipStyles = async (pg) => {
				const out = new Set();
				let shown = 0;
				const charts = await pg.locator('main .chart[tabindex="0"]').all();
				for (const c of charts) {
					await c.focus();
					await pg.keyboard.press('ArrowRight');
					const t = await c.locator('.tip').evaluate((el) => {
						const s = getComputedStyle(el);
						return [s.width, s.borderRadius, s.padding, s.fontSize, s.boxShadow !== 'none', s.backgroundColor, s.borderTopColor].join(' ');
					}).catch(() => null);
					if (t) (out.add(t), shown++);
					await pg.keyboard.press('Escape');
				}
				return { n: charts.length, shown, styles: out.size };
			};
			const READY = { '#/data': 'pool-shares', '#/reserves': 'reserve-cards', '#/run': 'split' };
			// A fallback build's Reserves page has no chart (ROADMAP L14).
			const CHART_ROUTES = FB ? ['#/data', '#/run'] : ['#/data', '#/reserves', '#/run'];
			for (const route of CHART_ROUTES) {
				await desk.goto(base + route);
				await desk.getByTestId(READY[route]).waitFor({ timeout: 120000 }).catch(() => {});
				await settled(desk);
				const S = await style(desk);
				const T = await tipStyles(desk);
				const where = `PP6 ${route}`;
				if (route !== '#/run') {
					expect(T.n >= (FB ? 2 : 4) && T.shown === T.n && T.styles === 1, `${where}: all ${T.n} charts show one tooltip style on the arrow keys (${T.shown} shown, ${T.styles} styles)`);
					expect(S.fonts.length === 1 && S.fonts[0] === '"IBM Plex Mono" 12px', `${where}: every axis label is IBM Plex Mono 12 px (${S.fonts.join('; ')})`);
					expect(S.badTicks.length === 0, `${where}: every tick uses the shared formats (BTC/count, #height, time, day)${S.badTicks.length ? ': ' + S.badTicks.slice(0, 5).join(', ') : ''}`);
					expect(S.bareHeights.length === 0, `${where}: every block height on an axis reads "#971,234"${S.bareHeights.length ? ': ' + S.bareHeights.slice(0, 3).join(', ') : ''}`);
					expect(S.charts === T.n && S.tabled === S.charts, `${where}: all ${S.charts} charts have a folded data table right under them`);
				}
				expect(S.foreign.length === 0, `${where}: every chart mark is drawn in an app.css token colour${S.foreign.length ? ': ' + S.foreign.join(', ') : ''}`);
				expect(S.weak.length === 0, `${where}: every chart colour is >= 3:1 on the panel${S.weak.length ? ': ' + S.weak.join(', ') : ''}`);
				expect(S.legends >= 1 && S.swatches.every((x) => x === '10pxx10px' || x === '14pxx2px'), `${where}: ${S.legends} legend(s) with one swatch set, a 10 px square or a 14 px line (${S.swatches.join(', ')})`);
			}
			// Run: the to-scale step bars fold their numbers into a table too; the fee-split bar and its legend share colours.
			const runJ = JSON.parse(await readFile(join(root, 'run/live-run.json'), 'utf8'));
			const hWant = runJ.steps.filter((s) => !s.step.startsWith('r2.'));
			const ht = desk.getByTestId('honest-table');
			await ht.locator('summary').click();
			// E12: every row, not the first one within 10 s (the table is built when it opens).
			await desk.waitForFunction((k) => document.querySelectorAll('[data-testid="honest-table"] tbody tr').length >= k, hWant.length, { timeout: WAIT_CAP }).catch(() => {});
			const hRows = await ht.locator('tbody tr').evaluateAll((trs) => trs.map((tr) => [...tr.children].map((c) => c.textContent.trim())));
			expect(hRows.length === hWant.length && hRows.every((r, i) => r[2] === n(hWant[i].block) && r[3] === n(hWant[i].txs)), `PP6 run: the honest round's table lists its ${hWant.length} steps with the recorded end blocks and transactions`);
			expect((await desk.getByTestId('dispute-table').count()) === (runJ.dispute ? 1 : 0), 'W6 run: the dispute bars have their table');
			const split = await desk.getByTestId('split').evaluate((el) => [[...el.querySelectorAll('.bar i')].map((e) => getComputedStyle(e).backgroundColor), [...el.querySelectorAll('.shares .key')].map((e) => getComputedStyle(e).backgroundColor)]);
			expect(split[0].length === 4 && JSON.stringify(split[0]) === JSON.stringify(split[1]) && new Set(split[0]).size === 4, `PP6 run: the fee-split bar and its legend use the same 4 category colours (${split[0].join(', ')})`);
			// Pools: the stacked bars are a keyboard chart like the others; the tooltip names the day and its blocks.
			await desk.goto(base + '#/data');
			await desk.getByTestId('pool-shares').waitFor({ timeout: 120000 }).catch(() => {});
			await settled(desk);
			const poolsJ = JSON.parse(await readFile(join(root, 'history/pools-week.json'), 'utf8'));
			const pc = desk.getByTestId('pool-shares').locator('xpath=ancestor::div[contains(@class,"chart")][1]');
			await pc.focus();
			await desk.keyboard.press('ArrowRight');
			const ptip = await pc.locator('.tip').innerText().catch(() => '');
			const d0 = poolsJ.days[0];
			expect(ptip.includes(`${d0.date} UTC`) && ptip.includes(`${d0.to - d0.from + 1} blocks`), `PP6 pools: arrow keys open the day tooltip (${d0.date}, ${d0.to - d0.from + 1} blocks)`);
			// Loading: while the data is on its way, every placeholder says what it waits for.
			// E10: the data is held back until the placeholders are read (it was a 5 s delay with a
			// 4 s wait for the first placeholder, which a loaded machine could miss either way).
			const slow = await browser.newContext({ viewport: { width: 1280, height: 800 } });
			let release;
			const held = new Promise((r) => (release = r));
			await slow.route('**/*.json', async (route) => {
				await held;
				await route.continue().catch(() => {});
			});
			for (const route of CHART_ROUTES) {
				const sp = await slow.newPage();
				await sp.goto(base + route);
				await sp.locator('.sk').first().waitFor({ timeout: WAIT_CAP }).catch(() => {});
				const labels = await sp.locator('.sk').evaluateAll((els) => els.map((e) => e.getAttribute('aria-label')));
				expect(labels.length > 0 && labels.every((l) => l && l !== 'Loading the data'), `PP6 ${route}: while loading, every placeholder is named (${labels.join('; ')})`);
				await sp.close();
			}
			release();
			await slow.close();
		}

		// Motion: reduced motion leaves nothing running; the header button pauses it (remembered).
		const running = (pg) => pg.evaluate(() => document.getAnimations().filter((a) => a.playState === 'running').length);
		const still = await browser.newContext({ viewport: { width: 1280, height: 800 }, reducedMotion: 'reduce' });
		const sp = await still.newPage();
		let quiet = true;
		for (const route of ['', '#/explorer', '#/data']) {
			await sp.goto(base + route);
			await settled(sp);
			quiet &&= (await running(sp)) === 0;
		}
		expect(quiet && (await sp.getByTestId('motion-button').count()) === 0, 'a11y motion: with reduced motion asked for, nothing animates (home, explorer, data) and no pause button is needed');
		await still.close();
		await desk.goto(base + '#/explorer');
		await settled(desk);
		const before = await running(desk);
		const mbtn = desk.getByTestId('motion-button');
		await mbtn.click();
		// The state the click sets, waited for (it was a fixed 200 ms).
		const paused = await desk
			.waitForFunction(() => document.querySelector('[data-testid="motion-button"]')?.getAttribute('aria-pressed') === 'true' && document.documentElement.dataset.motion === 'off' && document.getAnimations().filter((a) => a.playState === 'running').length === 0, null, { timeout: WAIT_CAP })
			.then(() => true, () => false);
		await desk.reload();
		await settled(desk);
		const kept = (await running(desk)) === 0 && (await desk.getByTestId('motion-button').getAttribute('aria-pressed')) === 'true';
		await desk.getByTestId('motion-button').click();
		const back = await desk
			.waitForFunction(() => document.documentElement.dataset.motion === 'on' && document.getAnimations().filter((a) => a.playState === 'running').length > 0, null, { timeout: WAIT_CAP })
			.then(() => true, () => false);
		expect(before > 0 && paused && kept && back, `a11y motion: the pause button stops all ${before} running animations on /#/explorer, is remembered after a reload, and plays them again`);
		await desk.close();
	}
	await product(browser, base, root, expect);
	await markets(browser, base, expect);
	await daily(browser, base, root, expect);
	// NORTH-STAR NS5 (ADR-0048): level badges and the level history.
	await ns5(browser, base, root, expect);
	await ns11(browser, base, root, expect);
	await ns14(browser, base, root, expect);
	await ns15(browser, base, root, expect);
	await downloads(browser, base, root, expect);
	await ns16(browser, base, root, expect);
	await t10(browser, base, expect);
	// Pre-public loop PP1: a fed receipt's observer evidence (the live view is in submission-live.mjs).
	try { expect(true, pp1Parser()); } catch (e) { expect(false, `PP1 receipt parser: ${e.message}`); }
	// Pre-public loop PP6: the API page's Reference is the OpenAPI file, every route.
	await pp6(browser, base, expect);
	// Pre-public loop PP3: a salted label row checked in the browser (ROADMAP D30).
	await pp3(browser, base, expect);
	// Pre-public loop PP4: Browse data opens with the datasets (U5).
	await pp4(browser, base, expect, FB);
	// W10 visual polish (tokens only, hover and press states, motion, placeholders) and the W11
	// follow-ups (tooltips inside their charts, the reserve down week, legend words, tag class).
	await w10(browser, base, root, expect);
	// W4 speed: heavy files only where and when needed, one cached verifier, the slab in a worker.
	await w4(browser, base, root, expect);
	// W12 continuous styling: section openers, page kickers, sentences in the body face, one
	// figure tile, the footer's page list and state, the Reserves table inside its panel.
	await w12(browser, base, root, expect);
	// ROADMAP C6: level-5 track records (no real one yet; the example, checked and labelled).
	await c6(browser, base, root, expect);
	// ROADMAP C61: the level-3 card (no live book; the recorded rehearsal, checked and labelled).
	await c61(browser, base, root, expect);
	// ROADMAP S1: the Add data page (shared intake vectors, the Treasury and FRED walk-throughs, offline download).
	await s1(browser, base, root, expect);
	expect(errors.length === 0, `no console errors${errors.length ? ': ' + errors.join('; ') : ''}`);
} finally {
	await browser.close();
	server.close();
}
if (failures.length) {
	console.log(`${failures.length} failed`);
	process.exit(1);
}
console.log('E2E PASSED');
