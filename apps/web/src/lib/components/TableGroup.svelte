<script lang="ts" module>
	/** Context key: a DataTable inside a TableGroup is opened and closed by the group's one switch. */
	export const TABLES = Symbol('onyx-tables');
	/** `add` names a table's element id for the switch's aria-controls; it returns the undo. */
	export type TableState = { readonly open: boolean; add: (id: string) => () => void };
</script>

<script lang="ts">
	// W12 round 4: small multiples (one chart per company) share one data table switch instead
	// of a "Data table" fold under every chart (8 folds in a row on the Data page). Each chart's
	// table stays right under its chart (W3); the switch opens or closes them all. The switch is
	// handed to the group's markup as a snippet, to sit in the legend row. It is a disclosure
	// button: aria-expanded says the state, aria-controls names every table it opens.
	import { setContext, untrack, type Snippet } from 'svelte';
	let { children, testid }: { children: Snippet<[Snippet]>; testid?: string } = $props();
	let open = $state(false);
	let ids = $state<string[]>([]);
	setContext<TableState>(TABLES, {
		get open() {
			return open;
		},
		// untrack: a table's effect that calls add must not depend on the list it changes.
		add(id) {
			untrack(() => (ids = [...ids, id]));
			return () => untrack(() => (ids = ids.filter((x) => x !== id)));
		}
	});
</script>

{#snippet toggle()}
	<button type="button" class="tablesw" aria-expanded={open} aria-controls={ids.length ? ids.join(' ') : undefined} data-testid={testid} onclick={() => (open = !open)}>Data tables</button>
{/snippet}
{@render children(toggle)}
