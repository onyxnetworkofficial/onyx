// e2e "NS11 live markets" (ADR-0042 amendment 2): the Markets page shows the futures markets that
// run on Base Sepolia, read from the chain through the reader's own RPC only when asked. The reads
// here are answered by a stand-in RPC with the bytes Base Sepolia returned for market 1 on
// 2026-10-27 (four bets, open), so the test needs no network; the verifier decodes and checks them
// (verify/src/markets_view.rs). A market whose fact is not the page's, another chain, and a
// resolved market whose winner is not the bucket of the recorded value are refused. Before the
// reader asks, the page sends no request off the site. The same read shows the test insurance
// products (NL18), answered with the bytes Base Sepolia returned for them on 2026-10-28; a pool
// that holds back more than it has is refused. Every read is an eth_call at one block (THREATS
// NL6-20, NL6-23), and the RPC address is never stored or put in the address bar (NL6-24).
// Called by tests/e2e.mjs at 1280 and 390 px.
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

/** `market(1)` on Base Sepolia, 2026-10-27 (eth_call at the latest block). */
const MARKET_1 = [
	'0x00000000000000000000000000000000000000000000000000000000000000201598f049997f46cda70ff579a1cd92a426ba7dac420709fe9b86b9fe8f7c89',
	'75000000000000000000000000000000000000000000000000000000006ae1e3c0000000000000000000000000000000000000000000000000000000006ae28c',
	'80000000000000000000000000000000000000000000000000000000006aebc70000000000000000000000000000000000000000000000000000000000000000',
	'01000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000',
	'00000000000000000000000000000000000000000000000000000000000000000400000000000000000000000000000000000000000000000000000000000000',
	'00000000000000000000000000000000000000000000000006c6b935b8bbd4000000000000000000000000000000000000000000000000000000000000000000',
	'00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000',
	'00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002',
	'20000000000000000000000000000000000000000000000000000000000000028000000000000000000000000000000000000000000000000000000000000003',
	'000000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000e5ee7f41dd0',
	'00000000000000000000000000000000000000000000000000000e63741b57200000000000000000000000000000000000000000000000000000000000000000',
	'03000000000000000000000000000000000000000000000001a055690d9db8000000000000000000000000000000000000000000000000000410d586a20a4c00',
	'00000000000000000000000000000000000000000000000001158e460913d0000000000000000000000000000000000000000000000000000000000000000000',
	'03000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000',
	'020000000000000000000000000000000000000000000000000000000000000001',
].join('');
/** `market(1)` on the guarded futures, Base Sepolia 2026-10-29: the page's market 2 (NL23; four bets, open). */
const GUARDED_1 = [
	'0x00000000000000000000000000000000000000000000000000000000000000203dd92d8cbf2890ecfd7cfc0c1d88b223ef5f3498995480736187ed9bfd9e27',
	'92000000000000000000000000000000000000000000000000000000006ae7c470000000000000000000000000000000000000000000000000000000006aea75',
	'80000000000000000000000000000000000000000000000000000000006af3b00000000000000000000000000000000000000000000000000000000000000000',
	'01000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000',
	'00000000000000000000000000000000000000000000000000000000000000000400000000000000000000000000000000000000000000000000000000000000',
	'000000000000000000000000000000000000000000000000068155a43676e0000000000000000000000000000000000000000000000000000000000000000000',
	'00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000',
	'00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002',
	'20000000000000000000000000000000000000000000000000000000000000028000000000000000000000000000000000000000000000000000000000000003',
	'000000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000e6c8c69c9c0',
	'00000000000000000000000000000000000000000000000000000e75a4b83c600000000000000000000000000000000000000000000000000000000000000000',
	'03000000000000000000000000000000000000000000000001a055690d9db80000000000000000000000000000000000000000000000000003cb71f51fc55800',
	'00000000000000000000000000000000000000000000000001158e460913d0000000000000000000000000000000000000000000000000000000000000000000',
	'03000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000',
	'020000000000000000000000000000000000000000000000000000000000000001',
].join('');
const GUARDED_FUTURES = '0x07e89ae47579f9297c28ccab782acf9443354a65';
// THREATS NL6-16, NL6-18 cite these fixed titles.
const NO_VALUE = 'NS11: the live markets say tONYX is a test token with no value';
const OFF_SITE = 'NS11: opening the Markets page sends nothing off the site';
const OTHER_FACT = 'NS11: a market naming another fact is refused';
const OTHER_CHAIN = 'NS11: an RPC on another chain is refused';
const SEL = { count: '0xec979082', market: '0x28861d22', fact: '0x1bb5ae63', products: '0xe0f6ef87', pool: '0x16f0115b', reserved: '0xfe60d12c', product: '0x780bd3f1' };
/** `product(1)` and `product(2)` on Base Sepolia, 2026-10-28 (open, none sold). */
const PRODUCT = {
	'1': '0xc1ba8d6dad265d88506a37094c709084b4a98366e0ac1dba7e252bd6563e23ef0000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000014000000000000000000000000000000000000000000000000000000006ae52f80000000000000000000000000000000000000000000000000000000006b0e0e00000000000000000000000000000000000000000000000000000000006b359b0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000056bc75e2d631000000000000000000000000000000000000000000000000000008ac7230489e800000000000000000000000000000000000000000000000000000000000000000000',
	'2': '0xbcec4881cd913049dffb6463a010052534adc47d99d6cf2600096b194b09eeee0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000014000000000000000000000000000000000000000000000000000000006ae52f80000000000000000000000000000000000000000000000000000000006b6a5700000000000000000000000000000000000000000000000000000000006b84b5000000000000000000000000000000000000000000000000000000000000000006000000000000000000000000000000000000000000000002b5e3af16b1880000000000000000000000000000000000000000000000000000d02ab486cedc00000000000000000000000000000000000000000000000000000000000000000000',
};
const READ_ONLY = 'NS11: the page only reads from the RPC, at one block';
const NOT_STORED = 'NS11: the RPC address is not stored or put in the address bar';
const OVERSIZED = 'NS11: an oversized RPC answer is refused';
const SHARES = 'NS11: pool shares are not shown as chances below ten bettors';
const BASIS = 'NL18: the page names basis risk';
const UNCOVERED = 'NL18: a pool that holds back more than it has is refused';
const word = (n) => BigInt.asUintN(256, BigInt(n)).toString(16).padStart(64, '0');
const factValue = (fin, v) => '0x' + word(fin ? 1 : 0) + word(v) + word(fin ? 2 : 0) + word(fin ? 1793240000 : 0);

/** A stand-in JSON-RPC: chain id, block, market count, `market(1)`, `factValue` and the policy reads. */
function rpc({ chain = '0x14a34', market = MARKET_1, guarded = GUARDED_1, fact = factValue(false, 0), pool = 0n, reserved = 0n, seen = [] } = {}) {
	return async (route) => {
		const req = route.request().postDataJSON();
		seen.push(req.method === 'eth_call' ? `eth_call@${req.params[1]}` : req.method);
		let result = null;
		if (req.method === 'eth_chainId') result = chain;
		else if (req.method === 'eth_blockNumber') result = '0x30a1b2c';
		else if (req.method === 'eth_call') {
			const d = req.params[0].data;
			const g = req.params[0].to.toLowerCase() === GUARDED_FUTURES;
			if (d.startsWith(SEL.count)) result = '0x' + word(1);
			else if (d.startsWith(SEL.market)) result = g ? guarded : market;
			else if (d.startsWith(SEL.fact)) result = fact;
			else if (d.startsWith(SEL.products)) result = '0x' + word(2);
			else if (d.startsWith(SEL.pool)) result = '0x' + word(pool);
			else if (d.startsWith(SEL.reserved)) result = '0x' + word(reserved);
			else if (d.startsWith(SEL.product)) result = PRODUCT[String(Number(BigInt('0x' + d.slice(10))))];
		}
		await route.fulfill({ json: { jsonrpc: '2.0', id: req.id, result } });
	};
}

export async function ns11(browser, base, root, expect) {
	const file = JSON.parse(await readFile(join(root, 'markets/base-sepolia.json'), 'utf8'));
	// The page's addresses are the deployment's (docs/ops/deployment.md "Markets contract").
	const dep = await readFile(new URL('../../../docs/ops/deployment.md', import.meta.url), 'utf8');
	for (const k of ['token', 'stakes', 'panel', 'facts', 'futures'])
		expect(dep.includes('`' + file[k] + '`'), `NS11 live markets: the ${k} address is the deployment's (${file[k]})`);
	for (const k of ['address', 'facts', 'futures'])
		expect(dep.includes('`' + file.insurance[k] + '`'), `NL18 insurance: the ${k} address is the deployment's (${file.insurance[k]})`);
	for (const [id, hex] of Object.entries(PRODUCT))
		expect(hex.slice(2, 66) === file.insurance.products[Number(id) - 1].fact.slice(2), `NL18 insurance: the recorded product ${id} names the page's fact`);
	expect(MARKET_1.slice(66, 130) === file.markets[0].fact.slice(2), 'NS11 live markets: the recorded return names the page\'s fact');
	expect(file.insurance.futures === GUARDED_FUTURES && GUARDED_1.slice(66, 130) === file.insurance.markets[0].fact.slice(2), 'NL23: market 2 is on the guarded futures and the recorded return names its fact');

	for (const width of [1280, 390]) {
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
		const pg = await ctx.newPage();
		const off = [];
		pg.on('request', (r) => new URL(r.url()).origin !== new URL(base).origin && off.push(r.url()));
		await pg.goto(base + '#/markets');
		await pg.getByTestId('live-market-1').waitFor();
		expect(off.length === 0, `${OFF_SITE} (${width}) ${off.join(', ')}`);
		const card = await pg.getByTestId('live-market-1').innerText();
		expect(card.includes('Treasury') && card.includes('2026-10-27'), `NS11 ${width}: the card names the question`);
		expect((await pg.getByTestId('live-markets').innerText()).includes('test token with no value'), `${NO_VALUE} (${width})`);

		const seen = [];
		await pg.route('https://sepolia.base.org/**', rpc({ seen }));
		// MP3: the RPC field sits under "Use a different RPC".
		await pg.getByTestId('live-rpc-more').locator('summary').click();
		await pg.getByTestId('live-rpc').fill('https://sepolia.base.org/');
		await pg.getByTestId('live-read').click();
		await pg.getByTestId('live-buckets-1').waitFor({ timeout: 30000 });
		const b = await pg.getByTestId('live-buckets-1').innerText();
		expect(b.includes('Under $40.45T') && b.includes('$40.45T to $40.5T') && b.includes('$40.5T or more'), `NS11 ${width}: the buckets read from the chain's edges (${b.replace(/\s+/g, ' ').slice(0, 120)})`);
		expect(!b.includes('%') && b.includes('75 tONYX · 2 bets') && b.includes('30 tONYX · 1 bet'), `${SHARES} (${width})`);
		expect((await pg.getByTestId('live-markets').innerText()).includes('shows from 10 bets'), `NS11 ${width}: the page says when shares show`);
		await pg.getByTestId('product-terms-2').waitFor({ timeout: 30000 });
		const p1 = await pg.getByTestId('product-terms-1').innerText();
		const p2 = await pg.getByTestId('product-terms-2').innerText();
		expect(p1 === '100 tONYX if at most 0.0 °C' && p2 === '50 tONYX if at least 6 earthquakes', `NL18 ${width}: the policy terms come from the chain (${p1}; ${p2})`);
		const ps = await pg.getByTestId('product-state-1').innerText();
		expect(/^(Open for cover|No cover for sale: the pool has no free funds|Closed, waiting for the observed value)$/.test(ps), `NL18 ${width}: the product state comes from the chain and the reader's clock (${ps})`);
		expect((await pg.getByTestId('live-markets').innerText()).includes('basis risk'), `${BASIS} (${width})`);
		expect(seen.length > 0 && seen.every((m) => m === 'eth_chainId' || m === 'eth_blockNumber' || m === 'eth_call@0x30a1b2c'), `${READ_ONLY} (${width}) ${[...new Set(seen)].join(', ')}`);
		const stored = await pg.evaluate(() => JSON.stringify({ ...localStorage }) + JSON.stringify({ ...sessionStorage }) + location.href);
		expect(!stored.includes('sepolia.base.org'), `${NOT_STORED} (${width})`);
		expect((await pg.getByTestId('live-total-1').innerText()) === '125 tONYX' && (await pg.getByTestId('live-value-1').innerText()) === 'Not on chain yet', `NS11 ${width}: pool 125 tONYX, no value yet`);
		const state = await pg.getByTestId('live-state-1').innerText();
		expect(/^Trading (open|closed, waiting for the observed value)$/.test(state), `NS11 ${width}: the state comes from the chain and the reader's clock (${state})`);
		expect(off.every((u) => u.startsWith('https://sepolia.base.org')), `NS11 ${width}: the reads go only to the reader's RPC`);
		const g = await pg.getByTestId('live-market-g1').innerText();
		expect(/market 2/i.test(g) && g.includes('2026-11-02'), `NL23 ${width}: the guarded market shows as market 2 with its record date`);
		const gb = await pg.getByTestId('live-buckets-g1').innerText();
		expect(gb.includes('Under $40.6T') && gb.includes('$40.7T or more') && gb.includes('70 tONYX · 2 bets') && gb.includes('30 tONYX · 1 bet'), `NL23 ${width}: market 2's buckets and pools come from the guarded futures (${gb.replace(/\s+/g, ' ').slice(0, 120)})`);
		expect((await pg.getByTestId('live-total-g1').innerText()) === '120 tONYX', `NL23 ${width}: market 2's pool is 120 tONYX`);
		expect(!(await pg.getByTestId('live-markets').innerText()).includes('Refused'), `NL23 ${width}: nothing is refused with both futures read`);
		const over = await pg.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1);
		expect(!over, `NS11 ${width}: no sideways scroll`);

		// Another market's bytes under this title: refused.
		await pg.unroute('https://sepolia.base.org/**');
		await pg.route('https://sepolia.base.org/**', rpc({ market: MARKET_1.slice(0, 66) + '22'.repeat(32) + MARKET_1.slice(130) }));
		await pg.getByTestId('live-read').click();
		await pg.getByTestId('live-problems-1').waitFor({ timeout: 30000 });
		expect((await pg.getByTestId('live-problems-1').innerText()).includes('another fact'), `${OTHER_FACT} (${width})`);
		// A pool that holds back more than it has: refused.
		await pg.unroute('https://sepolia.base.org/**');
		await pg.route('https://sepolia.base.org/**', rpc({ pool: 10n ** 18n, reserved: 2n * 10n ** 18n }));
		await pg.getByTestId('live-read').click();
		await pg.getByTestId('ins-refused').waitFor({ timeout: 30000 });
		expect((await pg.getByTestId('ins-refused').innerText()).includes('exceed the pool'), `${UNCOVERED} (${width})`);
		// An answer over 64 KiB is refused before it is parsed (THREATS NL6-21).
		await pg.unroute('https://sepolia.base.org/**');
		await pg.route('https://sepolia.base.org/**', (route) => route.fulfill({ contentType: 'application/json', body: '{"jsonrpc":"2.0","id":1,"result":"0x' + '0'.repeat(70000) + '"}' }));
		await pg.getByTestId('live-read').click();
		await pg.getByTestId('live-error').waitFor({ timeout: 30000 });
		expect((await pg.getByTestId('live-error').innerText()).includes('over 64 KiB'), `${OVERSIZED} (${width})`);
		// Another chain: refused.
		await pg.unroute('https://sepolia.base.org/**');
		await pg.route('https://sepolia.base.org/**', rpc({ chain: '0x2105' }));
		await pg.getByTestId('live-read').click();
		await pg.getByTestId('live-refused').waitFor({ timeout: 30000 });
		expect((await pg.getByTestId('live-refused').innerText()).includes('Base Sepolia'), `${OTHER_CHAIN} (${width})`);
		await ctx.close();
	}
}
