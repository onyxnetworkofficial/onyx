// U1: historical "home" verification assertions now target #/how-it-works. Keep their
// names stable for THREATS citations; tests/product.mjs covers the new contributor homepage.
// W4 speed (docs/website-plan.md), in a real browser. Called by tests/e2e.mjs.
// Heavy files load only where and when a page needs them: the verifier WASM (1.8 MiB gzip),
// the private preview's recipe input (1.1 MiB gzip) and the history week files. The home page
// and the API page load none of them until the reader scrolls to a checked value or presses a
// button; the WASM is fetched once per visit, from a content-hashed (cacheable) path; the slab's
// shader runs in a worker; the data files ship without indentation.
import { readdir, readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import { checksDone, routeDrawn, WAIT_CAP } from './wait.mjs';

const HEAVY = { wasm: /onyx_web_verify_bg-[^/]*\.wasm$/, recipe: /\/data\/recipe\.wasm$/, verifier: /verifier\.worker[^/]*\.js$/, input: /input-[^/]*\.b64\.txt$/, history: /\/history\/week\// };
const heavy = (urls) => Object.entries(HEAVY).filter(([, re]) => urls.some((u) => re.test(u))).map(([k]) => k);

/** The page's idle-time loads have started (pages that need the verifier start it when the
 * browser is idle, after the fonts): the fonts, one idle period of our own, then a quiet network. */
async function idleLoads(p) {
	// page.evaluate has no timeout of its own: the fonts get 60 s, then this fails (E10 review).
	// WAIT_CAP lives in Node: the browser function gets it as its argument.
	const fonts = await p.evaluate((cap) =>
		Promise.race([
			document.fonts.ready.then(() => new Promise((ok) => (window.requestIdleCallback ? requestIdleCallback(() => ok(true), { timeout: cap }) : setTimeout(() => ok(true), 200)))),
			new Promise((ok) => setTimeout(() => ok(false), 60000))
		]), WAIT_CAP
	);
	if (!fonts) throw new Error(`${p.url()}: the fonts did not load in 60 s`);
	await p.waitForLoadState('networkidle').catch(() => {});
}

/** Open `hash` in a fresh context (cold cache) and record every request path. */
async function visit(browser, base, hash, size = { width: 1280, height: 800 }) {
	const ctx = await browser.newContext({ viewport: size });
	const p = await ctx.newPage();
	const urls = [];
	ctx.on('request', (r) => urls.push(new URL(r.url()).pathname));
	await p.goto(base + (hash || '#/how-it-works'));
	// E10: the route has drawn (its code loads on demand, which a loaded machine slows a lot).
	await routeDrawn(p);
	await idleLoads(p);
	// The window the "nothing heavy loads" checks look across (the W1 audit's first-visit wait,
	// shortened): a check that something does not happen needs one; it is not a wait for the page.
	await p.waitForTimeout(2500);
	return { ctx, p, urls };
}

export async function w4(browser, base, root, expect) {
	const demo = JSON.parse(await readFile(join(root, 'data/demo.json'), 'utf8'));
	const PUB = demo.public === true;
	// ROADMAP L14: a rights-fallback build runs 5 checks (no company rows), shows no API sample
	// and ships no history week files.
	const FB = demo.rights === 'fallback';
	const N = FB ? 5 : 7;

	// 1. Home: the first screen (desktop and phone) fetches no verifier, input or history.
	for (const size of [{ width: 1280, height: 800 }, { width: 390, height: 844 }]) {
		const { ctx, p, urls } = await visit(browser, base, '', size);
		await p.getByTestId('proof-run').waitFor();
		const h = heavy(urls);
		expect(h.length === 0, `W4 home ${size.width} px: first visit loads no verifier, input or history (${urls.length} requests${h.length ? '; got ' + h.join(', ') : ''})`);
		// The stats still say "Loading…" (not a made-up number) until the verifier has run, and say they are loading.
		const live = await p.locator('.stats .stat').first().locator('b').evaluate((b) => [b.querySelector('[aria-hidden="true"]')?.textContent, b.querySelector('.sr-only')?.textContent]);
		expect(live[0] === 'Loading…' && live[1] === 'Loading the live block count', `W4 home ${size.width} px: the pinned figures wait for the verifier ("${live.join('" / "')}")`);
		if (size.width === 1280) {
			// Scrolling to the figures loads the verifier and fills them from it.
			await p.locator('.stats').scrollIntoViewIfNeeded();
			const filled = await p
				.waitForFunction((want) => document.querySelector('.stats .stat b')?.textContent?.replace(/\D/g, '') === want, String(demo.live.blocks.length), { timeout: 120000 })
				.then(() => true, () => false);
			const wasm = urls.filter((u) => HEAVY.wasm.test(u));
			expect(filled && wasm.length === 1, `W4 home: scrolled to the figures, the verifier loads once (${wasm.length} WASM request) and fills them (${demo.live.blocks.length} live blocks)`);
			expect(PUB ? !urls.some((u) => HEAVY.input.test(u)) : urls.some((u) => HEAVY.input.test(u)), PUB ? 'W4 public home: still no recipe input after scrolling' : 'W4 home: the label count decodes the input once the figures are on screen');
		}
		await ctx.close();
	}
	// 2. Home: the Verify button alone loads the verifier and runs the checks.
	{
		const { ctx, p, urls } = await visit(browser, base, '');
		await p.waitForFunction(() => !document.querySelector('[data-testid="proof-verify"] button')?.disabled, null, { timeout: WAIT_CAP });
		const before = heavy(urls);
		await p.getByTestId('proof-verify').getByRole('button').click();
		await checksDone(p);
		const wasm = urls.filter((u) => HEAVY.wasm.test(u));
		expect(
			before.length === 0 && wasm.length === 1 && (await p.locator('.tally').innerText()).startsWith(`${N} passed`),
			`W4 home: nothing heavy before the press; "Verify" loads the verifier once (${wasm.length} WASM request) and ${N} of ${N} pass`
		);
		await ctx.close();
	}
	// 3. API: the sample shows from the demo file; the verifier loads on the button. A fallback
	// build has no sample (no company row), so nothing heavy loads at all.
	if (FB) {
		const { ctx, p, urls } = await visit(browser, base, '#/api');
		const sample = await p.getByRole('region', { name: 'Sample response' }).count();
		const h = heavy(urls);
		expect(sample === 0 && h.length === 0, `W4 fallback api: no sample and nothing heavy loads (${h.join(', ') || 'none'})`);
		await ctx.close();
	} else {
		const { ctx, p, urls } = await visit(browser, base, '#/api');
		const sample = await p.getByRole('region', { name: 'Sample response' }).isVisible();
		const h = heavy(urls);
		expect(sample && h.length === 0, `W4 api: the sample shows and nothing heavy loads before the button (${h.join(', ') || 'none'})`);
		await p.getByRole('button', { name: 'Check the proof against the pinned root' }).click();
		const ok = await p.getByText('Valid against the pinned root').waitFor({ timeout: 120000 }).then(() => true, () => false);
		expect(ok && urls.filter((u) => HEAVY.wasm.test(u)).length === 1, 'W4 api: the button loads the verifier once and the proof checks');
		await ctx.close();
	}
	// 4. Every page: history week files only on Data, the input only where its rows are shown
	// (private Explorer and Labels; never in the public build), no WASM on Run.
	{
		const want = {
			'#/data': FB ? ['wasm', 'verifier'] : ['wasm', 'verifier', 'history'],
			'#/reserves': FB ? [] : ['wasm', 'verifier'],
			'#/run': [],
			'#/explorer': PUB ? ['wasm', 'verifier'] : ['wasm', 'verifier', 'input'],
			'#/labels': PUB ? ['wasm', 'verifier'] : ['wasm', 'verifier', 'input']
		};
		for (const [hash, kinds] of Object.entries(want)) {
			const { ctx, urls } = await visit(browser, base, hash);
			const got = heavy(urls);
			const wasm = urls.filter((u) => HEAVY.wasm.test(u)).length;
			expect(
				got.join() === kinds.join() && wasm === (kinds.includes('wasm') ? 1 : 0),
				`W4 ${hash}: first visit loads ${kinds.join(', ') || 'nothing heavy'} (got ${got.join(', ') || 'nothing heavy'}; ${wasm} WASM request)`
			);
			await ctx.close();
		}
	}
	// 5. One verifier per visit: moving between pages through the site's links reuses it (one
	// WASM request in all), and the WASM sits under app/immutable with a content hash in its
	// name (cacheable forever).
	{
		const { ctx, p, urls } = await visit(browser, base, '#/data');
		const go = (hash) => p.locator(`[data-testid="footer-nav"] a[href="${hash || '#/how-it-works'}"]`).first().click();
		for (const hash of ['#/reserves', '#/labels', '#/explorer', '#/api', '']) {
			await go(hash);
			// Each page has drawn and started its verifier work (it was a fixed 1.5 s).
			await routeDrawn(p);
			await idleLoads(p);
		}
		await go('#/reserves');
		await p.getByTestId('reserve-live').waitFor({ timeout: WAIT_CAP }).catch(() => {});
		const wasm = urls.filter((u) => HEAVY.wasm.test(u));
		expect(
			wasm.length === 1 && /^\/app\/immutable\/workers\/assets\/onyx_web_verify_bg-[A-Za-z0-9_-]{6,}\.wasm$/.test(wasm[0]),
			`W4: six page changes in one visit fetch the verifier WASM once (${wasm.length}), from a content-hashed path (${wasm[0]})`
		);
		await ctx.close();
	}
	// 6. The slab's stone is drawn in a worker: the page's canvas was handed over (it gives no
	// context on the main thread any more), on every page.
	{
		const ctx = await browser.newContext({ viewport: { width: 1280, height: 800 } });
		const p = await ctx.newPage();
		const workers = [];
		p.on('worker', (w) => workers.push(w.url()));
		let all = true;
		for (const [i, hash] of ['', '#/data', '#/run'].entries()) {
			await p.goto(base + (hash || '#/how-it-works'));
			await p.locator('.slab canvas').waitFor({ state: 'attached' });
			// E12: this page's slab has handed its canvas over once its stone worker starts (the hand-over
			// comes first, in OnyxSlab's onMount); it was a fixed 300 ms, read on the old page under load.
			await routeDrawn(p);
			for (const t0 = Date.now(); workers.filter((u) => /stone/.test(u)).length <= i && Date.now() - t0 < WAIT_CAP; ) await p.waitForTimeout(50);
			const handed = await p.locator('.slab canvas').evaluate((c) => {
				try {
					c.getContext('2d');
					return false;
				} catch {
					return true;
				}
			});
			all &&= handed;
		}
		expect(all && workers.some((u) => /stone\.worker/.test(u)), `W4: the slab's canvas is handed to a worker on Home, Data and Run (${workers.filter((u) => /stone/.test(u)).length} stone workers)`);
		await ctx.close();
	}
	// 9. Review: the market says "checked" only after the live-root check passed in this
	// browser (decoding the OKX row is not a check): "Not yet checked" before Verify, "Checked
	// against the pinned root" after, and a failure with a changed live row. A fallback build has
	// no market (ROADMAP L14).
	if (!FB) {
		const { ctx, p } = await visit(browser, base, '');
		await p.locator('.market').scrollIntoViewIfNeeded();
		const mc = p.getByTestId('market-check');
		await mc.waitFor({ timeout: 120000 }).catch(() => {});
		const before = (await mc.innerText().catch(() => '')).trim();
		await p.getByTestId('proof-verify').getByRole('button').click();
		await checksDone(p);
		const after = (await mc.innerText()).trim();
		expect(before === 'Not yet checked' && after === 'Checked against the pinned root', `W4 review: the market row says "${before}" before Verify and "${after}" after`);
		await ctx.close();
		const forged = await browser.newContext({ viewport: { width: 1280, height: 800 } });
		await forged.route('**/data/demo.json', async (route) => {
			const d = await (await route.fetch()).json();
			const r = d.live.rows.find((x) => x.entity === 'entity:okx');
			// Flip the lowest bit of net_sats (the 25th byte from the end): still a valid row.
			const b = r.leaf.slice(2);
			const i = b.length - 50;
			r.leaf = '0x' + b.slice(0, i) + (parseInt(b.slice(i, i + 2), 16) ^ 1).toString(16).padStart(2, '0') + b.slice(i + 2);
			await route.fulfill({ json: d });
		});
		const fp = await forged.newPage();
		await fp.goto(base + '#/how-it-works');
		await fp.waitForFunction(() => !document.querySelector('[data-testid="proof-verify"] button')?.disabled, null, { timeout: WAIT_CAP });
		await fp.getByTestId('proof-verify').getByRole('button').click();
		await checksDone(fp);
		await fp.locator('.market').scrollIntoViewIfNeeded();
		const forgedText = (await fp.getByTestId('market-check').innerText({ timeout: WAIT_CAP }).catch(() => '')).trim();
		expect(forgedText.startsWith('Check failed'), `W4 review: with one live OKX row changed, the market says "${forgedText}"`);
		await forged.close();
	}
	// 10. Review: until filled, every lazy figure, the market answer and the flow chart slot carry
	// a "Loading ..." name (the dash is hidden from screen readers); axe finds nothing there.
	{
		const { ctx, p } = await visit(browser, base, '');
		if (!FB) await p.locator('.flowslot').waitFor({ state: 'attached', timeout: WAIT_CAP }).catch(() => {});
		const names = await p.evaluate(() =>
			[...document.querySelectorAll('.stats .stat b, .fuel b, .mq .q, .mres .verdict, .flowslot')].map((e) => [e.className || e.tagName, e.querySelector('.sr-only')?.textContent ?? '', e.querySelector('[aria-hidden="true"]') !== null || e.classList.contains('flowslot')])
		);
		const unfilled = names.filter(([, , dash]) => dash);
		const bad = unfilled.filter(([, n]) => !/^\(?[Ll]oading /.test(n));
		const busy = FB ? null : await p.locator('.flowslot').getAttribute('aria-busy');
		// A fallback build has no OKX figure, market or flow chart slot: only the verifier's counts
		// (live blocks, labels) wait; the vector count comes from demo.json.
		expect(
			unfilled.length >= (FB ? 2 : 6) && bad.length === 0 && (FB ? (await p.locator('.flowslot').count()) === 0 : busy === 'true'),
			`W4 review: ${unfilled.length} placeholders before load each have a loading name (${unfilled.map(([, n]) => n).join('; ')}), ${FB ? 'no chart slot (fallback)' : 'the chart slot is aria-busy'}`
		);
		const axeSrc = await readFile(createRequire(import.meta.url).resolve('axe-core/axe.min.js'), 'utf8');
		await p.addScriptTag({ content: axeSrc });
		const v = await p.evaluate(async () => {
			const r = await window.axe.run({ include: [['.stats'], ['.market'], ['.preview'], ['.bento']] }, { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa', 'best-practice'] } });
			return r.violations.map((x) => `${x.id} (${x.nodes.length})`);
		});
		expect(v.length === 0, `W4 review: axe on the figures, market and live chart before they load: ${v.length ? v.join(', ') : 'no violations'}`);
		await ctx.close();
	}
	// 11. The slab: without OffscreenCanvas the stone runs on the page (no worker); when the
	// worker fails, the page draws it on a fresh canvas; in the worker, pausing stops the frames.
	{
		const info = (pg) => pg.locator('.slab canvas').evaluate((c) => c.__stone?.info() ?? null);
		const noOff = await browser.newContext({ viewport: { width: 1280, height: 800 } });
		await noOff.addInitScript(() => {
			delete HTMLCanvasElement.prototype.transferControlToOffscreen;
		});
		const np = await noOff.newPage();
		const nw = [];
		np.on('worker', (w) => nw.push(w.url()));
		await np.goto(base + '#/how-it-works');
		await np.locator('.slab canvas').waitFor({ state: 'attached' });
		// E12: the frame the check needs, waited for (it was a fixed 500 ms).
		await np.waitForFunction(() => (document.querySelector('.slab canvas')?.__stone?.info().frames ?? 0) >= 1, null, { timeout: WAIT_CAP }).catch(() => {});
		const ni = await info(np);
		expect(ni && ni.frames >= 1 && !nw.some((u) => /stone/.test(u)), `W4 review: without OffscreenCanvas the stone is drawn on the page (${ni?.frames} frame(s), no stone worker)`);
		await noOff.close();

		const broken = await browser.newContext({ viewport: { width: 1280, height: 800 } });
		await broken.route('**/stone.worker-*.js', (r) => r.fulfill({ contentType: 'text/javascript', body: 'throw new Error("stone worker test failure");' }));
		const bp = await broken.newPage();
		const perr = [];
		bp.on('pageerror', (e) => perr.push(String(e)));
		await bp.goto(base + '#/how-it-works');
		const fell = await bp
			.waitForFunction(() => !!document.querySelector('.slab canvas')?.__stone, null, { timeout: WAIT_CAP })
			.then(() => true, () => false);
		const bi = fell ? await info(bp) : null;
		const canvases = await bp.locator('.slab canvas').count();
		expect(fell && bi.frames >= 1 && canvases === 1 && perr.length === 0, `W4 review: when the stone worker fails, the page draws the stone on a fresh canvas (${bi?.frames} frame(s), ${canvases} canvas, ${perr.length} page errors)`);
		await broken.close();

		const anim = await browser.newContext({ viewport: { width: 1280, height: 800 }, reducedMotion: 'no-preference' });
		await anim.addInitScript(() => {
			try {
				localStorage.setItem('onyx-stone-test', 'animate');
				localStorage.removeItem('onyx-motion');
			} catch {}
		});
		const ap = await anim.newPage();
		const stoneWorker = ap.waitForEvent('worker', { predicate: (w) => /stone/.test(w.url()), timeout: WAIT_CAP });
		await ap.goto(base + '#/how-it-works');
		const w = await stoneWorker;
		await w.evaluate(() => new Promise((r) => { const t = () => (self.__stone ? r() : setTimeout(t, 50)); t(); }));
		const wi = () => w.evaluate(() => self.__stone.info());
		// Frames advance (polled up to wait.mjs's cap, E12; it was 20 s: a loaded machine starts the loop late).
		const advances = async (from) => {
			for (const t0 = Date.now(); Date.now() - t0 < WAIT_CAP; ) {
				const x = await wi();
				if (x.frames > from) return x;
				await ap.waitForTimeout(250);
			}
			return wi();
		};
		const a0 = await wi();
		const a1 = await advances(a0.frames);
		await ap.getByTestId('motion-button').click();
		// E12: the pause has reached the worker (it was a fixed 300 ms) ...
		for (const t0 = Date.now(); Date.now() - t0 < WAIT_CAP; await ap.waitForTimeout(50)) {
			const x = await wi();
			if (x.motionOff && !x.scheduled) break;
		}
		const p0 = await wi();
		// ... then a window in which no frame may be drawn (a check that something does not happen needs one).
		await ap.waitForTimeout(800);
		const p1 = await wi();
		await ap.getByTestId('motion-button').click();
		const r1 = await advances(p1.frames);
		expect(
			a1.frames > a0.frames && a1.scheduled && p0.motionOff && !p1.scheduled && p1.frames === p0.frames && !r1.motionOff && r1.frames > p1.frames,
			`W4 review: in the worker the stone drifts (${a0.frames} -> ${a1.frames} frames), the pause button stops it (${p0.frames} -> ${p1.frames}, none scheduled) and play starts it again (${r1.frames})`
		);
		await anim.close();
	}
	// 7. run/heads.json holds, for each run the site lists (src/lib/runs.ts, in its order), the
	// fields the home page reads, equal to the run files'; the home page reads it, not the files.
	{
		const listed = [...(await readFile(new URL('../src/lib/runs.ts', import.meta.url), 'utf8')).matchAll(/'(run\/base-sepolia-[0-9-]+\.json)'/g)].map((m) => m[1]);
		const heads = JSON.parse(await readFile(join(root, 'run/heads.json'), 'utf8'));
		const same = await Promise.all(
			listed.map(async (f, i) => {
				const d = JSON.parse(await readFile(join(root, f), 'utf8'));
				const h = heads[i];
				return h && h.file === f && h.verdict === d.verdict && h.network === d.network && h.date === d.date && h.totals.sent_txs === d.totals.sent_txs && !!h.reserve === !!d.reserve && (!d.reserve || h.reserve.round === d.reserve.round);
			})
		);
		expect(listed.length >= 3 && heads.length === listed.length && same.every(Boolean), `W4: run/heads.json equals the ${listed.length} listed run files on every field the home page reads`);
		const { ctx, p, urls } = await visit(browser, base, '');
		await p.getByTestId('testnet-headline').waitFor({ timeout: WAIT_CAP });
		expect(urls.includes('/run/heads.json') && !urls.some((u) => /\/run\/base-sepolia-/.test(u)), 'W4 home: the proof card reads run/heads.json, not the full run files');
		await ctx.close();
	}
	// 8. Data files ship without indentation (values unchanged: build-web.sh refuses otherwise,
	// and every check above ran on these files).
	{
		const files = [];
		for (const dir of ['data', 'history', 'history/week', 'level2', 'run'])
			for (const f of await readdir(join(root, dir)).catch(() => [])) if (f.endsWith('.json')) files.push(`${dir}/${f}`);
		const loose = [];
		for (const f of files) if (/\n[ \t]/.test(await readFile(join(root, f), 'utf8'))) loose.push(f);
		// A fallback build ships 9 fewer (no history/week index and 7 days, no reserves-week.json).
		expect(files.length >= (FB ? 6 : 15) && loose.length === 0, `W4: ${files.length} data files ship without indentation${loose.length ? ' (not: ' + loose.join(', ') + ')' : ''}`);
	}
}
