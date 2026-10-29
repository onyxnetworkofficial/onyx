<script lang="ts" module>
	// ROADMAP L14, ADR-0035: the one sentence a rights-fallback build shows wherever it
	// leaves out a total derived from the entity label sets (THREATS L8-5).
	export const HELD_BACK = 'Company totals are held back until data rights are cleared.';
	/** The public site's banner on every page (the owner, 2026-10-25). */
	export const DEV_NOTICE = 'In development · testnet preview. Data and features may change; some company data is not shown yet.';
	export const HELD_WHY =
		'They are computed from exchange and ETF address lists (OKX, Binance, BitMEX, Bitwise BITB) whose terms do not yet let us share what is derived from them. Bitcoin block data, mining-pool shares, U.S. Treasury and SEC values are not affected.';
</script>

<script lang="ts">
	// A section in place of one that shows company totals: its own heading, what is held
	// back, and why.
	let { title, what, testid = 'held-back' }: { title: string; what: string; testid?: string } = $props();
</script>

<section class="section" data-testid={testid}>
	<div class="sh"><h2>{title}</h2></div>
	<div class="panel held">
		<p><b>{HELD_BACK}</b></p>
		<p class="muted">{what} {HELD_WHY}</p>
	</div>
</section>

<style>
	.held {
		display: grid;
		gap: var(--sp-3);
	}
	.held p {
		margin: 0;
	}
</style>
