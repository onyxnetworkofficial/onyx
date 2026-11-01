#!/usr/bin/env node
// One CommonJS file from onyx-contrib's ES modules, for Node's single executable application
// (SEA, which runs one CommonJS script) — CL10, ADR-0061. No bundler dependency: this handles only
// the module syntax our own files use and refuses anything else, so a new construct fails the
// build instead of shipping a program that breaks at run time.
//
//   node scripts/sea-bundle.mjs <out.cjs>
//
// What it does: follows the relative imports from apps/contrib/src/cli.mjs; wraps each module in a
// function; turns `import {a, b as c} from './x.mjs'` into a lookup of that module's exports and
// `import … from 'node:…'` into require; `export function|const|let|class NAME` into getters (live
// bindings); `import.meta.url` into a virtual file URL (file:///onyx-sea/<path>); dynamic
// `import('node:…')` into a resolved require. The data files the code reads next to itself
// (apps/agent/openapi.json and catalog.json, apps/contrib/ui/*) are embedded and served to those
// virtual URLs by a thin wrapper of node:fs; every other path goes to the real file system.
// Refused: default exports, `export * / export {…} from`, side-effect imports, import cycles, and
// a module that does not compile as a function body (top-level await, for one).
import { readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const ENTRY = 'apps/contrib/src/cli.mjs';
const ASSETS = ['apps/agent/openapi.json', 'apps/agent/catalog.json', ...readdirSync(join(ROOT, 'apps/contrib/ui')).sort().map((f) => `apps/contrib/ui/${f}`)];
const VROOT = 'file:///onyx-sea/';

const fail = (m) => {
	throw new Error(`sea-bundle: ${m}`);
};

/** Transform one module's source. Returns { body, deps: [relative path], names }. */
export function transform(rel, src) {
	const deps = [];
	const names = [];
	let out = src.replace(/^#![^\n]*\n/, '\n');
	// Imports: whole statements at the start of a line, possibly over several lines.
	out = out.replace(/^import\s+([^'";]+?)\s+from\s+'([^']+)';?[ \t]*$/gm, (_, clause, spec) => {
		clause = clause.trim();
		let target;
		if (spec.startsWith('node:')) target = `__builtin(${JSON.stringify(spec)})`;
		else if (spec.startsWith('./') || spec.startsWith('../')) {
			const dep = relative(ROOT, resolve(ROOT, dirname(rel), spec)).split('\\').join('/');
			if (!dep.endsWith('.mjs')) fail(`${rel}: import of ${spec} (only .mjs files)`);
			deps.push(dep);
			target = `__load(${JSON.stringify(dep)})`;
		} else fail(`${rel}: import from a package (${spec})`);
		if (clause.startsWith('* as ')) return `const ${clause.slice(5).trim()} = ${target};`;
		if (clause === 'vm' && spec === 'node:vm') return `const vm = ${target};`;
		const m = /^\{([^}]*)\}$/.exec(clause);
		if (!m) fail(`${rel}: import clause "${clause}" (named imports or * as only)`);
		const parts = m[1]
			.split(',')
			.map((s) => s.trim())
			.filter(Boolean)
			.map((s) => {
				const [a, b] = s.split(/\s+as\s+/);
				if (!/^[A-Za-z_$][\w$]*$/.test(a) || (b && !/^[A-Za-z_$][\w$]*$/.test(b))) fail(`${rel}: import name "${s}"`);
				return b ? `${a}: ${b}` : a;
			});
		return `const { ${parts.join(', ')} } = ${target};`;
	});
	if (/^import\s/m.test(out)) fail(`${rel}: an import statement this bundler does not read`);
	// Dynamic imports of built-ins.
	out = out.replace(/\bimport\(\s*'(node:[a-z_/]+)'\s*\)/g, (_, spec) => `Promise.resolve(__builtin(${JSON.stringify(spec)}))`);
	if (/\bimport\s*\(/.test(out)) fail(`${rel}: a dynamic import other than a node: built-in`);
	out = out.replace(/\bimport\.meta\.url\b/g, '__meta_url');
	if (/\bimport\.meta\b/.test(out)) fail(`${rel}: import.meta other than import.meta.url`);
	// Exports.
	out = out.replace(/^export\s+(async\s+function\*?|function\*?|const|let|class)\s+([A-Za-z_$][\w$]*)/gm, (_, kind, name) => {
		names.push(name);
		return `${kind} ${name}`;
	});
	out = out.replace(/^export\s*\{([^}]*)\};?[ \t]*$/gm, (_, list) => {
		for (const s of list.split(',').map((x) => x.trim()).filter(Boolean)) {
			const [a, b] = s.split(/\s+as\s+/);
			names.push(b ? `${b}=${a}` : a);
		}
		return '';
	});
	if (/^export\s/m.test(out)) fail(`${rel}: an export form this bundler does not read (default, *, or from)`);
	const getters = names.map((n) => {
		const [as, local] = n.includes('=') ? n.split('=') : [n, n];
		return `Object.defineProperty(__exports, ${JSON.stringify(as)}, { enumerable: true, get: () => ${local} });`;
	});
	const body = `${getters.join('\n')}\n${out}`;
	try {
		new vm.Script(`(function (__exports, __meta_url) {\n${body}\n})`, { filename: rel });
	} catch (e) {
		fail(`${rel} does not compile as a function body: ${e.message}`);
	}
	return { body, deps, names };
}

/** The bundle's text. */
export function bundle() {
	const mods = new Map();
	const order = [];
	const visiting = new Set();
	const visit = (rel, chain) => {
		if (mods.has(rel)) return;
		if (visiting.has(rel)) fail(`import cycle: ${[...chain, rel].join(' -> ')}`);
		visiting.add(rel);
		const t = transform(rel, readFileSync(join(ROOT, rel), 'utf8'));
		for (const d of t.deps) visit(d, [...chain, rel]);
		visiting.delete(rel);
		mods.set(rel, t);
		order.push(rel);
	};
	visit(ENTRY, []);
	const assets = Object.fromEntries(ASSETS.map((a) => [a, readFileSync(join(ROOT, a)).toString('base64')]));
	const parts = [
		`'use strict';`,
		`// onyx-contrib as one file for Node's single executable application (scripts/sea-bundle.mjs; CL10).`,
		`// Generated: do not edit. Modules: ${order.length}; embedded files: ${ASSETS.length}.`,
		`const __VROOT = ${JSON.stringify(VROOT)};`,
		`const __ASSETS = ${JSON.stringify(assets)};`,
		`const __realFs = require('node:fs');`,
		`const __realUrl = require('node:url');`,
		`function __virtual(p) {`,
		`  const s = p instanceof URL ? p.href : typeof p === 'string' ? p : '';`,
		`  if (s.startsWith(__VROOT)) return s.slice(__VROOT.length);`,
		`  if (s.startsWith('/onyx-sea/')) return s.slice('/onyx-sea/'.length);`,
		`  return null;`,
		`}`,
		`function __asset(rel) {`,
		`  if (!Object.prototype.hasOwnProperty.call(__ASSETS, rel)) { const e = new Error('ENOENT: no such embedded file, ' + rel); e.code = 'ENOENT'; throw e; }`,
		`  return Buffer.from(__ASSETS[rel], 'base64');`,
		`}`,
		`const __fs = Object.assign({}, __realFs, {`,
		`  readFileSync(p, o) { const v = __virtual(p); if (v === null) return __realFs.readFileSync(p, o); const b = __asset(v); const enc = typeof o === 'string' ? o : o && o.encoding; return enc ? b.toString(enc) : b; },`,
		`  existsSync(p) { const v = __virtual(p); return v === null ? __realFs.existsSync(p) : Object.prototype.hasOwnProperty.call(__ASSETS, v); },`,
		`  realpathSync: Object.assign(function realpathSync(p, o) { return __virtual(p) === null ? __realFs.realpathSync(p, o) : '/onyx-sea/' + __virtual(p); }, { native: __realFs.realpathSync.native }),`,
		`});`,
		`const __url = Object.assign({}, __realUrl, { fileURLToPath(u) { const v = __virtual(u); return v === null ? __realUrl.fileURLToPath(u) : '/onyx-sea/' + v; } });`,
		`function __builtin(name) { if (name === 'node:fs') return __fs; if (name === 'node:url') return __url; return require(name); }`,
		`const __defs = {};`,
		`const __cache = {};`,
		`function __load(rel) { if (__cache[rel]) return __cache[rel]; const e = (__cache[rel] = {}); __defs[rel](e, __VROOT + rel); return e; }`,
	];
	for (const rel of order) parts.push(`__defs[${JSON.stringify(rel)}] = function (__exports, __meta_url) {\n${mods.get(rel).body}\n};`);
	parts.push(
		`let __sea = false;`,
		`try { __sea = require('node:sea').isSea(); } catch {}`,
		`module.exports = __load(${JSON.stringify(ENTRY)});`,
		`if (require.main === module || __sea) {`,
		`  module.exports.main(process.argv.slice(2)).then((code) => { if (code !== null) process.exitCode = code; });`,
		`}`,
	);
	return parts.join('\n') + '\n';
}

const self = fileURLToPath(import.meta.url);
if (process.argv[1] && resolve(process.argv[1]) === self) {
	const out = process.argv[2];
	if (!out) {
		process.stderr.write('usage: node scripts/sea-bundle.mjs <out.cjs>\n');
		process.exit(2);
	}
	const text = bundle();
	writeFileSync(out, text);
	process.stdout.write(`sea-bundle: ${out} (${text.length} bytes)\n`);
}
