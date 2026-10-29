// W12 round 13: sections below the first screen fade in as they scroll into view (app.css,
// `[data-fade]`). The fade is never on the first screen: each section's first observation decides,
// and one already in view (or above it) then is left alone, so no text a reader sees on load is ever
// dimmed. A section first seen below the screen waits at opacity 0 (`data-fade="wait"`), which axe
// treats as hidden, not as low contrast, and turns `in` once the reader reaches it: once its top is
// above the bottom of the viewport, in view or already scrolled past. CSS shows every section at
// once when the system asks for reduced motion or the viewer pauses motion (<html data-motion="off">),
// and in print. Sections a page adds later (W4's lazy sections) are watched as they appear.
//
// Round 14 review: "reached", not "seen intersecting". An IntersectionObserver samples positions only
// at rendered frames, so a scroll that passes a section between two frames (a fling, a jump, or a
// machine too loaded to draw every frame: e2e on a loaded host left a Home section at opacity 0)
// never reports it in view, and it stayed waiting. Each scroll and resize therefore reveals every
// waiting section whose top is above the viewport's bottom edge, whatever frames were skipped.
export function fadeSections(root: HTMLElement): () => void {
	if (typeof IntersectionObserver !== 'function') return () => {};
	const seen = new WeakSet<Element>();
	const waiting = new Set<HTMLElement>();
	const show = (el: HTMLElement) => {
		el.dataset.fade = 'in';
		waiting.delete(el);
		io.unobserve(el);
	};
	const reveal = () => {
		for (const el of waiting) if (!el.isConnected || el.getBoundingClientRect().top < innerHeight) show(el);
	};
	const io = new IntersectionObserver((entries) => {
		for (const e of entries) {
			const el = e.target as HTMLElement;
			const reached = e.isIntersecting || e.boundingClientRect.top < (e.rootBounds?.bottom ?? innerHeight);
			if (!seen.has(el)) {
				seen.add(el);
				if (reached) {
					io.unobserve(el);
					continue;
				}
				el.dataset.fade = 'wait';
				waiting.add(el);
			} else if (reached) show(el);
		}
		reveal();
	});
	const onMove = () => {
		if (waiting.size) reveal();
	};
	addEventListener('scroll', onMove, { passive: true });
	addEventListener('resize', onMove, { passive: true });
	const watched = new WeakSet<Element>();
	const scan = () => {
		for (const s of root.querySelectorAll('section.section')) {
			if (watched.has(s)) continue;
			watched.add(s);
			io.observe(s);
		}
	};
	const mo = new MutationObserver(scan);
	mo.observe(root, { childList: true, subtree: true });
	scan();
	return () => {
		removeEventListener('scroll', onMove);
		removeEventListener('resize', onMove);
		mo.disconnect();
		io.disconnect();
	};
}
