// Keyboard for the charts (W3, WCAG 2.1.1): a chart is one Tab stop; Left/Right step the
// tooltip through the points, Home/End jump to the ends, Esc hides it. Returns the new
// hovered index, or undefined when the key is not the chart's.
export function stepKey(e: KeyboardEvent, hover: number | null, n: number): number | null | undefined {
	if (n === 0) return undefined;
	const last = n - 1;
	let k: number | null;
	if (e.key === 'ArrowRight') k = hover === null ? 0 : Math.min(last, hover + 1);
	else if (e.key === 'ArrowLeft') k = hover === null ? last : Math.max(0, hover - 1);
	else if (e.key === 'Home') k = 0;
	else if (e.key === 'End') k = last;
	else if (e.key === 'Escape' && hover !== null) k = null;
	else return undefined;
	e.preventDefault();
	return k;
}
