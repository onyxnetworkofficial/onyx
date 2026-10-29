// Meeting polish MP2: whether this site's own origin takes proposals (/v1/submit). A build made with
// VITE_ONYX_SUBMIT_API=1 always does (local dev with the submit-dev server). Otherwise the page reads
// the site's `intake.json`: every build ships `{"submit": false}`, and scripts/deploy-site.sh writes
// `{"submit": true}` into a release for the hosted node, whose intake is on (scripts/deploy-api.sh,
// ONYX_API_INTAKE). One same-origin GET of a static file, never a request to /v1 before the
// visitor asks; a missing or unreadable file keeps the page offline (it downloads the proposal).
export const submitApi = $state({ on: import.meta.env.VITE_ONYX_SUBMIT_API === '1' });

let asked: Promise<void> | null = null;
export function probeSubmitApi(): Promise<void> {
	if (submitApi.on) return Promise.resolve();
	asked ??= fetch('intake.json', { cache: 'no-store', credentials: 'omit' })
		.then((r) => (r.ok ? r.json() : null))
		.then((j: unknown) => {
			if (j && typeof j === 'object' && (j as { submit?: unknown }).submit === true) submitApi.on = true;
		})
		.catch(() => {});
	return asked;
}
