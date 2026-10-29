<script lang="ts">
	// Every kind of data Onyx is built to carry, placed by the strongest proof
	// its kind allows (the proof ladder, docs/TRACKER.md N9). Only what runs in
	// code today is marked Live; nothing planned shows a number.
	import { FALLBACK } from '#lib/mode.ts';
	const levels = ['Recomputed', 'Re-observed', 'Source-proven', 'Bonded claim', 'Track record'];
	type Item = { name: string; level: number; status: 'Live' | 'Built' | 'Planned' | 'Held back'; note: string };
	const families: { name: string; items: Item[] }[] = [
		{
			name: 'On-chain',
			items: [
				// Data review: a rights-fallback build holds the flows back (the section says so), so the tile does too.
				FALLBACK
					? { name: 'Bitcoin company flows', level: 1, status: 'Held back', note: 'Built; held back until label rights are cleared' }
					: { name: 'Bitcoin company flows', level: 1, status: 'Live', note: 'Rerun from block bytes' },
				{ name: 'EVM chain flows', level: 1, status: 'Planned', note: 'Same recipe model' },
				// AUD-2-F6: the pool list is a published claim, so pool shares are floored at level 4
				// (onyx_source_btc::POOL_TABLE_LEVEL, ADR-0032 amendment 1).
				{ name: 'Bitcoin pool shares', level: 4, status: 'Live', note: 'Rerun from blocks; the pool list is a claim' },
				{ name: 'Signed company labels', level: 3, status: 'Live', note: 'OKX signs its own addresses' },
				// LBL-V2R-F2: a company's unsigned list is its claim, not a proof; staking it is planned.
				{ name: 'Published company labels', level: 4, status: 'Live', note: 'Unsigned company lists: a claim, bond planned' }
			]
		},
		{
			name: 'Markets',
			items: [
				{ name: 'Prices and volumes', level: 2, status: 'Planned', note: 'Bonded observers, sealed answers' },
				{ name: 'Forecasts', level: 5, status: 'Planned', note: 'Scored when the truth arrives' }
			]
		},
		{
			name: 'Web and APIs',
			items: [
				// AUD-2-F11: both run as recorded level-2 rounds the Data page checks; observers simulated.
				{ name: 'Open APIs', level: 2, status: 'Live', note: 'U.S. Treasury data: recorded rounds, observers simulated' },
				{ name: 'Company filings', level: 2, status: 'Live', note: 'SEC EDGAR: recorded rounds, observers simulated' },
				{ name: 'Filings, source-proven', level: 3, status: 'Planned', note: 'zkTLS proof of the page' }
			]
		},
		{
			name: 'Real world',
			items: [
				{ name: 'Sensors and weather', level: 3, status: 'Planned', note: 'Device signs the reading' },
				{ name: 'Field reports', level: 4, status: 'Planned', note: 'Staked, open to challenge' },
				{ name: 'New datasets', level: 4, status: 'Planned', note: 'Staked, open to challenge' }
			]
		},
		{
			name: 'Scores',
			items: [{ name: 'Label track records', level: 5, status: 'Built', note: 'Brier scoring built; no labeler scored yet' }]
		}
	];
</script>

<div class="map" role="table" aria-label="Data kinds by proof level" data-testid="data-map">
	<div class="hrow" role="row">
		<span role="columnheader" class="fam">Kind</span>
		{#each levels as l, i (l)}
			<span role="columnheader" class="lh"><b>{i + 1}</b>{l}</span>
		{/each}
	</div>
	{#each families as f (f.name)}
		<div class="frow" role="row">
			<span role="rowheader" class="fam">{f.name}</span>
			{#each f.items as it (it.name)}
				<span role="cell" class="item" class:live={it.status === 'Live'} style:--col={it.level + 1}>
					<span class="nm">{it.name}</span>
					<span class="nt">{it.note}</span>
					<span class="st"><i></i>{it.status}<em> · L{it.level} {levels[it.level - 1]}</em></span>
				</span>
			{/each}
		</div>
	{/each}
</div>

<style>
	.map {
		display: grid;
		gap: var(--sp-2);
	}
	.hrow,
	.frow {
		display: grid;
		grid-template-columns: 112px repeat(5, minmax(0, 1fr));
		gap: var(--sp-2);
		align-items: stretch;
	}
	.lh {
		display: flex;
		align-items: baseline;
		gap: var(--sp-2);
		font-size: var(--text-xs);
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--muted);
		padding: 0 var(--sp-1) var(--sp-2);
		border-bottom: 1px solid var(--line);
	}
	.lh b {
		font-family: var(--display);
		font-size: var(--text-lg);
		font-weight: 400;
		color: var(--ink);
		letter-spacing: 0;
	}
	.fam {
		font-size: var(--text-sm);
		color: var(--muted);
		padding-top: var(--sp-3);
	}
	.hrow .fam {
		visibility: hidden;
	}
	.frow {
		grid-auto-flow: dense;
		border-top: 1px solid var(--line);
		padding-top: var(--sp-2);
	}
	.frow .fam {
		grid-column: 1;
	}
	.item {
		grid-column: var(--col);
		display: grid;
		gap: var(--sp-0);
		align-content: start;
		padding: var(--sp-3) var(--sp-3);
		border-radius: var(--radius-md);
		border: 1px dashed var(--line-strong);
		min-width: 0;
	}
	.item.live {
		border: 1px solid var(--good-line);
		background: var(--good-soft);
	}
	.nm {
		font-size: var(--text-sm);
		font-weight: 500;
		line-height: var(--lh-snug);
	}
	.nt {
		font-size: var(--text-sm);
		color: var(--muted);
		line-height: var(--lh-snug);
	}
	.st {
		display: inline-flex;
		align-items: center;
		gap: var(--sp-2);
		margin-top: var(--sp-2);
		font-family: var(--mono);
		font-size: var(--text-xs);
		color: var(--muted);
	}
	.st i {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		border: 1px solid var(--muted);
	}
	.live .st {
		color: var(--good);
	}
	.live .st i {
		background: var(--good);
		border-color: var(--good);
		box-shadow: var(--shadow-glow-good);
	}
	.st em {
		font-style: normal;
		display: none;
	}
	/* Narrow: one column per family; each item names its level instead. */
	@media (max-width: 860px) {
		.hrow {
			display: none;
		}
		.frow {
			grid-template-columns: minmax(0, 1fr);
		}
		.item {
			grid-column: 1;
		}
		.fam {
			padding-top: var(--sp-1);
		}
		.st em {
			display: inline;
		}
	}
</style>
