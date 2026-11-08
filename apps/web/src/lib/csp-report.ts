// THREATS WEB-CSP-2: a web worker runs under its own Content-Security-Policy (the server's
// `@workers` header, deploy/caddy/onyx-web.Caddyfile). A refusal inside a worker is reported in the
// worker, never to the page, so each worker logs it with the tag the browser tests record
// (tests/e2e.mjs, CSP-VIOLATION): a worker the policy breaks fails the run.
export function reportRefusals(): void {
	self.addEventListener('securitypolicyviolation', (e) =>
		console.error(`CSP-VIOLATION ${e.violatedDirective} ${e.blockedURI} in ${self.location.pathname}`)
	);
}
