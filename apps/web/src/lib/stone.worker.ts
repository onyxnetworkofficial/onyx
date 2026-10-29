// Draws the onyx slab's stone off the main thread (W4, src/lib/stone.ts): the page hands over
// its canvas once, then only sends what changed (size, motion, on screen).
import { stone, type StoneState } from './stone.ts';

let s: ReturnType<typeof stone> | null = null;
self.onmessage = (e: MessageEvent<{ canvas?: OffscreenCanvas; state: Partial<StoneState> }>) => {
	if (e.data.canvas) {
		s = stone(e.data.canvas, e.data.state as StoneState);
		// For the browser tests (tests/w4.mjs): what the stone is doing, read in this worker.
		(self as unknown as { __stone: typeof s }).__stone = s;
	} else s?.set(e.data.state);
};
