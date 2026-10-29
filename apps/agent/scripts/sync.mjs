#!/usr/bin/env node
// Copy what the toolkit ships with into apps/agent, so it runs outside this repository (NL21):
// openapi.json (byte for byte docs/api/openapi.json) and catalog.json (the website's dataset
// list, apps/web/src/lib/catalog.ts, as data). `--check` only compares; scripts/check-openapi.py
// and the toolkit's tests fail on a stale copy.
// Usage: node apps/agent/scripts/sync.mjs [--check]   (Node 22.18 or later: it reads a .ts file)
import { readFileSync, writeFileSync } from 'node:fs';

const root = new URL('../../../', import.meta.url);
const here = new URL('../', import.meta.url);

export async function wanted() {
	const { DATASETS, LEVEL_NAME } = await import(new URL('apps/web/src/lib/catalog.ts', root).href);
	const datasets = DATASETS.map((d) => {
		const out = { id: d.id, title: d.title, what: d.what, level: d.level, source: d.source, unit: d.unit, rights: d.rights };
		if (d.api) out.api = d.api;
		if (d.labelDerived) out.labelDerived = true;
		return out;
	});
	const catalog = { version: 1, generatedBy: 'node apps/agent/scripts/sync.mjs', levels: LEVEL_NAME, datasets };
	return {
		'openapi.json': readFileSync(new URL('docs/api/openapi.json', root), 'utf8'),
		'catalog.json': JSON.stringify(catalog, null, '\t') + '\n',
	};
}

if (process.argv[1] && new URL(import.meta.url).pathname === process.argv[1]) {
	const check = process.argv.includes('--check');
	const stale = [];
	for (const [name, text] of Object.entries(await wanted())) {
		const at = new URL(name, here);
		let have = null;
		try {
			have = readFileSync(at, 'utf8');
		} catch {}
		if (have === text) continue;
		if (check) stale.push(`apps/agent/${name}`);
		else writeFileSync(at, text);
	}
	if (stale.length) {
		console.error(`stale: ${stale.join(', ')} (run node apps/agent/scripts/sync.mjs)`);
		process.exit(1);
	}
	console.log(check ? 'agent copies ok' : 'agent copies written');
}
