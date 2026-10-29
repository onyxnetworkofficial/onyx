// Accessibility audit (docs/website-plan.md W3, re-run in W9): axe-core (WCAG 2.0/2.1/2.2 A and
// AA rules plus best practices) on every page of a build, at 1280 px and 390 px, first as
// loaded and then with every <details> opened (folded tables, chart data tables, menus).
// Measures only; prints a summary table and writes the full JSON report.
// Usage: node tests/a11y.mjs <build dir> [out.json]   (Chrome at /usr/bin/google-chrome;
// axe-core from apps/web/node_modules, or AXE_JS=/path/to/axe.min.js)
import { readFile, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';
import { chromium } from 'playwright-core';
import { serve } from './serve.mjs';

const [buildDir, outFile] = process.argv.slice(2);
if (!buildDir) throw new Error('usage: node tests/a11y.mjs <build dir> [out.json]');
const axeSrc = await readFile(process.env.AXE_JS ?? createRequire(import.meta.url).resolve('axe-core/axe.min.js'), 'utf8');
export const AXE_TAGS = ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa', 'best-practice'];
const PAGES = ['', 'how-it-works', 'data', 'reserves', 'run', 'explorer', 'labels', 'api', 'submit'];
const VIEWS = [
	['desktop', { width: 1280, height: 800 }],
	['phone', { width: 390, height: 844 }]
];

const server = await serve(0, resolve(buildDir));
const base = `http://127.0.0.1:${server.address().port}/`;
const browser = await chromium.launch({ executablePath: '/usr/bin/google-chrome', args: ['--headless=new'] });
const report = { build: resolve(buildDir), axe: '', date: new Date().toISOString(), runs: [] };
try {
	for (const [view, viewport] of VIEWS) {
		for (const p of PAGES) {
			const ctx = await browser.newContext({ viewport, reducedMotion: 'reduce' });
			const pg = await ctx.newPage();
			await pg.goto(base + (p ? `#/${p}` : ''));
			await pg.waitForFunction(() => !document.querySelector('.sk'), null, { timeout: 120000 }).catch(() => {});
			await pg.waitForTimeout(1500);
			await pg.addScriptTag({ content: axeSrc });
			for (const state of ['loaded', 'details open']) {
				if (state === 'details open') {
					await pg.evaluate(() => document.querySelectorAll('details').forEach((d) => (d.open = true)));
					await pg.waitForTimeout(400);
				}
				const r = await pg.evaluate((tags) => window.axe.run(document, { runOnly: { type: 'tag', values: tags }, resultTypes: ['violations'] }), AXE_TAGS);
				report.axe = r.testEngine.version;
				report.runs.push({
					page: p || 'home',
					view,
					state,
					violations: r.violations.map((v) => ({ id: v.id, impact: v.impact, help: v.help, nodes: v.nodes.length, targets: v.nodes.slice(0, 5).map((n) => n.target.join(' ')) }))
				});
			}
			await ctx.close();
		}
	}
} finally {
	await browser.close();
	server.close();
}
if (outFile) await writeFile(outFile, JSON.stringify(report, null, 1));
console.log(`axe-core ${report.axe}, ${report.build}`);
console.log('| Page | View | State | Rules violated | Nodes | Critical | Serious | Moderate | Minor | Rules |');
console.log('|---|---|---|---|---|---|---|---|---|---|');
for (const r of report.runs) {
	const by = (i) => r.violations.filter((v) => v.impact === i).reduce((a, v) => a + v.nodes, 0);
	const nodes = r.violations.reduce((a, v) => a + v.nodes, 0);
	console.log(`| ${r.page} | ${r.view} | ${r.state} | ${r.violations.length} | ${nodes} | ${by('critical')} | ${by('serious')} | ${by('moderate')} | ${by('minor')} | ${r.violations.map((v) => `${v.id} (${v.nodes})`).join(', ')} |`);
}
