<script lang="ts">
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import OnyxSlab from '#lib/components/OnyxSlab.svelte';
</script>

<svelte:head><title>Not found · Onyx</title></svelte:head>

<OnyxSlab>
	<p class="meta code">{page.status}</p>
	<h1>No page here</h1>
	<p class="lede">The link may be old. Every page that exists is one click away.</p>
	<div class="row links">
		<!-- Newcomer test 3 (2026-11-18): the two places a reader most likely wanted come first. -->
		<a class="btn" href={resolve('/data')} data-testid="notfound-data">Browse data</a>
		<a class="btn ghost" href={resolve('/datasets')} data-testid="notfound-datasets">All datasets</a>
		<a class="btn ghost" href={resolve('/')}>Home</a>
		<a class="btn ghost" href={resolve('/api')}>Query API</a>
	</div>
</OnyxSlab>

<style>
	.code {
		color: var(--slab-muted);
		margin-bottom: var(--sp-3);
	}
	.lede {
		margin-top: var(--sp-4);
	}
	.links {
		margin-top: var(--sp-6);
	}
	.links .ghost {
		color: var(--slab-ink);
	}
</style>
