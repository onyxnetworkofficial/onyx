//! The time-lock decoder (ROADMAP C50, THREATS C12-9): `onyx_tlock` opens
//! attacker-chosen ciphertexts, signatures and keys without panicking; near
//! the real Go-made quicknet vector (round 746511 and its published
//! signature), nothing but the real bytes opens; any `(msg, sigma)` locked to
//! that round opens to itself and re-encrypts byte for byte.
#![no_main]

use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use onyx_tlock::{decrypt_unverified, BeaconKey};

/// drand/tlock-js `test/rfc-scheme.test.ts` (made by Go), quicknet round 746511.
const CT: [u8; 128] = hex_lit("9036411afba8c9e0b16fdd4f5d487f7233581a4a484003b2b57a113959826af119c82c3d10b195017377aba2763e46e2003ac078cb9b9f7fb4f36353dd1195074d28efceb4c54db2c3b241994c715783db19ee038a1bf96e4fb96e8e461432121616a12a64104241f6afc033542184c3dccb3a174fd8a7a79531d70c325eb2b8");
/// api.drand.sh quicknet round 746511.
const SIG: [u8; 48] = hex_lit("a9080432fbe9dd72d25a78c092f6d507a418fd9f985f8b5e99ac380dc63d6d203198f4160e353055e9fbd8fccdfdbd37");
const ROUND: u64 = 746_511;

const fn hex_lit<const N: usize>(s: &str) -> [u8; N] {
    let b = s.as_bytes();
    let mut out = [0u8; N];
    let mut i = 0;
    while i < N {
        let hi = (b[2 * i] as char).to_digit(16).unwrap() as u8;
        let lo = (b[2 * i + 1] as char).to_digit(16).unwrap() as u8;
        out[i] = hi << 4 | lo;
        i += 1;
    }
    out
}

#[derive(Arbitrary, Debug)]
struct Input {
    round: u64,
    raw_ct: Vec<u8>,
    raw_sig: Vec<u8>,
    raw_key: Vec<u8>,
    /// (position, xor) edits applied to the real ciphertext.
    edits: Vec<(u8, u8)>,
    msg: [u8; 16],
    sigma: [u8; 16],
}

fuzz_target!(|i: Input| {
    let k = BeaconKey::quicknet().expect("pinned key");
    // Raw attacker bytes everywhere: errors only.
    let _ = k.open(i.round, &i.raw_sig, &i.raw_ct);
    let _ = decrypt_unverified(&i.raw_sig, &i.raw_ct);
    let _ = k.verify(i.round, &i.raw_sig);
    if let Ok(other) = BeaconKey::from_compressed(&i.raw_key) {
        assert_eq!(
            other.to_compressed().as_slice(),
            i.raw_key.as_slice(),
            "canonical keys only"
        );
        let _ = other.open(i.round, &i.raw_sig, &i.raw_ct);
        let _ = other.lock(i.round, &i.msg, &i.sigma);
    }
    // Near the real vector: only the real bytes open.
    let mut ct = CT;
    for (p, x) in &i.edits {
        ct[usize::from(*p) % ct.len()] ^= x;
    }
    if let Ok(o) = k.open(ROUND, &SIG, &ct) {
        assert_eq!(ct, CT, "an edited ciphertext opened");
        assert_eq!(k.lock(ROUND, &o.msg, &o.sigma).ok(), Some(CT));
    }
    // Any (msg, sigma): lock, open with the real signature, re-encrypt.
    let Ok(t) = k.lock(ROUND, &i.msg, &i.sigma) else {
        return;
    };
    let o = k.open(ROUND, &SIG, &t).expect("a fresh lock opens");
    assert_eq!((o.msg, o.sigma), (i.msg, i.sigma));
    assert_eq!(k.lock(ROUND, &o.msg, &o.sigma).ok(), Some(t));
});
