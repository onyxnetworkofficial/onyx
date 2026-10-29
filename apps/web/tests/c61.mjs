// ROADMAP C61: the level-3 card on the Data page, in a real browser. Called by tests/e2e.mjs.
// No live book is deployed: the card must say so, show only the recorded rehearsal answer
// (labelled rehearsal), checked by the Rust verifier against its pinned chain answers. A forged
// value, an edited record and another chain must be refused; the reader's own RPC read is checked
// by the verifier too (here a fake RPC on the test server's origin: no outside network). Figures
// are compared with the shipped file, never literals.
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { settled, WAIT_CAP } from './wait.mjs';

const FILE = '**/level3/signed-rehearsal.json';

async function open(browser, base, doc) {
	const pg = await browser.newPage();
	if (doc) await pg.route(FILE, (r) => r.fulfill({ json: doc }));
	await pg.goto(base + '#/data');
	await settled(pg);
	const s = pg.getByTestId('level3');
	await s.scrollIntoViewIfNeeded();
	// Data review: the section is folded by default.
	await pg.getByTestId('level3-fold').locator('> summary').click();
	return { pg, s };
}

/** A JSON-RPC answer for each method, from `answers`. */
function fakeRpc(pg, url, answers) {
	const seen = [];
	return pg
		.route(url, async (r) => {
			const req = JSON.parse(r.request().postData() ?? '{}');
			seen.push({ method: req.method, params: req.params, ua: r.request().headers()['user-agent'] ?? '' });
			const result = typeof answers[req.method] === 'function' ? answers[req.method](req.params) : answers[req.method];
			await r.fulfill({ json: { jsonrpc: '2.0', id: req.id, result: result ?? null } });
		})
		.then(() => seen);
}

export async function c61(browser, base, root, expect) {
	const shipped = JSON.parse(await readFile(join(root, 'level3/signed-rehearsal.json'), 'utf8'));
	const want = Object.values(shipped.body.value)[0];
	{
		const { pg, s } = await open(browser, base);
		// Data review: the checked rehearsal's badge is a neutral "Rehearsal".
		await s.locator('.pill', { hasText: /^Rehearsal$/ }).waitFor({ timeout: 120000 });
		const none = await pg.getByTestId('level3-norecord').innerText();
		expect(/No live level-3 book yet/.test(none) && /not deployed on Base Sepolia/.test(none), 'C61 level 3: the section says no live book is deployed and why');
		const value = (await pg.getByTestId('level3-value').innerText()).replace(/,/g, '');
		const tag = (await pg.getByTestId('level3-tag').innerText()).toLowerCase();
		expect(value === want && tag.includes('rehearsal'), `C61 level 3: the card shows the recorded value (${want}) labelled rehearsal`);
		const book = await pg.getByTestId('level3-book').innerText();
		const copy = await pg.getByTestId('level3-copy').innerText();
		expect(
			/chain 31337/.test(book) && /rehearsal/.test(book) && /settleable/.test(copy) && /recorded in the rehearsal/.test(copy),
			'C61 level 3: the card names the local rehearsal chain and says the chain answers are recorded'
		);
		await pg.getByTestId('level3-forge').click();
		const forged = await pg.getByTestId('level3-forged').innerText();
		expect(/^Refused: /.test(forged), `C61 level 3: a forged value is refused (${forged.slice(0, 90)})`);

		// The reader's own RPC: a public Base Sepolia RPC answers chain 84532, which is not the
		// rehearsal's chain: refused, and only the chain id was asked.
		const rpc = base + 'rpc-sepolia';
		const seen = await fakeRpc(pg, rpc, { eth_chainId: '0x14a34', eth_getBlockByNumber: { number: '0x10', hash: '0x' + '11'.repeat(32), timestamp: '0x10' } });
		await pg.getByTestId('level3-rpc').fill(rpc);
		await pg.getByTestId('level3-read').click();
		await pg.getByTestId('level3-live').waitFor({ timeout: WAIT_CAP });
		const sep = await pg.getByTestId('level3-live').innerText();
		expect(
			/your RPC is on chain 84532/.test(sep) && /local chain/.test(sep) && !seen.some((x) => x.method === 'eth_call' || x.method === 'eth_getCode'),
			'C61 level 3: a Base Sepolia RPC is refused on the chain id (the rehearsal is a local chain); no book read is made there'
		);
		expect(seen.every((x) => !/@/.test(x.ua) && !/@/.test(JSON.stringify(x.params))), 'C61 level 3: the RPC requests carry no personal identifier');

		// A fresh finalized block on the rehearsal's chain with the recorded book answers: checks out,
		// every read made at the finalized block's hash. An old block: refused as stale.
		const r = shipped.reads;
		for (const [label, ts, ok] of [
			['fresh', () => '0x' + Math.floor(Date.now() / 1000).toString(16), true],
			// Older than the 3,600 s the client takes (the recorded block's own time is the rehearsal's
			// chain clock, 90 minutes ahead of the host's, so it is not used here).
			['stale', () => '0x' + (Math.floor(Date.now() / 1000) - 4000).toString(16), false]
		]) {
			const url = base + `rpc-${label}`;
			// The page sends settleable first, then factView (SignedFact.svelte); this fake answers in
			// that order, and the verifier refuses the result if they were swapped.
			let n = 0;
			const calls = await fakeRpc(pg, url, {
				eth_chainId: r.chainId,
				eth_getBlockByNumber: () => ({ ...r.finalized, timestamp: ts() }),
				eth_getCode: r.code,
				eth_call: () => (n++ % 2 === 0 ? r.settleable : r.factView)
			});
			await pg.getByTestId('level3-rpc').fill(url);
			await pg.getByTestId('level3-read').click();
			await pg.waitForFunction((want) => document.querySelector('[data-testid="level3-live"]')?.textContent?.includes(want), ok ? 'says the book settles' : 'stale or stuck RPC', { timeout: WAIT_CAP });
			const atHash = calls.filter((x) => x.method === 'eth_call' || x.method === 'eth_getCode').every((x) => x.params[1]?.blockHash === r.finalized.hash);
			expect(atHash && calls.length === 5, `C61 level 3: ${label} block: ${ok ? 'checks out' : 'refused as stale'}, every book read at the finalized block's hash`);
			if (ok) {
				// Review C61-R2-2: a "live" read that agrees is one RPC answering for the rehearsal's local
				// chain (here made-up answers with a fresh timestamp): never shown as a live value.
				const said = await pg.getByTestId('level3-live').innerText();
				const livePill = (await s.locator('.rpcrow .pill').innerText()).trim();
				expect(
					(await pg.getByTestId('level3-live-local').count()) === 1 && /not a live level-3 value/.test(said) && /takes that\s+RPC's word/.test(said) && livePill === 'Your RPC agrees',
					`C61 level 3: an RPC agreeing on the rehearsal's chain is labelled local, one RPC's word, not live (pill "${livePill}")`
				);
			}
		}
		await pg.close();
	}
	{
		// An edited record (another value in the body): nothing is shown.
		const doc = JSON.parse(JSON.stringify(shipped));
		doc.body.value = { uint: String(BigInt(want) + 1n) };
		const { pg, s } = await open(browser, base, doc);
		await s.locator('.pill.bad').first().waitFor({ timeout: 120000 });
		expect(
			(await pg.getByTestId('level3-failed').count()) === 1 && (await pg.getByTestId('level3-card').count()) === 0,
			'C61 level 3: an edited record shows nothing'
		);
		await pg.close();
	}
}
