// Screenshots of every page (W12, docs/website-plan.md): full page at 1280 and 390 px, reduced
// motion (the resting state), as <out>/<page>-<tag>-<desktop|390>.jpg; with FOLD=1 also the
// first screen alone (<page>-<tag>-<width>-fold.jpg). WIDTHS=375,761,1000,1280,1600 picks other widths
// (each named by its width; phone emulation below 500 px).
// Usage: node tests/shots.mjs <build dir> <out dir> <tag>   (AUDIT_PAGES=home,data picks pages)
import { mkdir } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { chromium } from 'playwright-core';
import { serve } from './serve.mjs';

const [buildDir, outDir, tag = 'shot'] = process.argv.slice(2);
if (!buildDir || !outDir) throw new Error('usage: node tests/shots.mjs <build dir> <out dir> [tag]');
await mkdir(outDir, { recursive: true });
const ALL = ['', 'how-it-works', 'data', 'reserves', 'run', 'explorer', 'labels', 'api'];
const PAGES = process.env.AUDIT_PAGES ? ALL.filter((p) => process.env.AUDIT_PAGES.split(',').includes(p || 'home')) : ALL;
const server = await serve(0, resolve(buildDir));
const base = `http://127.0.0.1:${server.address().port}/`;
const browser = await chromium.launch({ executablePath: '/usr/bin/google-chrome', args: ['--headless=new'] });
try {
	const WIDTHS = process.env.WIDTHS ? process.env.WIDTHS.split(',').map((w) => [w, Number(w)]) : [['desktop', 1280], ['390', 390]];
	for (const [wname, width] of WIDTHS) {
		const ctx = await browser.newContext({ viewport: { width, height: width < 500 ? 844 : 900 }, reducedMotion: 'reduce', isMobile: width < 500, hasTouch: width < 500 });
		for (const page of PAGES) {
			const p = await ctx.newPage();
			await p.goto(base + (page ? `#/${page}` : ''));
			await p.locator('main').waitFor();
			await p.waitForFunction(() => !document.querySelector('.sk'), null, { timeout: 150000 }).catch(() => {});
			// Scroll through once so lazy sections (W4) load, then back to the top.
			await p.evaluate(async () => {
				for (let y = 0; y < document.body.scrollHeight; y += 600) (scrollTo(0, y), await new Promise((r) => setTimeout(r, 120)));
				scrollTo(0, 0);
			});
			await p.waitForTimeout(2500);
			const name = `${page || 'home'}-${tag}-${wname}`;
			if (process.env.FOLD) await p.screenshot({ path: join(outDir, `${name}-fold.jpg`), type: 'jpeg', quality: 70 });
			await p.screenshot({ path: join(outDir, `${name}.jpg`), fullPage: true, type: 'jpeg', quality: 55 });
			await p.close();
		}
		await ctx.close();
	}
} finally {
	await browser.close();
	server.close();
}
console.log('shots done');
