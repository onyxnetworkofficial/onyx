// Motion (W3, WCAG 2.2.2 and 2.3.3): the stone slab, the live-block strip and the
// pipeline beams move on their own. They stop when the system asks for reduced motion,
// or when the viewer presses the header's pause button (remembered in localStorage).
// `motion.off` is true in either case; the layout mirrors it on <html data-motion>.
const KEY = 'onyx-motion';

class Motion {
	system = $state(false);
	paused = $state(false);
	off = $derived(this.system || this.paused);
	constructor() {
		if (typeof matchMedia !== 'function') return;
		const q = matchMedia('(prefers-reduced-motion: reduce)');
		this.system = q.matches;
		q.addEventListener('change', (e) => (this.system = e.matches));
		try {
			this.paused = localStorage.getItem(KEY) === 'off';
		} catch {
			/* storage blocked: start with motion on */
		}
	}
	setPaused(p: boolean) {
		this.paused = p;
		try {
			if (p) localStorage.setItem(KEY, 'off');
			else localStorage.removeItem(KEY);
		} catch {
			/* storage blocked: the choice lasts for this visit */
		}
	}
}

export const motion = new Motion();
