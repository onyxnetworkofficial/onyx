<script lang="ts">
	// NS11 (ADR-0042 amendment 2): the futures markets live on Base Sepolia. The site's file names
	// only what the chain cannot say (addresses, each market's title and scale); pools, state, the
	// observed value and the winner are read from the chain through the reader's own RPC (a public
	// Base Sepolia RPC by default, no API key), and the Rust verifier decodes and checks every answer
	// (verify/src/markets_view.rs). Nothing is read until the reader asks, so opening the page sends
	// no request off the site. tONYX is a test token with no value. The same read shows the test
	// insurance products (NL18, OnyxParametric). Every read is pinned to one block (THREATS NL6-20),
	// each answer is capped before it is parsed (NL6-21), and the page only ever reads (NL6-23).
	import { onMount } from 'svelte';
	import Pill, { type PillState } from '#lib/components/Pill.svelte';
	import { verifier, type LiveMarket, type LiveProduct, type MarketsLive } from '#lib/verifier.ts';

	const FILE = 'markets/base-sepolia.json';
	const DEFAULT_RPC = 'https://sepolia.base.org';
	// `number` is the market's name on the page; markets on the guarded futures (NL23) count on from the first futures' ids.
	type Meta = { id: number; number?: number; title: string; question: string; recordDate: string; source: string; sourceUrl: string; unit: string };
	type ProductMeta = { id: number; title: string; question: string; recordDate: string; source: string; sourceUrl: string; unit: string };
	type File = {
		network: string;
		explorer: string;
		futures: string;
		facts: string;
		token: string;
		stakes: string;
		panel: string;
		registry: string;
		markets: Meta[];
		insurance?: { address: string; facts: string; futures: string; products: ProductMeta[]; markets?: Meta[] };
	};
	/** Largest RPC answer the page parses (THREATS NL6-21). */
	const MAX_ANSWER = 64 * 1024;
	/** Pool shares are shown only from this many bets: a few bets are no odds (THREATS NL6-22). */
	const MIN_SHARE_BETS = 10;

	let text = '';
	let file = $state<File | null>(null);
	let fileError = $state('');
	let rpc = $state(DEFAULT_RPC);
	let live = $state<MarketsLive | null>(null);
	let readError = $state('');
	let pill = $state<{ state: PillState; text: string }>({ state: 'idle', text: 'Not read yet' });
	let readAt = $state(0);

	onMount(async () => {
		try {
			const r = await fetch(FILE);
			if (!r.ok) throw new Error(`${FILE}: HTTP ${r.status}`);
			text = await r.text();
			file = JSON.parse(text) as File;
		} catch (e) {
			fileError = e instanceof Error ? e.message : String(e);
		}
	});

	/** Only an https RPC, or one on this machine; never with credentials. */
	function rpcUrl(s: string): URL {
		const u = new URL(s.trim());
		const local = u.protocol === 'http:' && (u.hostname === '127.0.0.1' || u.hostname === 'localhost');
		if (u.protocol !== 'https:' && !local) throw new Error('the RPC must be an https:// URL (or http on 127.0.0.1)');
		if (u.username || u.password) throw new Error('the RPC URL must not carry a user name or password');
		return u;
	}

	async function rpcCall(url: URL, method: string, params: unknown[]): Promise<unknown> {
		const ctl = new AbortController();
		const t = setTimeout(() => ctl.abort(), 15000);
		try {
			const r = await fetch(url, {
				method: 'POST',
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
				credentials: 'omit',
				referrerPolicy: 'no-referrer',
				signal: ctl.signal
			});
			if (!r.ok) throw new Error(`${method}: HTTP ${r.status}`);
			const body = await r.text();
			if (body.length > MAX_ANSWER) throw new Error(`${method}: the RPC's answer is over ${MAX_ANSWER / 1024} KiB`);
			const j = JSON.parse(body) as { result?: unknown; error?: { message?: string } };
			if (j.error) throw new Error(`${method}: ${j.error.message ?? 'error'}`);
			return j.result ?? null;
		} finally {
			clearTimeout(t);
		}
	}

	// The page only carries the reads; the verifier names the calls and decides.
	async function read() {
		readError = '';
		pill = { state: 'run', text: 'Reading' };
		try {
			const url = rpcUrl(rpc);
			const { calls } = await verifier.marketsCalls(text);
			const chainId = await rpcCall(url, 'eth_chainId', []);
			// One block for every read, so the view never mixes two blocks (THREATS NL6-20).
			const block = await rpcCall(url, 'eth_blockNumber', []);
			if (typeof block !== 'string' || !/^0x[0-9a-f]{1,16}$/i.test(block)) throw new Error('eth_blockNumber: not a block number');
			const results: Record<string, unknown> = {};
			for (const c of calls) results[c.key] = await rpcCall(url, 'eth_call', [{ to: c.to, data: c.data }, block]);
			readAt = Math.floor(Date.now() / 1000);
			live = await verifier.marketsLive(text, JSON.stringify({ chainId, results }), readAt);
			pill = live.ok ? { state: 'good', text: 'Read from the chain' } : { state: 'bad', text: live.problems[0] ?? 'Refused' };
		} catch (e) {
			live = null;
			readError = e instanceof Error ? e.message : String(e);
			pill = { state: 'bad', text: 'Could not read' };
		}
	}

	const compact = new Intl.NumberFormat('en-US', { notation: 'compact', maximumFractionDigits: 2 });
	const money = (d: string, unit: string) => `${unit === 'USD' ? '$' : ''}${compact.format(Number(d))}${unit === 'USD' ? '' : ` ${unit}`}`;
	const exact = (d: string, unit: string) => `${unit === 'USD' ? '$' : ''}${Number(d).toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}${unit === 'USD' ? '' : ` ${unit}`}`;
	function bucketLabel(b: { low: string | null; high: string | null }, unit: string) {
		if (b.low === null && b.high !== null) return `Under ${money(b.high, unit)}`;
		if (b.high === null && b.low !== null) return `${money(b.low, unit)} or more`;
		return `${money(b.low ?? '0', unit)} to ${money(b.high ?? '0', unit)}`;
	}
	const tokens = (d: string) => Number(d).toLocaleString('en-US', { maximumFractionDigits: 2 });
	const pct = (ppm: string) => `${Math.round(Number(ppm) / 10000)}%`;
	const when = (t: number) => `${new Date(t * 1000).toISOString().slice(0, 16).replace('T', ' ')} UTC`;
	const short = (a: string) => `${a.slice(0, 6)}…${a.slice(-4)}`;
	function status(m: LiveMarket): string {
		if (m.state === 'resolved') return 'Settled';
		if (m.state === 'void') return 'Void, every bet refunded';
		return m.trading ? 'Trading open' : 'Trading closed, waiting for the observed value';
	}
	const liveOf = (id: number) => live?.markets.find((m) => m.id === id) ?? null;
	const guardedOf = (id: number) => live?.insurance?.markets?.find((m) => m.id === id) ?? null;
	const productOf = (id: number) => live?.insurance?.products.find((p) => p.id === id) ?? null;
	const value = (d: string, unit: string) => (unit === '°C' ? `${d} °C` : `${d} ${unit}`);
	function cover(p: LiveProduct, unit: string): string {
		return `${p.cmp === 'atmost' ? 'at most' : 'at least'} ${value(p.threshold ?? '0', unit)}`;
	}
	function productStatus(p: LiveProduct): string {
		if (p.state === 'paid') return 'Paid out';
		if (p.state === 'expired') return 'Expired, no payout';
		if (p.state === 'void') return 'Void, premiums refunded';
		if (!p.buying) return 'Closed, waiting for the observed value';
		return sellable(p) ? 'Open for cover' : 'No cover for sale: the pool has no free funds';
	}
	// A policy is sold only when the pool's free funds back its whole payout (R-PI-4).
	const sellable = (p: LiveProduct) => !!p.buying && Number(live?.insurance?.free ?? '0') >= Number(p.payout ?? '0');
</script>

{#snippet card(meta: Meta, m: LiveMarket | null, tid: string)}
	<article class="lcard" data-testid={`live-market-${tid}`}>
		<div class="ctop"><span class="cat">Economy · market {meta.number ?? meta.id}</span>{#if m?.ok}<span class="state" class:open={m.trading} class:done={m.state === 'resolved'} data-testid={`live-state-${tid}`}>{status(m)}</span>{/if}</div>
		<h3>{meta.question}</h3>
		{#if m && !m.ok}
			<p class="err" data-testid={`live-problems-${tid}`}>Refused: {m.problems.join('; ')}</p>
		{:else if m?.buckets}
			<ul class="buckets" data-testid={`live-buckets-${tid}`}>
				{#each m.buckets as b, k}
					{@const shares = (m.bettors ?? 0) >= MIN_SHARE_BETS}
					<li class:win={m.winner === k} class:few={!shares}>
						<span class="bl">{bucketLabel(b, meta.unit)}{#if m.winner === k}<span class="won">Won</span>{/if}</span>
						{#if shares}
							<span class="bar"><i style:width={pct(b.chancePpm)}></i></span>
							<b class="mono">{pct(b.chancePpm)}</b>
						{/if}
						<span class="fine mono">{tokens(b.pool)} tONYX · {b.bettors} {b.bettors === 1 ? 'bet' : 'bets'}</span>
					</li>
				{/each}
			</ul>
			<p class="fine">
				{(m.bettors ?? 0) >= MIN_SHARE_BETS ? 'Percentages are each bucket\'s share of the pool, not odds.' : `Each bucket's share of the pool shows from ${MIN_SHARE_BETS} bets.`} Winners split the
				losing pools, less a 2% fee.
			</p>
			<dl class="lfacts">
				<div><dt>Pool</dt><dd class="mono" data-testid={`live-total-${tid}`}>{tokens(m.total ?? '0')} tONYX</dd></div>
				<div><dt>Bettors</dt><dd class="mono">{m.bettors}</dd></div>
				<div><dt>Trading closes</dt><dd class="mono">{when(m.tradingClose ?? 0)}</dd></div>
				<div><dt>Observers may read it from</dt><dd class="mono">{when(m.observableAt ?? 0)}</dd></div>
				<div><dt>Observed value</dt><dd class="mono" data-testid={`live-value-${tid}`}>{m.value ? `${exact(m.value, meta.unit)} (level ${m.level})` : 'Not on chain yet'}</dd></div>
				<div><dt>Refund if no value by</dt><dd class="mono">{when(m.resolveBy ?? 0)}</dd></div>
			</dl>
		{:else}
			<div class="pending" aria-hidden="true"><i></i><i></i><i></i></div>
		{/if}
		<p class="src fine">Settles from <a href={meta.sourceUrl} rel="noopener noreferrer" target="_blank">{meta.source}</a>, record date <span class="nowrap">{meta.recordDate}</span>, as Onyx observers record it in the registry.</p>
	</article>
{/snippet}

<section class="live" aria-labelledby="live-title" data-testid="live-markets">
	<div class="lhead">
		<div>
			<p class="eyebrow"><span class="ldot" aria-hidden="true"></span>Live on {file?.network ?? 'Base Sepolia'} · test token</p>
			<h2 id="live-title">Test markets and insurance on Base Sepolia</h2>
		</div>
		<Pill live state={pill.state} text={pill.text} />
	</div>
	<p class="lede">
		These markets and insurance policies run on smart contracts on Base Sepolia, a public test network. Bets and cover are in tONYX, a test token with no value:
		no real money is at stake. Each one settles from the value Onyx observers record on chain (level 2), never from a vote. Pools and results below come from
		the chain itself, read by your browser.
	</p>
	{#if fileError}
		<p class="err" role="alert">{fileError}</p>
	{:else if file}
		<div class="reader">
			<button type="button" class="readbtn" onclick={read} disabled={pill.state === 'run'} data-testid="live-read">{live ? 'Read the chain again' : 'Read the chain'}</button>
			<p class="fine readnote">{live ? '' : 'Press to load the pools, results and policies. '}Your browser reads the contracts straight from {file.network}, at one block. Nothing goes through Onyx, and the page only reads.</p>
		</div>
		<details class="rpcmore" data-testid="live-rpc-more">
			<summary>Use a different RPC</summary>
			<label class="rpc"><span>RPC your browser asks</span><input type="url" bind:value={rpc} spellcheck="false" autocomplete="off" data-testid="live-rpc" /></label>
			<p class="fine">The RPC's operator sees your IP address. Do not paste an RPC URL that carries an API key: the page does not store it, but the RPC sees it.</p>
		</details>
		{#if readError}<p class="err" role="alert" data-testid="live-error">{readError}</p>{/if}
		{#if live && !live.ok && live.problems.length}<p class="err" role="alert" data-testid="live-refused">{live.problems.join('; ')}</p>{/if}
		<div class="lcards">
			{#each file.markets as meta (meta.id)}
				{@render card(meta, liveOf(meta.id), String(meta.id))}
			{/each}
			{#each file.insurance?.markets ?? [] as meta (meta.id)}
				{@render card(meta, guardedOf(meta.id), `g${meta.id}`)}
			{/each}
		</div>
		{#if file.insurance}
			<h3 class="ihead" id="ins-title">Test insurance</h3>
			<p class="fine ilede">
				Each policy pays a fixed amount when the observed value crosses its line, with no claim to file. It pays on the recorded value at one place and day,
				not on anyone's actual loss (basis risk). An underwriter's pool backs every payout before a policy is sold.
			</p>
			{#if live?.insurance && !live.insurance.ok && live.insurance.problems.length}<p class="err" role="alert" data-testid="ins-refused">{live.insurance.problems.join('; ')}</p>{/if}
			{#if live?.insurance?.pool !== undefined}
				<p class="fine mono" data-testid="ins-pool">Pool {tokens(live.insurance.pool ?? '0')} tONYX · held for payouts {tokens(live.insurance.reserved ?? '0')} tONYX</p>
			{/if}
			<div class="lcards" data-testid="live-insurance">
				{#each file.insurance.products as meta (meta.id)}
					{@const p = productOf(meta.id)}
					<article class="lcard" data-testid={`live-product-${meta.id}`}>
						<div class="ctop"><span class="cat">Insurance · product {meta.id}</span>{#if p?.ok}<span class="state" class:open={sellable(p)} class:done={p.state === 'paid'} data-testid={`product-state-${meta.id}`}>{productStatus(p)}</span>{/if}</div>
						<h3>{meta.title}</h3>
						<p class="fine">{meta.question}</p>
						{#if p && !p.ok}
							<p class="err" data-testid={`product-problems-${meta.id}`}>Refused: {p.problems.join('; ')}</p>
						{:else if p?.payout}
							<dl class="lfacts">
								<div><dt>Pays</dt><dd class="mono" data-testid={`product-terms-${meta.id}`}>{tokens(p.payout)} tONYX if {cover(p, meta.unit)}</dd></div>
								<div><dt>Premium</dt><dd class="mono">{tokens(p.premium ?? '0')} tONYX</dd></div>
								<div><dt>Policies sold</dt><dd class="mono">{p.sold} of {p.maxPolicies}</dd></div>
								<div><dt>Cover sold until</dt><dd class="mono">{when(p.buyBy ?? 0)}</dd></div>
								<div><dt>Observers may read it from</dt><dd class="mono">{when(p.observableAt ?? 0)}</dd></div>
								<div><dt>Observed value</dt><dd class="mono" data-testid={`product-value-${meta.id}`}>{p.value ? `${value(p.value, meta.unit)} (level ${p.level})` : 'Not on chain yet'}</dd></div>
							</dl>
						{:else}
							<div class="pending" aria-hidden="true"><i></i><i></i></div>
						{/if}
						<p class="src fine">Settles from <a href={meta.sourceUrl} rel="noopener noreferrer" target="_blank">{meta.source}</a>, record date <span class="nowrap">{meta.recordDate}</span>, as Onyx observers record it in the registry.</p>
					</article>
				{/each}
			</div>
		{/if}
		<details class="contracts">
			<summary>Contracts on {file.network}</summary>
			<ul>
				{#each [['Futures', file.futures], ['Observed facts', file.facts], ['Stakes', file.stakes], ['Drawn panel', file.panel], ['tONYX test token', file.token], ['Registry', file.registry], ...(file.insurance ? [['Insurance', file.insurance.address], ['Guarded facts', file.insurance.facts], ['Guarded futures', file.insurance.futures]] : [])] as [name, addr]}
					<li><span>{name}</span><a class="mono" href={`${file.explorer}/address/${addr}`} rel="noopener noreferrer" target="_blank">{short(addr)}</a></li>
				{/each}
			</ul>
		</details>
	{/if}
</section>

<style>
	.live { padding: var(--sp-5); margin-bottom: var(--sp-6); border: 1px solid var(--accent-line); border-radius: var(--radius-lg); background: var(--panel); }
	.lhead { display: flex; justify-content: space-between; align-items: flex-start; gap: var(--sp-3); flex-wrap: wrap; }
	.lhead h2 { font-size: var(--text-xl); line-height: var(--lh-tight); margin: var(--sp-1) 0 0; }
	.eyebrow { display: inline-flex; align-items: center; gap: var(--sp-2); }
	.ldot { width: 8px; height: 8px; border-radius: 50%; background: var(--good); flex-shrink: 0; }
	.lede { color: var(--muted); max-width: 46rem; margin: var(--sp-3) 0 var(--sp-4); }
	.reader { display: flex; flex-wrap: wrap; align-items: center; gap: var(--sp-3) var(--sp-4); }
	.readnote { margin: 0; flex: 1 1 18rem; max-width: 40rem; }
	.rpcmore { margin-top: var(--sp-2); font-size: var(--text-sm); }
	.rpcmore summary { cursor: pointer; min-height: 44px; display: inline-flex; align-items: center; color: var(--muted); }
	.rpcmore[open] { display: grid; gap: var(--sp-2); }
	.rpc { display: grid; gap: var(--sp-1); max-width: 32rem; min-width: 0; font-size: var(--text-xs); color: var(--muted); }
	/* Before the first read: the card's shape, quietly, instead of a lone line of text. */
	.pending { display: grid; gap: var(--sp-2); }
	.pending i { display: block; height: 28px; border-radius: var(--radius-sm); background: var(--line); opacity: 0.5; }
	.pending i:nth-child(2) { width: 85%; }
	.pending i:nth-child(3) { width: 70%; }
	.rpc input { min-height: 44px; width: 100%; font-family: var(--mono); font-size: var(--text-sm); }
	.readbtn { min-height: 44px; padding-inline: var(--sp-5); border-radius: var(--radius-pill); border: 1px solid var(--accent); background: var(--accent); color: var(--accent-ink); font-weight: 500; }
	.readbtn:disabled { opacity: 0.6; }
	.fine { font-size: var(--text-xs); color: var(--muted); }
	.err { color: var(--bad); font-size: var(--text-sm); overflow-wrap: anywhere; }
	.lcards { display: grid; grid-template-columns: repeat(auto-fill, minmax(min(100%, 22rem), 1fr)); gap: var(--sp-3); margin-top: var(--sp-4); }
	.lcard { display: grid; gap: var(--sp-3); padding: var(--sp-4); border: 1px solid var(--line); border-radius: var(--radius-lg); background: var(--bg, transparent); }
	.ctop { display: flex; justify-content: space-between; align-items: center; gap: var(--sp-2); flex-wrap: wrap; }
	.cat { font-size: var(--text-xs); color: var(--muted); text-transform: uppercase; letter-spacing: 0.06em; }
	.state { padding: var(--sp-0) var(--sp-2); border-radius: var(--radius-pill); border: 1px solid var(--line-strong); font-size: var(--text-xs); color: var(--muted); }
	.state.open { border-color: var(--good-line); color: var(--good); background: var(--good-soft); }
	.state.done { border-color: var(--accent-line); color: var(--ink); background: var(--accent-soft); }
	.lcard h3 { font-size: var(--text-base); line-height: var(--lh-snug); font-weight: 500; margin: 0; text-wrap: pretty; }
	.buckets { list-style: none; padding: 0; margin: 0; display: grid; gap: var(--sp-2); }
	.buckets li { display: grid; grid-template-columns: minmax(0, 1fr) 5rem 3rem; grid-template-rows: auto auto; align-items: center; column-gap: var(--sp-2); padding: var(--sp-2); border-radius: var(--radius-sm); }
	.buckets li.win { background: var(--good-soft); }
	.buckets .bl { font-size: var(--text-sm); }
	.buckets .bar { height: 6px; border-radius: var(--radius-pill); background: var(--line); overflow: hidden; }
	.buckets .bar i { display: block; height: 100%; background: var(--accent); }
	.buckets li.win .bar i { background: var(--good); }
	.buckets b { text-align: right; font-weight: 500; }
	.buckets .fine { grid-column: 1 / -1; }
	/* Under ten bets there are no shares to draw: the bucket and its pool sit on one line. */
	.buckets li.few { grid-template-columns: minmax(0, 1fr) auto; grid-template-rows: auto; }
	.buckets li.few .fine { grid-column: auto; text-align: right; }
	.won { margin-left: var(--sp-2); padding: 0 var(--sp-2); border-radius: var(--radius-pill); font-size: var(--text-xs); color: var(--good); border: 1px solid var(--good-line); }
	.mono { font-family: var(--mono); font-variant-numeric: tabular-nums; }
	.lfacts { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: var(--sp-2) var(--sp-4); margin: 0; }
	.lfacts div { display: grid; gap: var(--sp-0); min-width: 0; }
	.lfacts dt { font-size: var(--text-xs); color: var(--muted); }
	.lfacts dd { margin: 0; font-size: var(--text-sm); overflow-wrap: anywhere; }
	.src { margin: 0; }
	.ihead { font-family: var(--display); font-weight: 400; font-size: var(--text-xl); line-height: var(--lh-tight); margin: var(--sp-6) 0 var(--sp-1); }
	.nowrap { white-space: nowrap; }
	.ilede { max-width: 46rem; margin: 0 0 var(--sp-2); }
	.contracts { margin-top: var(--sp-4); font-size: var(--text-sm); }
	.contracts summary { cursor: pointer; min-height: 44px; display: flex; align-items: center; color: var(--muted); }
	.contracts ul { list-style: none; padding: 0; margin: var(--sp-2) 0 0; display: grid; gap: var(--sp-1); }
	.contracts li { display: flex; justify-content: space-between; gap: var(--sp-3); flex-wrap: wrap; }
	.contracts a { display: inline-flex; align-items: center; min-height: 44px; }
	@media (max-width: 640px) { .lfacts { grid-template-columns: 1fr; } .readbtn { width: 100%; } }
</style>
