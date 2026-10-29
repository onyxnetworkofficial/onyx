<script lang="ts">
	// Command palette: Ctrl/Cmd-K (or the header button) to jump to a page or
	// run an action. Native <dialog> gives focus trapping and Esc to close.
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import Search from '@lucide/svelte/icons/search';
	import CornerDownLeft from '@lucide/svelte/icons/corner-down-left';
	import { verifier } from '#lib/verifier.ts';
	import { checks } from '#lib/checks.ts';

	type Item = { label: string; hint: string; run: () => void | Promise<void> };
	let dialog: HTMLDialogElement;
	let input = $state<HTMLInputElement>();
	let query = $state('');
	let active = $state(0);
	let note = $state('');

	const items: Item[] = [
		{ label: 'Home', hint: 'Page', run: () => goto(resolve('/')) },
		{ label: 'Browse data', hint: 'Page', run: () => goto(resolve('/data')) },
		{ label: 'Markets preview', hint: 'Page', run: () => goto(resolve('/markets')) },
		{ label: 'How it works', hint: 'Page', run: () => goto(resolve('/how-it-works')) },
		{ label: 'Reserves', hint: 'Page', run: () => goto(resolve('/reserves')) },
		{ label: 'Live run', hint: 'Page', run: () => goto(resolve('/run')) },
		{ label: 'Shard explorer', hint: 'Page', run: () => goto(resolve('/explorer')) },
		{ label: 'Address labels', hint: 'Page', run: () => goto(resolve('/labels')) },
		{ label: 'Query API', hint: 'Page', run: () => goto(resolve('/api')) },
		{ label: 'Add data', hint: 'Page', run: () => goto(resolve('/submit')) },
		{
			label: `Run all ${checks.length} checks`,
			hint: 'Action',
			run: async () => {
				await goto(resolve('/how-it-works'));
				dispatchEvent(new CustomEvent('onyx:run-checks'));
			}
		},
		{
			label: 'Copy the pinned root',
			hint: 'Action',
			run: async () => {
				const root = (await verifier.pinned()).root;
				try {
					await navigator.clipboard.writeText(root);
					note = `Copied ${root.slice(0, 10)}…`;
				} catch {
					note = root;
				}
			}
		}
	];
	const shown = $derived(items.filter((i) => i.label.toLowerCase().includes(query.trim().toLowerCase())));

	export function open() {
		query = '';
		note = '';
		active = 0;
		dialog.showModal();
		input?.focus();
	}
	async function choose(i: Item | undefined) {
		if (!i) return;
		await i.run();
		if (i.hint === 'Page' || i.label.startsWith('Run')) dialog.close();
	}
	function key(e: KeyboardEvent) {
		if (e.key === 'ArrowDown') (active = Math.min(active + 1, shown.length - 1)), e.preventDefault();
		else if (e.key === 'ArrowUp') (active = Math.max(active - 1, 0)), e.preventDefault();
		else if (e.key === 'Enter') (e.preventDefault(), choose(shown[active]));
	}

	onMount(() => {
		const k = (e: KeyboardEvent) => {
			if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
				e.preventDefault();
				if (dialog.open) dialog.close();
				else open();
			}
		};
		addEventListener('keydown', k);
		return () => removeEventListener('keydown', k);
	});
</script>

<dialog bind:this={dialog} class="pal" aria-label="Command palette" onclick={(e) => e.target === dialog && dialog.close()}>
	<div class="box">
		<label class="field">
			<Search size={16} />
			<input bind:this={input} bind:value={query} oninput={() => (active = 0)} onkeydown={key}
				placeholder="Jump to a page or run an action" aria-label="Search commands" role="combobox"
				aria-expanded="true" aria-controls="pal-list" aria-activedescendant={shown[active] ? `pal-${active}` : undefined} />
			<kbd>Esc</kbd>
		</label>
		<!-- W3: the options are the listbox's own items (no button inside an option); the input
		     keeps focus and moves through them with the arrow keys, Enter picks (combobox pattern). -->
		<ul id="pal-list" role="listbox" aria-label="Pages and actions">
			{#each shown as it, i (it.label)}
				<!-- svelte-ignore a11y_click_events_have_key_events -->
				<li id="pal-{i}" role="option" aria-selected={i === active} class:on={i === active} onclick={() => choose(it)} onmousemove={() => (active = i)}>
					<span>{it.label}</span><em>{it.hint}</em>{#if i === active}<CornerDownLeft size={16} />{/if}
				</li>
			{/each}
		</ul>
		{#if !shown.length}<p class="none" role="status">No match</p>{/if}
		{#if note}<p class="note mono">{note}</p>{/if}
	</div>
</dialog>

<style>
	.pal {
		padding: 0;
		border: 0;
		background: transparent;
		max-width: min(560px, calc(100vw - 32px));
		width: 100%;
		margin-top: 14vh;
	}
	.pal::backdrop {
		background: var(--scrim);
		backdrop-filter: blur(3px);
	}
	.box {
		border-radius: var(--radius-lg);
		border: 1px solid var(--line-strong);
		background: var(--panel-solid);
		box-shadow: var(--shadow-pop);
		overflow: hidden;
		color: var(--ink);
	}
	.field {
		display: flex;
		align-items: center;
		gap: var(--sp-3);
		padding: var(--sp-4) var(--sp-4);
		border-bottom: 1px solid var(--line);
		color: var(--muted);
	}
	/* The input draws no outline of its own; the field shows the focus (W3, WCAG 2.4.7). */
	.field:focus-within {
		box-shadow: var(--shadow-underline);
	}
	input {
		flex: 1;
		min-width: 0;
		font: inherit;
		font-size: var(--text-base);
		background: transparent;
		border: 0;
		color: var(--ink);
		outline: none;
	}
	kbd {
		font-family: var(--mono);
		font-size: var(--text-xs);
		border: 1px solid var(--line-strong);
		border-radius: var(--radius-sm);
		padding: var(--sp-0) var(--sp-2);
	}
	ul {
		list-style: none;
		margin: 0;
		padding: var(--sp-2);
		max-height: 50vh;
		overflow-y: auto;
	}
	li {
		display: flex;
		align-items: center;
		gap: var(--sp-3);
		padding: var(--sp-3) var(--sp-3);
		min-height: 44px;
		border-radius: var(--radius-md);
		color: var(--ink);
		cursor: pointer;
	}
	/* The active option: a fill and a bar at its edge (not colour alone). */
	li.on {
		background: var(--hover);
		box-shadow: var(--shadow-edge);
	}
	li span {
		flex: 1;
	}
	li em {
		font-style: normal;
		font-size: var(--text-xs);
		color: var(--muted);
	}
	.none {
		margin: 0;
		padding: var(--sp-4);
		color: var(--muted);
	}
	.note {
		margin: 0;
		padding: var(--sp-3) var(--sp-4);
		border-top: 1px solid var(--line);
		color: var(--good);
		overflow-wrap: anywhere;
	}
</style>
