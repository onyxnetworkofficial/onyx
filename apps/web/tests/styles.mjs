// Computed-style inventory of one page (W10, docs/website-plan.md): what the browser actually
// draws, read from every visible element. Shared by tests/style-audit.mjs (counts, before and
// after) and tests/e2e.mjs (the W10 rules: only token sizes, spacing, radii, shadows, colours).
// `inventory` runs in the page (page.evaluate); it returns plain data.

/** The design tokens of app.css, resolved in the page, by kind. */
export function tokens() {
	const root = getComputedStyle(document.documentElement);
	const names = [];
	for (const s of document.styleSheets) {
		let rules;
		try {
			rules = s.cssRules;
		} catch {
			continue;
		}
		for (const r of rules) if (r.selectorText === ':root' && r.style) for (const p of r.style) if (p.startsWith('--')) names.push(p);
	}
	const probe = document.createElement('i');
	probe.style.position = 'absolute';
	document.body.append(probe);
	const out = { colors: {}, fontSizes: {}, space: {}, radii: {}, shadows: {} };
	for (const n of new Set(names)) {
		const raw = root.getPropertyValue(n).trim();
		probe.removeAttribute('style');
		probe.style.position = 'absolute';
		probe.style.color = `var(${n})`;
		if (probe.style.color && /^(#|rgb|hsl|color-mix)/.test(raw)) out.colors[getComputedStyle(probe).color] = n;
		if (n.startsWith('--text-')) {
			probe.style.fontSize = `var(${n})`;
			out.fontSizes[getComputedStyle(probe).fontSize] = n;
		}
		if (n.startsWith('--sp-')) {
			probe.style.paddingTop = `var(${n})`;
			out.space[getComputedStyle(probe).paddingTop] = n;
		}
		if (n.startsWith('--radius')) {
			probe.style.borderRadius = `var(${n})`;
			out.radii[getComputedStyle(probe).borderTopLeftRadius] = n;
		}
		if (n.startsWith('--shadow')) {
			probe.style.boxShadow = `var(${n})`;
			out.shadows[getComputedStyle(probe).boxShadow] = n;
		}
	}
	probe.remove();
	return out;
}

/** Distinct computed values on the page, with where each was first seen. */
export function inventory() {
	const vis = (e) => {
		const r = e.getBoundingClientRect();
		const s = getComputedStyle(e);
		return r.width > 0 && r.height > 0 && s.visibility !== 'hidden' && s.display !== 'none' && !e.closest('.sr-only, [aria-hidden="true"] .sr-only');
	};
	const where = (e) => {
		const c = typeof e.className === 'string' ? e.className.split(/\s+/).filter((x) => x && !x.startsWith('svelte-')).slice(0, 2).join('.') : '';
		return `${e.tagName.toLowerCase()}${c ? '.' + c : ''} "${(e.textContent || '').trim().replace(/\s+/g, ' ').slice(0, 24)}"`;
	};
	const add = (m, k, e) => {
		if (!m[k]) m[k] = { n: 0, at: where(e) };
		m[k].n++;
	};
	const out = { fontSizes: {}, lineHeights: {}, space: {}, spaceOther: {}, radii: {}, shadows: {}, colors: {} };
	const SPACE = ['padding-top', 'padding-right', 'padding-bottom', 'padding-left', 'margin-top', 'margin-right', 'margin-bottom', 'margin-left', 'row-gap', 'column-gap'];
	const svgShape = new Set(['path', 'rect', 'circle', 'line', 'polyline', 'polygon', 'ellipse']);
	for (const e of document.querySelectorAll('body *')) {
		if (['SCRIPT', 'STYLE', 'TEMPLATE', 'NOSCRIPT'].includes(e.tagName) || !vis(e)) continue;
		const s = getComputedStyle(e);
		const inSvg = e instanceof SVGElement;
		const hasText = inSvg ? e.tagName === 'text' : [...e.childNodes].some((n) => n.nodeType === 3 && n.textContent.trim());
		if (hasText) {
			add(out.fontSizes, s.fontSize, e);
			const lh = s.lineHeight === 'normal' ? 'normal' : (parseFloat(s.lineHeight) / parseFloat(s.fontSize)).toFixed(2);
			if (!inSvg) add(out.lineHeights, lh, e);
			if (!/, 0\)$/.test(s.color)) add(out.colors, s.color, e);
		}
		if (inSvg) {
			if (svgShape.has(e.tagName)) {
				if (s.fill !== 'none' && !s.fill.startsWith('url(') && e.tagName !== 'line') add(out.colors, s.fill, e);
				if (s.stroke !== 'none' && !s.stroke.startsWith('url(')) add(out.colors, s.stroke, e);
			}
			continue;
		}
		// Spacing as authored (computed, not used, value): auto, % and calc() are layout, not spacing.
		const map = e.computedStyleMap();
		const inline = e.getAttribute('style') ?? '';
		for (const p of SPACE) {
			if (inline.includes(p.split('-')[0]) || (p.endsWith('gap') && !/flex|grid/.test(s.display))) continue;
			const v = map.get(p);
			if (!v || !('unit' in v)) {
				if (v && v.toString() !== 'auto' && v.toString() !== 'normal') add(out.spaceOther, `${p} ${v}`, e);
				continue;
			}
			if (v.unit !== 'px') {
				if (v.unit !== 'percent' && v.value !== 0) add(out.spaceOther, `${p} ${v}`, e);
				continue;
			}
			if (v.value !== 0 && !e.classList.contains('sr-only')) add(out.space, `${Math.abs(v.value)}px`, e);
		}
		for (const c of ['border-top-left-radius', 'border-top-right-radius', 'border-bottom-right-radius', 'border-bottom-left-radius']) {
			const v = s.getPropertyValue(c);
			if (v !== '0px') add(out.radii, v, e);
		}
		if (s.boxShadow !== 'none') add(out.shadows, s.boxShadow, e);
		if (!/rgba\(0, 0, 0, 0\)|transparent/.test(s.backgroundColor)) add(out.colors, s.backgroundColor, e);
		for (const side of ['top', 'right', 'bottom', 'left']) {
			if (parseFloat(s.getPropertyValue(`border-${side}-width`)) > 0 && s.getPropertyValue(`border-${side}-style`) !== 'none') {
				const c = s.getPropertyValue(`border-${side}-color`);
				if (!/rgba\(0, 0, 0, 0\)/.test(c)) add(out.colors, c, e);
			}
		}
	}
	return out;
}
