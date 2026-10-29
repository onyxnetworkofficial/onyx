// Waits on real page state, never on a fixed sleep (ROADMAP E9/E10). On a machine at load
// 25-60 a fixed sleep or a "no skeleton yet" test passes before the page has drawn: SvelteKit
// loads each hash route's code on demand, so right after goto() the old route can still be on
// screen, with no skeleton and none of the new page's charts. These helpers wait for the state
// the next check reads and give up only at a cap far above any healthy run; a timeout here
// throws (it is never swallowed), so a page that does not settle fails loudly.

/** Caps for waits that name no timeout of their own (ROADMAP E12). Playwright's default, 30 s, is a
 * guess at the machine's speed: e2e's first page wait ran out at load 33 and a Home click's wait at load
 * 93, both on pages that drew a little later. These are as far above a healthy run as the waits below;
 * a page that never gets there still fails, at the cap. */
export const WAIT_CAP = 120000;
export const NAV_CAP = 120000;

/** Give every page and context made from `browser` the caps above as its defaults (call once, right
 * after launch). Explicit `{ timeout }`s keep their own value. */
export function capWaits(browser) {
	const cap = (x) => (x.setDefaultTimeout(WAIT_CAP), x.setDefaultNavigationTimeout(NAV_CAP), x);
	const newContext = browser.newContext.bind(browser);
	const newPage = browser.newPage.bind(browser);
	browser.newContext = async (...a) => cap(await newContext(...a));
	browser.newPage = async (...a) => cap(await newPage(...a));
	return browser;
}

/** The route in the address bar has drawn: the full footer page list marks it current (set by SvelteKit once the
 * new page is rendered), no loading placeholder (`.sk`) and no running check (`.pill.run`) is
 * left, and the DOM then stays unchanged for `quiet` ms (count-ups, late sections and focus
 * reset are done, so nothing moves under the next click or measurement). */
export async function settled(pg, { timeout = 240000, quiet = 500 } = {}) {
	await routeDrawn(pg, { timeout, idle: true });
	await quietDom(pg, { timeout, quiet });
}

/** The route in the address bar has drawn (the footer marks it current and its h1 is there); with
 * `idle`, also no loading placeholder and no running check is left. */
export async function routeDrawn(pg, { timeout = 120000, idle = false } = {}) {
	await pg
		.waitForFunction(
			(idle) => {
				const a = document.querySelector('[data-testid="footer-nav"] a[aria-current="page"]');
				const here = (location.hash || '#/').replace(/^#$/, '#/');
				return !!a && (a.hash || '#/') === here && !!document.querySelector('main h1') && !(idle && document.querySelector('.sk, .pill.run'));
			},
			idle,
			{ timeout, polling: 100 }
		)
		.catch(async (e) => {
			// Say what the page showed instead, so a timeout names its cause.
			const seen = await pg
				.evaluate(() => ({
					hash: location.hash,
					readyState: document.readyState,
					current: document.querySelector('[data-testid="footer-nav"] a[aria-current="page"]')?.getAttribute('href') ?? null,
					h1: document.querySelector('h1')?.textContent?.trim() ?? null,
					waiting: [...document.querySelectorAll('.sk, .pill.run')].map((x) => x.getAttribute('aria-label') || x.textContent.trim()).slice(0, 4),
					body: document.body?.innerText.slice(0, 160).replace(/\s+/g, ' ') ?? ''
				}))
				.catch((x) => String(x));
			throw new Error(`${pg.url()} not drawn in ${timeout} ms: ${JSON.stringify(seen)}; ${await pageLog(pg)} (${e.message.split('\n')[0]})`);
		});
}

/** What the page logged and what it is still loading: its errors (a route chunk that failed to
 * load leaves the old route on screen), console errors, and requests that failed or never got a
 * response. For a wait's error message (E10 review: the one public "not drawn" stop said only
 * what was on screen). */
export async function pageLog(pg) {
	// Never waits on the page: request.response() waits for a response that may never come
	// (an open request hung the first version of this), so each gets 200 ms and the whole log 10 s.
	const cap = (ms, v) => new Promise((ok) => setTimeout(() => ok(v), ms));
	const log = async () => {
		const errors = (await pg.pageErrors()).map((e) => String(e).slice(0, 200));
		const consoleErrors = (await pg.consoleMessages()).filter((m) => m.type() === 'error').map((m) => m.text().slice(0, 200));
		const pending = [];
		for (const r of await pg.requests()) {
			const failed = r.failure()?.errorText;
			if (failed) pending.push(`${new URL(r.url()).pathname} failed: ${failed}`);
			else if ((await Promise.race([r.response(), cap(200, 'open')])) === 'open') pending.push(`${new URL(r.url()).pathname} open`);
		}
		return `page errors ${JSON.stringify(errors.slice(-4))}, console errors ${JSON.stringify(consoleErrors.slice(-4))}, requests failed or open ${JSON.stringify(pending.slice(-6))}`;
	};
	return Promise.race([log(), cap(10000, 'page log: no answer in 10 s')]).catch((x) => `page log unavailable: ${String(x).split('\n')[0]}`);
}

/** Resolves once the DOM has not changed for `quiet` ms; throws after `timeout` ms. */
export async function quietDom(pg, { timeout = 60000, quiet = 500 } = {}) {
	const ok = await pg.evaluate(
		([quiet, timeout]) =>
			new Promise((done) => {
				let t;
				const mo = new MutationObserver(() => arm());
				const cap = setTimeout(() => (mo.disconnect(), clearTimeout(t), done(false)), timeout);
				const arm = () => {
					clearTimeout(t);
					t = setTimeout(() => (mo.disconnect(), clearTimeout(cap), done(true)), quiet);
				};
				mo.observe(document.documentElement, { subtree: true, childList: true, attributes: true, characterData: true });
				arm();
			}),
		[quiet, timeout]
	);
	if (!ok) throw new Error(`the page at ${pg.url()} never went ${quiet} ms without a DOM change in ${timeout} ms`);
}

/** The overview console has finished a run: every check is passed or failed and the button is
 * enabled again. (The old wait, /7 passed|failed · / on the tally, matched the idle tally
 * "0 passed · 0 failed · ..." too, so it returned before the checks had run.) */
export async function checksDone(pg, { timeout = 240000 } = {}) {
	await pg
		.waitForFunction(
			() => {
				const m = /^(\d+) passed · (\d+) failed/.exec(document.querySelector('.tally')?.textContent ?? '');
				const total = document.querySelectorAll('.console ol.checks > li').length;
				const btn = document.querySelector('.console button');
				return !!m && total > 0 && +m[1] + +m[2] === total && !!btn && !btn.disabled;
			},
			null,
			{ timeout, polling: 100 }
		)
		.catch(async (e) => {
			// A check that never ends fails here, naming each check's state (E10 review).
			const seen = await pg
				.evaluate(() => ({
					tally: document.querySelector('.tally')?.textContent ?? null,
					button: document.querySelector('.console button')?.textContent?.trim() ?? null,
					checks: [...document.querySelectorAll('.console ol.checks > li')].map((li) => `${li.querySelector('.ct > span')?.textContent ?? '?'}: ${li.className}`)
				}))
				.catch((x) => String(x));
			throw new Error(`${pg.url()}: the checks did not finish in ${timeout} ms: ${JSON.stringify(seen)}; ${await pageLog(pg)} (${e.message.split('\n')[0]})`);
		});
}

/** Wait until `locator`'s `name` attribute is `value` (the state a click should have set). */
export async function attr(locator, name, value, { timeout = 60000 } = {}) {
	await locator.evaluate(
		(e, [name, value, timeout]) =>
			new Promise((ok, fail) => {
				if (e.getAttribute(name) === value) return ok();
				const cap = setTimeout(() => (mo.disconnect(), fail(new Error(`${name} stayed ${e.getAttribute(name)}, not ${value}`))), timeout);
				const mo = new MutationObserver(() => e.getAttribute(name) === value && (mo.disconnect(), clearTimeout(cap), ok()));
				mo.observe(e, { attributes: true, attributeFilter: [name] });
			}),
		[name, value, timeout]
	);
}
