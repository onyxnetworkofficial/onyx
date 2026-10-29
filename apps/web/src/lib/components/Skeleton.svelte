<script lang="ts">
	// Placeholder cards while the data loads or the verifier checks it (W6: each one names what
	// it waits for). W10: one look everywhere: on the page, card-shaped bones like the panels
	// they stand in for; inside a panel, bare bones (no card in a card). The shimmer stops under
	// reduced motion or the header's pause button (app.css).
	let { rows = 3, label }: { rows?: number; label: string } = $props();
</script>

<div class="sk" role="status" aria-label={label}>
	{#each Array(rows) as _, i (i)}
		<div class="bone"><i class="t"></i><i class="l"></i><i class="l short"></i></div>
	{/each}
</div>

<style>
	.sk {
		display: grid;
		gap: var(--sp-4);
		padding-block: var(--sp-8) var(--sp-6);
	}
	:global(.panel) > .sk {
		padding-block: 0;
	}
	.bone {
		display: grid;
		gap: var(--sp-3);
		padding: var(--sp-6);
		border: 1px solid var(--line);
		border-radius: var(--radius-lg);
		background: var(--panel);
	}
	:global(.panel) .bone {
		padding: 0;
		border: 0;
		background: none;
	}
	i {
		display: block;
		height: 12px;
		border-radius: var(--radius-sm);
		background: linear-gradient(90deg, var(--code-bg), var(--hover), var(--code-bg));
		background-size: 200% 100%;
		animation: shimmer 1.4s ease-in-out infinite;
	}
	.t {
		height: 22px;
		width: 30%;
	}
	.short {
		width: 55%;
	}
	@keyframes shimmer {
		to {
			background-position: -200% 0;
		}
	}
</style>
