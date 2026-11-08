import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig, type Plugin } from 'vite';

// Review L14-R1 (THREATS L14-R1-1): Svelte hoists every block's static HTML into a module-level
// `$.from_html(...)` call. The bundler drops a branch behind a build-time constant
// (`{#if FALLBACK}`, `{#if PUBLIC}`), but kept those calls, so the dead branch's text still
// shipped (a rights-fallback build carried the cleared Reserves page's prose and its figures).
// `from_html` and its siblings only wrap a string in a lazy factory (Svelte marks them
// `#__NO_SIDE_EFFECTS__`), so marking each call pure is safe: an unused template is dropped,
// a used one is kept as is.
const TEMPLATE_CALL = /\$\.(from_html|from_svg|from_mathml|from_tree)\(/g;
function pureTemplates(): Plugin {
	return {
		name: 'onyx-pure-svelte-templates',
		enforce: 'post',
		transform(code, id) {
			if (!/\.svelte(\?|$)/.test(id) || !TEMPLATE_CALL.test(code)) return null;
			TEMPLATE_CALL.lastIndex = 0;
			return { code: code.replace(TEMPLATE_CALL, '/* @__PURE__ */ $.$1('), map: null };
		}
	};
}

// No @types/node here: read the one variable through globalThis.
const SUBMIT_PROXY = (globalThis as { process?: { env: Record<string, string | undefined> } }).process?.env.ONYX_SUBMIT_PROXY;

export default defineConfig({
	plugins: [
		sveltekit({
			// One index.html with hash routes (#/explorer): works on any static host
			// and as a single published page (ADR-0020).
			adapter: adapter({ fallback: 'index.html' }),
			router: { type: 'hash' },
			paths: { relative: true },
			// Not `_app`: some hosts reserve names that start with an underscore.
			appDir: 'app',
			// A Content-Security-Policy in a meta tag of every page (launch hardening, THREATS
			// WEB-CSP-1): scripts only from this origin plus the hashes of SvelteKit's own inline
			// bootstrap (computed at build), WebAssembly for the verifier, workers, images and fonts
			// from this origin. Fetches may also go to any https origin: the Markets and insurance
			// pages read the chain from an RPC the reader may pick (sepolia.base.org by default).
			// Inline style attributes stay allowed (Svelte writes them); plugins, base and form
			// targets are locked. frame-ancestors cannot be a meta directive: the server's
			// X-Frame-Options DENY covers it.
			csp: {
				mode: 'hash',
				directives: {
					'default-src': ['self'],
					'script-src': ['self', 'wasm-unsafe-eval'],
					'style-src': ['self', 'unsafe-inline'],
					'img-src': ['self', 'data:'],
					'font-src': ['self'],
					// A reader's own node on this machine over plain http too (the RPC fields
					// offer http://127.0.0.1 and localhost; review of WEB-CSP-1, finding 1).
					'connect-src': ['self', 'https:', 'http://127.0.0.1:*', 'http://localhost:*'],
					'worker-src': ['self'],
					'object-src': ['none'],
					'base-uri': ['self'],
					'form-action': ['self']
				}
			}
		}),
		pureTemplates()
	],
	worker: { format: 'es' },
	// ROADMAP S1: in `pnpm dev` only, ONYX_SUBMIT_PROXY=http://127.0.0.1:8788 forwards /v1/submit to the
	// local submit-dev server (crates/onyx-api/examples/submit-dev.rs), so a VITE_ONYX_SUBMIT_API=1 page
	// posts same-origin. Builds never proxy anything.
	server: SUBMIT_PROXY ? { proxy: { '/v1/submit': SUBMIT_PROXY } } : {}
});
