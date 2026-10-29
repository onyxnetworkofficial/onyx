// e2e "NS3 daily": the earthquake and weather rounds on the data page (ADR-0027 amendment 17).
// Each card shows only what the browser's own check returned; free public-source values ship
// unsealed in every build (THREATS NS5-8); a forged summary shows no value. Also runs alone:
// node tests/daily.mjs [build dir].
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

const CARDS = [
	{ id: 'usgs', file: 'level2/usgs-quakes.json', shown: (v) => `${v} earthquakes` },
	{ id: 'ncei', file: 'level2/ncei-tmax.json', shown: (v) => `${v} °C` }
];

export async function daily(browser, base, root, expect) {
	const page = await browser.newPage();
	await page.goto(base + '#/data');
	const sec = page.getByTestId('daily');
	expect(
		await sec.getByText('Checks out', { exact: true }).waitFor({ timeout: 120000 }).then(() => true, () => false),
		'NS3 daily: both recorded rounds check out in the browser'
	);
	for (const c of CARDS) {
		const j = JSON.parse(await readFile(join(root, c.file), 'utf8'));
		const card = page.getByTestId(`daily-${c.id}`);
		expect(await card.isVisible(), `NS3 daily: the ${c.id} card shows`);
		expect(!j.sealed, `NS3 daily: the ${c.id} value ships unsealed (a free public source, THREATS NS5-8)`);
		if (j.sealed) {
			expect((await page.getByTestId(`daily-${c.id}-sealed`).innerText()) === 'Available via the paid API', `NS3 daily: a public build seals the ${c.id} value`);
			expect(!(await card.innerText()).includes(j.response_body ?? '\u0000'), `NS3 daily: the sealed ${c.id} card shows no value`);
		} else {
			expect((await page.getByTestId(`daily-${c.id}-value`).innerText()) === c.shown(j.value), `NS3 daily: the ${c.id} card shows the checked value (${j.value})`);
			expect((await page.getByTestId(`daily-${c.id}-observers`).innerText()) === '5 of 5 agreed', `NS3 daily: the ${c.id} card shows 5 of 5 observers`);
		}
		const link = await page.getByTestId(`daily-${c.id}-source`).getAttribute('href');
		expect(link === j.source.terms_url && link.startsWith('https://'), `NS3 daily: the ${c.id} source links its terms page`);
	}
	// ROADMAP B6: the Base Sepolia rounds are linked: the registry's events and the API's answers.
	const tn = page.getByTestId('daily-testnet');
	expect(await tn.isVisible(), 'B6 daily: the Base Sepolia rounds are named');
	expect(
		(await page.getByTestId('daily-testnet-registry').getAttribute('href')) === 'https://sepolia.basescan.org/address/0x497389e04f7d58e07a79ac8d395c3927aecbdf2b#events',
		'B6 daily: the registry links its events on Basescan'
	);
	for (const [id, metric, subject] of [
		['usgs', 'usgs.earthquakes.m5_count', 'usgs.earthquakes.m5_world:2026-07-19'],
		['ncei', 'noaa.ghcnd.usw00003017.tmax', 'noaa.ghcnd.usw00003017:2026-09-27']
	]) {
		const href = await page.getByTestId(`daily-testnet-${id}`).getAttribute('href');
		expect(href === `https://onyx.cryptagraph.com/v1/observations/${metric}?subject=${subject}`, `B6 daily: the ${id} round links its API answer`);
	}
	expect((await tn.innerText()).includes('not independence'), 'B6 daily: the one-operator limit is stated');
	expect((await tn.innerText()).includes('free to read'), 'NS5-8 daily: the page says free public-source values are free to read');
	await page.close();

	// A forged USGS summary (another count, same everything else) passes no check and shows nothing.
	const forged = await browser.newPage();
	const j = JSON.parse(await readFile(join(root, CARDS[0].file), 'utf8'));
	if (j.sealed) j.commits = j.commits.slice(1);
	else j.value = String(Number(j.value) + 1);
	await forged.route('**/level2/usgs-quakes.json', (r) => r.fulfill({ json: j }));
	await forged.goto(base + '#/data');
	expect(
		await forged.getByTestId('daily-usgs-failed').waitFor({ timeout: 120000 }).then(() => true, () => false),
		'NS3 daily: a forged summary does not pass and shows no value'
	);
	expect((await forged.getByTestId('daily-usgs-value').count()) === 0, 'NS3 daily: no forged value on the page');
	await forged.close();
}

if (import.meta.url === `file://${process.argv[1]}`) {
	const { chromium } = await import('playwright-core');
	const { serve } = await import('./serve.mjs');
	const { resolve } = await import('node:path');
	const root = resolve(process.argv[2] ?? new URL('../build/', import.meta.url).pathname);
	const server = await serve(0, root);
	const browser = await chromium.launch({ channel: 'chrome' });
	const failures = [];
	const expect = (ok, what) => (ok ? console.log(`ok   ${what}`) : (failures.push(what), console.log(`FAIL ${what}`)));
	try {
		await daily(browser, `http://127.0.0.1:${server.address().port}/`, root, expect);
	} finally {
		await browser.close();
		server.close();
	}
	if (failures.length) process.exit(1);
	console.log('DAILY PASSED');
}
