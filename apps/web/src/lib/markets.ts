// The S3/S6 contract rules in the browser (ADR-0042, partner tracker T10): the stake vote, the
// attack-cost bound, the payouts of a decided claim and of a parimutuel futures market. Integer
// math only (bigint), the same rounding as contracts/src/OnyxPayout.sol. The shared vectors in
// fixtures/markets/vectors.json run here (tests/t10.mjs) and through the real contracts
// (contracts/test/OnyxMarketsVectors.t.sol), so the two cannot drift. Nothing here moves tokens:
// the Markets page uses it to simulate the testnet contracts.

export const PPM = 1_000_000n;

export type Side = 'correct' | 'wrong' | 'none';
export type Outcome = 'correct' | 'wrong' | 'void';
export type Stake = { who: string; amount: bigint; side: Side };
export type Bet = { who: string; bucket: number; amount: bigint };

/** The treasury fee on the losing side's money: floor(losers × feePpm / 1e6). */
export const fee = (losers: bigint, feePpm: bigint): bigint => (losers * feePpm) / PPM;

/** A winner's payout: the stake back plus floor(stake × distributable / winningPool). */
export const share = (stake: bigint, distributable: bigint, winningPool: bigint): bigint =>
	winningPool === 0n ? 0n : stake + (stake * distributable) / winningPool;

/** What the vote alone decides: a side with at least `decisivePpm` of at least `quorum` revealed. */
export function voteOutcome(correct: bigint, wrong: bigint, quorum: bigint, decisivePpm: bigint): 'correct' | 'wrong' | null {
	const r = correct + wrong;
	if (r === 0n || r < quorum) return null;
	if (correct * PPM >= decisivePpm * r) return 'correct';
	if (wrong * PPM >= decisivePpm * r) return 'wrong';
	return null;
}

/** The smallest stake that wins a vote outright against `honest` on the other side; 0n when the
 * per-claim cap makes it impossible (the claim then goes to the fallback oracle). */
export function minAttackStake(honest: bigint, quorum: bigint, decisivePpm: bigint, minStake: bigint, maxPerClaim: bigint): bigint {
	const den = PPM - decisivePpm;
	let a = (decisivePpm * honest + den - 1n) / den;
	if (honest < quorum && quorum - honest > a) a = quorum - honest;
	if (a < minStake) a = minStake;
	return a + honest > maxPerClaim ? 0n : a;
}

export type ClaimInput = { bond: bigint; feePpm: bigint; submitReward: bigint; rewardPool: bigint; outcome: Outcome; stakes: Stake[] };
export type Settlement = { fee: bigint; dust: bigint; treasury: bigint; payouts: Record<string, bigint> };

/** Everyone's payout once a claim is decided, and what the treasury gets (OnyxStakes._finish and
 * withdraw, after every winner has withdrawn). The submitter is `submitter`. */
export function settleClaim(c: ClaimInput): Settlement {
	const payouts: Record<string, bigint> = { submitter: 0n };
	for (const s of c.stakes) payouts[s.who] = 0n;
	if (c.outcome === 'void') {
		payouts.submitter = c.bond;
		for (const s of c.stakes) payouts[s.who] += s.amount;
		return { fee: 0n, dust: 0n, treasury: 0n, payouts };
	}
	const sum = (side: Side) => c.stakes.filter((s) => s.side === side).reduce((n, s) => n + s.amount, 0n);
	const correct = sum('correct'), wrong = sum('wrong'), unrevealed = sum('none');
	const right = c.outcome === 'correct';
	const winningPool = right ? correct + c.bond : wrong;
	const losers = right ? wrong : correct + c.bond;
	const reward = right ? (c.submitReward <= c.rewardPool ? c.submitReward : c.rewardPool) : 0n;
	if (winningPool === 0n) return { fee: losers, dust: 0n, treasury: unrevealed + losers, payouts };
	const f = fee(losers, c.feePpm);
	const distributable = losers - f;
	let paid = 0n;
	if (right) {
		payouts.submitter = share(c.bond, distributable, winningPool) + reward;
		paid += payouts.submitter - reward;
	}
	for (const s of c.stakes) {
		if (s.side !== c.outcome) continue;
		const p = share(s.amount, distributable, winningPool);
		payouts[s.who] += p;
		paid += p;
	}
	const dust = winningPool + distributable - paid;
	return { fee: f, dust, treasury: unrevealed + f + dust, payouts };
}

/** The bucket a value falls in: the number of edges at or below it. */
export const bucketOf = (edges: bigint[], value: bigint): number => edges.filter((e) => e <= value).length;

export type MarketInput = { edges: bigint[]; feePpm: bigint; value: bigint | null; bets: Bet[] };
export type MarketResult = Settlement & { state: 'resolved' | 'void'; winner: number | null };

/** A parimutuel market's result: resolved from `value` (null: no answer by the deadline), or void
 * with every bet refunded when the winning bucket has no bets (OnyxFutures.resolve, withdraw). */
export function settleMarket(m: MarketInput): MarketResult {
	const payouts: Record<string, bigint> = {};
	const staked: Record<string, bigint> = {};
	for (const b of m.bets) {
		payouts[b.who] = 0n;
		staked[b.who] = (staked[b.who] ?? 0n) + b.amount;
	}
	const pools = Array.from({ length: m.edges.length + 1 }, () => 0n);
	for (const b of m.bets) pools[b.bucket] += b.amount;
	const total = pools.reduce((n, p) => n + p, 0n);
	const winner = m.value === null ? null : bucketOf(m.edges, m.value);
	if (winner === null || pools[winner] === 0n) {
		for (const w of Object.keys(staked)) payouts[w] = staked[w];
		return { state: 'void', winner, fee: 0n, dust: 0n, treasury: 0n, payouts };
	}
	const winningPool = pools[winner];
	const losers = total - winningPool;
	const f = fee(losers, m.feePpm);
	const distributable = losers - f;
	const onWinner: Record<string, bigint> = {};
	for (const b of m.bets) if (b.bucket === winner) onWinner[b.who] = (onWinner[b.who] ?? 0n) + b.amount;
	let paid = 0n;
	for (const [w, a] of Object.entries(onWinner)) {
		payouts[w] = share(a, distributable, winningPool);
		paid += payouts[w];
	}
	const dust = winningPool + distributable - paid;
	return { state: 'resolved', winner, fee: f, dust, treasury: f + dust, payouts };
}

/** What a futures market's badge reads from: the chain's state and times, the fact's value. */
export type MarketView = { state?: 'open' | 'resolved' | 'void'; trading?: boolean; resolveBy?: number; value?: string | null };

/** True when an open market has no observed value at or after its `resolveBy` (unix seconds, the
 * reader's clock at the read). From then anyone may call OnyxFutures.resolve, which voids the
 * market (reason 2), and only after that call does `withdraw` pay each bet back: nothing is paid
 * back by itself. A value recorded first settles it instead. */
export function voidable(m: MarketView, now: number): boolean {
	return m.state === 'open' && !m.trading && !m.value && !!m.resolveBy && now >= m.resolveBy;
}

/** A futures market's badge. From `resolveBy` on with no value it no longer says "waiting". A void
 * market pays nothing by itself: each bettor withdraws the whole bet. */
export function marketStatus(m: MarketView, now: number): string {
	if (m.state === 'resolved') return 'Settled';
	if (m.state === 'void') return 'Void, bets can be withdrawn';
	if (m.trading) return 'Trading open';
	return voidable(m, now) ? 'No value yet, can be voided' : 'Trading closed, waiting for the observed value';
}

/** Why a void futures market is void, in plain words (newcomer test 3, 2026-11-18: the card said
 * "Void" and gave no reason). OnyxFutures voids for two reasons only: no observed value by
 * `resolveBy`, or a value whose winning range had no bet. Empty for a market that is not void. */
export function voidReason(m: MarketView): string {
	if (m.state !== 'void') return '';
	return m.value
		? 'The observed value fell in a range nobody had bet on, so the market was voided. Every bet can be withdrawn in full.'
		: 'No value was recorded by the date, so the market was voided. Every bet can be withdrawn in full.';
}

/** How many of the markets read from the chain are in each state, as one line ("2 test markets
 * read: none open for bets, 1 settled, 1 void"). The heading never calls a market open by itself:
 * the word comes from the chain's own state. Empty before a read. */
export function marketTally(list: MarketView[]): string {
	if (!list.length) return '';
	const open = list.filter((m) => m.state === 'open' && m.trading).length;
	const waiting = list.filter((m) => m.state === 'open' && !m.trading).length;
	const settled = list.filter((m) => m.state === 'resolved').length;
	const voided = list.filter((m) => m.state === 'void').length;
	const parts = [open ? `${open} open for bets` : 'none open for bets', ...(waiting ? [`${waiting} closed and waiting for a value`] : []), ...(settled ? [`${settled} settled`] : []), ...(voided ? [`${voided} void`] : [])];
	return `${list.length} test ${list.length === 1 ? 'market' : 'markets'} read: ${parts.join(', ')}.`;
}
