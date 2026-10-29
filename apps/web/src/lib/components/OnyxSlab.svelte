<script lang="ts">
	// The page opener: polished onyx, rendered live with a small WebGL shader
	// (warped bands, jade veins, a soft glow; src/lib/stone.ts). Still image when the viewer
	// prefers reduced motion or pauses motion (header button, W3); 2D canvas fallback without WebGL.
	// W4: drawn in a worker on an OffscreenCanvas where the browser can, so compiling the
	// shader never holds up the page.
	import { onMount, tick, type Snippet } from 'svelte';
	import { motion } from '#lib/motion.svelte.ts';
	import { stone, type StoneState } from '#lib/stone.ts';
	import { page } from '$app/state';
	import { NAV } from '#lib/nav.ts';
	// W12 round 10: one opener height on every page (the 404 page alone kept a 96 / 80 px slab, the
	// only use of two spacing steps); the `compact` switch is gone.
	let { children }: { children: Snippet } = $props();
	let canvas = $state.raw() as unknown as HTMLCanvasElement;
	// W12: the page's place in the nav, shown above its title (not on Home, whose hero opens it).
	const at = $derived(NAV.findIndex((n) => n.path === page.route.id));
	// Passes a change (size, motion, on screen) to the stone, wherever it runs.
	let send: ((s: Partial<StoneState>) => void) | null = null;
	$effect(() => {
		const off = motion.off;
		send?.({ motionOff: off });
	});

	/** WebGL on an OffscreenCanvas in a worker (Chrome, Firefox; Safari 17+). */
	function offscreenGl(): boolean {
		if (typeof Worker !== 'function' || typeof OffscreenCanvas !== 'function' || !('transferControlToOffscreen' in canvas)) return false;
		try {
			return new OffscreenCanvas(1, 1).getContext('webgl') !== null;
		} catch {
			return false;
		}
	}

	// A new canvas when the worker fails: a canvas handed to a worker cannot be drawn on here.
	let gen = $state(0);

	onMount(() => {
		let animate = false;
		try {
			animate = localStorage.getItem('onyx-stone-test') === 'animate';
		} catch {
			/* storage blocked */
		}
		// The latest state, so a fallback starts where the worker was.
		const state: StoneState = { width: canvas.clientWidth, height: canvas.clientHeight, motionOff: motion.off, visible: true, ...(animate ? { animate } : {}) };
		let end: () => void = () => {};
		let gone = false;
		const ro = new ResizeObserver(() => send?.({ width: canvas.clientWidth, height: canvas.clientHeight }));
		const io = new IntersectionObserver(([e]) => send?.({ visible: e.isIntersecting }));
		const watch = () => {
			ro.disconnect();
			io.disconnect();
			ro.observe(canvas);
			io.observe(canvas);
		};
		const onMain = () => {
			const s = stone(canvas, state);
			// For the browser tests (tests/w4.mjs): what the stone is doing on this canvas.
			(canvas as HTMLCanvasElement & { __stone?: typeof s }).__stone = s;
			send = (x) => (Object.assign(state, x), s.set(x));
			end = s.stop;
			watch();
		};
		if (offscreenGl()) {
			const off = canvas.transferControlToOffscreen();
			const w = new Worker(new URL('../stone.worker.ts', import.meta.url), { type: 'module' });
			w.onerror = (e) => {
				// The worker failed (it did not load, or the stone threw there): draw the stone on
				// the page instead, on a fresh canvas.
				e.preventDefault();
				w.terminate();
				if (gone) return;
				send = null;
				gen++;
				tick().then(() => {
					if (!gone) onMain();
				});
			};
			w.postMessage({ canvas: off, state }, [off]);
			send = (x) => (Object.assign(state, x), w.postMessage({ state: x }));
			end = () => w.terminate();
			watch();
		} else onMain();
		return () => {
			gone = true;
			send = null;
			ro.disconnect();
			io.disconnect();
			end();
		};
	});
</script>

<section class="slab">
	{#key gen}<canvas bind:this={canvas} aria-hidden="true"></canvas>{/key}
	<div class="fade" aria-hidden="true"></div>
	<div class="wrap inner">
		{#if at > 0 && page.route.id !== '/'}<p class="kicker" aria-hidden="true" data-testid="kicker">{NAV[at].label}</p>{/if}
		{@render children()}
	</div>
</section>

<style>
	.slab {
		position: relative;
		overflow: hidden;
		background: var(--slab);
		color: var(--slab-ink);
	}
	canvas,
	.fade {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
	}
	canvas {
		image-rendering: auto;
		filter: blur(0.5px);
	}
	/* W12: the stone darkens behind the text column (left), so the title and lede sit on a
	   calm ground; the veins stay bright on the right. */
	.fade {
		background: linear-gradient(180deg, transparent 60%, var(--bg)), linear-gradient(90deg, var(--slab-glass), transparent 75%);
	}
	.inner {
		position: relative;
		padding-block: var(--sp-10) var(--sp-9);
	}
	.slab :global(.muted),
	.slab :global(.lede) {
		color: var(--slab-muted);
	}
</style>
