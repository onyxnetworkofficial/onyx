//! drand/tlock time-lock encryption for drand quicknet (ROADMAP C50, ADR-0027
//! amendment 13; THREATS C12-*, C50-*).
//!
//! Boneh-Franklin identity-based encryption with the Fujisaki-Okamoto
//! transform, exactly as the Go reference drand/tlock v1.2.0 does it for the
//! `bls-unchained-g1-rfc9380` scheme (kyber v1.3.1 `ibe.EncryptCCAonG2`):
//! round signatures on G1, the group public key on G2, the identity
//! `sha256(round as 8 bytes big-endian)` hashed to G1 with RFC 9380's
//! `BLS_SIG_BLS12381G1_XMD:SHA-256_SSWU_RO_NUL_`.
//!
//! ```text
//! lock(round, msg, sigma), msg and sigma 16 bytes:
//!   Q   = hash_to_G1(sha256(round_be8), DST)
//!   r   = H3(sigma, msg)                     drand's scalar derivation
//!   U   = r·G2                               96 bytes, compressed
//!   V   = sigma XOR H2(e(r·Q, P))            P the chain's G2 public key
//!   W   = msg   XOR H4(sigma)
//!   ct  = U ‖ V ‖ W                          128 bytes
//! open(signature s = sk·Q, ct):
//!   sigma = V XOR H2(e(s, U)),  msg = W XOR H4(sigma),
//!   accept only if compress(H3(sigma, msg)·G2) == U byte for byte
//! H2(g) = sha256("IBE-H2" ‖ g in kilic's byte order)[..16]
//! H3(sigma, msg) = first i = 1, 2, … with
//!       h = sha256(i as 2 bytes little-endian ‖ sha256("IBE-H3" ‖ sigma ‖ msg)),
//!       h[0] >>= 1, h big-endian < the group order
//! H4(sigma) = sha256("IBE-H4" ‖ sigma)[..16]
//! ```
//!
//! Every curve operation is `blst`'s safe Rust API (`min_sig` keys and
//! signatures, `blst_fp12::miller_loop`, `final_exp`, `to_bendian`); this
//! crate holds no `unsafe` (the workspace denies it). `lock` is deterministic
//! in `(round, msg, sigma)` and `open` returns `sigma` with the message, so a
//! dismissal can carry the opening and anyone re-encrypts it (ADR-0027
//! amendment 12 item 5). Nothing here panics on attacker bytes: every parse
//! and check returns an [`Error`] (fuzzed: `fuzz/fuzz_targets/tlock_open.rs`
//! and the seeded sweep in `tests/robust.rs`).
//!
//! Constant time (what `blst` 0.3.17 gives, read in its C source): the scalar
//! multiplications by the secret `r` (`SecretKey::sk_to_pk`, `sign`) are
//! blst's GLV/GLS windowed ladders (`ec_mult.h`, "constant-time
//! subroutines"), the affine conversions after them use a Fermat inversion,
//! and the final exponentiation inverts with `ct_inverse_mod_384`. NOT
//! constant time: `H3`'s rejection loop runs a number of rounds that depends
//! on `(sigma, msg)` (as in Go; it leaks only how many hash outputs were out
//! of range: the range test itself has no early exit, review C50-R1-2), and
//! the comparisons in [`BeaconKey::open`], which touch only public data (the
//! ciphertext and a published signature).

#![forbid(unsafe_code)]

use blst::min_sig::{PublicKey, SecretKey, Signature};
use blst::{blst_fp12, blst_p1_affine, blst_p2_affine};
use sha2::{Digest, Sha256};

/// RFC 9380 domain separation tag of quicknet's hash to G1 (drand's
/// `bls-unchained-g1-rfc9380`). The older `bls-unchained-on-g1` chains hashed
/// with the G2 tag; that is another scheme, not supported here.
pub const QUICKNET_DST: &[u8] = b"BLS_SIG_BLS12381G1_XMD:SHA-256_SSWU_RO_NUL_";

/// drand quicknet's chain hash (api.drand.sh, read 2026-10-17).
pub const QUICKNET_CHAIN_HASH: [u8; 32] = [
    0x52, 0xdb, 0x9b, 0xa7, 0x0e, 0x0c, 0xc0, 0xf6, 0xea, 0xf7, 0x80, 0x3d, 0xd0, 0x74, 0x47, 0xa1,
    0xf5, 0x47, 0x77, 0x35, 0xfd, 0x3f, 0x66, 0x17, 0x92, 0xba, 0x94, 0x60, 0x0c, 0x84, 0xe9, 0x71,
];

/// drand quicknet's group public key, G2 compressed (api.drand.sh
/// `/52db9ba7…e971/info`, read 2026-10-17). Pinned in code (THREATS C12-3).
pub const QUICKNET_PUBLIC_KEY: [u8; PUBLIC_KEY_LEN] = [
    0x83, 0xcf, 0x0f, 0x28, 0x96, 0xad, 0xee, 0x7e, 0xb8, 0xb5, 0xf0, 0x1f, 0xca, 0xd3, 0x91, 0x22,
    0x12, 0xc4, 0x37, 0xe0, 0x07, 0x3e, 0x91, 0x1f, 0xb9, 0x00, 0x22, 0xd3, 0xe7, 0x60, 0x18, 0x3c,
    0x8c, 0x4b, 0x45, 0x0b, 0x6a, 0x0a, 0x6c, 0x3a, 0xc6, 0xa5, 0x77, 0x6a, 0x2d, 0x10, 0x64, 0x51,
    0x0d, 0x1f, 0xec, 0x75, 0x8c, 0x92, 0x1c, 0xc2, 0x2b, 0x0e, 0x17, 0xe6, 0x3a, 0xaf, 0x4b, 0xcb,
    0x5e, 0xd6, 0x63, 0x04, 0xde, 0x9c, 0xf8, 0x09, 0xbd, 0x27, 0x4c, 0xa7, 0x3b, 0xab, 0x4a, 0xf5,
    0xa6, 0xe9, 0xc7, 0x6a, 0x4b, 0xc0, 0x9e, 0x76, 0xea, 0xe8, 0x99, 0x1e, 0xf5, 0xec, 0xe4, 0x5a,
];

/// Bytes of a compressed G2 point (the public key and `U`).
pub const PUBLIC_KEY_LEN: usize = 96;
/// Bytes of a compressed G1 point (a round signature).
pub const SIGNATURE_LEN: usize = 48;
/// Bytes of the locked message and of `sigma` (drand/tlock's age file key).
pub const MSG_LEN: usize = 16;
/// Bytes of a ciphertext: `U` (96) ‖ `V` (16) ‖ `W` (16).
pub const CIPHERTEXT_LEN: usize = PUBLIC_KEY_LEN + 2 * MSG_LEN;

const H2_TAG: &[u8] = b"IBE-H2";
const H3_TAG: &[u8] = b"IBE-H3";
const H4_TAG: &[u8] = b"IBE-H4";

/// The BLS12-381 group order, big-endian (H3's rejection bound).
const GROUP_ORDER: [u8; 32] = [
    0x73, 0xed, 0xa7, 0x53, 0x29, 0x9d, 0x7d, 0x48, 0x33, 0x39, 0xd8, 0x08, 0x09, 0xa1, 0xd8, 0x05,
    0x53, 0xbd, 0xa4, 0x02, 0xff, 0xfe, 0x5b, 0xfe, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x01,
];

/// Why a key, signature or ciphertext was refused. Never a panic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// Not a canonical compressed G2 point in the prime-order subgroup, or the
    /// point at infinity.
    #[error("bad beacon public key")]
    BadPublicKey,
    /// Not a canonical compressed G1 point in the prime-order subgroup, or the
    /// point at infinity.
    #[error("bad round signature")]
    BadSignature,
    /// Not the chain's signature of this round.
    #[error("not the chain's signature of the round")]
    WrongSignature,
    /// Wrong length, or `U` is not a canonical compressed G2 point in the
    /// subgroup (infinity included).
    #[error("malformed ciphertext")]
    Malformed,
    /// The Fujisaki-Okamoto check failed: the ciphertext does not open under
    /// this signature (another round, chain or key, or tampered bytes).
    #[error("ciphertext does not open under this signature")]
    NoOpening,
    /// H3 found no scalar in 65,534 tries or found zero (probability about
    /// 2^-255 per try; Go's reference fails the first case the same way).
    #[error("no scalar for this (sigma, message)")]
    NoScalar,
}

/// What opening a ciphertext gives: the IBE randomness and the message.
/// `lock(round, &o.msg, &o.sigma)` re-creates the ciphertext byte for byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opening {
    /// The Fujisaki-Okamoto randomness `sigma`.
    pub sigma: [u8; MSG_LEN],
    /// The locked message (Onyx's `tau`; drand/tlock's age file key).
    pub msg: [u8; MSG_LEN],
}

/// The identity of a round: `sha256(round as 8 bytes big-endian)`, the
/// message drand's unchained schemes sign.
pub fn round_identity(round: u64) -> [u8; 32] {
    Sha256::digest(round.to_be_bytes()).into()
}

/// A drand chain's group public key, validated once.
#[derive(Clone, Copy, Debug)]
pub struct BeaconKey {
    key: PublicKey,
}

impl BeaconKey {
    /// Parse a compressed G2 public key: canonical encoding, on the curve, in
    /// the subgroup, not infinity.
    pub fn from_compressed(bytes: &[u8]) -> Result<Self, Error> {
        let key = g2_point(bytes).ok_or(Error::BadPublicKey)?;
        Ok(BeaconKey { key })
    }

    /// drand quicknet's pinned key.
    pub fn quicknet() -> Result<Self, Error> {
        Self::from_compressed(&QUICKNET_PUBLIC_KEY)
    }

    /// The key, compressed.
    pub fn to_compressed(&self) -> [u8; PUBLIC_KEY_LEN] {
        self.key.compress()
    }

    /// Whether `signature` is this chain's BLS signature of `round`
    /// (`e(s, G2) == e(H(id), P)`, the signature checked canonical, in the
    /// subgroup and not infinity).
    pub fn verify(&self, round: u64, signature: &[u8]) -> bool {
        self.check_signature(round, signature).is_ok()
    }

    fn check_signature(&self, round: u64, signature: &[u8]) -> Result<Signature, Error> {
        let s = g1_point(signature).ok_or(Error::BadSignature)?;
        let q = hash_identity(round)?;
        let lhs = pairing(&g2_generator()?, &s.into());
        let rhs = pairing(&self.key.into(), &q);
        if lhs == rhs {
            Ok(s)
        } else {
            Err(Error::WrongSignature)
        }
    }

    /// Lock `msg` to `round` with the randomness `sigma`. Deterministic; needs
    /// no signature (anyone locks to a future round).
    pub fn lock(
        &self,
        round: u64,
        msg: &[u8; MSG_LEN],
        sigma: &[u8; MSG_LEN],
    ) -> Result<[u8; CIPHERTEXT_LEN], Error> {
        self.lock_with_dst(round, msg, sigma, QUICKNET_DST)
    }

    /// [`Self::lock`] with another hash-to-G1 tag (tests: a wrong DST must not
    /// open under the real signature).
    fn lock_with_dst(
        &self,
        round: u64,
        msg: &[u8; MSG_LEN],
        sigma: &[u8; MSG_LEN],
        dst: &[u8],
    ) -> Result<[u8; CIPHERTEXT_LEN], Error> {
        let id = round_identity(round);
        let r = h3_scalar(sigma, msg)?;
        let u = r.sk_to_pk().compress();
        // r·Q, then e(r·Q, P) = e(Q, P)^r.
        let rq: blst_p1_affine = r.sign(&id, dst, &[]).into();
        let mask = h2(&pairing(&self.key.into(), &rq));
        let mut ct = [0u8; CIPHERTEXT_LEN];
        ct[..PUBLIC_KEY_LEN].copy_from_slice(&u);
        ct[PUBLIC_KEY_LEN..PUBLIC_KEY_LEN + MSG_LEN].copy_from_slice(&xor16(sigma, &mask));
        ct[PUBLIC_KEY_LEN + MSG_LEN..].copy_from_slice(&xor16(msg, &h4(sigma)));
        Ok(ct)
    }

    /// Open `ciphertext` with the chain's signature of `round`: the signature
    /// is verified first (`WrongSignature` otherwise), then decrypted as
    /// [`decrypt_unverified`] does.
    pub fn open(&self, round: u64, signature: &[u8], ciphertext: &[u8]) -> Result<Opening, Error> {
        let s = self.check_signature(round, signature)?;
        decrypt_with(&s, ciphertext)
    }
}

/// Decrypt with a round signature WITHOUT checking whose signature it is (the
/// Fujisaki-Okamoto check still binds the result to `U`). NOT for Onyx's
/// paths: a ciphertext made by its poster for a point it chose opens under
/// that point, so "opens" means nothing until the signature is the chain's.
/// Use [`BeaconKey::open`] (review C50-R1-1; tests and the fuzz target only).
pub fn decrypt_unverified(signature: &[u8], ciphertext: &[u8]) -> Result<Opening, Error> {
    let s = g1_point(signature).ok_or(Error::BadSignature)?;
    decrypt_with(&s, ciphertext)
}

fn decrypt_with(s: &Signature, ciphertext: &[u8]) -> Result<Opening, Error> {
    if ciphertext.len() != CIPHERTEXT_LEN {
        return Err(Error::Malformed);
    }
    let (u_bytes, rest) = ciphertext.split_at(PUBLIC_KEY_LEN);
    let (v, w) = rest.split_at(MSG_LEN);
    let u = g2_point(u_bytes).ok_or(Error::Malformed)?;
    let mask = h2(&pairing(&u.into(), &(*s).into()));
    let sigma = xor16(v, &mask);
    let msg = xor16(w, &h4(&sigma));
    let r = h3_scalar(&sigma, &msg)?;
    if r.sk_to_pk().compress().as_slice() != u_bytes {
        return Err(Error::NoOpening);
    }
    Ok(Opening { sigma, msg })
}

/// A compressed G2 point, canonical (re-compresses to the same bytes), in the
/// subgroup, not infinity.
fn g2_point(bytes: &[u8]) -> Option<PublicKey> {
    if bytes.len() != PUBLIC_KEY_LEN {
        return None;
    }
    let p = PublicKey::uncompress(bytes).ok()?;
    p.validate().ok()?;
    (p.compress().as_slice() == bytes).then_some(p)
}

/// A compressed G1 point, canonical, in the subgroup, not infinity.
fn g1_point(bytes: &[u8]) -> Option<Signature> {
    if bytes.len() != SIGNATURE_LEN {
        return None;
    }
    let s = Signature::uncompress(bytes).ok()?;
    s.validate(true).ok()?;
    (s.compress().as_slice() == bytes).then_some(s)
}

fn scalar_one() -> Result<SecretKey, Error> {
    let mut one = [0u8; 32];
    one[31] = 1;
    SecretKey::from_bytes(&one).map_err(|_| Error::NoScalar)
}

/// The G2 generator, as `1·G2` through blst's safe API.
fn g2_generator() -> Result<blst_p2_affine, Error> {
    Ok(scalar_one()?.sk_to_pk().into())
}

/// `hash_to_G1(sha256(round), QUICKNET_DST)`, as `1·H(id)`.
fn hash_identity(round: u64) -> Result<blst_p1_affine, Error> {
    Ok(scalar_one()?
        .sign(&round_identity(round), QUICKNET_DST, &[])
        .into())
}

/// The reduced pairing `e(p, q)`.
fn pairing(q: &blst_p2_affine, p: &blst_p1_affine) -> blst_fp12 {
    blst_fp12::miller_loop(q, p).final_exp()
}

/// A GT element in kilic/bls12-381's byte order (what kyber-bls12381 v0.3.1
/// marshals and drand/tlock hashes): every coefficient big-endian, highest
/// first. blst's `to_bendian` writes `c[j][i][k]` (`fp12 = c0 + c1·w`,
/// `fp6 = c0 + c1·v + c2·v²`, `fp2 = c0 + c1·u`) at `((2i + j)·2 + k)·48`;
/// kilic writes `j = 1, 0`, then `i = 2, 1, 0`, then `k = 1, 0`.
pub fn gt_bytes(g: &blst_fp12) -> [u8; 576] {
    let b = g.to_bendian();
    let mut out = [0u8; 576];
    let mut at = 0;
    for j in [1usize, 0] {
        for i in [2usize, 1, 0] {
            for k in [1usize, 0] {
                let from = ((2 * i + j) * 2 + k) * 48;
                out[at..at + 48].copy_from_slice(&b[from..from + 48]);
                at += 48;
            }
        }
    }
    out
}

fn h2(g: &blst_fp12) -> [u8; MSG_LEN] {
    let mut h = Sha256::new();
    h.update(H2_TAG);
    h.update(gt_bytes(g));
    first16(&h.finalize())
}

fn h4(sigma: &[u8; MSG_LEN]) -> [u8; MSG_LEN] {
    let mut h = Sha256::new();
    h.update(H4_TAG);
    h.update(sigma);
    first16(&h.finalize())
}

/// drand's H3: the scalar `r` as a blst secret key (zeroized on drop).
fn h3_scalar(sigma: &[u8; MSG_LEN], msg: &[u8; MSG_LEN]) -> Result<SecretKey, Error> {
    let mut h = Sha256::new();
    h.update(H3_TAG);
    h.update(sigma);
    h.update(msg);
    let buffer = h.finalize();
    for i in 1u16..u16::MAX {
        let mut h = Sha256::new();
        h.update(i.to_le_bytes());
        h.update(buffer);
        let mut hashed: [u8; 32] = h.finalize().into();
        // The group order has 255 bits: drop the top bit, as Go does.
        hashed[0] >>= 1;
        if lt_be(&hashed, &GROUP_ORDER) {
            // Zero is a valid Go scalar but not a blst secret key; it occurs
            // with probability 2^-255 and is refused, never a panic.
            return SecretKey::from_bytes(&hashed).map_err(|_| Error::NoScalar);
        }
    }
    Err(Error::NoScalar)
}

/// `a < b` for big-endian 32-byte numbers, without a data-dependent branch or
/// early exit (review C50-R1-2: `<` on arrays stops at the first differing
/// byte, which would time the leading bytes of the accepted scalar `r`).
fn lt_be(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut borrow = 0u16;
    for i in (0..32).rev() {
        let d = u16::from(a[i])
            .wrapping_sub(u16::from(b[i]))
            .wrapping_sub(borrow);
        borrow = (d >> 8) & 1;
    }
    borrow == 1
}

fn first16(d: &[u8]) -> [u8; MSG_LEN] {
    let mut o = [0u8; MSG_LEN];
    o.copy_from_slice(&d[..MSG_LEN]);
    o
}

fn xor16(a: &[u8], b: &[u8; MSG_LEN]) -> [u8; MSG_LEN] {
    let mut o = [0u8; MSG_LEN];
    for (i, x) in o.iter_mut().enumerate() {
        *x = a[i] ^ b[i];
    }
    o
}

#[cfg(test)]
mod tests;
