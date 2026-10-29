<script lang="ts">
	export type PillState = 'idle' | 'run' | 'good' | 'bad' | 'warn';
	// `live` marks the one pill per panel whose change a screen reader should
	// announce; the rest stay quiet so a run of 7 checks is not 7 interruptions.
	let { state = 'idle', text, live = false }: { state?: PillState; text: string; live?: boolean } = $props();
</script>

<span class="pill {state}" role={live ? 'status' : undefined} aria-live={live ? 'polite' : undefined}>{#if state === 'run'}<span class="dot"></span>{/if}{text}</span>

<style>
	.pill {
		display: inline-flex;
		align-items: center;
		gap: var(--sp-2);
		font-size: var(--text-sm);
		font-weight: 600;
		border-radius: var(--radius-pill);
		padding: var(--sp-1) var(--sp-3);
		border: 1px solid currentColor;
		color: var(--muted);
		/* NL15 review: one line while it fits, wrapped inside its row when wider (a long check
		   result widened the page at 390 px). */
		max-width: 100%;
		/* W12 round 11: a count in a pill ("2 of 3 roots agree") changes in place: tabular figures. */
		font-variant-numeric: tabular-nums;
	}
	.good {
		color: var(--good);
	}
	.bad {
		color: var(--bad);
	}
	.warn {
		color: var(--warn);
	}
	.run {
		color: var(--accent);
	}
	.dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: currentColor;
	}
	@media (prefers-reduced-motion: no-preference) {
		.dot {
			animation: pulse 1s ease-in-out infinite;
		}
	}
	@keyframes pulse {
		50% {
			opacity: 0.25;
		}
	}
</style>
