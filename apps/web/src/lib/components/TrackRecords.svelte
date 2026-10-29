<script lang="ts">
	// ROADMAP C6: level-5 track records (docs/spec/proof-ladder.md "Level 5", "Display").
	// No real record exists yet (no metric has a closing-time rule), so the section says so
	// and shows the example, labelled example everywhere. Every figure comes from the Rust
	// verifier's check of the shipped file (verify/src/track_view.rs), never from the JSON.
	// ROADMAP C20: the browser remembers the commitment it saw last per record and asks with
	// `since` (the site's since file stands in for the server's answers), and reruns the example's
	// logs on request. ROADMAP C23: the rerun uses the registry as the server serves it
	// (level5/track-example-registry.json), rebuilt and checked to the commitment's registry root
	// first. TODO LC-8: a live node URL for this section needs a deployed node and a domain.
	import { onMount } from 'svelte';
	import Pill, { type PillState } from '#lib/components/Pill.svelte';
	import Skeleton from '#lib/components/Skeleton.svelte';
	import { forget, remember, seenOf, type Seen } from '#lib/trackSeen.ts';
	import { dayText } from '#lib/catalog.ts';
	import { verifier, type TrackExampleCheck, type TrackLogCheck, type TrackView } from '#lib/verifier.ts';

	const SOURCES = ['example.forecast', 'example.guess'];
	const METRIC = 'example.signed.reserve';
	// NL4 (ADR-0036 amendment 8): the NWS forecast of the Denver high, a testnet forecaster.
	const LIVE_SOURCE = 'noaa.nws.forecast.bou';
	const LIVE_METRIC = 'noaa.ghcnd.usw00003017.tmax';
	let livePill = $state<{ state: PillState; text: string }>({ state: 'idle', text: 'Not asked' });
	let live = $state<TrackView | null>(null);
	let liveAsked = $state(false);
	let liveError = $state<string | null>(null);

	let pill = $state<{ state: PillState; text: string }>({ state: 'idle', text: 'Waiting' });
	let check = $state<TrackExampleCheck | null>(null);
	let asked = $state<string | null>(null);
	let logPill = $state<{ state: PillState; text: string }>({ state: 'idle', text: 'Not run' });
	let logCheck = $state<TrackLogCheck | null>(null);
	const short = (h: string) => `${h.slice(0, 10)}…`;

	const date = (unix: number) => new Date(unix * 1000).toISOString().slice(0, 10);
	// Review TR-S-R2-5: a break opportunity after the subject's kind ("entity:" / "example-a"), so
	// the history fits a 390 px phone without scrolling sideways.
	const kind = (s: string) => s.slice(0, s.indexOf(':') + 1);
	const rest = (s: string) => s.slice(s.indexOf(':') + 1);
	const closes = (r: TrackView) =>
		r.period_secs !== null
			? `${r.lead_secs ?? 0} s before the ${r.period_secs} s period it totals begins`
			: r.close_after_secs === 0
				? 'at the value’s own date'
				: r.close_after_secs === null
					? 'no rule'
					: `${r.close_after_secs} s after the value’s date`;

	async function load(file: string): Promise<string> {
		const r = await fetch(file);
		if (!r.ok) throw new Error(`${file}: HTTP ${r.status}`);
		return r.text();
	}

	async function run() {
		pill = { state: 'run', text: 'Checking' };
		check = null;
		asked = null;
		try {
			const seen: Record<string, Seen | null> = Object.fromEntries(SOURCES.map((s) => [s, seenOf(s, METRIC)]));
			const c = SOURCES.every((s) => seen[s])
				? await verifier.trackExampleSince(await load('level5/track-example-since.json'), JSON.stringify(seen))
				: await verifier.trackExample(await load('level5/track-example.json'));
			check = c;
			if (c.ok) {
				asked = c.records[0]?.since ?? null;
				for (const r of c.records) remember(r.source, r.metric, { commitment: c.commitment, as_of: c.as_of });
			}
			// Data review: the example checks out, but it is invented data: a neutral badge, not the green
			// "Checks out" of a real record (the section says none exists yet).
			pill = c.ok ? { state: 'idle', text: 'Example' } : { state: 'bad', text: c.problems[0] ?? 'Does not check out' };
		} catch (e) {
			pill = { state: 'bad', text: String(e) };
		}
	}

	function forgetSeen() {
		for (const s of SOURCES) forget(s, METRIC);
		void run();
	}

	async function rerun() {
		logPill = { state: 'run', text: 'Rerunning' };
		logCheck = null;
		try {
			const known = seenOf(SOURCES[0], METRIC)?.commitment ?? '';
			const [log, registry] = await Promise.all([load('level5/track-example-log.json'), load('level5/track-example-registry.json')]);
			const c = await verifier.trackExampleLog(log, known, registry);
			logCheck = c;
			logPill = c.ok ? { state: 'good', text: 'Same commitment' } : { state: 'bad', text: c.problems[0] ?? 'Does not check out' };
		} catch (e) {
			logPill = { state: 'bad', text: String(e) };
		}
	}

	// The node's record, checked in the browser as served, asked only when the reader presses the
	// button (a node with nothing published answers 404). No record yet (404, or a page with no node
	// behind it) is not a failure: every forecast stays sealed until its day ends.
	async function runLive() {
		liveAsked = true;
		livePill = { state: 'run', text: 'Asking the node' };
		live = null;
		liveError = null;
		try {
			const r = await fetch(`/v1/track/${LIVE_SOURCE}/${LIVE_METRIC}`, { headers: { accept: 'application/json' } });
			const type = r.headers.get('content-type') ?? '';
			if (!r.ok || !type.includes('json')) {
				livePill = { state: 'idle', text: 'No record yet' };
				return;
			}
			live = await verifier.trackRecord(await r.text(), LIVE_SOURCE, LIVE_METRIC);
			livePill = { state: 'good', text: 'Checks out' };
		} catch (e) {
			livePill = { state: 'bad', text: 'Does not check out' };
			liveError = String(e);
		}
	}

	onMount(run);
</script>

<section class="section" data-testid="track">
	<!-- Data review: folded by default (a less central section of a long page). -->
	<details class="secfold" data-testid="track-fold">
	<summary class="sh">
		<h2>Track records</h2>
		<Pill live state={pill.state} text={pill.text} />
		<p class="foldnote">How a source's earlier values compared with the answer that came later (level 5). No real track record yet: a test-network forecast and an invented example are shown.</p>
	</summary>
	<p class="muted lede">
		Level 5 is for values nobody can check yet, such as an early estimate. A track record shows how a source’s earlier values compared
		with the stronger answer that came later. It never raises a value’s level and never settles money.
	</p>
	<div class="panel" data-testid="track-empty">
		<!-- W12 round 14: the panel's title in the display face (`.ptitle`; it was bold 15 px sans). -->
		<p class="tight"><b class="ptitle">No real track records yet.</b></p>
		<p class="tight muted">
			A closing-time rule says when statements about a value must be in, before anyone can know the answer. Bitcoin entity flows now have one:
			statements on a day’s flow close two hours before that day starts. No source states on them yet, and the check that reads their answers
			from the chain is not built yet, so no source can be scored.
		</p>
	</div>

	<div class="panel" data-testid="track-live">
		<div class="sh">
			<p class="tight"><b class="ptitle">Forecast of the Denver high</b> <span class="ex-tag">test network</span></p>
			<Pill live state={livePill.state} text={livePill.text} />
		</div>
		<p class="tight muted" data-testid="track-live-note">
			The U.S. National Weather Service’s forecast of each day’s high at Denver International Airport, stated before the day starts through
			Onyx’s intake on the Base Sepolia test network. Each forecast stays sealed until its day ends. It is graded by the day’s NOAA NCEI record
			once Onyx observers check it, 30 to 60 days later.
		</p>
		<p class="tight muted" data-testid="track-live-limits">
			A demo of how scoring works, not an independent track record: an Onyx test key passes the forecast on (the NWS signs nothing), and NOAA
			runs both the forecast and the record that grades it.
		</p>
		<div class="rerun">
			<button type="button" class="btn ghost" data-testid="track-live-ask" onclick={runLive}>Ask the node</button>
		</div>
		{#if live}
			<p class="tight ititle" data-testid="track-live-headline">
				{live.statements} forecasts stated, {live.resolved} graded{#if live.rate}: agreed {live.rate}{/if}
			</p>
			{#if !live.established}
				<p class="tight muted" data-testid="track-live-too-few">
					Too few to score: {live.resolved} graded days ({live.min_resolved} needed). No rate or Brier score is shown yet.
				</p>
			{/if}
			<dl class="kv">
				<dt>Graded</dt>
				<dd>{live.agreed} within 0.5 °C, {live.disagreed} off</dd>
				<dt>Waiting</dt>
				<dd>{live.unresolved} not graded yet, {live.late} late</dd>
			</dl>
		{:else if liveError}
			<p class="tight muted err" data-testid="track-live-error">The record did not pass the check, so nothing is shown: {liveError}</p>
		{:else if liveAsked && livePill.state === 'idle'}
			<p class="tight muted" data-testid="track-live-none">
				No record served yet. The first forecasts were stated on <span class="nw">28 Oct 2026</span>; the first one opens after <span class="nw">30 Oct 2026, 00:00 UTC</span>, and the first grade
				can come in December.
			</p>
		{/if}
	</div>
	<!-- W12 round 13: the example's heading in the display face like every other title on the page (it
	     was the last bold 15 px sans heading, under a 46 px serif h2 and beside 28 px serif panel titles). -->
	<div class="sh">
		<h3 class="ptitle">Example <span class="ex-tag" data-testid="track-example-tag">example · invented data</span></h3>
	</div>
	{#if check?.ok}
		<p class="muted tight" data-testid="track-example-note">
			Invented sources and values on the example level-3 metric, to show what a record looks like. Not real data. Your browser checked
			each record against the example’s pinned commitment (as of <span class="nw">{dayText(date(check.as_of))}</span>) and recounted its whole history.
		</p>
		<p class="muted tight" data-testid="track-since">
			{#if asked}
				Your browser asked with the commitment it saw last (<span class="mono">{short(asked)}</span>) and checked that this one chains back to
				it and is not older.
			{:else}
				First visit: your browser now remembers this commitment and, next time, asks for the record since it.
			{/if}
		</p>
		<div class="rerun">
			<button type="button" class="btn ghost" data-testid="track-rerun" onclick={rerun}>Rerun from the logs</button>
			<span class="runstat {logPill.state}" aria-live="polite"><i aria-hidden="true"></i><span>{logPill.text}</span></span>
		</div>
		{#if logCheck?.ok}
			<p class="muted tight" data-testid="track-rerun-result">
				Your browser recomputed both records from the logs ({logCheck.statements} statements, {logCheck.answers} answers) and got the pinned
				commitment again. {logCheck.links} commitments checked, each keeping every statement and answer of the one before. The registry
				it recomputed with is the one served, rebuilt in your browser and checked to the commitment’s registry root
				(<span class="mono">{short(logCheck.registry_root ?? '')}</span>).
			</p>
		{:else if logCheck}
			<p class="muted tight" data-testid="track-rerun-result">The logs did not check, so they show nothing: {logCheck.problems[0]}</p>
		{/if}
		<div class="trk">
			{#each check.records as r (r.source)}
				<div class="panel" data-testid="track-ex-{r.source}">
					<p class="tight label"><span class="mono">{r.source}</span> <span class="ex-tag">example</span></p>
					<!-- W12 round 14: the headline is the card's title, an item title in the display face (`.ititle`) like
					     Run's steps and Home's roles (it was a mono 18 px sentence under the display-face "Example" heading);
					     the source's id above it stays in mono. -->
					<p class="tight ititle" data-testid="track-headline-{r.source}">
						Agreed {r.agreed} of {r.resolved} resolved{#if r.rate}&nbsp;({r.rate}){/if}
					</p>
					{#if !r.established}
						<p class="tight muted" data-testid="track-threshold-{r.source}">
							Not established: {r.resolved} resolved ({r.min_resolved} needed), {r.distinct_subjects} subjects ({r.min_subjects} needed). No rate
							is shown.
						</p>
					{/if}
					<dl class="kv">
						<dt>Level</dt>
						<dd>5, track record</dd>
						<dt>Metric</dt>
						<dd class="mono">{r.metric}</dd>
						<dt>First to agree</dt>
						<dd>{r.agreed_first}</dd>
						{#if r.mean_brier}
							<dt>Mean Brier</dt>
							<dd data-testid="track-brier-{r.source}">{r.mean_brier} (0 is best)</dd>
						{/if}
						<dt>Misses</dt>
						<dd>{r.disagreed} disagreed, {r.equivocated} with two answers to one question (a miss)</dd>
						<dt>Not counted</dt>
						<dd>{r.late} late, {r.void} void, {r.contested} contested, {r.self_resolved} self-resolved, {r.unresolved} unresolved</dd>
						{#if r.skill}
							<dt>Against repeating</dt>
							<dd data-testid="track-skill-{r.source}">
								{r.skill}: on {r.baseline_entries} resolved values with an earlier known value, this source agreed {r.baseline_source_agreed}; repeating
								that earlier value would have agreed {r.baseline_agreed}
							</dd>
						{/if}
						<dt>Sources</dt>
						<dd>one of {r.metric_sources} on this metric</dd>
						<dt>Coverage</dt>
						<dd>stated {r.resolved} of {r.metric_resolved_facts} resolved values</dd>
						<dt>Rule</dt>
						<dd>
							closes {closes(r)}; {r.tolerance === '0' ? 'exact match' : `within ${r.tolerance}`}; answered at level {r.resolver_levels.join(' or ')}
						</dd>
					</dl>
					<details>
						<summary>History, {r.statements} statements</summary>
						<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
						<div class="tablebox" tabindex="0" role="region" aria-label="History of {r.source}, example">
							<table>
								<thead><tr><th>Date (UTC)</th><th>Subject</th><th>Outcome</th><th>First</th></tr></thead>
								<tbody>
									{#each r.history as h (h.subject + h.as_of)}
										<tr><td class="mono">{date(h.as_of)}</td><td class="mono subj">{kind(h.subject)}<wbr />{rest(h.subject)}</td><td>{h.outcome}</td><td>{h.first ? 'yes' : ''}</td></tr>
									{/each}
								</tbody>
							</table>
						</div>
					</details>
				</div>
			{/each}
		</div>
	{:else if check?.unknown}
		<div class="panel" data-testid="track-unknown">
			<p class="tight">
				This server does not know the commitment your browser saw last, so nothing is shown. It may have rewritten its records, or it is
				another server.
			</p>
			<button type="button" class="btn ghost" data-testid="track-forget" onclick={forgetSeen}>Forget what this browser saw</button>
		</div>
	{:else if check && !check.ok}
		<p class="panel muted" data-testid="track-failed">The example did not pass the check, so nothing is shown.</p>
	{:else}
		<div class="panel"><Skeleton rows={2} label="Checking the example track records" /></div>
	{/if}
	</details>
</section>

<style>
	.tight {
		margin: 0;
	}
	.trk {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 22rem), 1fr));
		gap: var(--sp-4);
		/* Each card as tall as its own content (a short record is not stretched). */
		align-items: start;
	}
	.err {
		overflow-wrap: anywhere;
	}
	.label {
		color: var(--muted);
		font-size: var(--text-sm);
	}
	.rerun {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--sp-3);
	}
	@media (max-width: 40rem) {
		.tablebox th,
		.tablebox td {
			padding-inline: var(--sp-1);
		}
		.subj {
			white-space: normal;
		}
	}
	/* The tag stays in the body face inside the display-face heading (round 13). */
	.ex-tag {
		font-family: var(--body);
		font-size: var(--text-xs);
		font-weight: 500;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--muted);
		border: 1px solid currentColor;
		border-radius: var(--radius-pill);
		padding: var(--sp-0) var(--sp-2);
		vertical-align: middle;
	}
/* Owner review: a rerun's status beside its button is a dot and plain text (a pill there read as a
	   second, disabled button). */
	.runstat {
		display: inline-flex;
		align-items: center;
		gap: var(--sp-2);
		font-size: var(--text-sm);
		color: var(--muted);
	}
	.runstat > i {
		width: var(--sp-2);
		height: var(--sp-2);
		border-radius: 50%;
		background: currentColor;
		flex: none;
	}
	.runstat.run {
		color: var(--accent);
	}
	.runstat.good {
		color: var(--good);
	}
	.runstat.bad {
		color: var(--bad);
	}
</style>
