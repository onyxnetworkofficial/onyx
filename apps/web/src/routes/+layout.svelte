<script lang="ts">
	import '#lib/app.css';
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import { onMount } from 'svelte';
	import { onNavigate } from '$app/navigation';
	import CommandPalette from '#lib/components/CommandPalette.svelte';
	import Search from '@lucide/svelte/icons/search';
	import Menu from '@lucide/svelte/icons/menu';
	import X from '@lucide/svelte/icons/x';
	import Pause from '@lucide/svelte/icons/pause';
	import Play from '@lucide/svelte/icons/play';
	import { FALLBACK, PUBLIC } from '#lib/mode.ts';
	import { DEV_NOTICE } from '#lib/components/HeldBack.svelte';
	import { motion } from '#lib/motion.svelte.ts';
	import { fadeSections } from '#lib/fade.ts';
	import { FOOTER_NAV, PRIMARY_NAV } from '#lib/nav.ts';
	let palette: CommandPalette;
	let { children } = $props();

	// Cross-fade between pages with the View Transitions API; the header stays
	// put. Skipped where unsupported or when the viewer prefers less motion.
	onNavigate((navigation) => {
		const doc = document as Document & { startViewTransition?: (cb: () => Promise<void>) => unknown };
		if (!doc.startViewTransition || motion.off) return;
		return new Promise<void>((resolve) => {
			doc.startViewTransition!(async () => {
				resolve();
				await navigation.complete;
			});
		});
	});

	// W3: <html data-motion="off"> stops every CSS animation and transition (app.css).
	$effect(() => {
		document.documentElement.dataset.motion = motion.off ? 'off' : 'on';
	});

	// Light that follows the pointer across panels (CSS reads --mx/--my).
	onMount(() => {
		if (matchMedia('(hover: none)').matches) return;
		const move = (e: PointerEvent) => {
			const el = (e.target as Element | null)?.closest?.('.panel') as HTMLElement | null;
			if (!el) return;
			const r = el.getBoundingClientRect();
			el.style.setProperty('--mx', `${e.clientX - r.left}px`);
			el.style.setProperty('--my', `${e.clientY - r.top}px`);
		};
		addEventListener('pointermove', move, { passive: true });
		return () => removeEventListener('pointermove', move);
	});

	// Phones (W2): the nav is a menu button that opens the primary destinations; it closes on
	// a pick, on Esc (focus back on the button) and on a tap outside the header.
	let menuOpen = $state(false);
	let menuBtn = $state<HTMLButtonElement>();
	let header = $state<HTMLElement>();
	let mainEl = $state<HTMLElement>();
	$effect(() => {
		void page.url.href;
		menuOpen = false;
	});
	function onKey(e: KeyboardEvent) {
		if (e.key === 'Escape' && menuOpen) {
			menuOpen = false;
			menuBtn?.focus();
		}
	}
	function onDown(e: PointerEvent) {
		if (menuOpen && header && !header.contains(e.target as Node)) menuOpen = false;
	}
	// Hash routes (#/data) own the URL fragment, so the skip link moves focus itself.
	function skip(e: MouseEvent) {
		e.preventDefault();
		mainEl?.focus();
		mainEl?.scrollIntoView();
	}

	// W12 round 13: sections below the first screen fade in as they come into view (#lib/fade.ts).
	onMount(() => (mainEl ? fadeSections(mainEl) : undefined));

	const nav = PRIMARY_NAV;
</script>

<svelte:window onkeydown={onKey} onpointerdown={onDown} />

<a class="skip" href="#main" onclick={skip} data-testid="skip-link">Skip to content</a>

{#if !PUBLIC}
	<!-- AUD-2-F1: every build but ONYX_PUBLIC=1 is the private preview. A labelled landmark (W3),
	     so no page text sits outside one. -->
	<aside class="preview" aria-label="Build notice" data-testid="preview-banner">Private preview · test labels, sharing rights not cleared</aside>
{:else if FALLBACK}
	<!-- ROADMAP L14, ADR-0035: a rights-fallback build ships no company totals; every page says the
	     site is in development and that some company data is not shown yet (the sections say which). -->
	<aside class="preview" aria-label="Build notice" data-testid="rights-banner">{DEV_NOTICE}</aside>
{/if}

<header class="top" bind:this={header}>
	<div class="wrap bar">
		<a class="mark" href={resolve('/')}><span class="glyph" aria-hidden="true"></span>Onyx</a>
		<nav aria-label="Main" id="site-nav" class:open={menuOpen} data-testid="site-nav">
			{#each nav as n (n.path)}
				<a href={n.href} class:contribute={n.path === '/submit'} aria-current={page.route.id === n.path ? 'page' : undefined} onclick={() => (menuOpen = false)}>{n.label}</a>
			{/each}
		</nav>
		<!-- Named by its words, not an aria-label, so the visible shortcut is part of the name (W3:
		     axe label-content-name-mismatch). -->
		<button type="button" class="kbtn" onclick={() => palette.open()} aria-keyshortcuts="Control+K Meta+K" title="Search pages and actions (Ctrl+K)"
			><Search size={16} /><span class="sr-only">Search pages and actions</span><kbd>⌘K</kbd></button
		>
		<!-- WCAG 2.2.2: the slab, the block strip and the beams move on their own; this stops them
		     (remembered). Hidden when the system already asks for reduced motion. -->
		{#if !motion.system}
			<button
				type="button"
				class="kbtn motionbtn"
				aria-pressed={motion.paused}
				aria-label="Pause animations"
				title={motion.paused ? 'Animations paused: press to play' : 'Pause animations'}
				onclick={() => motion.setPaused(!motion.paused)}
				data-testid="motion-button">{#if motion.paused}<Play size={16} />{:else}<Pause size={16} />{/if}</button
			>
		{/if}
		<button
			type="button"
			class="menubtn"
			bind:this={menuBtn}
			aria-expanded={menuOpen}
			aria-controls="site-nav"
			onclick={() => (menuOpen = !menuOpen)}
			data-testid="menu-button"
			>{#if menuOpen}<X size={18} aria-hidden="true" />{:else}<Menu size={18} aria-hidden="true" />{/if}<span>Menu</span></button
		>
	</div>
</header>

<CommandPalette bind:this={palette} />

<main id="main" tabindex="-1" bind:this={mainEl}>
	{@render children()}
</main>

<!-- W12: the footer closes every page with the wordmark, the page list and the network's
     state (the same words as the home page's note), so it no longer ends on an empty field.
     The list is not a second nav landmark (W3: one header, nav, main and footer per page). -->
<footer class="foot">
	<div class="wrap fgrid">
		<div class="big" aria-hidden="true">Onyx</div>
		<div class="fnav" data-testid="footer-nav">
			<ul>
				{#each FOOTER_NAV as n (n.path)}
					<li><a href={n.href} aria-current={page.route.id === n.path ? 'page' : undefined}>{n.label}</a></li>
				{/each}
			</ul>
		</div>
		<div class="fline">
			<span class="fstate"><span class="dot" aria-hidden="true"></span>Testnet only · {PUBLIC ? 'early preview' : 'private preview'}</span>
			<span>Every check runs in your browser, on the network's own code.</span>
		</div>
	</div>
</footer>

<style>
	.preview {
		padding: var(--sp-2) var(--sp-4);
		text-align: center;
		font-size: var(--text-xs);
		letter-spacing: 0.02em;
		color: var(--warn);
		background: var(--btc-soft);
		border-bottom: 1px solid var(--line);
	}
	.top {
		view-transition-name: site-header;
		position: sticky;
		top: env(safe-area-inset-top, 0px);
		z-index: 10;
		background: var(--header-bg);
		backdrop-filter: blur(14px) saturate(140%);
		border-bottom: 1px solid var(--slab-line);
		color: var(--slab-ink);
	}
	.bar {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--sp-2) var(--sp-6);
		padding-block: var(--sp-3);
	}
	.mark {
		display: inline-flex;
		align-items: center;
		gap: var(--sp-3);
		font-family: var(--display);
		font-size: var(--text-xl);
		line-height: var(--lh-none);
		color: var(--slab-ink);
		text-decoration: none;
	}
	.mark:hover .glyph {
		transform: rotate(135deg);
	}
	.glyph {
		transition: transform var(--dur) var(--ease);
		width: 15px;
		height: 15px;
		transform: rotate(45deg);
		border-radius: var(--radius-xs);
		background: linear-gradient(135deg, #3a3833, #050505 60%);
		box-shadow: var(--shadow-mark);
	}
	nav {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-0);
	}
	nav a {
		color: var(--slab-muted);
		text-decoration: none;
		padding: var(--sp-2) var(--sp-3);
		border-radius: var(--radius-pill);
		font-weight: 500;
		font-size: var(--text-sm);
	}
	nav a {
		transition:
			color var(--dur) var(--ease),
			background-color var(--dur) var(--ease);
	}
	nav a:hover {
		color: var(--slab-ink);
		background: var(--hover);
	}
	nav a[aria-current='page'] {
		color: var(--slab-ink);
		background: var(--line);
	}
	nav a[aria-current='page']:hover {
		background: var(--line-strong);
	}
	nav a.contribute, nav a.contribute[aria-current='page'] {
		color: var(--accent-ink);
		background: var(--accent);
	}
	nav a.contribute:hover, nav a.contribute[aria-current='page']:hover {
		color: var(--accent-ink);
		background: var(--ink);
	}
	.kbtn {
		margin-left: auto;
		display: inline-flex;
		align-items: center;
		gap: var(--sp-2);
		padding: var(--sp-1) var(--sp-2);
		border-radius: var(--radius-sm);
		border: 1px solid var(--slab-line);
		background: transparent;
		color: var(--slab-muted);
		box-shadow: none;
	}
	.kbtn:hover:not(:disabled) {
		color: var(--slab-ink);
		border-color: var(--line-bright);
		transform: none;
		box-shadow: none;
	}
	.kbtn kbd {
		font-family: var(--mono);
		font-size: var(--text-xs);
	}
	.motionbtn {
		margin-left: 0;
		min-width: 32px;
		justify-content: center;
	}
	.motionbtn[aria-pressed='true'] {
		color: var(--slab-ink);
		background: var(--line);
	}
	/* Content loads after first paint; keep the footer below the fold meanwhile. */
	main {
		min-height: 100vh;
	}
	main:focus {
		outline: none;
	}
	/* Skip link: off-screen until focused, then the first thing on the page. */
	.skip {
		position: absolute;
		left: 12px;
		top: 8px;
		z-index: 100;
		transform: translateY(-200%);
		padding: var(--sp-3) var(--sp-4);
		min-height: 44px;
		border-radius: var(--radius-pill);
		background: var(--accent);
		color: var(--accent-ink);
		font-weight: 600;
		text-decoration: none;
	}
	.skip:focus {
		transform: none;
	}
	.menubtn {
		display: none;
	}
	/* Phones and narrow tablets: logo, search and a menu button on one row; the menu
	   opens the full page list below the header, each row a 48 px tap target. */
	@media (max-width: 760px) {
		.bar {
			flex-wrap: nowrap;
			gap: var(--sp-2);
			padding-block: var(--sp-2);
			position: relative;
		}
		.mark {
			font-size: var(--text-lg);
			min-height: 44px;
			padding-right: var(--sp-2);
		}
		.kbtn {
			min-width: 44px;
			min-height: 44px;
			justify-content: center;
			border-radius: var(--radius-md);
		}
		.kbtn kbd {
			display: none;
		}
		.menubtn {
			display: inline-flex;
			align-items: center;
			justify-content: center;
			gap: var(--sp-2);
			min-height: 44px;
			min-width: 44px;
			padding: 0 var(--sp-4);
			border-radius: var(--radius-md);
			border: 1px solid var(--slab-line);
			background: transparent;
			color: var(--slab-ink);
			box-shadow: none;
			font-size: var(--text-sm);
		}
		.menubtn:hover:not(:disabled) {
			transform: none;
			box-shadow: none;
		}
		.menubtn[aria-expanded='true'] {
			background: var(--line);
		}
		nav {
			display: none;
			position: absolute;
			top: 100%;
			left: 0;
			right: 0;
			flex-direction: column;
			flex-wrap: nowrap;
			gap: var(--sp-0);
			padding: var(--sp-2) var(--sp-3) var(--sp-4);
			background: var(--slab);
			border-bottom: 1px solid var(--slab-line);
			box-shadow: var(--shadow-pop);
		}
		nav.open {
			display: flex;
		}
		nav a {
			display: flex;
			align-items: center;
			min-height: 48px;
			padding: 0 var(--sp-4);
			border-radius: var(--radius-md);
			font-size: var(--text-base);
		}
	}
	.foot {
		border-top: 1px solid var(--line);
		padding-block: var(--sp-9) var(--sp-8);
		color: var(--muted);
		font-size: var(--text-sm);
	}
	.fgrid {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		align-items: end;
		gap: var(--sp-7) var(--sp-9);
	}
	.big {
		font-family: var(--display);
		font-size: var(--text-display);
		line-height: var(--lh-display);
		letter-spacing: -0.03em;
		background: linear-gradient(180deg, var(--wordmark-top), var(--wordmark-end));
		-webkit-background-clip: text;
		background-clip: text;
		color: transparent;
		user-select: none;
	}
	.fnav ul {
		list-style: none;
		padding: 0;
		display: grid;
		grid-template-columns: repeat(2, minmax(0, max-content));
		gap: var(--sp-1) var(--sp-8);
	}
	.fnav a {
		color: var(--muted);
		text-decoration: none;
		font-size: var(--text-base);
	}
	.fnav a[aria-current='page'] {
		color: var(--ink);
	}
	/* Hover: the link's ink and an underline (the current page's link changes too, W10). */
	.fnav a:hover {
		color: var(--ink);
		text-decoration: underline;
		text-decoration-color: var(--accent-line);
	}
	.fline {
		grid-column: 1 / -1;
		display: flex;
		flex-wrap: wrap;
		justify-content: space-between;
		gap: var(--sp-2) var(--sp-6);
		padding-top: var(--sp-5);
		border-top: 1px solid var(--line);
	}
	.fstate {
		display: inline-flex;
		align-items: center;
		gap: var(--sp-2);
		font-family: var(--mono);
		font-size: var(--text-xs);
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--ink);
	}
	.fstate .dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--btc);
		box-shadow: var(--shadow-ring-btc);
	}
	/* W12 round 7: on a wide screen the page list and the state line share one column beside the
	   wordmark (the list sat at the far right, 500 px from the wordmark at 1,280 px, and the state
	   line ran under both); the list reads in rows of four across that column. Source order is
	   unchanged: wordmark, list, state line. */
	@media (min-width: 761px) {
		.fgrid {
			grid-template-columns: auto minmax(0, 1fr);
			grid-template-rows: minmax(0, 1fr) auto;
			column-gap: var(--sp-10);
			row-gap: var(--sp-6);
		}
		.big {
			grid-row: 1 / span 2;
			align-self: end;
		}
		.fnav ul {
			grid-template-columns: repeat(4, minmax(0, 1fr));
			gap: var(--sp-2) var(--sp-6);
		}
		.fline {
			grid-column: 2;
			padding-top: var(--sp-4);
		}
	}
	@media (max-width: 760px) {
		.fgrid {
			grid-template-columns: minmax(0, 1fr);
			align-items: start;
		}
		.fnav ul {
			gap: 0 var(--sp-8);
		}
		/* W2: each page link a 44 x 44 px tap target. */
		.fnav a {
			display: inline-flex;
			align-items: center;
			min-height: 44px;
			min-width: 44px;
		}
	}
</style>
