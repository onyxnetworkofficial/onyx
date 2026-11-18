// e2e "datasets" (ROADMAP S22): the #/datasets page against a fake node (page.route): the list
// opens with the datasets updated in the last 30 days (majors first), then the rest, test data
// last; "Show more" lists the next 50; a search sends `any`, shows only the matches and ranks the
// searched asset first; the page says how many datasets are being updated and what a proof shows;
// an opened dataset charts its headline column, lists its newest rows first and downloads CSV; an item out of shape is never printed, a dataset opens to its newest rows (a 64-column
// sample stays inside its scroll box on a phone) and the two check commands, a link to a dataset
// that moved opens its live successor (the node's 410, R-BULK-17), and a busy node (429) says so. Called by tests/e2e.mjs; also runs alone against a build: node tests/datasets.mjs [build dir].
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
// READINESS A20: the node says each dataset's newest day and its age. btc.alpha is current,
// btc.beta stopped 80 days back, eth.gamma is at the 30-day edge (not stale), and eth.wide has a
// key the node does not read as a time (neither field: nothing shown, never stale).
const FRESHNESS = {
	'btc.alpha': { newest: '2026-11-13', newest_age_days: 3 },
	'btc.beta': { newest: '2026-08-28', newest_age_days: 80 },
	'eth.gamma': { newest: '2026-10-17', newest_age_days: 30 }
};
// Newcomer test 2026-11-18: the list opens with the datasets updated in the last 30 days (majors
// first), then the rest, test data last. aave.btc-beta is fresh and only holds the word btc (a
// search for "bitcoin beta" must still put the stale btc.beta first); 60 zec.* fillers stopped 80
// days back, so the first screen of 50 needs "Show more"; one test set of the test publisher.
const FILL = Array.from({ length: 60 }, (_, i) => ds(`zec.m${String(i).padStart(2, '0')}`, { newest: '2026-08-28', newest_age_days: 80 }));
const TESTSET = ds('testnet.early-contributors', { publisher: 'early-record', key_min: '348177563648', key_max: '348529885186' });
// In the node's order (publisher/dataset).
const ALL = [TESTSET, ds('aave.btc-beta', { newest: '2026-11-15', newest_age_days: 1 }), ...['btc.alpha', 'btc.beta', 'eth.gamma', 'eth.wide'].map((n) => ds(n, FRESHNESS[n] ?? {})), ...FILL];
const N = ALL.length;
const TOP = ['btc.alpha', 'eth.gamma', 'aave.btc-beta', 'btc.beta', 'eth.wide'];
// Review FRESH-R1 M1: a dataset whose newest key is dated in the future (found by its own name).
const AHEAD = ds('sol.ahead', { key_max: '4102358400', newest_in_future: true, newest: '2099-12-31', newest_age_days: 0 });
// R-BULK-17: what the fake node's 410 says of each removed dataset: eth.old moved to eth.gamma
// (live), eth.parked's successor was removed too, eth.astray's is called live but is not in the
// list, eth.waiting's links are not settled yet, eth.ended has none (null).
const succ = (final, status) => ({ dataset: final, final, generation: 1, hops: 1, proof: '/v1/bulk/onchain-archive/x/successor', row: '1', status });
const SUCCESSOR = { 'eth.old': succ('eth.gamma', 'live'), 'eth.parked': succ('eth.nowhere', 'removed'), 'eth.astray': succ('eth.unlisted', 'live'), 'eth.waiting': { status: 'pending' }, 'eth.ended': null };
const WIDE = ['day', 'series', 'i', 'j', ...Array.from({ length: 60 }, (_, i) => `v${String(i).padStart(2, '0')}`)];

// `history`: a page of a full-history download (limit=1000): two pages of 5 rows, by `next_page`.
function rowsAnswer(name, history = null) {
	// eth.gamma: the column named like the metric is not the first one (the chart must open on it).
	const cols = name === 'eth.wide' ? WIDE : name === 'eth.gamma' ? ['day', 'a_first', 'gamma'] : ['day', 'price_usd'];
	const first = history === null ? 20 : history * 5;
	const rows = Array.from({ length: 5 }, (_, i) => {
		const key = String(1_700_006_400 + (first + i) * DAY);
		const values = Object.fromEntries(cols.map((c, j) => [c, j === 0 ? key : c === 'series' ? 'bands' : `${1000 + i}.${j}`]));
		return { index: String(first + i), key, row: '0x00', shard: '0', values, proof: { index: 0, leaf_count: 1, siblings: [] } };
	});
	return { anchor: { book: BOOK }, manifest: { schema: { columns: cols.map((name) => ({ name, kind: 'int' })) } }, rows, shards: [], next_page: history === 0 ? 1 : null };
}
const rowsRoute = (route) => {
	const u = new URL(route.request().url());
	const history = u.searchParams.get('limit') === '1000' ? Number(u.searchParams.get('page') ?? 0) : null;
	return route.fulfill({ json: rowsAnswer(u.pathname.split('/')[4], history) });
};

// The node's `any` search (bulk_serve.rs AnyTerm): groups by ';', terms by ',', whole words of the
// id in a row, a last '*' for a prefix; every group needs one term.
const anyOf = (v) => (v ? v.split(';').map((g) => g.split(',').filter(Boolean)) : []);
function matchesAny(groups, id) {
	const words = id.split(/[^a-z0-9]+/).filter(Boolean);
	const term = (t) => {
		const prefix = t.endsWith('*');
		const tw = (prefix ? t.slice(0, -1) : t).split('-');
		return words.some((_, i) => tw.every((w, j) => (prefix && j === tw.length - 1 ? words[i + j]?.startsWith(w) : words[i + j] === w)));
	};
	return groups.every((g) => g.some(term));
}

export async function datasets(browser, base, expect) {
	for (const width of [1280, 390]) {
		const at = `datasets ${width}`;
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
		const pg = await ctx.newPage();
		const errors = [];
		const asked = [];
		let busy = false;
		// Review FRESH-R1 M2: a node that does not know `fresh` refuses it (400), as the live one did.
		let oldNode = false;
		pg.on('pageerror', (e) => errors.push(String(e)));
		await pg.exposeFunction('__xss', () => errors.push('a hostile name ran script'));
		await pg.route('**/v1/bulk?*', (route) => {
			const u = new URL(route.request().url());
			asked.push(u.search);
			if (busy) return route.fulfill({ status: 429, json: { error: { code: 'rate_limited' } } });
			const groups = anyOf(u.searchParams.get('any'));
			const after = u.searchParams.get('after');
			const limit = Number(u.searchParams.get('limit') ?? 50);
			const fresh = u.searchParams.get('fresh');
			if (fresh && oldNode) return route.fulfill({ status: 400, json: { error: { code: 'bad_request', message: 'unknown query parameter fresh' } } });
			if (groups.length && matchesAny(groups, 'onchain-archive/sol.ahead')) return route.fulfill({ json: { datasets: fresh ? [] : [AHEAD], total: fresh ? 0 : 1, next: null } });
			// The node's `fresh=30d`: a newest day at most 30 days behind; no newest day, left out.
			const hit = ALL.filter((d) => matchesAny(groups, `${d.publisher}/${d.dataset}`)).filter((d) => !fresh || (fresh === '30d' && d.newest_age_days <= 30));
			const rest = hit.filter((d) => !after || `${d.publisher}/${d.dataset}` > after);
			// The fake pages 2 at a time whatever the limit, so "Show more" is reached with 4 datasets.
			const n = Math.min(limit, 2);
			const pageItems = rest.slice(0, n);
			const next = rest.length > n ? `${pageItems[n - 1].publisher}/${pageItems[n - 1].dataset}` : null;
			// One hostile item rides along on the first page: it must never be printed.
			const evil = !after && !groups.length ? [ds('<img src=x onerror=__xss()>')] : [];
			return route.fulfill({ json: { datasets: [...pageItems, ...evil], total: hit.length, next } });
		});
		await pg.route('**/v1/bulk/onchain-archive/*/rows?*', rowsRoute);
		await pg.goto(base + '#/datasets');
		const list = pg.getByTestId('datasets-list');
		await list.waitFor({ timeout: 90000 });
		expect((await pg.locator('h1').innerText()).trim() === 'All datasets', `${at}: one h1, "All datasets"`);
		const names = async () => (await pg.getByTestId('datasets-id').allInnerTexts()).map((t) => t.trim());
		const top = async () => JSON.stringify((await names()).slice(0, TOP.length));
		// The list fills step by step (one node list a step): wait for the first screen of 50.
		await pg.waitForFunction((n) => new RegExp(`50 of ${n}`).test(document.querySelector('[data-testid="datasets-count"]')?.textContent ?? ''), N, { timeout: 15000 }).catch(() => {});
		expect((await top()) === JSON.stringify(TOP) && (await names()).length === 50, `${at}: the list opens with the datasets updated in the last 30 days (Bitcoin, Ethereum, then by name), then the rest, 50 shown (${(await names()).slice(0, 6)})`);
		expect((await pg.getByTestId('datasets-count').innerText()).includes(`50 of ${N}`), `${at}: "Showing 50 of ${N}"`);
		// Staleness in plain numbers, from the node's own two counts; and what a proof does not show.
		const fresh30 = (await pg.getByTestId('datasets-freshness').innerText()).replace(/\s+/g, ' ').trim();
		expect(fresh30 === `3 of ${N} datasets were updated in the last 30 days. The other ${N - 3} were not: each of them shows its last day.` && asked.some((q) => q === '?fresh=30d&limit=1') && asked.some((q) => q === '?limit=1'), `${at}: the page says how many datasets are being updated, from the node's counts ("${fresh30}")`);
		const proofNote = (await pg.getByTestId('datasets-proof-note').innerText()).replace(/\s+/g, ' ').trim();
		expect(proofNote === 'The proof shows these are exactly the rows the publisher signed and anchored on the test chain. It does not show that the numbers are right.', `${at}: the page says what a proof shows and what it does not ("${proofNote}")`);
		const pub = await pg.getByTestId('datasets-publisher').first().evaluate((dd) => ({ text: dd.textContent.trim(), tip: dd.parentElement.getAttribute('title') }));
		expect(pub.text === 'onchain-archive' && /signed this version with its own key\. Onyx did not check the numbers\./.test(pub.tip ?? ''), `${at}: each card names its publisher and says what that means (${JSON.stringify(pub)})`);
		expect((await pg.getByTestId('datasets-updated').first().innerText()).trim() === '2026-11-03', `${at}: each dataset shows the day its version was made`);
		// READINESS A20: the newest day on every time dataset, and a plain mark on the stale one.
		const F = await pg.evaluate(() =>
			[...document.querySelectorAll('[data-testid="datasets-row"]')].map((li) => {
				const m = li.querySelector('[data-testid="datasets-stale"]');
				const cs = m ? getComputedStyle(m) : null;
				return {
					id: li.querySelector('[data-testid="datasets-id"]')?.textContent.trim(),
					newest: li.querySelector('[data-testid="datasets-newest"]')?.textContent.trim() ?? null,
					stale: m ? m.textContent.replace(/\s+/g, ' ').trim() : null,
					// Not colour alone: words, and a dashed frame.
					framed: cs ? cs.borderTopStyle === 'dashed' && parseFloat(cs.borderTopWidth) >= 1 : null,
					inside: m ? m.getBoundingClientRect().right <= innerWidth + 1 : null
				};
			})
		);
		expect(JSON.stringify(F.slice(0, 5).map((x) => [x.id, x.newest, x.stale])) === JSON.stringify([['btc.alpha', '2026-11-13', null], ['eth.gamma', '2026-10-17', null], ['aave.btc-beta', '2026-11-15', null], ['btc.beta', '2026-08-28', '!No data after 2026-08-28'], ['eth.wide', null, null]]), `${at}: each dataset shows its newest day; the one that stopped says "No data after", the detail line's words (review FRESH-R1 L1) (${JSON.stringify(F.slice(0, 5))})`);
		expect(F[3].framed === true && F[3].inside === true, `${at}: the mark is words in a dashed frame, inside the page`);
		expect(await pg.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), `${at}: the list with its marks does not scroll sideways`);
		expect(!(await pg.content()).includes('onerror=__xss'), `${at}: an item out of shape is dropped, never printed`);
		// The CSP would refuse the handler anyway, so the canary cannot fire: check the markup
		// itself (review of WEB-CSP-1, finding 3).
		expect(await pg.evaluate(() => document.querySelector('[onerror]') === null), `${at}: no element carries an onerror handler`);
		await pg.getByTestId('datasets-more').click();
		await pg.waitForFunction((n) => document.querySelectorAll('[data-testid="datasets-row"]').length === n, N, { timeout: 15000 }).catch(() => {});
		const allNames = await names();
		expect(allNames.length === N && new Set(allNames).size === N && allNames.slice(5, 65).every((n) => n.startsWith('zec.')), `${at}: "Show more" lists the rest, each dataset once (${allNames.length})`);
		expect((await pg.getByTestId('datasets-more').count()) === 0, `${at}: no "Show more" on the last page`);
		// Test data is last, with a tag.
		const lastRow = await pg.getByTestId('datasets-row').last().evaluate((li) => ({ id: li.querySelector('[data-testid="datasets-id"]').textContent.trim(), tag: li.querySelector('[data-testid="datasets-test"]')?.textContent.trim() ?? null, pub: li.querySelector('[data-testid="datasets-publisher"]').textContent.trim() }));
		expect(lastRow.id === 'testnet.early-contributors' && lastRow.tag === 'test data' && lastRow.pub === 'early-record' && (await pg.getByTestId('datasets-test').count()) === 1, `${at}: test data is listed last with a "test data" tag (${JSON.stringify(lastRow)})`);
		const marks = async () => pg.evaluate(() => [...document.querySelectorAll('[data-testid="datasets-row"]')].filter((li) => li.querySelector('[data-testid="datasets-stale"]')).map((li) => li.querySelector('[data-testid="datasets-id"]').textContent.trim()).filter((id) => !id.startsWith('zec.')));
		const newestOf = async (id) => pg.evaluate((id) => [...document.querySelectorAll('[data-testid="datasets-row"]')].find((li) => li.querySelector('[data-testid="datasets-id"]').textContent.trim() === id)?.querySelector('[data-testid="datasets-newest"]')?.textContent.trim() ?? null, id);
		expect(JSON.stringify(await marks()) === '["btc.beta"]', `${at}: 30 days behind is not marked, and a key that is no time is never marked (${await marks()})`);
		expect((await newestOf('eth.gamma')) === '2026-10-17' && (await newestOf('eth.wide')) === null, `${at}: a key that is no time shows no newest day`);
		// The filter asks the node for fresh datasets only, and "All" brings the rest back.
		expect((await pg.getByTestId('datasets-all').getAttribute('aria-pressed')) === 'true' && (await pg.getByTestId('datasets-fresh').getAttribute('aria-pressed')) === 'false', `${at}: the list starts with all datasets`);
		await pg.getByTestId('datasets-fresh').click();
		await pg.waitForFunction(() => /3 of 3/.test(document.querySelector('[data-testid="datasets-count"]')?.textContent ?? ''), null, { timeout: 15000 }).catch(() => {});
		expect(JSON.stringify(await names()) === JSON.stringify(['btc.alpha', 'eth.gamma', 'aave.btc-beta']) && asked.some((s) => new URLSearchParams(s).get('fresh') === '30d') && (await pg.getByTestId('datasets-stale').count()) === 0, `${at}: "Updated in the last 30 days" asks the node's fresh=30d and lists only those (${await names()})`);
		expect((await pg.getByTestId('datasets-fresh').getAttribute('aria-pressed')) === 'true', `${at}: the chosen filter says so (aria-pressed)`);
		expect(await pg.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), `${at}: the filter fits the page`);
		await pg.getByTestId('datasets-all').click();
		await pg.waitForFunction((n) => new RegExp(`50 of ${n}`).test(document.querySelector('[data-testid="datasets-count"]')?.textContent ?? ''), N, { timeout: 15000 }).catch(() => {});
		expect((await top()) === JSON.stringify(TOP), `${at}: "All" lists every dataset again (${(await names()).slice(0, 6)})`);
		// Review FRESH-R1 M2: the filter against a node without `fresh`: the page says the filter is
		// not available, turns it back and lists all; it never says the node has no datasets.
		oldNode = true;
		await pg.getByTestId('datasets-fresh').click();
		await pg.getByTestId('datasets-filter-off').waitFor({ timeout: 15000 }).catch(() => {});
		await pg.waitForFunction((n) => new RegExp(`50 of ${n}`).test(document.querySelector('[data-testid="datasets-count"]')?.textContent ?? ''), N, { timeout: 15000 }).catch(() => {});
		const O = {
			note: (await pg.getByTestId('datasets-filter-off').allInnerTexts()).join(' '),
			none: await pg.getByTestId('datasets-none').count(),
			all: await pg.getByTestId('datasets-all').getAttribute('aria-pressed'),
			fresh: await pg.getByTestId('datasets-fresh').getAttribute('aria-pressed'),
			names: (await names()).slice(0, 3)
		};
		// Without the filter the page cannot tell which datasets are fresh: the node's own order.
		expect(/30-day filter is not available/.test(O.note) && O.none === 0 && O.all === 'true' && O.fresh === 'false' && JSON.stringify(O.names) === JSON.stringify(['testnet.early-contributors', 'aave.btc-beta', 'btc.alpha']), `${at}: a node without the filter: the page says so, turns the switch back and lists all (${JSON.stringify(O)})`);
		oldNode = false;
		await pg.getByTestId('datasets-fresh').click();
		await pg.waitForFunction(() => /3 of 3/.test(document.querySelector('[data-testid="datasets-count"]')?.textContent ?? ''), null, { timeout: 15000 }).catch(() => {});
		expect((await pg.getByTestId('datasets-filter-off').count()) === 0 && JSON.stringify(await names()) === JSON.stringify(['btc.alpha', 'eth.gamma', 'aave.btc-beta']), `${at}: once the node takes the filter the note goes`);
		await pg.getByTestId('datasets-all').click();
		await pg.waitForFunction((n) => new RegExp(`50 of ${n}`).test(document.querySelector('[data-testid="datasets-count"]')?.textContent ?? ''), N, { timeout: 15000 }).catch(() => {});
		// Review FRESH-R1 M1: dates in the future are said plainly: no newest day, no "fresh".
		await pg.getByTestId('datasets-q').fill('sol ahead');
		await pg.getByTestId('datasets-future').waitFor({ timeout: 15000 }).catch(() => {});
		const A = {
			future: (await pg.getByTestId('datasets-future').allInnerTexts()).join(' ').replace(/\s+/g, ' ').trim(),
			newest: await pg.getByTestId('datasets-newest').count(),
			stale: await pg.getByTestId('datasets-stale').count(),
			inside: await pg.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1)
		};
		expect(/Dates in the future: the newest day is not known/.test(A.future) && A.newest === 0 && A.stale === 0 && A.inside, `${at}: a dataset dated in the future says so and shows no newest day (${JSON.stringify(A)})`);
		await pg.getByTestId('datasets-fresh').click();
		await pg.getByTestId('datasets-empty').waitFor({ timeout: 15000 }).catch(() => {});
		expect((await pg.getByTestId('datasets-row').count()) === 0, `${at}: a dataset dated in the future is not in the 30-day list`);
		await pg.getByTestId('datasets-all').click();
		await pg.getByTestId('datasets-q').fill('');
		await pg.waitForFunction((n) => new RegExp(`50 of ${n}`).test(document.querySelector('[data-testid="datasets-count"]')?.textContent ?? ''), N, { timeout: 15000 }).catch(() => {});
		// Search ranking: the dataset of the asset searched comes first, even when it is stale and
		// another asset's dataset that only holds the word is fresh (the node sends that one first).
		await pg.getByTestId('datasets-q').fill('bitcoin beta');
		await pg.waitForFunction(() => document.querySelectorAll('[data-testid="datasets-row"]').length === 2 && /2 of 2/.test(document.querySelector('[data-testid="datasets-count"]')?.textContent ?? ''), null, { timeout: 15000 }).catch(() => {});
		const ranked = { names: await names(), note: (await pg.getByTestId('datasets-order').innerText()).replace(/\s+/g, ' ').trim() };
		expect(JSON.stringify(ranked.names) === JSON.stringify(['btc.beta', 'aave.btc-beta']) && /^Best match first/.test(ranked.note) && /1 of 2 found were updated in the last 30 days\./.test(ranked.note), `${at}: a search for "bitcoin beta" lists the Bitcoin dataset first, though it is stale and the other is fresh (${JSON.stringify(ranked)})`);
		await pg.getByTestId('datasets-q').fill('');
		await pg.waitForFunction((n) => new RegExp(`50 of ${n}`).test(document.querySelector('[data-testid="datasets-count"]')?.textContent ?? ''), N, { timeout: 15000 }).catch(() => {});
		// The opened stale dataset says its rows end there (the dataset view).
		await pg.getByTestId('datasets-open').nth(3).click();
		await pg.getByTestId('datasets-sample').waitFor({ timeout: 15000 }).catch(() => {});
		const stale = (await pg.getByTestId('datasets-detail-stale').allInnerTexts()).join(' ');
		expect(/no data after 2026-08-28/.test(stale) && /not current values/.test(stale), `${at}: an opened stale dataset says its rows are not current (${stale})`);
		expect(await pg.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), `${at}: the opened stale dataset does not scroll the page sideways`);
		await pg.getByTestId('datasets-open').nth(3).click();
		await pg.getByTestId('datasets-q').fill('ETH  Wide');
		await pg.waitForFunction(() => document.querySelectorAll('[data-testid="datasets-row"]').length === 1, null, { timeout: 15000 }).catch(() => {});
		expect(JSON.stringify(await names()) === '["eth.wide"]' && asked.some((s) => new URLSearchParams(s).get('any') === 'eth*;wide*'), `${at}: a search sends the node's whole-word any search and shows the matches (${await names()})`);
		const T = { title: (await pg.getByTestId('datasets-title').first().innerText()).trim(), about: await pg.getByTestId('datasets-about').count() };
		expect(T.title === 'Ethereum · Wide', `${at}: a dataset reads as a title, the asset by name (${T.title})`);
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
		expect(S.cols === 64 && S.rows === 5 && S.first === '2023-12-09', `${at}: the newest rows, newest first, 64 columns, day keys as dates (${S.cols}, ${S.rows}, ${S.first})`);
		const py = await pg.getByTestId('datasets-python').innerText();
		expect(py.split('\n').length === 5 && py.includes('import requests, pandas as pd') && py.includes('/v1/bulk/onchain-archive/eth.wide/rows"') && py.includes('"limit": 1000'), `${at}: five lines of Python read the same dataset (${py.split('\n').length} lines)`);
		expect((await pg.getByTestId('datasets-chart').count()) === 0, `${at}: a table of series (one row per series and day) gets no chart`);
		expect(!S.pageScroll && (width > 1000 || S.boxScrolls), `${at}: the wide sample scrolls in its own box, never the page`);
		expect(S.expanded === 'true', `${at}: the opened dataset says so (aria-expanded)`);
		const cmd = await pg.getByTestId('datasets-commands').innerText();
		expect(cmd.includes('/v1/bulk/onchain-archive/eth.wide/rows?from=1701734400&to=1702080000') && cmd.includes(`onyx-check --bulk rows.json --publisher onchain-archive --dataset eth.wide --from 1701734400 --to 1702080000`) && cmd.includes(`--book ${BOOK}`), `${at}: the two check commands name the rows shown and the book`);
		const hash = await pg.evaluate(() => location.hash);
		expect(hash === '#/datasets?q=eth+wide&d=onchain-archive%2Feth.wide', `${at}: the address keeps the search and the open dataset (${hash})`);
		// A dataset link, opened fresh, searches for it and opens it.
		const fresh = async () => {
			const p = await ctx.newPage();
			await p.route('**/v1/bulk?*', (route) => {
				const groups = anyOf(new URL(route.request().url()).searchParams.get('any'));
				const hit = ALL.filter((d) => matchesAny(groups, `${d.publisher}/${d.dataset}`));
				return route.fulfill({ json: { datasets: hit, total: hit.length, next: null } });
			});
			await p.route('**/v1/bulk/onchain-archive/*/rows?*', rowsRoute);
			// R-BULK-17: the node's answer about one dataset. A removed one is 410 with its successor.
			await p.route('**/v1/bulk/onchain-archive/*', (route) => {
				const name = new URL(route.request().url()).pathname.split('/')[4];
				const s = SUCCESSOR[name];
				if (s === undefined) return route.fulfill({ status: 404, json: { error: { code: 'bulk_unknown_dataset' } } });
				return route.fulfill({ status: 410, json: { catalog: 15, error: { code: 'bulk_dataset_removed', message: 'the publisher removed this dataset in its catalog 15' }, ...(s ? { successor: s } : {}) } });
			});
			return p;
		};
		const pg2 = await fresh();
		await pg2.goto(base + '#/datasets?d=onchain-archive%2Feth.gamma');
		await pg2.getByTestId('datasets-sample').waitFor({ timeout: 30000 }).catch(() => {});
		const L = { q: await pg2.getByTestId('datasets-q').inputValue(), open: await pg2.getByTestId('datasets-detail').count(), link: await pg2.getByTestId('datasets-link').getAttribute('href').catch(() => null) };
		expect(L.q === 'eth.gamma' && L.open === 1 && L.link === '#/datasets?d=onchain-archive%2Feth.gamma', `${at}: a dataset link opens that dataset (${JSON.stringify(L)})`);
		const C = await pg2.evaluate(() => {
			const svg = document.querySelector('[data-testid="datasets-chart"]');
			const labels = [...(svg?.querySelectorAll('text.axis') ?? [])].map((t) => t.getBoundingClientRect()).filter((r) => r.top > (svg?.getBoundingClientRect().top ?? 0) + 100);
			const overlap = labels.some((a, i) => labels.some((b, j) => i < j && a.left < b.right && b.left < a.right));
			return { line: !!svg?.querySelector('path.line'), overlap, scroll: document.documentElement.scrollWidth > innerWidth + 1 };
		});
		expect(C.line && !C.scroll && !C.overlap, `${at}: a dataset of plain numbers opens with a chart of its newest rows, inside the page, its dates apart (${JSON.stringify(C)})`);
		// The chart opens on the column named like the metric (gamma), not the first one (a_first).
		const col = await pg2.getByTestId('datasets-chart-col').evaluate((sel) => sel.options[sel.selectedIndex].textContent.trim());
		expect(col === 'gamma', `${at}: the chart opens on the dataset's headline column (${col})`);
		// A dataset link brings its opened card to the top of the screen.
		await pg2.waitForFunction(() => {
			const r = document.querySelector('[data-testid="datasets-row"]')?.getBoundingClientRect();
			return r && r.top >= 0 && r.top < innerHeight / 2;
		}, null, { timeout: 15000 }).catch(() => {});
		const cardTop = await pg2.evaluate(() => Math.round(document.querySelector('[data-testid="datasets-row"]').getBoundingClientRect().top));
		expect(cardTop >= 0 && cardTop < 450 && (await pg2.evaluate(() => scrollY)) > 0, `${at}: a dataset link scrolls the opened card into view (top at ${cardTop} px)`);
		// Download CSV: the rows loaded for the chart, then the full history page by page.
		const text = async (download) => (await import('node:fs/promises')).readFile(await download.path(), 'utf8');
		const [shownCsv] = await Promise.all([pg2.waitForEvent('download', { timeout: 15000 }), pg2.getByTestId('datasets-csv').click()]);
		const shownText = await text(shownCsv);
		expect(shownCsv.suggestedFilename() === 'onchain-archive_eth.gamma.csv' && shownText.startsWith('day,a_first,gamma\r\n2023-12-05,1000.1,1000.2\r\n') && shownText.trim().split('\r\n').length === 6, `${at}: "Download CSV" saves the rows shown, with a header and dates (${JSON.stringify(shownText.slice(0, 60))})`);
		const [allCsv] = await Promise.all([pg2.waitForEvent('download', { timeout: 15000 }), pg2.getByTestId('datasets-csv-all').click()]);
		const allText = await text(allCsv);
		await pg2.waitForFunction(() => /Saved 10 rows/.test(document.querySelector('[data-testid="datasets-csv-status"]')?.textContent ?? ''), null, { timeout: 15000 }).catch(() => {});
		const status = (await pg2.getByTestId('datasets-csv-status').innerText()).replace(/\s+/g, ' ').trim();
		expect(allText.trim().split('\r\n').length === 11 && allText.includes('\r\n2023-11-15,1000.1,1000.2\r\n') && allText.includes('\r\n2023-11-24,') && /^Saved 10 rows, oldest first\./.test(status), `${at}: "Download full history" pages through the node (2 calls here) and says what it saved ("${status}")`);
		// A link to a dataset this node does not hold says so and shows the closest names.
		await pg2.close();
		const pg3 = await fresh();
		await pg3.goto(base + '#/datasets?d=onchain-archive%2Feth.gone');
		await pg3.getByTestId('datasets-missing').waitFor({ timeout: 30000 }).catch(() => {});
		expect((await pg3.getByTestId('datasets-missing').count()) === 1 && (await pg3.getByTestId('datasets-detail').count()) === 0, `${at}: a link to a dataset the node does not hold says so`);
		await pg3.close();
		// R-BULK-17: a link to a dataset that moved says where it went and opens that one.
		const pg4 = await fresh();
		await pg4.goto(base + '#/datasets?d=onchain-archive%2Feth.old');
		await pg4.getByTestId('datasets-sample').waitFor({ timeout: 30000 }).catch(() => {});
		const M = {
			note: (await pg4.getByTestId('datasets-moved').allInnerTexts()).join(' ').replace(/\s+/g, ' ').trim(),
			missing: await pg4.getByTestId('datasets-missing').count(),
			open: await pg4.getByTestId('datasets-detail').count(),
			id: (await pg4.getByTestId('datasets-id').allInnerTexts()).join(','),
			hash: await pg4.evaluate(() => location.hash),
			inside: await pg4.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1)
		};
		expect(M.note.startsWith('This dataset moved to onchain-archive/eth.gamma.') && M.note.includes('onchain-archive/eth.old') && M.missing === 0 && M.open === 1 && M.id === 'eth.gamma' && M.hash === '#/datasets?q=eth.gamma&d=onchain-archive%2Feth.gamma' && M.inside, `${at}: a link to a moved dataset says where it moved and opens the successor (${JSON.stringify(M)})`);
		// A new search is a new question: the note goes.
		await pg4.getByTestId('datasets-q').fill('btc');
		await pg4.waitForFunction(() => !document.querySelector('[data-testid="datasets-moved"]'), null, { timeout: 15000 }).catch(() => {});
		expect((await pg4.getByTestId('datasets-moved').count()) === 0, `${at}: the moved note goes with the next search`);
		await pg4.close();
		// A successor that is not live, or one the node's list does not hold, is said and never opened.
		for (const [name, want] of [['eth.parked', /named onchain-archive\/eth\.nowhere in its place\. This node does not serve that dataset now, so nothing is opened\./], ['eth.astray', /named onchain-archive\/eth\.unlisted in its place\. This node does not serve that dataset now/], ['eth.waiting', /The publisher removed onchain-archive\/eth\.waiting\. No dataset that this node serves is named in its place\./]]) {
			const p = await fresh();
			await p.goto(base + `#/datasets?d=onchain-archive%2F${name}`);
			await p.getByTestId('datasets-moved').waitFor({ timeout: 30000 }).catch(() => {});
			const N = { note: (await p.getByTestId('datasets-moved').allInnerTexts()).join(' ').replace(/\s+/g, ' ').trim(), open: await p.getByTestId('datasets-detail').count(), missing: await p.getByTestId('datasets-missing').count(), asked: await p.getByTestId('datasets-moved').getAttribute('data-live').catch(() => null) };
			expect(want.test(N.note) && !/moved to/.test(N.note) && N.open === 0 && N.missing === 0 && N.asked === 'false', `${at}: ${name}: a successor that is not served is said plainly, nothing opened (${JSON.stringify(N)})`);
			await p.close();
		}
		// A removed dataset with no successor keeps the old sentence.
		const pg5 = await fresh();
		await pg5.goto(base + '#/datasets?d=onchain-archive%2Feth.ended');
		await pg5.getByTestId('datasets-missing').waitFor({ timeout: 30000 }).catch(() => {});
		expect((await pg5.getByTestId('datasets-missing').count()) === 1 && (await pg5.getByTestId('datasets-moved').count()) === 0, `${at}: a removed dataset without a successor says the node holds no such dataset`);
		await pg5.close();
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
