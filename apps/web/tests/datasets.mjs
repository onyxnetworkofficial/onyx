// e2e "datasets" (ROADMAP S22): the #/datasets page against a fake node (page.route): the first
// page lists the node's datasets, "Show more" follows `next`, a search sends `q` and shows only the
// matches, an item out of shape is never printed, a dataset opens to its newest rows (a 64-column
// sample stays inside its scroll box on a phone) and the two check commands, and a busy node (429)
// says so. Called by tests/e2e.mjs; also runs alone against a build: node tests/datasets.mjs [build dir].
const BOOK = '0x04bb31d80c8ce9df02c8a03486b5812ec6e5ccc5';
const DAY = 86400;
const ds = (name, extra = {}) => ({
	publisher: 'onchain-archive',
	dataset: name,
	version: 1,
	root: '0x' + 'ab'.repeat(32),
	rows: 25,
	bytes: 2_500_000,
	shards: 1,
	stored_shards: 1,
	key_min: String(1_700_006_400),
	key_max: String(1_700_006_400 + 24 * DAY),
	license: 'CC-BY-4.0',
	forked: false,
	created_unix: 1_793_727_822,
	...extra
});
const ALL = ['btc.alpha', 'btc.beta', 'eth.gamma', 'eth.wide'].map((n) => ds(n));
const WIDE = ['day', 'series', 'i', 'j', ...Array.from({ length: 60 }, (_, i) => `v${String(i).padStart(2, '0')}`)];

function rowsAnswer(name) {
	const cols = name === 'eth.wide' ? WIDE : ['day', 'price_usd'];
	const rows = Array.from({ length: 5 }, (_, i) => {
		const key = String(1_700_006_400 + (20 + i) * DAY);
		const values = Object.fromEntries(cols.map((c, j) => [c, j === 0 ? key : c === 'series' ? 'bands' : `${1000 + i}.${j}`]));
		return { index: String(20 + i), key, row: '0x00', shard: '0', values, proof: { index: 0, leaf_count: 1, siblings: [] } };
	});
	return { anchor: { book: BOOK }, manifest: { schema: { columns: cols.map((name) => ({ name, kind: 'int' })) } }, rows, shards: [], next_page: null };
}

export async function datasets(browser, base, expect) {
	for (const width of [1280, 390]) {
		const at = `datasets ${width}`;
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
		const pg = await ctx.newPage();
		const errors = [];
		const asked = [];
		let busy = false;
		pg.on('pageerror', (e) => errors.push(String(e)));
		await pg.exposeFunction('__xss', () => errors.push('a hostile name ran script'));
		await pg.route('**/v1/bulk?*', (route) => {
			const u = new URL(route.request().url());
			asked.push(u.search);
			if (busy) return route.fulfill({ status: 429, json: { error: { code: 'rate_limited' } } });
			const q = (u.searchParams.get('q') ?? '').split(' ').filter(Boolean);
			const after = u.searchParams.get('after');
			const limit = Number(u.searchParams.get('limit') ?? 50);
			const hit = ALL.filter((d) => q.every((w) => `${d.publisher}/${d.dataset}`.includes(w)));
			const rest = hit.filter((d) => !after || `${d.publisher}/${d.dataset}` > after);
			// The fake pages 2 at a time whatever the limit, so "Show more" is reached with 4 datasets.
			const n = Math.min(limit, 2);
			const pageItems = rest.slice(0, n);
			const next = rest.length > n ? `${pageItems[n - 1].publisher}/${pageItems[n - 1].dataset}` : null;
			// One hostile item rides along on the first page: it must never be printed.
			const evil = !after && !q.length ? [ds('<img src=x onerror=__xss()>')] : [];
			return route.fulfill({ json: { datasets: [...pageItems, ...evil], total: hit.length, next } });
		});
		await pg.route('**/v1/bulk/onchain-archive/*/rows?*', (route) => {
			const name = new URL(route.request().url()).pathname.split('/')[4];
			return route.fulfill({ json: rowsAnswer(name) });
		});
		await pg.goto(base + '#/datasets');
		const list = pg.getByTestId('datasets-list');
		await list.waitFor({ timeout: 90000 });
		expect((await pg.locator('h1').innerText()).trim() === 'All datasets', `${at}: one h1, "All datasets"`);
		const names = async () => (await pg.getByTestId('datasets-open').locator('.mono').allInnerTexts()).map((t) => t.trim());
		expect(JSON.stringify(await names()) === JSON.stringify(['btc.alpha', 'btc.beta']), `${at}: the first page lists the node's first datasets (${await names()})`);
		expect((await pg.getByTestId('datasets-count').innerText()).includes('2 of 4'), `${at}: "Showing 2 of 4"`);
		expect((await pg.getByTestId('datasets-updated').first().innerText()).trim() === '2026-11-03', `${at}: each dataset shows the day its version was made`);
		expect(!(await pg.content()).includes('onerror=__xss'), `${at}: an item out of shape is dropped, never printed`);
		await pg.getByTestId('datasets-more').click();
		await pg.waitForFunction(() => document.querySelectorAll('[data-testid="datasets-row"]').length === 4, null, { timeout: 15000 }).catch(() => {});
		expect(JSON.stringify(await names()) === JSON.stringify(['btc.alpha', 'btc.beta', 'eth.gamma', 'eth.wide']), `${at}: "Show more" follows next (${await names()})`);
		expect((await pg.getByTestId('datasets-more').count()) === 0, `${at}: no "Show more" on the last page`);
		await pg.getByTestId('datasets-q').fill('ETH  Wide');
		await pg.waitForFunction(() => document.querySelectorAll('[data-testid="datasets-row"]').length === 1, null, { timeout: 15000 }).catch(() => {});
		expect(JSON.stringify(await names()) === '["eth.wide"]' && asked.some((s) => s.includes('q=eth+wide')), `${at}: a search sends q in the node's form and shows the matches (${await names()})`);
		await pg.getByTestId('datasets-open').first().click();
		const sample = pg.getByTestId('datasets-sample');
		await sample.waitFor({ timeout: 15000 });
		const S = await pg.evaluate(() => {
			const t = document.querySelector('[data-testid="datasets-sample"]');
			const box = t.closest('.tablebox');
			return {
				cols: t.querySelectorAll('thead th').length,
				rows: t.querySelectorAll('tbody tr').length,
				first: t.querySelector('tbody td')?.textContent.trim(),
				pageScroll: document.documentElement.scrollWidth > innerWidth + 1,
				boxScrolls: box.scrollWidth > box.clientWidth,
				expanded: document.querySelector('[data-testid="datasets-open"]').getAttribute('aria-expanded')
			};
		});
		expect(S.cols === 64 && S.rows === 5 && S.first === '2023-12-05', `${at}: the newest rows, 64 columns, day keys as dates (${S.cols}, ${S.rows}, ${S.first})`);
		expect(!S.pageScroll && (width > 1000 || S.boxScrolls), `${at}: the wide sample scrolls in its own box, never the page`);
		expect(S.expanded === 'true', `${at}: the opened dataset says so (aria-expanded)`);
		const cmd = await pg.getByTestId('datasets-commands').innerText();
		expect(cmd.includes('/v1/bulk/onchain-archive/eth.wide/rows?from=1701734400&to=1702080000') && cmd.includes(`onyx-check --bulk rows.json --publisher onchain-archive --dataset eth.wide --from 1701734400 --to 1702080000`) && cmd.includes(`--book ${BOOK}`), `${at}: the two check commands name the rows shown and the book`);
		const hash = await pg.evaluate(() => location.hash);
		expect(hash === '#/datasets?q=eth+wide&d=onchain-archive%2Feth.wide', `${at}: the address keeps the search and the open dataset (${hash})`);
		// A dataset link, opened fresh, searches for it and opens it.
		const pg2 = await ctx.newPage();
		await pg2.route('**/v1/bulk?*', (route) => {
			const q = new URL(route.request().url()).searchParams.get('q') ?? '';
			const hit = ALL.filter((d) => q.split(' ').every((w) => d.dataset.includes(w)));
			return route.fulfill({ json: { datasets: hit, total: hit.length, next: null } });
		});
		await pg2.route('**/v1/bulk/onchain-archive/*/rows?*', (route) => route.fulfill({ json: rowsAnswer(new URL(route.request().url()).pathname.split('/')[4]) }));
		await pg2.goto(base + '#/datasets?d=onchain-archive%2Feth.gamma');
		await pg2.getByTestId('datasets-sample').waitFor({ timeout: 30000 }).catch(() => {});
		const L = { q: await pg2.getByTestId('datasets-q').inputValue(), open: await pg2.getByTestId('datasets-detail').count(), link: await pg2.getByTestId('datasets-link').getAttribute('href').catch(() => null) };
		expect(L.q === 'eth.gamma' && L.open === 1 && L.link === '#/datasets?d=onchain-archive%2Feth.gamma', `${at}: a dataset link opens that dataset (${JSON.stringify(L)})`);
		await pg2.close();
		busy = true;
		await pg.getByTestId('datasets-q').fill('btc');
		await pg.waitForFunction(() => /Busy/.test(document.querySelector('[data-testid="datasets"]')?.textContent ?? ''), null, { timeout: 15000 }).catch(() => {});
		expect(/Busy: try again in a minute/.test(await pg.getByTestId('datasets').innerText()), `${at}: a busy node (429) says so`);
		expect(errors.length === 0, `${at}: no page errors${errors.length ? ': ' + errors.join('; ') : ''}`);
		await ctx.close();
	}
}

if (import.meta.url === `file://${process.argv[1]}`) {
	const { chromium } = await import('playwright-core');
	const { createServer } = await import('node:http');
	const { readFile } = await import('node:fs/promises');
	const { extname, join, normalize } = await import('node:path');
	const root = process.argv[2] ?? new URL('../build/', import.meta.url).pathname;
	const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.json': 'application/json', '.wasm': 'application/wasm' };
	const server = createServer(async (req, res) => {
		const path = normalize(decodeURIComponent(new URL(req.url, 'http://x').pathname)).replace(/^(\.\.[/\\])+/, '');
		try {
			const file = path === '/' ? 'index.html' : path;
			res.writeHead(200, { 'content-type': types[extname(file)] ?? 'application/octet-stream' }).end(await readFile(join(root, file)));
		} catch {
			res.writeHead(404).end();
		}
	}).listen(0, '127.0.0.1');
	await new Promise((r) => server.once('listening', r));
	const browser = await chromium.launch({ channel: 'chrome' });
	let failed = 0;
	await datasets(browser, `http://127.0.0.1:${server.address().port}/`, (ok, what) => {
		console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`);
		if (!ok) failed++;
	});
	await browser.close();
	server.close();
	process.exit(failed ? 1 : 0);
}
