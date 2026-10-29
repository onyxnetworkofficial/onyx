<script lang="ts">
	// A short burst of confetti over the page each time `fire` changes (a win in the Markets
	// arena). Decoration only: hidden from screen readers, and off when motion is off.
	import { motion } from '#lib/motion.svelte.ts';
	let { fire }: { fire: number } = $props();
	let canvas = $state<HTMLCanvasElement>();
	let first = true;
	$effect(() => {
		void fire;
		if (first) {
			first = false;
			return;
		}
		if (motion.off || !canvas) return;
		const c = canvas;
		const ctx = c.getContext('2d');
		if (!ctx) return;
		c.width = innerWidth;
		c.height = innerHeight;
		const css = getComputedStyle(document.documentElement);
		const hues = ['--good', '--btc', '--accent', '--line-net', '--pool-5', '--pool-6'].map((v) => css.getPropertyValue(v).trim() || '#ffffff');
		const bits = Array.from({ length: 140 }, () => ({
			x: innerWidth / 2 + (Math.random() - 0.5) * 120,
			y: innerHeight * 0.55,
			vx: (Math.random() - 0.5) * 16,
			vy: -8 - Math.random() * 12,
			r: Math.random() * Math.PI,
			vr: (Math.random() - 0.5) * 0.3,
			w: 5 + Math.random() * 6,
			h: 8 + Math.random() * 8,
			c: hues[Math.floor(Math.random() * hues.length)]
		}));
		let frame = 0;
		let id = 0;
		const step = () => {
			ctx.clearRect(0, 0, c.width, c.height);
			for (const b of bits) {
				b.vy += 0.38;
				b.vx *= 0.985;
				b.x += b.vx;
				b.y += b.vy;
				b.r += b.vr;
				ctx.save();
				ctx.translate(b.x, b.y);
				ctx.rotate(b.r);
				ctx.globalAlpha = Math.max(0, 1 - frame / 110);
				ctx.fillStyle = b.c;
				ctx.fillRect(-b.w / 2, -b.h / 2, b.w, b.h);
				ctx.restore();
			}
			if (++frame < 110) id = requestAnimationFrame(step);
			else ctx.clearRect(0, 0, c.width, c.height);
		};
		id = requestAnimationFrame(step);
		return () => cancelAnimationFrame(id);
	});
</script>

<canvas bind:this={canvas} aria-hidden="true"></canvas>

<style>
	canvas { position: fixed; inset: 0; width: 100%; height: 100%; pointer-events: none; z-index: 60; }
</style>
