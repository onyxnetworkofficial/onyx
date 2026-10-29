// Style inventory and screenshots of every page (W10, docs/website-plan.md): distinct computed
// font sizes, line heights (as a ratio of the size), spacing values (padding, margin, gap), corner
// radii, shadows, and colours that are not an app.css token, over all 7 pages at 1280 and 390 px.
// Measures only; prints a JSON summary. With SHOTS=<prefix>, also saves full-page JPEGs as
// <out>/<page>-<prefix>-<desktop|390>.jpg.
// Usage: node tests/style-audit.mjs <build dir> <out dir>
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { chromium } from 'playwright-core';
import { serve } from './serve.mjs';
import { inventory, tokens } from './styles.mjs';

const [buildDir, outDir] = process.argv.slice(2);
if (!buildDir || !outDir) throw new Error('usage: node tests/style-audit.mjs <build dir> <out dir>');
await mkdir(outDir, { recursive: true });
const PAGES = ['', 'how-it-works', 'data', 'reserves', 'run', 'explorer', 'labels', 'api'];
const WIDTHS = { desktop: 1280, 390: 390 };
const server = await serve(0, resolve(buildDir));
const base = `http://127.0.0.1:${server.address().port}/`;
const browser = await chromium.launch({ executablePath: '/usr/bin/google-chrome', args: ['--headless=new'] });
const all = {};
const per = {};
let tok = null;
const merge = (into, from) => {
	for (const [k, m] of Object.entries(from)) {
		into[k] ??= {};
		for (const [v, x] of Object.entries(m)) {
			if (!into[k][v]) into[k][v] = { n: 0, at: x.at };
			into[k][v].n += x.n;
		}
	}
};
try {
	for (const [wname, width] of Object.entries(WIDTHS)) {
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce', isMobile: width < 500, hasTouch: width < 500 });
		for (const page of PAGES) {
			const p = await ctx.newPage();
			await p.goto(base + (page ? `#/${page}` : ''));
			await p.locator('main').waitFor();
			await p.waitForFunction(() => !document.querySelector('.sk'), null, { timeout: 150000 }).catch(() => {});
			await p.waitForTimeout(1500);
			tok ??= await p.evaluate(tokens);
			const inv = await p.evaluate(inventory);
			per[`${page || 'home'}@${wname}`] = Object.fromEntries(Object.entries(inv).map(([k, m]) => [k, Object.keys(m).length]));
			merge(all, inv);
			if (process.env.SHOTS) await p.screenshot({ path: join(outDir, `${page || 'home'}-${process.env.SHOTS}-${wname}.jpg`), fullPage: true, type: 'jpeg', quality: 55 });
			await p.close();
		}
		await ctx.close();
	}
} finally {
	await browser.close();
	server.close();
}
const colorsOff = Object.fromEntries(Object.entries(all.colors).filter(([c]) => !(c in tok.colors)));
const sortPx = (o) => Object.keys(o).sort((a, b) => parseFloat(a) - parseFloat(b) || a.localeCompare(b));
const summary = {
	build: resolve(buildDir),
	counts: {
		fontSizes: Object.keys(all.fontSizes).length,
		lineHeights: Object.keys(all.lineHeights).length,
		spacing: Object.keys(all.space).length,
		radii: Object.keys(all.radii).length,
		shadows: Object.keys(all.shadows).length,
		colorsNotToken: Object.keys(colorsOff).length,
		colorsUsed: Object.keys(all.colors).length
	},
	fontSizes: sortPx(all.fontSizes),
	lineHeights: sortPx(all.lineHeights),
	spacing: sortPx(all.space),
	radii: sortPx(all.radii),
	shadows: Object.keys(all.shadows),
	colorsNotToken: Object.fromEntries(Object.entries(colorsOff).map(([c, x]) => [c, `${x.n}x, e.g. ${x.at}`])),
	spaceOther: Object.keys(all.spaceOther ?? {}),
	where: Object.fromEntries(['fontSizes', 'lineHeights', 'space', 'radii'].map((k) => [k, Object.fromEntries(Object.entries(all[k]).map(([v, x]) => [v, `${x.n}x, e.g. ${x.at}`]))])),
	perPage: per,
	tokens: tok
};
await writeFile(join(outDir, 'styles.json'), JSON.stringify(summary, null, 1));
console.log(JSON.stringify(summary.counts));
