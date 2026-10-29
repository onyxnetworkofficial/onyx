// Website audit (docs/website-plan.md W1, re-run in W9): per page, the bytes a first visit
// loads (by kind, gzip on the wire and as decoded), first render and a CPU-quiet
// "interactive" time on a local static server (normal and throttled), a keyboard
// walk-through, and phone screenshots (360/390/430 px) with layout checks.
// Measures only; prints one JSON report. Lighthouse runs separately (tests/bench.sh).
// Usage: node tests/audit.mjs <build dir> <out dir>   (Chrome at /usr/bin/google-chrome; AUDIT_PAGES to pick pages,
// AUDIT_LOAD_ONLY=1 for bytes and timings only)
import { mkdir, writeFile, stat } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { chromium } from 'playwright-core';
import { serve } from './serve.mjs';

const [buildDir, outDir] = process.argv.slice(2);
if (!buildDir || !outDir) throw new Error('usage: node tests/audit.mjs <build dir> <out dir>');
const BUILD = resolve(buildDir);
await mkdir(join(outDir, 'phone'), { recursive: true });
// AUDIT_PAGES=home,run measures only those pages (W5 measured the home page alone).
const ALL_PAGES = ['', 'how-it-works', 'data', 'reserves', 'run', 'explorer', 'labels', 'api'];
const PAGES = process.env.AUDIT_PAGES ? ALL_PAGES.filter((p) => process.env.AUDIT_PAGES.split(',').includes(p || 'home')) : ALL_PAGES;
const pname = (p) => p || 'home';
// Throttled profile = Lighthouse's mobile preset numbers (slow 4G, 4x CPU slowdown), applied
// with the DevTools protocol on the page (requests a worker makes are not network-throttled).
const PROFILES = {
	normal: null,
	throttled: { latency: 150, downloadThroughput: (1638.4 * 1024) / 8, uploadThroughput: (750 * 1024) / 8, cpu: 4 }
};
const kind = (url, type) => {
	const p = new URL(url).pathname;
	if (/\.wasm$/.test(p)) return 'wasm';
	if (/\.js$/.test(p)) return 'js';
	if (/\.css$/.test(p)) return 'css';
	if (/\.woff2$/.test(p)) return 'font';
	if (/\.(png|jpe?g|svg|webp|ico)$/.test(p)) return 'image';
	if (/\.(json|txt)$/.test(p)) return 'data';
	if (type === 'document' || p === '/' || /\.html$/.test(p)) return 'html';
	return 'other';
};

const server = await serve(0, BUILD);
const base = `http://127.0.0.1:${server.address().port}/`;
const browser = await chromium.launch({ executablePath: '/usr/bin/google-chrome', args: ['--headless=new'] });
const report = { build: BUILD, chrome: browser.version(), date: new Date().toISOString(), pages: {} };

async function measure(page, profile) {
	const ctx = await browser.newContext({ viewport: { width: 412, height: 823 } });
	const p = await ctx.newPage();
	const reqs = new Map();
	let inflight = 0;
	let lastNet = Date.now();
	p.on('request', (r) => (inflight++, (lastNet = Date.now()), reqs.set(r, null)));
	const done = async (r) => {
		inflight--;
		lastNet = Date.now();
		try {
			const s = await r.sizes();
			const file = decodeURIComponent(new URL(r.url()).pathname).replace(/^\/$/, '/index.html');
			const raw = await stat(join(BUILD, file)).then((x) => x.size, () => 0);
			reqs.set(r, { url: r.url(), kind: kind(r.url(), r.resourceType()), wire: s.responseBodySize, raw });
		} catch {
			/* request without a body */
		}
	};
	p.on('requestfinished', done);
	p.on('requestfailed', (r) => (inflight--, (lastNet = Date.now())));
	const cdp = await ctx.newCDPSession(p);
	if (profile) {
		await cdp.send('Network.enable');
		await cdp.send('Network.emulateNetworkConditions', { offline: false, latency: profile.latency, downloadThroughput: profile.downloadThroughput, uploadThroughput: profile.uploadThroughput });
		await cdp.send('Emulation.setCPUThrottlingRate', { rate: profile.cpu });
	}
	await p.addInitScript(() => {
		window.__lt = [];
		window.__lcp = 0;
		new PerformanceObserver((l) => l.getEntries().forEach((e) => window.__lt.push([e.startTime, e.startTime + e.duration]))).observe({ type: 'longtask', buffered: true });
		new PerformanceObserver((l) => l.getEntries().forEach((e) => (window.__lcp = e.startTime))).observe({ type: 'largest-contentful-paint', buffered: true });
	});
	const t0 = Date.now();
	await p.goto(base + (page ? `#/${page}` : ''), { waitUntil: 'load', timeout: 180000 });
	// Settled: no request in flight for 3 s and no long task in the last 5 s (cap 150 s).
	for (;;) {
		await p.waitForTimeout(250);
		const lt = await p.evaluate(() => window.__lt.at(-1)?.[1] ?? 0);
		const now = await p.evaluate(() => performance.now());
		if ((inflight <= 0 && Date.now() - lastNet > 3000 && now - lt > 5000) || Date.now() - t0 > 150000) break;
	}
	const t = await p.evaluate(() => {
		const nav = performance.getEntriesByType('navigation')[0];
		const fcp = performance.getEntriesByName('first-contentful-paint')[0]?.startTime ?? null;
		// CPU-quiet interactive: the end of the last long task before a 5 s window without one, from FCP.
		let tti = fcp;
		for (const [s, e] of window.__lt) if (e > tti && s - tti < 5000) tti = e;
		// Blocking time (W4): each long task's time over 50 ms, from FCP to the end of the settle window.
		const tbt = window.__lt.reduce((a, [s, e]) => a + (s >= (fcp ?? 0) ? Math.max(0, e - s - 50) : 0), 0);
		return { fcp, lcp: window.__lcp || null, dcl: nav.domContentLoadedEventEnd, load: nav.loadEventEnd, tti, longTasks: window.__lt.length, tbt };
	});
	const rows = [...reqs.values()].filter(Boolean);
	const byKind = {};
	for (const r of rows) {
		const k = (byKind[r.kind] ??= { n: 0, wire: 0, raw: 0 });
		k.n++;
		k.wire += r.wire;
		k.raw += r.raw;
	}
	const total = rows.reduce((a, r) => ({ n: a.n + 1, wire: a.wire + r.wire, raw: a.raw + r.raw }), { n: 0, wire: 0, raw: 0 });
	const big = rows.sort((a, b) => b.raw - a.raw).slice(0, 5).map((r) => ({ path: new URL(r.url).pathname, raw: r.raw, wire: r.wire }));
	await ctx.close();
	return { timing: Object.fromEntries(Object.entries(t).map(([k, v]) => [k, v === null ? null : Math.round(v)])), byKind, total, biggest: big };
}

// Keyboard: Tab through the page from the top; record each stop and whether it shows a focus ring.
async function keyboard(page) {
	const ctx = await browser.newContext({ viewport: { width: 1280, height: 800 } });
	const p = await ctx.newPage();
	await p.goto(base + (page ? `#/${page}` : ''));
	await p.waitForTimeout(2500);
	const stops = [];
	let wrapped = false;
	for (let i = 0; i < 150; i++) {
		await p.keyboard.press('Tab');
		const s = await p.evaluate(() => {
			const e = document.activeElement;
			if (!e || e === document.body) return null;
			const cs = getComputedStyle(e);
			const ring = (cs.outlineStyle !== 'none' && parseFloat(cs.outlineWidth) > 0) || (cs.boxShadow !== 'none' && cs.boxShadow !== '');
			const r = e.getBoundingClientRect();
			const name = (e.getAttribute('aria-label') || e.textContent || e.getAttribute('title') || '').trim().replace(/\s+/g, ' ').slice(0, 40);
			const path = [];
			for (let n = e; n && n !== document.body; n = n.parentElement) path.push(n.tagName + (n.id ? '#' + n.id : '') + Array.prototype.indexOf.call(n.parentElement?.children ?? [], n));
			return { tag: e.tagName.toLowerCase(), name, ring, visible: r.width > 0 && r.height > 0, key: path.join('<') };
		});
		if (!s) {
			if (stops.length) {
				wrapped = true;
				break;
			}
			continue;
		}
		if (stops.length && s.key === stops[0].key) {
			wrapped = true;
			break;
		}
		stops.push(s);
	}
	// A trap: focus stuck on one element, or never returning to the top within 150 Tabs.
	const stuck = stops.some((s, i) => i > 0 && s.key === stops[i - 1].key);
	// The command palette: Ctrl+K opens it (a modal dialog), Escape must close it and return focus.
	await p.keyboard.press('Control+k');
	await p.waitForTimeout(300);
	const open = await p.evaluate(() => !!document.querySelector('dialog[open]'));
	await p.keyboard.press('Escape');
	await p.waitForTimeout(300);
	const closed = await p.evaluate(() => !document.querySelector('dialog[open]'));
	const skip = stops[0] ? /skip/i.test(stops[0].name) : false;
	await ctx.close();
	return {
		stops: stops.length,
		wrapped,
		stuck,
		noRing: stops.filter((s) => !s.ring).map((s) => `${s.tag} "${s.name}"`),
		hidden: stops.filter((s) => !s.visible).map((s) => `${s.tag} "${s.name}"`),
		first: stops.slice(0, 3).map((s) => `${s.tag} "${s.name}"`),
		skipLink: skip,
		palette: { opensWithCtrlK: open, escapeCloses: closed }
	};
}

// Phone: full-page screenshot and layout checks at one width.
async function phone(page, width) {
	const ctx = await browser.newContext({ viewport: { width, height: 800 }, deviceScaleFactor: 1, isMobile: true, hasTouch: true });
	const p = await ctx.newPage();
	await p.goto(base + (page ? `#/${page}` : ''));
	await p.waitForTimeout(4000);
	const file = join(outDir, 'phone', `${pname(page)}-${width}.jpg`);
	await p.screenshot({ path: file, fullPage: true, type: 'jpeg', quality: 55 });
	const r = await p.evaluate(() => {
		const W = innerWidth;
		const desc = (e) => (e.tagName.toLowerCase() + (e.dataset.testid ? `[${e.dataset.testid}]` : e.className && typeof e.className === 'string' ? '.' + e.className.trim().split(/\s+/)[0] : '') + ' "' + (e.textContent || '').trim().replace(/\s+/g, ' ').slice(0, 30) + '"');
		const scrolls = (e) => {
			for (let n = e.parentElement; n; n = n.parentElement) {
				const o = getComputedStyle(n).overflowX;
				if (o === 'auto' || o === 'scroll' || o === 'hidden' || o === 'clip') return true;
			}
			return false;
		};
		const all = [...document.querySelectorAll('body *')].filter((e) => {
			const r = e.getBoundingClientRect();
			return r.width > 0 && r.height > 0 && getComputedStyle(e).visibility !== 'hidden';
		});
		const overflow = all.filter((e) => e.getBoundingClientRect().right > W + 1 && !scrolls(e)).filter((e, _, a) => !a.includes(e.parentElement));
		const inner = all.filter((e) => {
			const o = getComputedStyle(e).overflowX;
			return (o === 'auto' || o === 'scroll') && e.scrollWidth > e.clientWidth + 1;
		});
		const taps = all.filter((e) => e.matches('a[href], button, input, select, textarea, summary, [role=button], [tabindex]:not([tabindex="-1"])'));
		const small = taps.filter((e) => {
			const r = e.getBoundingClientRect();
			return r.height < 44 || r.width < 44;
		});
		const tiny = taps.filter((e) => {
			const r = e.getBoundingClientRect();
			return r.height < 24 || r.width < 24;
		});
		const fonts = all.filter((e) => [...e.childNodes].some((n) => n.nodeType === 3 && n.textContent.trim()) && parseFloat(getComputedStyle(e).fontSize) < 12);
		const clipped = all.filter((e) => {
			const cs = getComputedStyle(e);
			return (cs.textOverflow === 'ellipsis' || cs.overflowX === 'hidden') && e.scrollWidth > e.clientWidth + 1 && [...e.childNodes].some((n) => n.nodeType === 3 && n.textContent.trim());
		});
		return {
			sideScroll: document.documentElement.scrollWidth - W,
			height: document.documentElement.scrollHeight,
			overflow: overflow.slice(0, 6).map(desc),
			overflowCount: overflow.length,
			innerScrollers: inner.slice(0, 6).map(desc),
			innerScrollerCount: inner.length,
			tapTargets: taps.length,
			under44: small.length,
			under24: tiny.length,
			under24Examples: tiny.slice(0, 6).map(desc),
			under12pxText: fonts.length,
			under12pxExamples: [...new Set(fonts.map((e) => getComputedStyle(e).fontSize + ' ' + desc(e)))].slice(0, 4),
			clippedText: clipped.slice(0, 4).map(desc)
		};
	});
	await ctx.close();
	return { file, ...r };
}

try {
	for (const page of PAGES) {
		const r = (report.pages[pname(page)] = {});
		for (const [k, prof] of Object.entries(PROFILES)) {
			r[k] = await measure(page, prof);
			console.error(`${pname(page)} ${k}: ${r[k].total.wire} B wire, ${r[k].total.n} requests, FCP ${r[k].timing.fcp} ms, TTI ${r[k].timing.tti} ms, TBT ${r[k].timing.tbt} ms`);
		}
		// AUDIT_LOAD_ONLY=1 (W4): bytes and timings only, no keyboard walk or phone screenshots.
		if (process.env.AUDIT_LOAD_ONLY === '1') continue;
		r.keyboard = await keyboard(page);
		r.phone = {};
		for (const w of [360, 390, 430]) r.phone[w] = await phone(page, w);
	}
} finally {
	await browser.close();
	server.close();
}
await writeFile(join(outDir, 'audit.json'), JSON.stringify(report, null, 2) + '\n');
console.log(join(outDir, 'audit.json'));
