// Runs the Rust verifier (crates compiled to WebAssembly) off the main thread,
// so a recipe rerun does not freeze the page.
import init, * as verify from './pkg/onyx_web_verify.js';

const ready = init();
type Fn = (...args: unknown[]) => unknown;

self.onmessage = async (e: MessageEvent<{ id: number; fn: string; args: unknown[] }>) => {
	const { id, fn, args } = e.data;
	try {
		await ready;
		const f = (verify as unknown as Record<string, Fn>)[fn];
		if (typeof f !== 'function') throw new Error(`no verifier function ${fn}`);
		self.postMessage({ id, ok: true, value: f(...args) });
	} catch (err) {
		self.postMessage({ id, ok: false, error: err instanceof Error ? err.message : String(err) });
	}
};
