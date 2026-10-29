<script lang="ts">
	// Progress ring: passed (accent) and failed (bad) out of total.
	let { passed, failed, total }: { passed: number; failed: number; total: number } = $props();
	const R = 34;
	const C = 2 * Math.PI * R;
	const pass = $derived((passed / total) * C);
	const fail = $derived((failed / total) * C);
</script>

<svg viewBox="0 0 84 84" width="84" height="84" role="img" aria-label="{passed} of {total} checks passed">
	<circle cx="42" cy="42" r={R} fill="none" stroke="var(--line)" stroke-width="7" />
	<circle cx="42" cy="42" r={R} fill="none" stroke="var(--good)" stroke-width="7" stroke-linecap="round"
		stroke-dasharray="{pass} {C}" transform="rotate(-90 42 42)" class="arc" />
	{#if failed}
		<circle cx="42" cy="42" r={R} fill="none" stroke="var(--bad)" stroke-width="7"
			stroke-dasharray="{fail} {C}" stroke-dashoffset={-pass} transform="rotate(-90 42 42)" class="arc" />
	{/if}
	<text x="42" y="47" text-anchor="middle" style:font-family="var(--mono)" style:font-size="var(--text-md)" style:fill="var(--ink)">{passed}/{total}</text>
</svg>

<style>
	@media (prefers-reduced-motion: no-preference) {
		.arc {
			transition: stroke-dasharray var(--dur-slow) var(--ease);
		}
	}
</style>
