//! Unit and adversarial tests (ROADMAP C50, THREATS C50-*). Interop vectors
//! from drand/tlock (Go) and published quicknet rounds are in
//! `tests/interop.rs`; malformed-input sweeps in `tests/robust.rs`.

use super::*;

fn hx(s: &str) -> Vec<u8> {
    hex::decode(s).expect("test input")
}

/// A test chain with a known master secret (never a real beacon).
struct TestChain {
    sk: SecretKey,
    key: BeaconKey,
}

impl TestChain {
    fn new(seed: u8) -> Self {
        let sk = SecretKey::from_bytes(&[seed; 32]).expect("test input");
        let key = BeaconKey::from_compressed(&sk.sk_to_pk().compress()).expect("test input");
        TestChain { sk, key }
    }

    fn sig(&self, round: u64) -> [u8; SIGNATURE_LEN] {
        self.sk
            .sign(&round_identity(round), QUICKNET_DST, &[])
            .compress()
    }
}

/// Go's quicknet vector (drand/tlock-js `test/rfc-scheme.test.ts`, "test
/// vectors from go codebase"): round 746511 of the pinned chain.
const GO_CT: &str = "9036411afba8c9e0b16fdd4f5d487f7233581a4a484003b2b57a113959826af119c82c3d10b195017377aba2763e46e2003ac078cb9b9f7fb4f36353dd1195074d28efceb4c54db2c3b241994c715783db19ee038a1bf96e4fb96e8e461432121616a12a64104241f6afc033542184c3dccb3a174fd8a7a79531d70c325eb2b8";
const GO_ROUND: u64 = 746_511;
/// api.drand.sh quicknet round 746511 (fixtures/tlock/drand/quicknet-round-746511.json).
const SIG_746511: &str = "a9080432fbe9dd72d25a78c092f6d507a418fd9f985f8b5e99ac380dc63d6d203198f4160e353055e9fbd8fccdfdbd37";
/// api.drand.sh quicknet round 1000.
const SIG_1000: &str = "b44679b9a59af2ec876b1a6b1ad52ea9b1615fc3982b19576350f93447cb1125e342b73a8dd2bacbe47e4b6b63ed5e39";

/// `(0, ±2)` on G1: on the curve, order 3 (blst's uncompress itself refuses
/// it as not in the group).
const G1_ORDER_3: &str = "800000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";
/// On the curve, outside the prime-order subgroup: `x = 4` on G1 and `x = u`
/// on G2 (the first `x` with `x^3 + b` a square; such a point is in the
/// subgroup only with probability 1/cofactor).
const G1_OFF_SUBGROUP: &str = "800000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000004";
const G2_OFF_SUBGROUP: &str = "800000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";
/// BLS12-381's base field modulus, big-endian.
const P: &str = "1a0111ea397fe69a4b1ba7b6434bacd764774b84f38512bf6730d2a0f6b0f6241eabfffeb153ffffb9feffffffffaaab";

fn add_be(a: &[u8], b: &[u8]) -> Option<Vec<u8>> {
    let mut out = vec![0u8; a.len()];
    let mut carry = 0u16;
    for i in (0..a.len()).rev() {
        let s = u16::from(a[i]) + u16::from(b[i]) + carry;
        out[i] = s as u8;
        carry = s >> 8;
    }
    (carry == 0).then_some(out)
}

#[test]
fn c50_gt_bytes_and_h2_match_drands_go_vectors() {
    // drand/tlock-js `test/crypto/ibe.test.ts`, "correctly pass the test
    // vectors generated from the go codebase": e(G1, G2) in kyber's byte
    // order, and gtToHash of it.
    let expected = "0f41e58663bf08cf068672cbd01a7ec73baca4d72ca93544deff686bfd6df543d48eaa24afe47e1efde449383b67663104c581234d086a9902249b64728ffd21a189e87935a954051c7cdba7b3872629a4fafc05066245cb9108f0242d0fe3ef03350f55a7aefcd3c31b4fcb6ce5771cc6a0e9786ab5973320c806ad360829107ba810c5a09ffdd9be2291a0c25a99a211b8b424cd48bf38fcef68083b0b0ec5c81a93b330ee1a677d0d15ff7b984e8978ef48881e32fac91b93b47333e2ba5706fba23eb7c5af0d9f80940ca771b6ffd5857baaf222eb95a7d2809d61bfe02e1bfd1b68ff02f0b8102ae1c2d5d5ab1a19f26337d205fb469cd6bd15c3d5a04dc88784fbb3d0b2dbdea54d43b2b73f2cbb12d58386a8703e0f948226e47ee89d018107154f25a764bd3c79937a45b84546da634b8f6be14a8061e55cceba478b23f7dacaa35c8ca78beae9624045b4b601b2f522473d171391125ba84dc4007cfbf2f8da752f7c74185203fcca589ac719c34dffbbaad8431dad1c1fb597aaa5193502b86edb8857c273fa075a50512937e0794e1e65a7617c90d8bd66065b1fffe51d7a579973b1315021ec3c19934f1368bb445c7c2d209703f239689ce34c0378a68e72a6b3b216da0e22a5031b54ddff57309396b38c881c4c849ec23e87089a1c5b46e5110b86750ec6a532348868a84045483c92b7af5af689452eafabf1a8943e50439f1d59882a98eaa0170f1250ebd871fc0a92a7b2d83168d0d727272d441befa15c503dd8e90ce98db3e7b6d194f60839c508a84305aaca1789b6";
    let one = scalar_one().expect("test input");
    let g1: blst_p1_affine = blst::min_pk::SecretKey::from_bytes(&one.to_bytes())
        .expect("test input")
        .sk_to_pk()
        .into();
    let g2 = g2_generator().expect("test input");
    let e = pairing(&g2, &g1);
    assert_eq!(hex::encode(gt_bytes(&e)), expected);
    assert_eq!(hex::encode(h2(&e)), "cb87319f24560b5231579a09ad79f12e");
    // The same vectors fix the G1 generator's compressed form.
    assert_eq!(
        hex::encode(blst::min_pk::PublicKey::from(g1).compress()),
        "97f1d3a73197d7942695638c4fa9ac0fc3688c4f9774b905a14e3a3f171bac586c55e83ff97a1aeffb3af00adb22c6bb"
    );
}

#[test]
fn c50_the_pinned_key_parses_and_round_identity_is_sha256_of_the_round() {
    let k = BeaconKey::quicknet().expect("test input");
    assert_eq!(k.to_compressed(), QUICKNET_PUBLIC_KEY);
    assert_eq!(
        round_identity(1).to_vec(),
        Sha256::digest([0, 0, 0, 0, 0, 0, 0, 1]).to_vec()
    );
}

#[test]
fn c50_go_vector_opens_and_re_encrypts_to_identical_bytes() {
    let k = BeaconKey::quicknet().expect("test input");
    let ct = hx(GO_CT);
    let o = k.open(GO_ROUND, &hx(SIG_746511), &ct).expect("test input");
    assert_eq!(
        k.lock(GO_ROUND, &o.msg, &o.sigma)
            .expect("test input")
            .to_vec(),
        ct
    );
    // decrypt_unverified() without the signature check gives the same opening.
    assert_eq!(
        decrypt_unverified(&hx(SIG_746511), &ct).expect("test input"),
        o
    );
}

#[test]
fn c50_lock_is_deterministic_in_round_msg_and_sigma() {
    let c = TestChain::new(0x11);
    let (m, s) = ([7u8; 16], [9u8; 16]);
    let a = c.key.lock(5, &m, &s).expect("test input");
    assert_eq!(a, c.key.lock(5, &m, &s).expect("test input"));
    let mut s2 = s;
    s2[15] ^= 1;
    let mut m2 = m;
    m2[0] ^= 1;
    assert_ne!(a, c.key.lock(5, &m, &s2).expect("test input"));
    assert_ne!(a, c.key.lock(5, &m2, &s).expect("test input"));
    assert_ne!(a, c.key.lock(6, &m, &s).expect("test input"));
    let o = c.key.open(5, &c.sig(5), &a).expect("test input");
    assert_eq!(o, Opening { sigma: s, msg: m });
}

#[test]
fn c50_messages_ending_in_zero_bytes_open_to_all_16_bytes() {
    // tlock-rs strips trailing zeros (RESEARCH.md "Time-lock crates"); here
    // every message is 16 bytes in and out.
    let c = TestChain::new(0x22);
    for tail in [1usize, 2, 8, 15, 16] {
        let mut m = [0xa5u8; 16];
        for b in m.iter_mut().skip(16 - tail) {
            *b = 0;
        }
        let mut s = [0x5au8; 16];
        s[16 - tail..].fill(0);
        let ct = c.key.lock(77, &m, &s).expect("test input");
        let o = c.key.open(77, &c.sig(77), &ct).expect("test input");
        assert_eq!(o.msg, m, "tail {tail}");
        assert_eq!(o.sigma, s, "tail {tail}");
        assert_eq!(c.key.lock(77, &o.msg, &o.sigma).expect("test input"), ct);
    }
}

#[test]
fn c50_1_round_substitution_opens_nothing() {
    // A real Go ciphertext for round 746511 and the real signature of round
    // 1000: open() refuses the signature for the claimed round, and the
    // ciphertext does not open under round 1000.
    let k = BeaconKey::quicknet().expect("test input");
    let ct = hx(GO_CT);
    assert_eq!(
        k.open(GO_ROUND, &hx(SIG_1000), &ct),
        Err(Error::WrongSignature)
    );
    assert_eq!(k.open(1000, &hx(SIG_1000), &ct), Err(Error::NoOpening));
    assert_eq!(
        decrypt_unverified(&hx(SIG_1000), &ct),
        Err(Error::NoOpening)
    );
    // Test chain: a lock to round 5 never opens with round 6's signature.
    let c = TestChain::new(0x33);
    let ct = c.key.lock(5, &[1; 16], &[2; 16]).expect("test input");
    assert_eq!(c.key.open(6, &c.sig(6), &ct), Err(Error::NoOpening));
    assert_eq!(c.key.open(5, &c.sig(6), &ct), Err(Error::WrongSignature));
}

#[test]
fn c50_2_another_chains_key_or_signature_opens_nothing() {
    let a = TestChain::new(0x44);
    let b = TestChain::new(0x45);
    let ct = a.key.lock(9, &[3; 16], &[4; 16]).expect("test input");
    assert_eq!(b.key.open(9, &b.sig(9), &ct), Err(Error::NoOpening));
    assert_eq!(a.key.open(9, &b.sig(9), &ct), Err(Error::WrongSignature));
    assert_eq!(decrypt_unverified(&b.sig(9), &ct), Err(Error::NoOpening));
}

#[test]
fn c50_3_a_wrong_dst_does_not_interoperate() {
    // drand's older G1 chains hashed to G1 with the G2 tag (kyber's
    // `ShortSigSchemeID` retro-compatibility). Locking with it does not
    // re-create Go's quicknet bytes and does not open under the real
    // signature.
    let k = BeaconKey::quicknet().expect("test input");
    let ct = hx(GO_CT);
    let o = k.open(GO_ROUND, &hx(SIG_746511), &ct).expect("test input");
    let g2_dst = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_NUL_";
    let wrong = k
        .lock_with_dst(GO_ROUND, &o.msg, &o.sigma, g2_dst)
        .expect("test input");
    assert_ne!(wrong.to_vec(), ct);
    // U is the same (it depends only on sigma and msg), V differs.
    assert_eq!(wrong[..PUBLIC_KEY_LEN], ct[..PUBLIC_KEY_LEN]);
    assert_eq!(
        k.open(GO_ROUND, &hx(SIG_746511), &wrong),
        Err(Error::NoOpening)
    );
    // A signature made with the wrong tag is not the round's signature.
    let c = TestChain::new(0x46);
    let bad = c.sk.sign(&round_identity(3), g2_dst, &[]).compress();
    assert!(!c.key.verify(3, &bad));
    assert!(c.key.verify(3, &c.sig(3)));
}

#[test]
fn c50_4_g1_g2_swap_and_wrong_lengths_are_refused() {
    let k = BeaconKey::quicknet().expect("test input");
    let ct = hx(GO_CT);
    let sig = hx(SIG_746511);
    // The G2 key where a G1 signature belongs, and the reverse.
    assert_eq!(
        k.open(GO_ROUND, &QUICKNET_PUBLIC_KEY, &ct),
        Err(Error::BadSignature)
    );
    assert_eq!(
        BeaconKey::from_compressed(&sig).err(),
        Some(Error::BadPublicKey)
    );
    // A G1 point (48 bytes) padded into U's 96 bytes.
    let mut swapped = ct.clone();
    swapped[..48].copy_from_slice(&sig);
    swapped[48..96].fill(0);
    assert_eq!(k.open(GO_ROUND, &sig, &swapped), Err(Error::Malformed));
    // A G1-keyed chain (drand's `bls-unchained-on-g1` keys are 48 bytes).
    assert!(BeaconKey::from_compressed(&hx(G1_ORDER_3)).is_err());
    for n in [0usize, 1, 127, 129, 256] {
        let v = vec![0x8cu8; n];
        assert_eq!(k.open(GO_ROUND, &sig, &v), Err(Error::Malformed), "{n}");
    }
    for n in [0usize, 47, 49, 96] {
        assert_eq!(
            k.open(GO_ROUND, &vec![0xa9; n], &ct),
            Err(Error::BadSignature)
        );
    }
}

#[test]
fn c50_5_infinity_and_small_subgroup_points_are_refused() {
    let k = BeaconKey::quicknet().expect("test input");
    let ct = hx(GO_CT);
    let sig = hx(SIG_746511);
    let mut inf1 = [0u8; 48];
    inf1[0] = 0xc0;
    let mut inf2 = [0u8; 96];
    inf2[0] = 0xc0;
    // blst decodes both infinities; we refuse them.
    assert!(Signature::uncompress(&inf1).is_ok());
    assert!(PublicKey::uncompress(&inf2).is_ok());
    assert_eq!(
        BeaconKey::from_compressed(&inf2).err(),
        Some(Error::BadPublicKey)
    );
    assert_eq!(decrypt_unverified(&inf1, &ct), Err(Error::BadSignature));
    assert!(!k.verify(GO_ROUND, &inf1));
    let mut u_inf = ct.clone();
    u_inf[..96].copy_from_slice(&inf2);
    assert_eq!(k.open(GO_ROUND, &sig, &u_inf), Err(Error::Malformed));
    // The order-3 point: blst refuses it at decoding.
    assert!(Signature::uncompress(&hx(G1_ORDER_3)).is_err());
    assert_eq!(
        decrypt_unverified(&hx(G1_ORDER_3), &ct),
        Err(Error::BadSignature)
    );
    // On the curve, off the subgroup: blst decodes them, validation refuses.
    let g1 = hx(G1_OFF_SUBGROUP);
    let g2 = hx(G2_OFF_SUBGROUP);
    assert!(Signature::uncompress(&g1).is_ok());
    assert!(Signature::uncompress(&g1)
        .expect("test input")
        .validate(false)
        .is_err());
    assert!(PublicKey::uncompress(&g2).is_ok());
    assert!(PublicKey::uncompress(&g2)
        .expect("test input")
        .validate()
        .is_err());
    assert_eq!(decrypt_unverified(&g1, &ct), Err(Error::BadSignature));
    assert!(!k.verify(GO_ROUND, &g1));
    assert_eq!(
        BeaconKey::from_compressed(&g2).err(),
        Some(Error::BadPublicKey)
    );
    let mut u_small = ct.clone();
    u_small[..96].copy_from_slice(&g2);
    assert_eq!(k.open(GO_ROUND, &sig, &u_small), Err(Error::Malformed));
}

#[test]
fn c50_6_non_canonical_encodings_are_refused() {
    let k = BeaconKey::quicknet().expect("test input");
    let ct = hx(GO_CT);
    let sig = hx(SIG_746511);
    let p = hx(P);
    // U's c0 (bytes 48..96, no flag bits) plus p: the same field element,
    // another encoding. Refused, so a time-lock opens for every
    // implementation or for none (review C12-R1-2).
    if let Some(c0p) = add_be(&ct[48..96], &p) {
        let mut nc = ct.clone();
        nc[48..96].copy_from_slice(&c0p);
        assert_eq!(k.open(GO_ROUND, &sig, &nc), Err(Error::Malformed));
    }
    // Flag bits: uncompressed flag on a 96-byte G2, the sign bit flipped (the
    // other point, a different U), the infinity bit on a non-zero point.
    for (byte, bit) in [(0usize, 0x80u8), (0, 0x20), (0, 0x40)] {
        let mut nc = ct.clone();
        nc[byte] ^= bit;
        assert!(k.open(GO_ROUND, &sig, &nc).is_err(), "flag {bit:#x}");
    }
    // The same for the signature: x + p, and the flags.
    let mut x = sig.clone();
    x[0] &= 0x1f;
    if let Some(xp) = add_be(&x, &p) {
        if xp[0] & 0xe0 == 0 {
            let mut nc = xp;
            nc[0] |= sig[0] & 0xe0;
            assert_eq!(decrypt_unverified(&nc, &ct), Err(Error::BadSignature));
        }
    }
    for bit in [0x80u8, 0x20, 0x40] {
        let mut nc = sig.clone();
        nc[0] ^= bit;
        assert!(!k.verify(GO_ROUND, &nc), "flag {bit:#x}");
    }
}

#[test]
fn c50_7_every_tampered_byte_fails_and_never_opens_to_another_value() {
    // Re-encryption mismatch: a ciphertext that opens re-encrypts to itself
    // (decrypt's Fujisaki-Okamoto check compares U byte for byte, and V, W
    // follow from sigma and msg), so a changed byte is an error, never a
    // different opening.
    let k = BeaconKey::quicknet().expect("test input");
    let ct = hx(GO_CT);
    let sig = hx(SIG_746511);
    for i in 0..CIPHERTEXT_LEN {
        for bit in [0x01u8, 0x80] {
            let mut t = ct.clone();
            t[i] ^= bit;
            let r = k.open(GO_ROUND, &sig, &t);
            assert!(r.is_err(), "byte {i} bit {bit:#x} opened: {r:?}");
        }
    }
}

#[test]
fn c50_8_whatever_opens_re_encrypts_byte_for_byte() {
    // The invariant the coordinator's `open_canonical` relies on, checked on
    // many test-chain locks.
    let c = TestChain::new(0x55);
    for i in 0u8..24 {
        let m = [i; 16];
        let mut s = [i.wrapping_mul(31); 16];
        s[0] = i;
        let ct = c.key.lock(u64::from(i) + 1, &m, &s).expect("test input");
        let o = c
            .key
            .open(u64::from(i) + 1, &c.sig(u64::from(i) + 1), &ct)
            .expect("test input");
        assert_eq!(
            c.key
                .lock(u64::from(i) + 1, &o.msg, &o.sigma)
                .expect("test input"),
            ct
        );
    }
}

#[test]
fn c50_h3_matches_gos_scalar_derivation() {
    // Go (kyber v1.3.1 ibe.h3): sha256(i_le16 ‖ sha256("IBE-H3" ‖ sigma ‖ msg))
    // with the top bit dropped, first in range. Re-derived here byte by byte
    // and compared with h3_scalar, so the loop and the masking are pinned.
    for seed in 0u8..32 {
        let sigma = [seed; 16];
        let msg = [seed ^ 0xff; 16];
        let buffer = Sha256::digest([H3_TAG, &sigma, &msg].concat());
        let mut want = None;
        for i in 1u16..10 {
            let mut h: [u8; 32] = Sha256::digest([&i.to_le_bytes()[..], &buffer].concat()).into();
            h[0] >>= 1;
            if h < GROUP_ORDER {
                want = Some(h);
                break;
            }
        }
        assert_eq!(
            h3_scalar(&sigma, &msg).expect("test input").to_bytes(),
            want.expect("test input")
        );
    }
}

#[test]
fn c50_r1_2_the_range_test_is_array_order() {
    // lt_be (no early exit) must agree with `<` on arrays, at the group
    // order's edges and on pseudorandom pairs that share leading bytes.
    let mut minus1 = GROUP_ORDER;
    minus1[31] -= 1;
    let mut plus1 = GROUP_ORDER;
    plus1[31] += 1;
    let edges = [[0u8; 32], [0xff; 32], GROUP_ORDER, minus1, plus1];
    for a in &edges {
        for b in &edges {
            assert_eq!(lt_be(a, b), a < b, "{a:x?} < {b:x?}");
        }
    }
    for seed in 0u32..2_000 {
        let a: [u8; 32] = Sha256::digest(seed.to_le_bytes()).into();
        let mut b: [u8; 32] = Sha256::digest((seed + 1).to_be_bytes()).into();
        // Share a prefix of (seed % 33) bytes, so every position decides.
        let k = (seed % 33) as usize;
        b[..k].copy_from_slice(&a[..k]);
        assert_eq!(lt_be(&a, &b), a < b);
        assert_eq!(lt_be(&b, &a), b < a);
    }
}
