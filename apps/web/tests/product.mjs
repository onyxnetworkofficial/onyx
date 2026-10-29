// Product U1: contributor entry, progressive technical detail and no surprise data loading.
import { readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { settled } from './wait.mjs';

export async function product(browser, base, root, expect) {
	const axeSrc = await readFile(createRequire(import.meta.url).resolve('axe-core/axe.min.js'), 'utf8');
	for (const width of [1280, 390]) {
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
		const pg = await ctx.newPage();
		const requests = [];
		const errors = [];
		pg.on('request', (r) => requests.push({ path: new URL(r.url()).pathname, method: r.method() }));
		pg.on('pageerror', (e) => errors.push(String(e)));
		await pg.goto(base);
		await settled(pg);
		await pg.waitForLoadState('networkidle');
		const cta = pg.getByTestId('home-primary');
		expect((await cta.innerText()).trim() === 'Add your data' && (await cta.getAttribute('href')) === '#/submit', `U1 ${width}: primary action invites the visitor to add their data`);
		expect((await pg.getByTestId('home-secondary').getAttribute('href')) === '#/data', `U1 ${width}: browsing is the secondary action`);
		expect((await pg.locator('.console, .tally, .stats').count()) === 0 && (await pg.getByTestId('contribution-steps').locator('li').count()) === 3, `U1 ${width}: home explains three contribution steps without the technical console`);
		const unwanted = requests.filter((r) => r.method !== 'GET' || /\/data\/|\.wasm$|verifier\.worker|\/v1\//.test(r.path));
		expect(unwanted.length === 0, `U1 ${width}: home makes no submissions and fetches no datasets, verifier or API (${JSON.stringify(unwanted)})`);
		expect(await pg.evaluate(() => document.documentElement.scrollWidth === innerWidth), `U1 ${width}: home fits the viewport`);
		await pg.addScriptTag({ content: axeSrc });
		const violations = await pg.evaluate(async () => (await window.axe.run(document, { runOnly: { type: 'tag', values: ['wcag2a','wcag2aa','wcag21aa','wcag22aa','best-practice'] } })).violations.filter((v) => ['serious','critical'].includes(v.impact)).map((v) => v.id));
		expect(violations.length === 0, `U1 ${width}: no serious/critical accessibility violations (${violations.join(', ')})`);
		await pg.screenshot({ path: `/tmp/onyx-product-home-${width}.png`, fullPage: true });
		await cta.click();
		await settled(pg);
		expect((await pg.locator('h1').innerText()).trim() === 'Add data' && (await pg.getByTestId('next').count()) === 1, `U1 ${width}: Add your data opens the real editable submission form`);
		// Meeting polish MP1: a clear second way in, the steps for an AI assistant (MCP) and for the API.
		const before = requests.length;
		const openBtn = pg.getByTestId('open-agents');
		expect((await openBtn.isVisible()) && /AI or the API/.test(await openBtn.innerText()) && (await openBtn.getAttribute('aria-expanded')) === 'false', `MP1 ${width}: Add data shows an "Add data with AI or the API" button, closed`);
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
		await pg.locator('header a.mark').click();
		await settled(pg);
		await pg.getByTestId('home-secondary').click();
		await settled(pg);
		expect(new URL(pg.url()).hash === '#/data', `U1 ${width}: browse opens the existing data page`);
		await pg.locator('header a.mark').click();
		await settled(pg);
		await pg.getByTestId('home-technical').click();
		await settled(pg);
		expect((await pg.locator('h1').innerText()).trim() === 'How Onyx checks data' && (await pg.locator('.console').count()) === 1, `U1 ${width}: How it works contains the actual verification console`);
		await pg.locator('header a.mark').click();
		await settled(pg);
		await pg.getByTestId('home-markets').click();
		await settled(pg);
		expect((await pg.locator('h1').innerText()).trim() === 'Call the next number.', `U1 ${width}: the Markets dashboard is one click from home`);
		await pg.locator('header a.mark').click();
		await settled(pg);
		const footer = await pg.getByTestId('footer-nav').getByRole('link').evaluateAll((as) => as.map((a) => a.hash));
		expect(['#/','#/data','#/how-it-works','#/submit','#/run','#/reserves','#/explorer','#/labels','#/api','#/markets'].every((h) => footer.includes(h)), `U1 ${width}: all ten destinations remain discoverable`);
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
