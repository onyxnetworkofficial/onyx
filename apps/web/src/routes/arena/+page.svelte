<script lang="ts">
	// The arena became the Markets page (2026-10-26); old #/arena links land there.
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { onMount } from 'svelte';
	onMount(() => goto(resolve('/markets'), { replaceState: true }));
</script>

<svelte:head><title>Markets · Onyx</title></svelte:head>
<div class="wrap"><p><a href={resolve('/markets')}>Markets moved here</a></p></div>
