//! ROADMAP C50 robustness (THREATS C12-9, C50-*): attacker bytes never panic
//! the decoder. A seeded sweep of random and mutated keys, signatures and
//! ciphertexts of every length near the real ones; each call returns, and a
//! ciphertext that is not the real one never opens. The coverage-guided twin
//! is the cargo-fuzz target `fuzz/fuzz_targets/tlock_open.rs`.

use onyx_tlock::{decrypt_unverified, BeaconKey, CIPHERTEXT_LEN};

const CT: &str = "9036411afba8c9e0b16fdd4f5d487f7233581a4a484003b2b57a113959826af119c82c3d10b195017377aba2763e46e2003ac078cb9b9f7fb4f36353dd1195074d28efceb4c54db2c3b241994c715783db19ee038a1bf96e4fb96e8e461432121616a12a64104241f6afc033542184c3dccb3a174fd8a7a79531d70c325eb2b8";
const SIG: &str = "a9080432fbe9dd72d25a78c092f6d507a418fd9f985f8b5e99ac380dc63d6d203198f4160e353055e9fbd8fccdfdbd37";
const ROUND: u64 = 746_511;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

/// Random bytes, or the real bytes with a few positions overwritten (keeps
/// the flag bits plausible so the curve decoders are reached).
fn mutate(rng: &mut Rng, real: &[u8]) -> Vec<u8> {
    match rng.next() % 4 {
        0 => {
            let n = (rng.next() % (real.len() as u64 + 3)) as usize;
            rng.bytes(n)
        }
        1 => {
            let mut v = real.to_vec();
            let i = (rng.next() as usize) % v.len();
            v[i] = rng.next() as u8;
            v
        }
        2 => {
            let mut v = rng.bytes(real.len());
            v[0] = (v[0] & 0x1f) | 0x80;
            v
        }
        _ => {
            let mut v = real.to_vec();
            v.truncate((rng.next() as usize) % (v.len() + 1));
            v
        }
    }
}

#[test]
fn c50_malformed_ciphertexts_signatures_and_keys_return_errors() {
    let k = BeaconKey::quicknet().expect("test input");
    let ct = hex::decode(CT).expect("test input");
    let sig = hex::decode(SIG).expect("test input");
    let key = k.to_compressed();
    let mut rng = Rng(0x0c50_0c50_0c50_0c50);
    let mut opened = 0;
    for _ in 0..1_500 {
        let c = mutate(&mut rng, &ct);
        let s = mutate(&mut rng, &sig);
        let p = mutate(&mut rng, &key);
        // Never a panic; never an opening of bytes that are not the real ones.
        if let Ok(o) = k.open(ROUND, &sig, &c) {
            assert_eq!(c, ct, "a changed ciphertext opened");
            assert_eq!(
                k.lock(ROUND, &o.msg, &o.sigma)
                    .expect("test input")
                    .to_vec(),
                ct
            );
            opened += 1;
        }
        let _ = k.open(ROUND, &s, &ct);
        let _ = decrypt_unverified(&s, &c);
        let _ = k.verify(rng.next(), &s);
        if let Ok(other) = BeaconKey::from_compressed(&p) {
            assert_eq!(
                other.to_compressed().to_vec(),
                p,
                "only canonical keys parse"
            );
            let _ = other.open(ROUND, &sig, &c);
        }
        if c.len() == CIPHERTEXT_LEN {
            let _ = k.lock(rng.next(), &[0; 16], &[0; 16]);
        }
    }
    // Branch 1 of `mutate` sometimes rewrites a byte with its own value.
    assert!(opened < 1_500);
}
