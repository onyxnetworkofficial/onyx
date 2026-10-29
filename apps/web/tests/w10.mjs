// W10 visual polish and W11 follow-ups (docs/website-plan.md), in a real browser. Called by
// tests/e2e.mjs; also runs alone against a build: node tests/w10.mjs [build dir].
// W10: on all 8 pages at 1280 and 390 px, every computed font size, line height, spacing value,
// corner radius, shadow and colour is an app.css token (tests/styles.mjs reads them); every
// icon has one stroke and a scale size; every control changes on hover and every button on
// press (1280 px); motion stops with reduced motion or the pause button; placeholders share one
// look and name what they wait for. W11: the tooltips stay inside their charts, the reserve line
// shows a down week without colour, the tooltip words match the legends, the tag class is named.
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { capWaits, routeDrawn, settled, WAIT_CAP } from './wait.mjs';
import { inventory, tokens } from './styles.mjs';

const PAGES = ['', 'how-it-works', 'data', 'reserves', 'run', 'explorer', 'labels', 'api', 'submit'];

/** Style of an element, its parent and its children: what a hover or press may change. */
function look(e) {
	const P = ['color', 'background-color', 'background-image', 'border-top-color', 'border-bottom-color', 'box-shadow', 'transform', 'text-decoration-color', 'text-decoration-thickness', 'opacity', 'accent-color', 'filter'];
	const one = (x) => (x ? P.map((p) => getComputedStyle(x).getPropertyValue(p)).join('|') : '');
	const up = [e.parentElement, e.parentElement?.parentElement, e.parentElement?.parentElement?.parentElement];
	return [one(e), ...up.map(one), ...[...e.children].map(one)].join('#');
}

export async function w10(browser, base, root, expect) {
	const demo = JSON.parse(await readFile(join(root, 'data/demo.json'), 'utf8'));
	const PUB = demo.public === true;
	// ROADMAP L14: a rights-fallback build has no company flow, series or reserve chart.
	const FB = demo.rights === 'fallback';
	// 1. Tokens only, every page, both widths (reduced motion: the resting state).
	for (const width of [1280, 390]) {
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce', isMobile: width < 500, hasTouch: width < 500 });
		const bad = { fontSizes: [], lineHeights: [], space: [], radii: [], shadows: [], colors: [], icons: [], spaceOther: [], folds: [] };
		let strokes = new Set();
		let sizes = new Set();
		for (const page of PAGES) {
			const p = await ctx.newPage();
			await p.goto(base + (page ? `#/${page}` : ''));
			await p.locator('main').waitFor();
			await settled(p);
			const tok = await p.evaluate(tokens);
			const inv = await p.evaluate(inventory);
			const LH = new Set(['0.96', '1.00', '1.15', '1.35', '1.55']);
			const where = (m, ok) => Object.entries(m).filter(([v]) => !ok(v)).map(([v, x]) => `${page || 'home'}: ${v} (${x.at})`);
			bad.fontSizes.push(...where(inv.fontSizes, (v) => v in tok.fontSizes));
			bad.lineHeights.push(...where(inv.lineHeights, (v) => LH.has(v)));
			bad.space.push(...where(inv.space, (v) => v in tok.space));
			bad.spaceOther.push(...where(inv.spaceOther, () => false));
			bad.radii.push(...where(inv.radii, (v) => v in tok.radii || v === '50%'));
			bad.shadows.push(...where(inv.shadows, (v) => v in tok.shadows));
			bad.colors.push(...where(inv.colors, (v) => v in tok.colors));
			const icons = await p.evaluate(() =>
				[...document.querySelectorAll('svg.lucide')]
					.filter((s) => s.getBoundingClientRect().width > 0)
					.map((s) => [Math.round(s.getBoundingClientRect().width), getComputedStyle(s).strokeWidth, s.getAttribute('class')])
			);
			const folds = await p.evaluate(() =>
				[...document.querySelectorAll('details > summary')]
					.filter((s) => s.getBoundingClientRect().width > 0)
					.map((s) => {
						const d = s.parentElement;
						const own = d.classList.contains('txset') ? s.querySelector('.show') : s;
						const m = getComputedStyle(own, '::before').content;
						return [m, d.open, getComputedStyle(s).listStyleType, s.textContent.trim().slice(0, 24)];
					})
			);
			for (const [m, open, ls, t] of folds) if (m !== (open ? '"− "' : '"+ "') || ls !== 'none') bad.folds.push(`${page || 'home'}: "${t}" ${m} ${ls}`);
			for (const [w, sw, c] of icons) {
				sizes.add(w);
				strokes.add(sw);
				if (![16, 18, 22].includes(w)) bad.icons.push(`${page || 'home'}: ${c} ${w}px`);
			}
			await p.close();
		}
		const show = (a) => (a.length ? ': ' + [...new Set(a)].slice(0, 6).join('; ') : '');
		const at = `W10 ${width} px`;
		expect(bad.fontSizes.length === 0, `${at}: every font size on all 8 pages is a --text-* token${show(bad.fontSizes)}`);
		expect(bad.lineHeights.length === 0, `${at}: every line height is one of 0.96, 1, 1.15, 1.35, 1.55 (the --lh-* tokens)${show(bad.lineHeights)}`);
		expect(bad.space.length === 0 && bad.spaceOther.length === 0, `${at}: every padding, margin and gap is a --sp-* step (4 px grid)${show([...bad.space, ...bad.spaceOther])}`);
		expect(bad.radii.length === 0, `${at}: every corner radius is a --radius-* token (or 50% for dots)${show(bad.radii)}`);
		expect(bad.shadows.length === 0, `${at}: every shadow is a --shadow-* token${show(bad.shadows)}`);
		expect(bad.colors.length === 0, `${at}: every text, background, border and chart colour is an app.css token${show(bad.colors)}`);
		expect(bad.folds.length === 0, `${at}: every fold (<details>) shows the same "+" / "−" marker${show(bad.folds)}`);
		expect(bad.icons.length === 0 && strokes.size === 1, `${at}: every icon is 16, 18 or 22 px (${[...sizes].sort((a, b) => a - b).join(', ')}) in one stroke (${[...strokes].join(', ')})${show(bad.icons)}`);
		await ctx.close();
	}

	// 2. Hover and press states on every control (1280 px, reduced motion so states apply at once).
	{
		const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 }, reducedMotion: 'reduce' });
		for (const page of PAGES) {
			const p = await ctx.newPage();
			await p.goto(base + (page ? `#/${page}` : ''));
			await p.locator('main').waitFor();
			await settled(p);
			const n = await p.evaluate(() => {
				const vis = (e) => {
					const r = e.getBoundingClientRect();
					return r.width > 0 && r.height > 0 && getComputedStyle(e).visibility !== 'hidden' && !e.closest('.sr-only');
				};
				const els = [...document.querySelectorAll('header a, header button, main a[href], main button, main summary, main select, main input, footer a')].filter((e) => vis(e) && !e.disabled && !e.classList.contains('skip'));
				els.forEach((e, i) => e.setAttribute('data-w10', String(i)));
				return els.length;
			});
			const noHover = [];
			const noPress = [];
			let pressed = 0;
			for (let i = 0; i < n; i++) {
				const el = p.locator(`[data-w10="${i}"]`);
				const box = await el.evaluate((e) => {
					e.scrollIntoView({ block: 'center' });
					const r = e.getBoundingClientRect();
					const x = r.left + Math.min(r.width / 2, 12);
					const y = r.top + r.height / 2;
					const top = document.elementFromPoint(x, y);
					return { x, y, free: !!top && (top === e || e.contains(top)), tag: e.tagName.toLowerCase(), btn: e.tagName === 'BUTTON' || e.classList.contains('btn'), name: (e.getAttribute('aria-label') || e.textContent || '').trim().replace(/\s+/g, ' ').slice(0, 30) };
				});
				if (!box.free) continue;
				await p.mouse.move(1279, 899);
				const rest = await el.evaluate(look);
				await p.mouse.move(box.x, box.y);
				const hovered = await el.evaluate(look);
				if (hovered === rest) noHover.push(`${box.tag} "${box.name}"`);
				if (box.btn) {
					await p.mouse.down();
					const down = await el.evaluate(look);
					await p.mouse.move(1279, 899);
					await p.mouse.up();
					pressed++;
					if (down === hovered) noPress.push(`${box.tag} "${box.name}"`);
				}
			}
			const name = page || 'home';
			expect(n > 0 && noHover.length === 0, `W10 ${name}: all ${n} controls change on hover${noHover.length ? ': ' + noHover.slice(0, 6).join(', ') : ''}`);
			expect(noPress.length === 0, `W10 ${name}: all ${pressed} buttons change when pressed${noPress.length ? ': ' + noPress.slice(0, 6).join(', ') : ''}`);
			await p.close();
		}
		await ctx.close();
	}

	// 3. Motion: transitions run normally, and stop with reduced motion or the pause button.
	{
		const dur = (pg) => pg.evaluate(() => [...document.querySelectorAll('main button, main a.btn, header a')].map((e) => parseFloat(getComputedStyle(e).transitionDuration)).filter((d) => d > 0).length);
		const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 } });
		const p = await ctx.newPage();
		await p.goto(base);
		await p.locator('main').waitFor();
		await settled(p);
		const moving = await dur(p);
		// E12: read once the click has set the state (the page's motion flag), not at once.
		const motion = (v) => p.waitForFunction((v) => document.documentElement.dataset.motion === v, v, { timeout: WAIT_CAP }).catch(() => {});
		await p.getByTestId('motion-button').click();
		await motion('off');
		const paused = await dur(p);
		await p.getByTestId('motion-button').click();
		await motion('on');
		await ctx.close();
		const still = await browser.newContext({ viewport: { width: 1280, height: 900 }, reducedMotion: 'reduce' });
		const q = await still.newPage();
		await q.goto(base);
		await q.locator('main').waitFor();
		const reduced = await dur(q);
		await still.close();
		expect(moving >= 5 && paused === 0 && reduced === 0, `W10 motion: ${moving} controls ease their hover and press; none with the pause button (${paused}) or reduced motion (${reduced})`);
	}

	// 4. Placeholders: one look on every page, each named, while the data is on its way.
	{
		// E10: the data is held back until each page's placeholders are read (it was a 6 s delay
		// and a 5 s wait for the first placeholder, both wall-clock races on a loaded machine).
		const slow = await browser.newContext({ viewport: { width: 1280, height: 900 }, reducedMotion: 'reduce' });
		let release;
		const held = new Promise((r) => (release = r));
		await slow.route('**/*.{json,txt}', async (route) => {
			await held;
			await route.continue().catch(() => {});
		});
		const looks = new Set();
		const names = [];
		const pagesWith = [];
		for (const page of PAGES) {
			const sp = await slow.newPage();
			await sp.goto(base + (page ? `#/${page}` : ''));
			// The page is drawn with its data still held: its placeholders, if any, are on screen.
			await routeDrawn(sp);
			if ((await sp.locator('.sk').count()) > 0) {
				pagesWith.push(page || 'home');
				const r = await sp.locator('.sk').evaluateAll((els) =>
					els.map((e) => {
						const b = e.querySelector('.bone');
						const s = getComputedStyle(b);
						const inPanel = !!e.closest('.panel');
						return [e.getAttribute('aria-label'), `${inPanel ? 'in-panel' : 'page'} ${s.borderRadius} ${s.padding} ${s.borderTopWidth} ${getComputedStyle(b.querySelector('i')).height}`];
					})
				);
				for (const [n, l] of r) (names.push(n), looks.add(l));
			}
			await sp.close();
		}
		release();
		await slow.close();
		const ok = names.length > 0 && names.every((n) => n && n !== 'Loading the data') && looks.size <= 2;
		expect(ok, `W10 placeholders on ${pagesWith.join(', ')}: ${names.length} named (${[...new Set(names)].slice(0, 4).join('; ')}…), ${looks.size} looks (page / inside a panel): ${[...looks].join(' | ')}`);
	}

	// W11.
	{
		const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 }, reducedMotion: 'reduce' });
		const p = await ctx.newPage();
		await p.goto(base + '#/data');
		await p.getByTestId('pool-shares').waitFor({ timeout: 150000 });
		await settled(p);
		// Tooltips stay inside their chart, at both ends of every chart and on every pool day.
		// The frame measures the tooltip after it renders (a resize observer), so allow it a few frames
		// (E10: polled until placed, not 10 polls of 50 ms; E12: capped by wait.mjs, not 10 s).
		const inside = async (c) => {
			for (const t0 = Date.now(); Date.now() - t0 < WAIT_CAP; ) {
				const [cb, tb] = await Promise.all([c.boundingBox(), c.locator('.tip').boundingBox().catch(() => null)]);
				if (tb && tb.y >= cb.y - 0.5 && tb.y + tb.height <= cb.y + cb.height + 0.5 && tb.x >= cb.x - 0.5 && tb.x + tb.width <= cb.x + cb.width + 0.5) return true;
				await p.waitForTimeout(50);
			}
			return false;
		};
		const pc = p.getByTestId('pool-shares').locator('xpath=ancestor::div[contains(@class,"chart")][1]');
		const days = JSON.parse(await readFile(join(root, 'history/pools-week.json'), 'utf8')).days.length;
		await pc.focus();
		let poolOut = [];
		await p.keyboard.press('Home');
		for (let d = 0; d < days; d++) {
			if (!(await inside(pc))) poolOut.push(d + 1);
			await p.keyboard.press('ArrowRight');
		}
		expect(days > 0 && poolOut.length === 0, `W11 pools: the day tooltip stays inside the chart on all ${days} days${poolOut.length ? ' (out on day ' + poolOut.join(', ') + ')' : ''}`);
		const charts = await p.locator('main .chart[tabindex="0"]').all();
		let out = 0;
		for (const c of charts) {
			await c.focus();
			for (const k of ['Home', 'End']) {
				await p.keyboard.press(k);
				if (!(await inside(c))) out++;
			}
			await p.keyboard.press('Escape');
		}
		expect(charts.length >= (FB ? 2 : 4) && out === 0, `W11 data: on all ${charts.length} charts the tooltip stays inside the chart at the first and last point (${out} outside)`);
		// Tooltip words match the legends.
		const tipOf = async (testid) => {
			const c = p.getByTestId(testid).locator('xpath=following::div[contains(@class,"chart")][@tabindex="0"][1]');
			await c.focus();
			await p.keyboard.press('End');
			const t = await c.locator('.tip').innerText().catch(() => '');
			await p.keyboard.press('Escape');
			return t;
		};
		if (!FB) {
		const flowLegend = await p.getByTestId('flow-legend').innerText();
		const flowTip = await tipOf('flow-legend');
		expect(flowLegend.includes('Running net') && /Running net /.test(flowTip), `W11 flows: the tooltip says "Running net", as the legend does (${flowTip.split('\n').pop()})`);
		const seriesLegend = await p.getByTestId('series-legend').innerText();
		const seriesTip = await tipOf('series-legend');
		expect(seriesLegend.includes('Running total') && /Running total /.test(seriesTip), `W11 series: the tooltip says "Running total", as the legend does (${seriesTip.split('\n').pop()})`);
		}
		// The tag class says what the tag says.
		const tags = await p.locator('.real-tag').allInnerTexts();
		expect(PUB || (tags.length >= 1 && tags.every((t) => t.toLowerCase() === 'real value · test round')), `W11 data: the level-2 tags use the .real-tag class (${tags.length})`);
		expect((await p.locator('.demo').count()) === 0, 'W11 data: no element keeps the old .demo class');

		// Reserves: a down week reads as down without colour (a down triangle, ▼ in words) and in
		// the "down / out" colour token; an up week the other way round.
		if (FB) {
			await ctx.close();
			return;
		}
		await p.goto(base + '#/reserves');
		await p.getByTestId('reserve-cards').waitFor({ timeout: 150000 });
		await settled(p);
		const cards = await p.evaluate(() => {
			const probe = document.createElement('i');
			document.body.append(probe);
			const tok = (v) => ((probe.style.color = `var(${v})`), getComputedStyle(probe).color);
			const btc = tok('--btc');
			const net = tok('--line-net');
			probe.remove();
			return [...document.querySelectorAll('[data-testid^="reserve-change-"]')].map((ch) => {
				const card = ch.closest('.card');
				const line = card.querySelector('[data-testid="reserve-line"]');
				const tr = card.querySelector('[data-testid="reserve-trend"]');
				const stroke = getComputedStyle(line).stroke;
				return { text: ch.textContent.trim(), stroke: stroke === btc ? '--btc' : stroke === net ? '--line-net' : stroke, trend: tr?.getAttribute('data-trend') ?? 'none', fill: tr ? getComputedStyle(tr).fill : '', name: card.querySelector('.chart').getAttribute('aria-label') };
			});
		});
		const want = (c) => (c.text.includes('▼ −') ? 'down' : c.text.includes('▲ +') ? 'up' : 'none');
		const right = cards.filter((c) => {
			const w = want(c);
			return w === c.trend && (w === 'down' ? c.stroke === '--btc' && / lower at the end of the week/.test(c.name) : c.stroke === '--line-net') && (w === 'up' ? / higher at the end of the week/.test(c.name) : true);
		});
		expect(cards.length >= 2 && right.length === cards.length, `W11 reserves: each of ${cards.length} lines shows its week: ${cards.map((c) => `${want(c)} → ${c.trend} triangle, ${c.stroke}`).join('; ')}`);
		const legend = await p.getByTestId('reserve-legend').innerText();
		expect(legend.includes('▲ the week ended higher') && legend.includes('▼ the week ended lower'), 'W11 reserves: the legend says what the up and down triangles and colours mean');
		await ctx.close();
	}
}

if (import.meta.url === `file://${process.argv[1]}`) {
	const { chromium } = await import('playwright-core');
	const { serve } = await import('./serve.mjs');
	const { resolve } = await import('node:path');
	const root = resolve(process.argv[2] ?? new URL('../build/', import.meta.url).pathname);
	const server = await serve(0, root);
	const browser = capWaits(await chromium.launch({ channel: 'chrome' }));
	const failures = [];
	const expect = (ok, what) => (ok ? console.log(`ok   ${what}`) : (failures.push(what), console.log(`FAIL ${what}`)));
	try {
		await w10(browser, `http://127.0.0.1:${server.address().port}/`, root, expect);
	} finally {
		await browser.close();
		server.close();
	}
	console.log(failures.length ? `${failures.length} failed` : 'W10 PASSED');
	process.exit(failures.length ? 1 : 0);
}
