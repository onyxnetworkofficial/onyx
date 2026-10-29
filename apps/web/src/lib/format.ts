// One number formatter for the whole site: `toLocaleString('en-US')` builds a new
// Intl.NumberFormat on every call (W4: 1.3 s of the Data page's main thread at 4x CPU
// throttling went to it); `format` on one shared instance gives the same text.
const EN = new Intl.NumberFormat('en-US');

/** A number or bigint grouped the en-US way ("846,000"); same text as `toLocaleString('en-US')`. */
export const group = (n: number | bigint): string => EN.format(n);

/** Satoshis (decimal string, may be negative) as BTC with 8 decimals. */
export function btc(sats: string | bigint): string {
	const s = BigInt(sats);
	const neg = s < 0n;
	const a = neg ? -s : s;
	const whole = EN.format(a / 100000000n);
	const frac = (a % 100000000n).toString().padStart(8, '0');
	return `${neg ? '−' : ''}${whole}.${frac}`;
}
export const short = (h: string, a = 10, b = 6) => (h.length > a + b + 1 ? `${h.slice(0, a)}…${h.slice(-b)}` : h);
export const int = (n: number | string) => EN.format(Number(n));
