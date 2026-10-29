<script lang="ts">
	// NS5 (ADR-0048): a value's proof level as a badge, and with a ladder its public history:
	// each rise, when, on which evidence and which chain record made it public. The history shows
	// only when checkLadder accepts the ladder; a refused one says so and shows no step.
	import { dayText, LEVEL_NAME } from '#lib/catalog.ts';
	// ADR-0048 amendment 2 (ROADMAP C64): a badge that no check on this page backs says "stated" (THREATS
	// NS5-13): in its tooltip and data-stated, and the catalog's lede says its levels are as stated (owner review: the
	// visible " · stated" read as jargon); a ladder must extend the one this reader accepted before (NS5-9); a stronger answer that
	// states another value is shown next to the value and never moves its level (R-LAD2-3).
	import { checkExtends, checkLadder, keepLadder, RECORD_TEXT, seenLadder, type Ladder } from '#lib/ladder.ts';
	import Hash from './Hash.svelte';
	let { level, ladder = null, checked = false, testid = '' }: { level: number; ladder?: Ladder | null; checked?: boolean; testid?: string } = $props();
	const shape = $derived(ladder ? checkLadder(ladder, level) : []);
	const before = $derived(ladder?.value && shape.length === 0 ? seenLadder(ladder.value) : null);
	const problems = $derived(ladder && before && shape.length === 0 ? checkExtends(before, ladder) : shape);
	const stated = $derived(!checked && (!ladder || problems.length > 0));
	$effect(() => {
		if (ladder && problems.length === 0) keepLadder(ladder);
	});
	const day = (t: number) => dayText(new Date(t * 1000).toISOString().slice(0, 10));
</script>

<span class="lb">
	<span
		class="lvl l{level}"
		data-testid={testid ? `${testid}-badge` : undefined}
		data-stated={stated ? '' : undefined}
		title={stated ? 'The level this dataset states. Nothing on this page has checked it yet: a level is checked against its chain record.' : undefined}
		>Level {level} · {LEVEL_NAME[level]}</span
	>
	{#if ladder}
		{#if problems.length}
			<span class="refused" data-testid={testid ? `${testid}-refused` : undefined} title={problems.join('; ')}>Level history refused: {problems[0]}</span>
		{:else}
			<details class="hist" data-testid={testid ? `${testid}-history` : undefined}>
				<summary>Level history, {ladder.steps.length} {ladder.steps.length === 1 ? 'step' : 'steps'}</summary>
				<ol>
					{#each ladder.steps as s, i (i)}
						<li data-testid={testid ? `${testid}-step` : undefined}>
							<span class="when mono">{day(s.at)}</span>
							<span>{i === 0 ? 'Entered at' : 'Rose to'} level {s.level} · {LEVEL_NAME[s.level]}: {RECORD_TEXT[s.record]}</span>
							{#if s.evidence}
								<span class="ev">Evidence <Hash value={s.evidence} short /></span>
							{:else}
								<span class="ev" data-testid={testid ? `${testid}-sealed` : undefined}>Evidence sealed: the paid API answer carries it</span>
							{/if}
						</li>
					{/each}
				</ol>
				<p class="fine">A level only rises, when stronger proof of the same value is recorded on chain. A history that falls is refused.</p>
			</details>
			{#each ladder.contradictions ?? [] as c, i (i)}
				<span class="contra" data-testid={testid ? `${testid}-contradiction` : undefined}
					>A stronger answer says otherwise: level {c.level} · {LEVEL_NAME[c.level]}, {day(c.at)}, {RECORD_TEXT[c.record]}. This value keeps its own level.</span
				>
			{/each}
		{/if}
	{/if}
</span>

<style>
	.lb {
		display: inline-flex;
		flex-direction: column;
		align-items: flex-start;
		gap: var(--sp-1);
		min-width: 0;
		max-width: 100%;
	}
	.lvl {
		font-size: var(--text-xs);
		color: var(--accent);
		background: var(--accent-soft);
		border-radius: var(--radius-sm);
		padding: var(--sp-0) var(--sp-2);
		white-space: nowrap;
	}
	.refused {
		font-size: var(--text-xs);
		color: var(--bad);
	}
	.contra {
		font-size: var(--text-xs);
		color: var(--warn);
		overflow-wrap: anywhere;
	}
	
	.hist {
		font-size: var(--text-xs);
		color: var(--muted);
		max-width: 100%;
	}
	.hist summary {
		cursor: pointer;
		color: var(--ink);
	}
	.hist ol {
		margin: var(--sp-2) 0;
		padding-left: var(--sp-4);
		display: grid;
		gap: var(--sp-2);
	}
	.hist li {
		display: flex;
		flex-direction: column;
		gap: var(--sp-0);
		min-width: 0;
		overflow-wrap: anywhere;
	}
	.when {
		color: var(--ink);
	}
	.ev {
		display: inline-flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--sp-1);
	}
	.fine {
		margin: 0;
	}
</style>
