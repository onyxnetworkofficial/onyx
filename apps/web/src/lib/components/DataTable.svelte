<script lang="ts">
	import { getContext } from 'svelte';
	import { TABLES, type TableState } from './TableGroup.svelte';
	// A chart's numbers as a table (W3, WCAG 1.1.1 / 1.3.1): folded under the chart, built
	// only when opened, the same values the drawing and its tooltip show. On phones the
	// rows stack as label/value lines (app.css `table.stack`), so nothing scrolls sideways.
	let {
		caption,
		columns,
		rows,
		testid
	}: { caption: string; columns: { label: string; right?: boolean }[]; rows: (string | number)[][]; testid?: string } = $props();
	// W12 round 4: inside a TableGroup (small multiples) the group's one switch opens every table,
	// and this table's own fold is not drawn.
	const group = getContext<TableState | undefined>(TABLES);
	const uid = $props.id();
	const id = `datatable-${uid}`;
	$effect(() => group?.add(id));
	let own = $state(false);
	const open = $derived(group ? group.open : own);
</script>

<details {id} class="more datatable" class:grouped={!!group} {open} ontoggle={(e) => group || (own = e.currentTarget.open)} data-testid={testid}>
	<summary hidden={group ? true : undefined}>Data table ({rows.length} {rows.length === 1 ? 'row' : 'rows'})</summary>
	{#if open}
		<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
		<div class="tablebox" tabindex="0" role="region" aria-label={caption}>
			<table class="stack">
				<caption>{caption}</caption>
				<thead>
					<tr>
						{#each columns as c (c.label)}<th scope="col" class:r={c.right}>{c.label}</th>{/each}
					</tr>
				</thead>
				<tbody>
					{#each rows as row, i (i)}
						<tr>
							{#each row as v, k (k)}
								{#if k === 0}<th scope="row" data-label={columns[k].label}>{v}</th>{:else}<td class:r={columns[k].right} data-label={columns[k].label}>{v}</td>{/if}
							{/each}
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
	{/if}
</details>

<style>
	/* In a group the switch sits in the panel's legend row: no fold rule or summary per chart. */
	.grouped {
		border-top: 0;
		padding-top: 0;
	}
	.grouped:not([open]) {
		display: none;
	}
	tbody th {
		color: var(--ink);
		font-weight: 500;
		font-family: var(--mono);
		font-size: var(--text-sm);
	}
</style>
