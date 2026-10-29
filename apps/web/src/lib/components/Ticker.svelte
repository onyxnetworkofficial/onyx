<script lang="ts">
	// Counts up to `value` once when mounted (Magic UI "number ticker" idea).
	import { onMount } from 'svelte';
	import { motion } from '#lib/motion.svelte.ts';
	import { group } from '#lib/format.ts';
	let { value, format = (n: number) => group(Math.round(n)) }: { value: number; format?: (n: number) => string } = $props();
	let shown = $state(0);
	onMount(() => {
		if (motion.off) {
			shown = value;
			return;
		}
		const t0 = performance.now();
		let raf = 0;
		const step = (t: number) => {
			const k = Math.min(1, (t - t0) / 900);
			shown = value * (1 - Math.pow(1 - k, 3));
			if (k < 1) raf = requestAnimationFrame(step);
		};
		raf = requestAnimationFrame(step);
		return () => cancelAnimationFrame(raf);
	});
	// Later value changes jump straight to the new number.
	let first = true;
	$effect(() => {
		const v = value;
		if (first) {
			first = false;
			return;
		}
		shown = v;
	});
</script>

<span class="ticker">{format(shown)}</span>

<style>
	.ticker {
		font-variant-numeric: tabular-nums;
	}
</style>
