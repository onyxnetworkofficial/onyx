// Keccak-256 (the Ethereum variant, ADR-0003), no dependencies. Node's crypto has SHA3-256, whose
// padding differs, so it would not match the digest the intake checks (`keyProof.responseKeccak256`).
// Lanes are pairs of 32-bit words (low, high), so it runs at a useful speed on a 1 MiB answer.

const RC = [
	0x00000001, 0x00000000, 0x00008082, 0x00000000, 0x0000808a, 0x80000000, 0x80008000, 0x80000000, 0x0000808b, 0x00000000, 0x80000001, 0x00000000,
	0x80008081, 0x80000000, 0x00008009, 0x80000000, 0x0000008a, 0x00000000, 0x00000088, 0x00000000, 0x80008009, 0x00000000, 0x8000000a, 0x00000000,
	0x8000808b, 0x00000000, 0x0000008b, 0x80000000, 0x00008089, 0x80000000, 0x00008003, 0x80000000, 0x00008002, 0x80000000, 0x00000080, 0x80000000,
	0x0000800a, 0x00000000, 0x8000000a, 0x80000000, 0x80008081, 0x80000000, 0x00008080, 0x80000000, 0x80000001, 0x00000000, 0x80008008, 0x80000000,
];
/** Rotation of lane x + 5y. */
const ROT = [0, 1, 62, 28, 27, 36, 44, 6, 55, 20, 3, 10, 43, 25, 39, 41, 45, 15, 21, 8, 18, 2, 61, 56, 14];
const RATE = 136;

function permute(s) {
	const c = new Uint32Array(10);
	const b = new Uint32Array(50);
	for (let round = 0; round < 24; round++) {
		for (let x = 0; x < 5; x++) {
			c[2 * x] = s[2 * x] ^ s[2 * x + 10] ^ s[2 * x + 20] ^ s[2 * x + 30] ^ s[2 * x + 40];
			c[2 * x + 1] = s[2 * x + 1] ^ s[2 * x + 11] ^ s[2 * x + 21] ^ s[2 * x + 31] ^ s[2 * x + 41];
		}
		for (let x = 0; x < 5; x++) {
			const l = (x + 4) % 5;
			const r = (x + 1) % 5;
			const dl = c[2 * l] ^ ((c[2 * r] << 1) | (c[2 * r + 1] >>> 31));
			const dh = c[2 * l + 1] ^ ((c[2 * r + 1] << 1) | (c[2 * r] >>> 31));
			for (let y = 0; y < 25; y += 5) {
				s[2 * (x + y)] ^= dl;
				s[2 * (x + y) + 1] ^= dh;
			}
		}
		for (let x = 0; x < 5; x++)
			for (let y = 0; y < 5; y++) {
				const i = x + 5 * y;
				const lo = s[2 * i];
				const hi = s[2 * i + 1];
				const n = ROT[i];
				let rl, rh;
				if (n === 0) [rl, rh] = [lo, hi];
				else if (n < 32) [rl, rh] = [(lo << n) | (hi >>> (32 - n)), (hi << n) | (lo >>> (32 - n))];
				else if (n === 32) [rl, rh] = [hi, lo];
				else {
					const m = n - 32;
					[rl, rh] = [(hi << m) | (lo >>> (32 - m)), (lo << m) | (hi >>> (32 - m))];
				}
				const j = y + 5 * ((2 * x + 3 * y) % 5);
				b[2 * j] = rl;
				b[2 * j + 1] = rh;
			}
		for (let y = 0; y < 25; y += 5)
			for (let x = 0; x < 5; x++) {
				const i = x + y;
				const i1 = ((x + 1) % 5) + y;
				const i2 = ((x + 2) % 5) + y;
				s[2 * i] = b[2 * i] ^ (~b[2 * i1] & b[2 * i2]);
				s[2 * i + 1] = b[2 * i + 1] ^ (~b[2 * i1 + 1] & b[2 * i2 + 1]);
			}
		s[0] ^= RC[2 * round];
		s[1] ^= RC[2 * round + 1];
	}
}

/** Keccak-256 of `data` (a string, read as UTF-8, or bytes) as lower-case `0x` + 64 hex. */
export function keccak256(data) {
	const bytes = typeof data === 'string' ? new TextEncoder().encode(data) : data;
	const blocks = Math.floor(bytes.length / RATE) + 1;
	const padded = new Uint8Array(blocks * RATE);
	padded.set(bytes);
	padded[bytes.length] ^= 0x01;
	padded[padded.length - 1] ^= 0x80;
	const s = new Uint32Array(50);
	const view = new DataView(padded.buffer);
	for (let off = 0; off < padded.length; off += RATE) {
		for (let w = 0; w < RATE / 4; w++) s[w] ^= view.getUint32(off + 4 * w, true);
		permute(s);
	}
	const out = new DataView(new ArrayBuffer(32));
	for (let w = 0; w < 8; w++) out.setUint32(4 * w, s[w], true);
	return '0x' + Buffer.from(out.buffer).toString('hex');
}
