// Product U1 (READINESS A19): the home page speaks to a data user first, then to a contributor;
// progressive technical detail and no surprise data loading.
import { readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { settled, WAIT_CAP } from './wait.mjs';

// Click a link that changes the route, wait for that route, then for the page to settle. On a
// loaded machine (other work on the box, the suites in lanes) the old page could count as settled
// before the click had changed the route; two gated checks failed so on 2026-11-02.
async function go(pg, link, hashes) {
	await link.click();
	await pg.waitForURL((u) => hashes.includes(new URL(u).hash), { timeout: WAIT_CAP }).catch(() => {});
	await settled(pg);
}
const HOME = ['#/', ''];

export async function product(browser, base, root, expect) {
	const axeSrc = await readFile(createRequire(import.meta.url).resolve('axe-core/axe.min.js'), 'utf8');
	for (const width of [1280, 390]) {
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
		const pg = await ctx.newPage();
		const requests = [];
		const errors = [];
		pg.on('request', (r) => requests.push({ path: new URL(r.url()).pathname, method: r.method() }));
		pg.on('pageerror', (e) => errors.push(String(e)));
		// Home asks the site's node for its two dataset counts (all, and updated in the last 30
		// days): the node is faked here; this run's server holds the built files only.
		await pg.route('**/v1/bulk?*', (r) => r.fulfill({ json: { datasets: [], total: new URL(r.request().url()).searchParams.get('fresh') ? 6302 : 9054, next: null } }));
		await pg.goto(base);
		await settled(pg);
		await pg.waitForLoadState('networkidle');
		// READINESS A19: the first screen says what Onyx is and what a data user can do today; adding
		// data is the second call to action. It claims a testnet only.
		const cta = pg.getByTestId('home-secondary');
		const browse = pg.getByTestId('home-primary');
		const what = (await pg.getByTestId('home-what').innerText()).replace(/\s+/g, ' ').trim();
		expect((await pg.title()) === 'Onyx · Data anyone can check' && (await pg.locator('h1').innerText()).replace(/\s+/g, ' ').trim() === 'Data anyone can check.', `U1 ${width}: the headline and the tab title say what Onyx is`);
		expect(/Every answer comes with a proof you can verify yourself\./.test(what) && /browse 9,054 datasets, get them by API, or connect an AI assistant/.test(what), `U1 ${width}: the first paragraph says what a data user gets and can do now ("${what}")`);
		expect((await browse.innerText()).trim() === 'Browse datasets' && (await browse.getAttribute('href')) === '#/datasets' && !/ghost/.test(await browse.getAttribute('class')), `U1 ${width}: the primary action opens the datasets`);
		expect((await cta.innerText()).trim() === 'Add your data' && (await cta.getAttribute('href')) === '#/submit' && /ghost/.test(await cta.getAttribute('class')), `U1 ${width}: adding data is the second action`);
		const testnet = pg.getByTestId('home-testnet');
		const testnetText = await testnet.innerText();
		expect(/^Testnet preview\./.test(testnetText) && /test tokens have no value/.test(testnetText) && /6,302 of 9,054 datasets were updated in the last 30 days; the other 2,752 were not\./.test(testnetText.replace(/\s+/g, ' ')), `U1 ${width}: the hero says testnet, tokens without value and how many datasets are being updated, from the node's counts ("${testnetText}")`);
		const proofText = (await pg.getByTestId('home-proof').innerText()).replace(/\s+/g, ' ').trim();
		expect(proofText === 'The proof shows these are exactly the rows the publisher signed and anchored on the test chain. It does not show that the numbers are right.', `U1 ${width}: the hero says what a proof shows and what it does not ("${proofText}")`);
		const assistantText = await pg.getByTestId('users-assistant').innerText();
		expect(!/checks answers/.test(assistantText) && /onyx-check program on your own machine/.test(assistantText), `U1 ${width}: the assistant card does not claim the connector checks answers`);
		const marketText = (await pg.locator('.market-preview').innerText()).replace(/\s+/g, ' ');
		expect(!/one settled|open for test bets/.test(marketText) && /Test markets on the U\.S\. Treasury’s debt that settle from the value Onyx observers recorded/.test(marketText), `U1 ${width}: the markets text names no state that changes`);
		const fold = await pg.evaluate(() => {
			const bottom = (id) => document.querySelector(`[data-testid="${id}"]`).getBoundingClientRect().bottom;
			const top = (id) => document.querySelector(`[data-testid="${id}"]`).getBoundingClientRect().top;
			return { what: bottom('home-what'), actions: bottom('home-secondary'), note: bottom('home-testnet'), users: top('data-users'), steps: top('contribution-steps'), h: innerHeight };
		});
		expect(fold.what <= fold.h && fold.actions <= fold.h && fold.note <= fold.h, `U1 ${width}: what Onyx is, both actions and the testnet note are on the first screen (${JSON.stringify(fold)})`);
		expect(fold.users < fold.steps, `U1 ${width}: what a data user can do comes before the contributor steps`);
		expect((await pg.locator('.console, .tally, .stats').count()) === 0 && (await pg.getByTestId('contribution-steps').locator('li').count()) === 3 && (await pg.getByTestId('home-add').getAttribute('href')) === '#/submit', `U1 ${width}: home explains three contribution steps without the technical console`);
		const users = await pg.getByTestId('data-users').getByRole('link').evaluateAll((as) => as.map((a) => a.getAttribute('href')));
		expect(JSON.stringify(users) === JSON.stringify(['#/datasets', '#/api', '#/submit?assistant']), `U1 ${width}: home lists what works today: datasets, API and the AI assistant (${users.join(', ')})`);
		const wide = await pg.evaluate(() => [...document.querySelectorAll('main code, main pre')].filter((e) => e.textContent.length > 24).length);
		expect(wide === 0, `U1 ${width}: home has no long code span`);
		const counts = requests.filter((r) => r.path === '/v1/bulk');
		const unwanted = requests.filter((r) => r.method !== 'GET' || (/\/data\/|\.wasm$|verifier\.worker|\/v1\//.test(r.path) && r.path !== '/v1/bulk'));
		expect(unwanted.length === 0 && counts.length === 2, `U1 ${width}: home makes no submissions and fetches no datasets or verifier; from the API only the node's two dataset counts (${JSON.stringify(unwanted)}, ${counts.length} counts)`);
		expect(await pg.evaluate(() => document.documentElement.scrollWidth === innerWidth), `U1 ${width}: home fits the viewport`);
		await pg.evaluate(axeSrc);
		const violations = await pg.evaluate(async () => (await window.axe.run(document, { runOnly: { type: 'tag', values: ['wcag2a','wcag2aa','wcag21aa','wcag22aa','best-practice'] } })).violations.filter((v) => ['serious','critical'].includes(v.impact)).map((v) => v.id));
		expect(violations.length === 0, `U1 ${width}: no serious/critical accessibility violations (${violations.join(', ')})`);
		await pg.screenshot({ path: `/tmp/onyx-product-home-${width}.png`, fullPage: true });
		await go(pg, cta, ['#/submit']);
		expect((await pg.locator('h1').innerText()).trim() === 'Add data' && (await pg.getByTestId('next').count()) === 1, `U1 ${width}: Add your data opens the real editable submission form`);
		// Meeting polish MP1: a clear second way in, the steps for an AI assistant (MCP) and for the API.
		const before = requests.length;
		const openBtn = pg.getByTestId('open-agents');
		expect((await openBtn.isVisible()) && /AI assistant or the API/.test(await openBtn.innerText()) && (await openBtn.getAttribute('aria-expanded')) === 'false', `MP1 ${width}: Add data shows a "Use an AI assistant or the API" button, closed`);
		await openBtn.click();
		const panel = pg.getByTestId('add-with-agents');
		await panel.waitFor();
		const focused = await pg.evaluate(() => document.activeElement?.id);
		const mcpUrl = (await pg.getByTestId('agents-ai').innerText());
		expect((await openBtn.getAttribute('aria-expanded')) === 'true' && focused === 'agents-title' && (await pg.getByTestId('agents-client-chatgpt').getAttribute('aria-pressed')) === 'true' && /Developer mode/.test(await pg.getByTestId('agents-client-steps').innerText()) && mcpUrl.includes('https://onyx.cryptagraph.com/mcp') && /sub_/.test(mcpUrl), `MP1 ${width}: the button opens the AI steps (ChatGPT first, the hosted /mcp address, receipt) and moves the focus to them`);
		// MP15: every common assistant has its own setup, all on the same address.
		const setups = {};
		for (const id of ['chatgpt', 'claude', 'claude-code', 'cursor', 'vscode', 'gemini', 'other']) {
			await pg.getByTestId(`agents-client-${id}`).click();
			setups[id] = await pg.getByTestId('agents-ai').innerText();
		}
		expect(setups['claude-code'].includes('claude mcp add --transport http onyx https://onyx.cryptagraph.com/mcp') && setups.cursor.includes('"mcpServers"') && setups.vscode.includes('"type": "http"') && setups.gemini.includes('"httpUrl": "https://onyx.cryptagraph.com/mcp"') && Object.values(setups).every((t) => t.includes('https://onyx.cryptagraph.com/mcp')), `MP15 ${width}: ChatGPT, Claude, Claude Code, Cursor, VS Code, Gemini CLI and other clients each show their setup with the hosted address`);
		await pg.getByTestId('agents-tab-api').click();
		const api = await pg.getByTestId('agents-api').innerText();
		expect(['/v1/submit/check', '/v1/submit \\', '/v1/submit/sub_', '"licenseId": "onyx:us-treasury-fiscal-data-open"'].every((t) => api.includes(t)), `MP1 ${width}: the API tab shows the proposal file and the check, send and status calls`);
		expect(await pg.evaluate(() => document.documentElement.scrollWidth === innerWidth), `MP1 ${width}: the open panel fits the viewport`);
		const panelIssues = await pg.evaluate(async () => (await window.axe.run({ include: ['[data-testid="add-with-agents"]'] }, { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21aa', 'wcag22aa'] } })).violations.map((v) => v.id));
		expect(panelIssues.length === 0, `MP1 ${width}: the panel passes axe (${panelIssues.join(', ')})`);
		await pg.getByTestId('agents-close').click();
		const sent = requests.slice(before).filter((r) => r.method !== 'GET' || /\/v1\//.test(r.path));
		expect((await panel.count()) === 0 && sent.length === 0, `MP1 ${width}: the panel closes and sent nothing (${JSON.stringify(sent)})`);
		if (width === 1280) {
			await pg.locator('#site-nav a[aria-current="page"]').hover();
			const hoverIssues = await pg.evaluate(async () => (await window.axe.run({ include: ['header'] }, { runOnly: { type: 'tag', values: ['wcag2aa'] } })).violations.filter((v) => ['serious','critical'].includes(v.impact)).map((v) => v.id));
			expect(hoverIssues.length === 0, `U1 ${width}: active Add data navigation remains accessible on hover (${hoverIssues.join(', ')})`);
		}
		await go(pg, pg.locator('header a.mark'), HOME);
		// READINESS A19: "Connect an AI assistant" opens the setup steps, focused, and sends nothing.
		const beforeAssistant = requests.length;
		await go(pg, pg.getByTestId('users-assistant'), ['#/submit?assistant']);
		await pg.getByTestId('add-with-agents').waitFor();
		await pg.waitForFunction(() => document.activeElement?.id === 'agents-title', null, { timeout: WAIT_CAP }).catch(() => {});
		const sentAssistant = requests.slice(beforeAssistant).filter((r) => r.method !== 'GET');
		// The link lands on the connector steps (not the top of Add data), and the first example reads data.
		await pg.waitForFunction(() => {
			const r = document.getElementById('add-with-agents')?.getBoundingClientRect();
			return r && r.top < innerHeight / 2 && r.bottom > 0;
		}, null, { timeout: WAIT_CAP }).catch(() => {});
		const landed = await pg.evaluate(() => {
			const r = document.getElementById('add-with-agents').getBoundingClientRect();
			return { top: Math.round(r.top), h: innerHeight, title: document.getElementById('agents-title').textContent.trim(), first: document.querySelector('[data-testid="agents-ai"] pre')?.textContent, second: document.querySelectorAll('[data-testid="agents-ai"] li')[1]?.querySelector('pre')?.textContent, claim: document.querySelector('[data-testid="agents-no-check"]')?.textContent.replace(/\s+/g, ' ') };
		});
		expect(landed.top < landed.h / 2 && landed.title === 'Connect an AI assistant' && landed.second === 'Does Onyx have daily Tron staking flows? Show the newest 10 rows.' && /cannot check a proof: for that, run the onyx-check program on your own machine/.test(landed.claim ?? ''), `U1 ${width}: the AI assistant link lands on "Connect an AI assistant", which leads with a read example and says the hosted connector cannot check a proof (${JSON.stringify(landed)})`);
		expect((await pg.locator('h1').innerText()).trim() === 'Add data' && (await pg.getByTestId('open-agents').getAttribute('aria-expanded')) === 'true' && (await pg.evaluate(() => document.activeElement?.id)) === 'agents-title' && (await pg.getByTestId('agents-ai').innerText()).includes('/mcp') && sentAssistant.length === 0, `U1 ${width}: the AI assistant link opens the connector steps with the focus on their title`);
		await go(pg, pg.locator('header a.mark'), HOME);
		await go(pg, pg.getByTestId('home-primary'), ['#/datasets']);
		expect(new URL(pg.url()).hash === '#/datasets' && (await pg.locator('h1').count()) === 1, `U1 ${width}: browse opens the datasets page`);
		await go(pg, pg.locator('header a.mark'), HOME);
		await go(pg, pg.getByTestId('home-technical'), ['#/how-it-works']);
		expect((await pg.locator('h1').innerText()).trim() === 'How Onyx checks data' && (await pg.locator('.console').count()) === 1, `U1 ${width}: How it works contains the actual verification console`);
		await go(pg, pg.locator('header a.mark'), HOME);
		await go(pg, pg.getByTestId('home-markets'), ['#/markets']);
		expect((await pg.locator('h1').innerText()).trim() === 'Call the next number.', `U1 ${width}: the Markets dashboard is one click from home`);
		await go(pg, pg.locator('header a.mark'), HOME);
		const footer = await pg.getByTestId('footer-nav').getByRole('link').evaluateAll((as) => as.map((a) => a.hash));
		expect(['#/','#/data','#/how-it-works','#/submit','#/download','#/run','#/reserves','#/explorer','#/labels','#/api','#/markets'].every((h) => footer.includes(h)), `U1 ${width}: all eleven destinations remain discoverable`);
		expect(errors.length === 0, `U1 ${width}: no browser errors (${errors.join('; ')})`);
		await ctx.close();
	}
}

if (import.meta.url === `file://${process.argv[1]}`) {
	const { chromium } = await import('playwright-core');
	const { serve } = await import('./serve.mjs');
	const root = process.argv[2] ?? new URL('../build/', import.meta.url).pathname;
	const server = await serve(0, root);
	const browser = await chromium.launch({ channel: 'chrome' });
	const failures = [];
	try {
		await product(browser, `http://127.0.0.1:${server.address().port}/`, root, (ok, msg) => { console.log(`${ok ? 'ok' : 'FAIL'} ${msg}`); if (!ok) failures.push(msg); });
	} finally { await browser.close(); server.close(); }
	process.exitCode = failures.length ? 1 : 0;
}
