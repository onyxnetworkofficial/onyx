<script lang="ts">
	// A full hash with a copy button. Falls back to selecting the text if the
	// clipboard is refused (some embedded views do).
	import Copy from '@lucide/svelte/icons/copy';
	import Check from '@lucide/svelte/icons/check';
	import { tick } from 'svelte';
	let { value, short = false }: { value: string; short?: boolean } = $props();
	let expanded = $state(false);
	const shown = $derived(short && !expanded && value.length > 22 ? `${value.slice(0, 10)}…${value.slice(-8)}` : value);
	let el: HTMLSpanElement;
	let copied = $state(false);
	async function copy() {
		try {
			await navigator.clipboard.writeText(value);
			copied = true;
			setTimeout(() => (copied = false), 1400);
		} catch {
			// Show the full value, then select it for a manual copy.
			expanded = true;
			await tick();
			const r = document.createRange();
			r.selectNodeContents(el);
			getSelection()?.removeAllRanges();
			getSelection()?.addRange(r);
		}
	}
</script>

<span class="h"><span class="hash" bind:this={el} title={value}>{shown}</span><button type="button" class="cp" onclick={copy} aria-label="Copy {value}">{#if copied}<Check size={16} />{:else}<Copy size={16} />{/if}</button></span>

<style>
	.h {
		display: inline-flex;
		align-items: flex-start;
		gap: var(--sp-2);
		min-width: 0;
		max-width: 100%;
	}
	.hash {
		min-width: 0;
	}
	.cp {
		flex: none;
		padding: var(--sp-1);
		border-radius: var(--radius-sm);
		background: transparent;
		border: 1px solid var(--line-strong);
		color: var(--muted);
		box-shadow: none;
		line-height: 0;
	}
	.cp:hover:not(:disabled) {
		color: var(--ink);
		transform: none;
		box-shadow: none;
	}
</style>
