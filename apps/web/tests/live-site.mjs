// Live-site audit (meeting polish MP0, docs/website-plan.md): opens the deployed site in Chrome and,
// per page and width, records console errors, failed requests, sideways scroll, axe-core (WCAG
// 2.0/2.1/2.2 A and AA) violations and a full-page screenshot; then checks every link once.
// Measures only; prints a summary and writes <out>/report.json.
// Usage: node tests/live-site.mjs [url] [out dir]   (default https://onyx.cryptagraph.com;
// WIDTHS=390,1280 and AUDIT_PAGES=data,api pick widths and pages; NO_LINKS=1 skips the link check)
import { chromium } from 'playwright-core';
import { createRequire } from 'node:module';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';

const base = (process.argv[2] ?? 'https://onyx.cryptagraph.com').replace(/\/$/, '');
const out = process.argv[3] ?? 'live-audit';
const ALL = ['', 'how-it-works', 'data', 'markets', 'submit', 'run', 'reserves', 'explorer', 'labels', 'api'];
const pages = process.env.AUDIT_PAGES ? ALL.filter((p) => process.env.AUDIT_PAGES.split(',').includes(p || 'home')) : ALL;
const widths = (process.env.WIDTHS ?? '390,768,1280,1440').split(',').map(Number);
const UA = 'onyx-dev';
const axeSrc = await readFile(createRequire(import.meta.url).resolve('axe-core/axe.min.js'), 'utf8');
await mkdir(out, { recursive: true });

const browser = await chromium.launch({ channel: 'chrome' });
const report = { base, at: new Date().toISOString(), pages: [], links: [] };
const links = new Map();
for (const page of pages) {
	for (const w of widths) {
		const ctx = await browser.newContext({
			viewport: { width: w, height: 900 },
			userAgent: UA,
			isMobile: w < 500,
			hasTouch: w < 500,
			deviceScaleFactor: 1,
			reducedMotion: 'reduce'
		});
		const p = await ctx.newPage();
		const errors = [];
		const failed = [];
		p.on('console', (m) => m.type() === 'error' && errors.push(m.text().slice(0, 300)));
		p.on('pageerror', (e) => errors.push('pageerror: ' + String(e).slice(0, 300)));
		p.on('response', (r) => r.status() >= 400 && failed.push(`${r.status()} ${r.url()}`));
		p.on('requestfailed', (r) => failed.push(`failed ${r.url()} ${r.failure()?.errorText ?? ''}`));
		const name = page || 'home';
		const t0 = Date.now();
		await p.goto(`${base}/#/${page}`, { waitUntil: 'networkidle', timeout: 60000 }).catch((e) => errors.push('goto: ' + e.message));
		await p.waitForTimeout(1500);
		const loadMs = Date.now() - t0;
		const layout = await p.evaluate(() => {
			const vw = document.documentElement.clientWidth;
			const wide = [];
			for (const el of document.querySelectorAll('body *')) {
				const r = el.getBoundingClientRect();
				if (r.width === 0 || r.height === 0) continue;
				if (r.right > vw + 1 || r.left < -1) {
					// Skip children of a box that scrolls sideways on purpose.
					let a = el.parentElement, scrolls = false;
					while (a) { const o = getComputedStyle(a).overflowX; if (o === 'auto' || o === 'scroll' || o === 'hidden' || o === 'clip') { scrolls = true; break; } a = a.parentElement; }
					if (!scrolls) wide.push(`${el.tagName.toLowerCase()}.${[...el.classList].join('.')} ${Math.round(r.left)}..${Math.round(r.right)}`);
				}
			}
			const small = [];
			for (const el of document.querySelectorAll('a, button, input, select, summary, [role=button], [role=tab]')) {
				const r = el.getBoundingClientRect();
				if (r.width && r.height && (r.height < 24 || r.width < 24)) small.push(`${el.tagName.toLowerCase()} "${(el.textContent || el.getAttribute('aria-label') || '').trim().slice(0, 30)}" ${Math.round(r.width)}x${Math.round(r.height)}`);
			}
			const imgs = [...document.querySelectorAll('img')].filter((i) => !i.hasAttribute('alt')).map((i) => i.src);
			const h1 = [...document.querySelectorAll('h1')].map((h) => h.textContent.trim().slice(0, 80));
			const anchors = [...document.querySelectorAll('a[href]')].map((a) => a.href);
			return { overflow: document.documentElement.scrollWidth > vw, scrollWidth: document.documentElement.scrollWidth, vw, wide: wide.slice(0, 15), small: small.slice(0, 20), imgsNoAlt: imgs, h1, title: document.title, anchors };
		});
		for (const a of layout.anchors) if (!links.has(a)) links.set(a, `${name}@${w}`);
		delete layout.anchors;
		await p.addScriptTag({ content: axeSrc });
		const axe = await p.evaluate(async () => {
			const r = await window.axe.run(document, { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa'] } });
			return r.violations.map((v) => ({ id: v.id, impact: v.impact, n: v.nodes.length, help: v.help, targets: v.nodes.slice(0, 4).map((n) => n.target.join(' ')) }));
		});
		await p.screenshot({ path: join(out, `${name}-${w}.jpg`), fullPage: true, type: 'jpeg', quality: 60 });
		report.pages.push({ page: name, width: w, loadMs, errors, failed, axe, ...layout });
		await ctx.close();
	}
}
if (process.env.NO_LINKS !== '1') {
	for (const [url, from] of links) {
		const u = new URL(url);
		if (u.origin === new URL(base).origin && u.hash.startsWith('#/')) {
			const route = u.hash.slice(2).split(/[?#]/)[0].split('/')[0];
			report.links.push({ url, from, ok: ALL.includes(route) || route === 'arena', status: 'route' });
			continue;
		}
		if (!/^https?:/.test(url)) continue;
		let status = 0;
		try {
			let r = await fetch(url, { method: 'HEAD', redirect: 'follow', headers: { 'user-agent': UA }, signal: AbortSignal.timeout(15000) });
			if (r.status === 405 || r.status === 403) r = await fetch(url, { redirect: 'follow', headers: { 'user-agent': UA }, signal: AbortSignal.timeout(15000) });
			status = r.status;
		} catch (e) { status = String(e.cause?.code ?? e.name); }
		report.links.push({ url, from, ok: typeof status === 'number' && status < 400, status });
	}
}
await browser.close();
await writeFile(join(out, 'report.json'), JSON.stringify(report, null, 1));
for (const r of report.pages) {
	const bad = [r.errors.length && `${r.errors.length} console errors`, r.failed.length && `${r.failed.length} failed requests`, r.overflow && `sideways scroll ${r.scrollWidth}>${r.vw}`, r.axe.length && `axe: ${r.axe.map((a) => `${a.id}(${a.n})`).join(',')}`, r.imgsNoAlt.length && `${r.imgsNoAlt.length} img without alt`, r.h1.length !== 1 && `${r.h1.length} h1`].filter(Boolean);
	console.log(`${r.page.padEnd(13)} ${String(r.width).padStart(4)}  ${r.loadMs}ms  ${bad.length ? bad.join('; ') : 'clean'}`);
}
const broken = report.links.filter((l) => !l.ok);
console.log(`links: ${report.links.length} checked, ${broken.length} broken`);
for (const l of broken) console.log(`  ${l.status} ${l.url} (from ${l.from})`);
