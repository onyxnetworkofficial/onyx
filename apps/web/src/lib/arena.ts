// The practice arena's market maker (the Markets arena page, src/routes/arena). A logarithmic
// market scoring rule (LMSR, Hanson 2003: https://mason.gmu.edu/~rhanson/mktscore.pdf) prices
// every market: each outcome share pays 1 credit if that outcome wins, the prices always sum to 1,
// and a buy moves the price by how much the market maker's cost function grows. Practice credits
// only: these are display numbers in one browser, not protocol data (ADR-0009 covers that), and no
// token moves. The real settlement rules stay in markets.ts and the testnet contracts.

/** Cost function C(q) = b ln(sum e^(q_i / b)), computed stably (log-sum-exp). */
export function cost(q: number[], b: number): number {
	const m = Math.max(...q.map((x) => x / b));
	return b * (m + Math.log(q.reduce((n, x) => n + Math.exp(x / b - m), 0)));
}

/** Each outcome's price (its implied probability): e^(q_i / b) / sum. */
export function prices(q: number[], b: number): number[] {
	const m = Math.max(...q.map((x) => x / b));
	const e = q.map((x) => Math.exp(x / b - m));
	const s = e.reduce((n, x) => n + x, 0);
	return e.map((x) => x / s);
}

/** Shares of outcome `i` that `spend` credits buy: solves C(q + s e_i) - C(q) = spend exactly. */
export function sharesFor(q: number[], b: number, i: number, spend: number): number {
	if (!(spend > 0)) return 0;
	const target = cost(q, b) + spend;
	// e^(target/b) = sum_{j != i} e^(q_j/b) + e^((q_i+s)/b); scaled by e^(-m) for stability.
	const m = Math.max(target / b, ...q.map((x) => x / b));
	const others = q.reduce((n, x, j) => (j === i ? n : n + Math.exp(x / b - m)), 0);
	return b * (Math.log(Math.exp(target / b - m) - others) + m) - q[i];
}

/** Credits returned for selling `shares` of outcome `i` back to the market maker. */
export function refundFor(q: number[], b: number, i: number, shares: number): number {
	if (!(shares > 0)) return 0;
	const after = q.slice();
	after[i] -= shares;
	return cost(q, b) - cost(after, b);
}

/** Starting quantities that give the wanted opening prices (q_i = b ln p_i). */
export function seed(p: number[], b: number): number[] {
	const s = p.reduce((n, x) => n + x, 0);
	return p.map((x) => b * Math.log(x / s));
}

/** A quote for a buy: shares, average price, the payout if it wins, and the price after. */
export function quote(q: number[], b: number, i: number, spend: number) {
	const shares = sharesFor(q, b, i, spend);
	const after = q.slice();
	after[i] += shares;
	return { shares, avg: shares > 0 ? spend / shares : 0, payout: shares, priceAfter: prices(after, b)[i] };
}

/** A small, repeatable random source (mulberry32), so a seeded history draws the same each load. */
export function rng(seed: number): () => number {
	let a = seed >>> 0;
	return () => {
		a = (a + 0x6d2b79f5) >>> 0;
		let t = a;
		t = Math.imul(t ^ (t >>> 15), t | 1);
		t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
		return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
	};
}

/** A parlay's payout: the stake over the product of the legs' prices (every leg must win). */
export const parlayPayout = (stake: number, legPrices: number[]): number =>
	legPrices.length === 0 ? 0 : stake / legPrices.reduce((n, p) => n * p, 1);

/** The level for an XP total: each level needs 150 more XP than the one before. */
export function level(xp: number): { level: number; into: number; need: number } {
	let lv = 1;
	let need = 200;
	let left = xp;
	while (left >= need) {
		left -= need;
		lv += 1;
		need += 150;
	}
	return { level: lv, into: left, need };
}
