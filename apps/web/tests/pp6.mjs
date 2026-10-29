// Pre-public loop PP6 (docs/pre-public-loop.md): the API page's Reference lists every route of
// docs/api/openapi.json (the file the node serves at /v1/openapi.json and scripts/check-openapi.py
// keeps equal to the code), its error codes, and the download link gives the same bytes. Called
// by tests/e2e.mjs with a built site; the reading of the spec also runs alone: node tests/pp6.mjs.
import { readFile } from 'node:fs/promises';

const O = await import(new URL('../src/lib/openapi.ts', import.meta.url).href);
const specText = await readFile(new URL('../../../docs/api/openapi.json', import.meta.url), 'utf8');
const spec = JSON.parse(specText);
const METHODS = ['get', 'post', 'put', 'patch', 'delete'];
const want = Object.entries(spec.paths).flatMap(([p, item]) => METHODS.filter((m) => item[m]).map((m) => `${m.toUpperCase()} ${p}`));

export function pp6Reading() {
	const ops = O.operations(spec);
	const got = ops.map((o) => `${o.method} ${o.path}`);
	const bad = [];
	if (JSON.stringify(got) !== JSON.stringify(want)) bad.push(`operations ${got.length} != ${want.length}`);
	for (const o of ops) {
		if (!o.summary || !o.description) bad.push(`${o.id}: no summary or description`);
		if (!o.responses.some((r) => r.status.startsWith('2'))) bad.push(`${o.id}: no success response`);
		const tmpl = [...o.path.matchAll(/\{([^}]+)\}/g)].map((m) => m[1]).sort();
		const named = o.params.filter((p) => p.in === 'path').map((p) => p.name).sort();
		if (JSON.stringify(tmpl) !== JSON.stringify(named)) bad.push(`${o.id}: path parameters`);
	}
	const grouped = O.byTag(spec).reduce((n, g) => n + g.ops.length, 0);
	if (grouped !== ops.length) bad.push(`grouped ${grouped} != ${ops.length}: an operation has no listed tag`);
	const codes = O.errorCodes(spec);
	if (codes.length !== Object.keys(spec['x-onyx-error-codes']).length) bad.push('error codes');
	if (JSON.stringify(O.runs('a `b` c')) !== JSON.stringify([{ code: false, text: 'a ' }, { code: true, text: 'b' }, { code: false, text: ' c' }])) bad.push('runs');
	return { bad, n: ops.length, codes: codes.length };
}

export async function pp6(browser, base, expect) {
	{
		const { bad, n, codes } = pp6Reading();
		expect(bad.length === 0, `PP6 openapi.ts reads all ${n} operations and ${codes} error codes (${bad.slice(0, 3).join('; ')})`);
	}
	for (const width of [1280, 390]) {
		const ctx = await browser.newContext({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
		const pg = await ctx.newPage();
		await pg.goto(base + '#/api');
		const ref = pg.getByTestId('api-reference');
		await ref.waitFor();
		const ops = await pg.getByTestId('api-op').evaluateAll((els) => els.map((e) => e.querySelector('summary').innerText.replace(/\s+/g, ' ').trim()));
		const missing = want.filter((w) => !ops.some((t) => t.startsWith(w + ' ')));
		expect(ops.length === want.length && missing.length === 0, `PP6 ${width}: the Reference lists all ${want.length} operations, method and path (${ops.length} shown; missing ${missing.join(', ') || 'none'})`);
		const paid = await pg.getByTestId('api-op-paid').count();
		const wantPaid = Object.values(spec.paths).flatMap((i) => METHODS.map((m) => i[m]).filter(Boolean)).filter((o) => o['x-onyx-paid']).length;
		expect(paid === wantPaid, `PP6 ${width}: ${wantPaid} paid routes are marked paid (${paid})`);
		// Every fold open: nothing wider than the screen, the error codes all listed.
		await pg.evaluate(() => document.querySelectorAll('[data-testid="api-reference"] details').forEach((d) => (d.open = true)));
		const over = await pg.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1);
		expect(!over, `PP6 ${width}: with every fold open the page does not scroll sideways`);
		const codes = await pg.getByTestId('api-error-code').count();
		expect(codes === Object.keys(spec['x-onyx-error-codes']).length, `PP6 ${width}: all ${codes} error codes are listed`);
		const receipt = (await pg.locator('[data-op="getReceipt"]').innerText()).replace(/\s+/g, ' ');
		expect(receipt.includes('id path, required') && receipt.includes('200') && receipt.includes('ReceiptStatus'), `PP6 ${width}: an operation shows its parameters and responses (${receipt.slice(0, 160)})`);
		if (width === 1280) {
			const href = await pg.getByTestId('api-openapi-link').getAttribute('href');
			const served = await pg.evaluate(async (h) => (await fetch(new URL(h, document.baseURI))).text(), href);
			// The build strips the whitespace of every JSON file (scripts/build-web.sh); the content is compared.
			let same = false;
			try { same = JSON.stringify(JSON.parse(served)) === JSON.stringify(spec); } catch {}
			expect(same, 'PP6: the download link serves the same description as docs/api/openapi.json');
		}
		await ctx.close();
	}
}

if (import.meta.url === `file://${process.argv[1]}`) {
	const r = pp6Reading();
	if (r.bad.length) throw new Error(r.bad.join('; '));
	console.log(`ok PP6 openapi.ts reads all ${r.n} operations and ${r.codes} error codes`);
}
