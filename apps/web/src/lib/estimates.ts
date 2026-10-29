// The estimate layer on the site (ADR-0050, docs/spec/estimates.md, NORTH-STAR NS14).
// An estimate is a value Onyx did not check. It is shown apart from checked values, with its
// method and its error band, never with a proof level and never inside a total.
//
// The one estimate shown today is made in the reader's browser from checked figures: the mean of
// the daily fees over the full UTC days of the checked Bitcoin window, with a 95% interval
// (Student's t). The inputs are checked (level 1); the mean is a statement about days outside
// the window, so it is an estimate. All math is integer satoshis (BigInt); no float touches a value.

/** One estimate as the card shows it. Amounts are whole satoshis as decimal strings. */
export type SiteEstimate = {
	metric: string;
	title: string;
	unit: string;
	value: string;
	lo: string;
	hi: string;
	coveragePpm: number;
	method: string;
	methodText: string;
	inputs: string[];
};

/** Student's t, 97.5% quantile, times 1000, for 1..30 degrees of freedom; 1960 above that. */
const T975 = [12706, 4303, 3182, 2776, 2571, 2447, 2365, 2306, 2262, 2228, 2201, 2179, 2160, 2145, 2131, 2120, 2110, 2101, 2093, 2086, 2080, 2074, 2069, 2064, 2060, 2056, 2052, 2048, 2045, 2042];
export const t975Milli = (df: number) => (df >= 1 && df <= T975.length ? T975[df - 1] : 1960);

/** Floor square root of a non-negative BigInt. */
export function isqrt(n: bigint): bigint {
	if (n < 0n) throw new Error('isqrt of a negative number');
	if (n < 2n) return n;
	let x = n;
	let y = (x + 1n) / 2n;
	while (y < x) {
		x = y;
		y = (x + n / x) / 2n;
	}
	return x;
}

export const MEAN_CI95_METHOD = 'onyx.mean-ci95.v1';
export const MEAN_CI95_TEXT =
	'Mean of the inputs; 95% interval = mean ± t(0.975, n−1) × s / √n, s the sample standard deviation; integers, rounded down.';

/** Mean and 95% interval of `xs` (whole units), or null below two inputs. */
export function meanCi95(xs: bigint[]): { mean: bigint; lo: bigint; hi: bigint } | null {
	const n = BigInt(xs.length);
	if (n < 2n) return null;
	const sum = xs.reduce((a, b) => a + b, 0n);
	const mean = sum / n;
	// Sample variance with the exact mean: Σ(n·x − sum)² / (n²·(n−1)).
	const ss = xs.reduce((a, x) => a + (n * x - sum) ** 2n, 0n);
	const varOverN = ss / (n * n * (n - 1n) * n);
	const half = (BigInt(t975Milli(xs.length - 1)) * isqrt(varOverN * 1_000_000n)) / 1_000_000n;
	return { mean, lo: mean - half, hi: mean + half };
}

type FeeDay = { date: string; fees_sats: string; first_time: number; last_time: number };

/** A UTC day the window covers whole (its first block before 01:00, its last after 23:00). */
const fullDay = (d: FeeDay) => {
	const hm = (t: number) => new Date(t * 1000).toISOString().slice(11, 16);
	return hm(d.first_time) < '01:00' && hm(d.last_time) > '23:00';
};

/** The daily-fee estimate from the checked window's days, or null when fewer than two are whole. */
export function feeMeanEstimate(days: FeeDay[]): SiteEstimate | null {
	const whole = days.filter(fullDay);
	const ci = meanCi95(whole.map((d) => BigInt(d.fees_sats)));
	if (!ci) return null;
	return {
		metric: 'estimate.btc.fees_per_day.mean',
		title: 'Bitcoin fees per day, typical',
		unit: 'BTC per UTC day',
		value: ci.mean.toString(),
		lo: (ci.lo < 0n ? 0n : ci.lo).toString(),
		hi: ci.hi.toString(),
		coveragePpm: 950_000,
		method: MEAN_CI95_METHOD,
		methodText: MEAN_CI95_TEXT,
		inputs: whole.map((d) => d.date)
	};
}
