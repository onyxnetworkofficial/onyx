<script lang="ts">
	// Received vs sent for one company, to one scale, with the net called out.
	import { btc } from '#lib/format.ts';
	let { received, sent, net }: { received: string; sent: string; net: string } = $props();
	const max = $derived(Number(BigInt(received) > BigInt(sent) ? BigInt(received) : BigInt(sent)) || 1);
	const pct = (v: string) => `${(Number(BigInt(v)) / max) * 100}%`;
	const neg = $derived(BigInt(net) < 0n);
</script>

<div class="flow">
	<div class="bar"><span>In</span><div class="track"><i class="in" style="width: {pct(received)}"></i></div><b>{btc(received)}</b></div>
	<div class="bar"><span>Out</span><div class="track"><i class="out" style="width: {pct(sent)}"></i></div><b>{btc(sent)}</b></div>
	<div class="net" class:neg>Net {btc(net)} BTC</div>
</div>

<style>
	.flow {
		display: grid;
		gap: var(--sp-3);
	}
	.bar {
		display: grid;
		grid-template-columns: 34px minmax(0, 1fr) auto;
		gap: var(--sp-3);
		align-items: center;
		font-size: var(--text-sm);
	}
	.bar span {
		color: var(--muted);
	}
	.bar b {
		font-family: var(--mono);
		font-weight: 500;
		font-variant-numeric: tabular-nums;
	}
	.track {
		height: 12px;
		border-radius: var(--radius-sm);
		background: var(--code-bg);
		overflow: hidden;
	}
	.track i {
		display: block;
		height: 100%;
		border-radius: var(--radius-sm);
	}
	.in {
		background: var(--accent);
	}
	.out {
		background: var(--btc);
	}
	.net {
		font-family: var(--mono);
		font-size: var(--text-sm);
		color: var(--accent);
	}
	.net.neg {
		color: var(--btc);
	}
</style>
