//! Sealed reveal posts in the browser (ROADMAP C11, ADR-0027 amendment 10):
//! mirrors of `onyx_coordinator::sealed` that a page needs to check a round's
//! `sealed_reveals` section (the coordinator is not built for wasm32), pinned
//! to the coordinator by `tests/sealed_reveals.rs`.
//!
//! What the page checks, all from its own recomputation (review C4-R1-2):
//! every post of a drawn observer, in the round's reveal phase, with a key
//! proof valid for this round; every dismissal (the Chaum-Pedersen proof fixes
//! `S`, so it fixes the decryption, and the plaintext must NOT be a valid
//! reveal opening the observer's commit); every post revealed or dismissed;
//! the `revealLogComplete` (and, for a round without a value, `attemptMatches`)
//! query built from the drawn set, the config's window and the recomputed
//! round id, with a record required for a round without a value. The page
//! cannot ask the rehearsal's chain (a local anvil that no longer exists), so
//! the chain's answers come recorded with the round (`chain`): each must be
//! for exactly the call the page built, and true; a node holder asks the same
//! calls itself.
//!
//! Time-locked draws (ROADMAP C51, ADR-0027 amendment 14): a section with
//! `time_lock` names the lock round of the window's reveal close, drand
//! quicknet's signature of it (verified here against the pinned key with
//! `onyx-tlock`, built for wasm32) and the opening of each dismissal whose
//! time-lock opened. Each such dismissal is checked by EXACT re-encryption
//! (the opening must lock to the post's time-lock byte for byte, then the body
//! it opens must be no valid reveal); a dismissal without one must hold a
//! time-lock that does not open canonically under the verified signature. The
//! chain's recorded `drawTimeLocked` must say the same.
//!
//! Far-date copies (ROADMAP C52, ADR-0027 amendment 15): the section names the
//! far round, which the page derives itself from the feed's pinned template
//! ([`far_round`], the record date's last observable day); a dismissal also
//! stands when the far copy does not re-encrypt from the unwrapped body key.

use onyx_core::{keccak256, observation_commit, Hash32, ObservationKind, ObservationSpec};
use secp256k1::{PublicKey, Scalar, Secp256k1, SecretKey};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

/// `x ‖ y`, 64 bytes.
pub type Point = [u8; 64];

const POK_TAG: &[u8] = b"onyx-sealed-key-pok-v1";
const KEY_TAG: &[u8] = b"onyx-sealed-reveal-key-v1";
const STREAM_TAG: &[u8] = b"onyx-sealed-reveal-stream-v1";
const DLEQ_TAG: &[u8] = b"onyx-sealed-dleq-v1";
const RECORD_TAG: &[u8] = b"onyx-attempt-record-v1";
const KEY_TAG_V3: &[u8] = b"onyx-sealed-reveal-key-v3";
const WRAP_TAG: &[u8] = b"onyx-sealed-far-wrap-v1";
const FAR_SIGMA_TAG: &[u8] = b"onyx-sealed-far-sigma-v1";
/// `timelock::TLOCK_LEN`: drand/tlock's `U` (96) ‖ `V` (16) ‖ `W` (16).
pub const TLOCK_LEN: usize = 128;
/// `sealed::TIME_LOCKED_HEAD_BYTES`: the time-lock, the far copy (128) and the
/// wrapped body key (16) (ROADMAP C52).
pub const TIME_LOCKED_HEAD: usize = TLOCK_LEN + TLOCK_LEN + 16;
/// `observe::MAX_COMMIT_GRACE_SECS`, `observe::MAX_REVEAL_SECS`,
/// `timelock::UNSEAL_MARGIN_SECS` and `timelock::MAX_UNSEAL_DELAY_SECS`.
const MAX_COMMIT_GRACE_SECS: u64 = 600;
const MAX_REVEAL_SECS: u64 = 86_400;
const UNSEAL_MARGIN_SECS: u64 = 86_400;
const MAX_UNSEAL_DELAY_SECS: u64 = 1_096 * 86_400;
/// `timelock::LOCK_MARGIN_SECS`.
const LOCK_MARGIN_SECS: u64 = 60;
/// drand quicknet's genesis and period (`timelock::QUICKNET_*`).
const QUICKNET_GENESIS: u64 = 1_692_803_367;
const QUICKNET_PERIOD: u64 = 3;
const ORDER: [u8; 32] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFE,
    0xBA, 0xAE, 0xDC, 0xE6, 0xAF, 0x48, 0xA0, 0x3B, 0xBF, 0xD2, 0x5E, 0x8C, 0xD0, 0x36, 0x41, 0x41,
];
/// `MAX_SEALED_BYTES`: 272 (time-lock, far copy, wrapped key) + 142 +
/// `MAX_VALUE_BYTES` (1,024) (ROADMAP C51, C52).
const MAX_SEALED_BYTES: usize = TIME_LOCKED_HEAD + 142 + 1_024;

fn ctx() -> Secp256k1<secp256k1::All> {
    Secp256k1::new()
}

fn to_point(pk: &PublicKey) -> Point {
    let u = pk.serialize_uncompressed();
    let mut p = [0u8; 64];
    p.copy_from_slice(&u[1..]);
    p
}

fn from_point(p: &Point) -> Option<PublicKey> {
    let mut u = [0u8; 65];
    u[0] = 4;
    u[1..].copy_from_slice(p);
    PublicKey::from_slice(&u).ok()
}

fn generator() -> Option<PublicKey> {
    let mut one = [0u8; 32];
    one[31] = 1;
    Some(SecretKey::from_slice(&one).ok()?.public_key(&ctx()))
}

fn reduce(v: [u8; 32]) -> [u8; 32] {
    if v < ORDER {
        return v;
    }
    let mut out = [0u8; 32];
    let mut borrow = 0i16;
    for i in (0..32).rev() {
        let d = i16::from(v[i]) - i16::from(ORDER[i]) - borrow;
        borrow = i16::from(d < 0);
        out[i] = u8::try_from(d.rem_euclid(256)).unwrap_or(0);
    }
    out
}

fn scalar_of(mut h: Hash32) -> Scalar {
    loop {
        if let Ok(s) = Scalar::from_be_bytes(h.0) {
            if s != Scalar::ZERO {
                return s;
            }
        }
        h = keccak256(&h.0);
    }
}

/// `a·P − c·Q`.
fn lin(a: &SecretKey, p: &PublicKey, c: &Scalar, q: &PublicKey) -> Option<PublicKey> {
    let x = ctx();
    let ap = p.mul_tweak(&x, &Scalar::from(*a)).ok()?;
    let cq = q.mul_tweak(&x, c).ok()?.negate(&x);
    ap.combine(&cq).ok()
}

/// The address of a point.
fn point_address(p: &Point) -> [u8; 20] {
    let mut a = [0u8; 20];
    a.copy_from_slice(&keccak256(p).0[12..]);
    a
}

/// Mirror of `sealed::key_proof_valid`: `addr(z·G − c·R) = addr(T)`.
pub fn key_proof_valid(
    ephemeral: &Point,
    round_id: &Hash32,
    attempt: u32,
    observer: &[u8; 20],
    proof: &[u8; 52],
) -> bool {
    let check = || -> Option<bool> {
        let r = from_point(ephemeral)?;
        if ephemeral[..32] >= ORDER[..] {
            return Some(false);
        }
        let mut t = [0u8; 20];
        t.copy_from_slice(&proof[..20]);
        if t == [0u8; 20] {
            return Some(false);
        }
        let z = SecretKey::from_slice(&proof[20..]).ok()?;
        let mut b = POK_TAG.to_vec();
        b.extend_from_slice(ephemeral);
        b.extend_from_slice(&t);
        b.extend_from_slice(&round_id.0);
        b.extend_from_slice(&attempt.to_be_bytes());
        b.extend_from_slice(observer);
        let c = Scalar::from_be_bytes(reduce(keccak256(&b).0)).ok()?;
        if c == Scalar::ZERO {
            return Some(false);
        }
        let tp = lin(&z, &generator()?, &c, &r)?;
        Some(point_address(&to_point(&tp)) == t)
    };
    check().unwrap_or(false)
}

/// Mirror of `sealed::dismissal_proof_checks`: `S` and `c ‖ z` prove
/// `log_G(K) = log_R(S)` for this round, attempt and observer.
pub fn dismissal_proof_checks(
    key: &Point,
    ephemeral: &Point,
    round_id: &Hash32,
    attempt: u32,
    observer: &[u8; 20],
    shared: &Point,
    proof: &[u8; 64],
) -> bool {
    let check = || -> Option<bool> {
        let k = from_point(key)?;
        let r = from_point(ephemeral)?;
        let s = from_point(shared)?;
        let mut cb = [0u8; 32];
        cb.copy_from_slice(&proof[..32]);
        let c = Scalar::from_be_bytes(cb).ok()?;
        if c == Scalar::ZERO {
            return Some(false);
        }
        let z = SecretKey::from_slice(&proof[32..]).ok()?;
        let t1 = to_point(&lin(&z, &generator()?, &c, &k)?);
        let t2 = to_point(&lin(&z, &r, &c, &s)?);
        let mut b = DLEQ_TAG.to_vec();
        for p in [key, ephemeral, shared, &t1, &t2] {
            b.extend_from_slice(p);
        }
        b.extend_from_slice(&round_id.0);
        b.extend_from_slice(&attempt.to_be_bytes());
        b.extend_from_slice(observer);
        Some(scalar_of(keccak256(&b)) == c)
    };
    check().unwrap_or(false)
}

/// Mirror of `sealed::post_key` and `stream_xor`: decrypt a post with `S`.
pub fn decrypt(
    shared: &Point,
    ephemeral: &Point,
    round_id: &Hash32,
    attempt: u32,
    observer: &[u8; 20],
    ciphertext: &[u8],
) -> Vec<u8> {
    let mut b = KEY_TAG.to_vec();
    b.extend_from_slice(shared);
    b.extend_from_slice(ephemeral);
    b.extend_from_slice(&round_id.0);
    b.extend_from_slice(&attempt.to_be_bytes());
    b.extend_from_slice(observer);
    let key = keccak256(&b);
    let mut out = Vec::with_capacity(ciphertext.len());
    for (i, chunk) in ciphertext.chunks(32).enumerate() {
        let mut s = STREAM_TAG.to_vec();
        s.extend_from_slice(&key.0);
        s.extend_from_slice(&u32::try_from(i).unwrap_or(u32::MAX).to_be_bytes());
        let ks = keccak256(&s);
        out.extend(chunk.iter().zip(ks.0.iter()).map(|(d, k)| d ^ k));
    }
    out
}

/// Mirror of `timelock::lock_round`: the first quicknet round published at or
/// after `reveal_close + 60`.
pub fn lock_round(reveal_close: u64) -> Option<u64> {
    let t = reveal_close.checked_add(LOCK_MARGIN_SECS)?;
    if t <= QUICKNET_GENESIS {
        return Some(1);
    }
    (t - QUICKNET_GENESIS)
        .div_ceil(QUICKNET_PERIOD)
        .checked_add(1)
}

/// Mirror of `timelock::unseal_time` and `LockRounds::for_window`: the far
/// round of a round of `spec` (an instance of `template`) closing at
/// `reveal_close`: the first quicknet round at or after the fact's latest
/// possible reveal close (its last observable day) plus a day; `None` when the
/// spec is not the template's, the far date is less than a day after
/// `reveal_close` or past the horizon, or the template's rights do not allow
/// free redistribution (`NotRedistributable`, review C52-R1-2; review C52-R2-1:
/// the page refuses what the coordinator and the Rust client refuse).
pub fn far_round(
    template: &onyx_core::FeedTemplate,
    spec: &ObservationSpec,
    reveal_close: u64,
) -> Option<u64> {
    if template.rights.redistribution != onyx_core::Redistribution::Allowed {
        return None;
    }
    template.matches(spec).ok()?;
    let unseal = template
        .latest_window_start(&spec.subject, spec.window_start)?
        .checked_add(template.window_secs)?
        .checked_add(MAX_COMMIT_GRACE_SECS)?
        .checked_add(MAX_REVEAL_SECS)?
        .checked_add(UNSEAL_MARGIN_SECS)?;
    if unseal < reveal_close.checked_add(UNSEAL_MARGIN_SECS)?
        || unseal - reveal_close > MAX_UNSEAL_DELAY_SECS
    {
        return None;
    }
    if unseal <= QUICKNET_GENESIS {
        return Some(1);
    }
    (unseal - QUICKNET_GENESIS)
        .div_ceil(QUICKNET_PERIOD)
        .checked_add(1)
}

/// Mirror of `timelock::far_sigma`.
pub fn far_sigma(kappa: &[u8; 16], far_round: u64) -> [u8; 16] {
    let mut b = FAR_SIGMA_TAG.to_vec();
    b.extend_from_slice(kappa);
    b.extend_from_slice(&far_round.to_be_bytes());
    let mut o = [0u8; 16];
    o.copy_from_slice(&keccak256(&b).0[..16]);
    o
}

/// The context both keys of a time-locked post bind (mirror of
/// `timelock::KeyContext`).
#[allow(clippy::too_many_arguments)]
fn key_context(
    b: &mut Vec<u8>,
    ephemeral: &Point,
    lock: &[u8],
    far: &[u8],
    rho: u64,
    rho_far: u64,
    round_id: &Hash32,
    attempt: u32,
    observer: &[u8; 20],
) {
    b.extend_from_slice(ephemeral);
    b.extend_from_slice(&keccak256(lock).0);
    b.extend_from_slice(&keccak256(far).0);
    b.extend_from_slice(&rho.to_be_bytes());
    b.extend_from_slice(&rho_far.to_be_bytes());
    b.extend_from_slice(&onyx_tlock::QUICKNET_CHAIN_HASH);
    b.extend_from_slice(&round_id.0);
    b.extend_from_slice(&attempt.to_be_bytes());
    b.extend_from_slice(observer);
}

/// Mirror of `timelock::wrap_key`, the unwrap, the far-copy re-encryption and
/// `body_key` with `stream_xor`: from `S` and the opened `tau`, the body key,
/// whether the post's far copy is exactly that key's, and the body's
/// plaintext. `None` for a rest too short to hold the far copy, the wrapped
/// key and a body byte.
#[allow(clippy::too_many_arguments)]
pub fn decrypt_timelocked(
    beacon: &onyx_tlock::BeaconKey,
    shared: &Point,
    ephemeral: &Point,
    lock: &[u8],
    tau: &[u8; 16],
    rho: u64,
    rho_far: u64,
    round_id: &Hash32,
    attempt: u32,
    observer: &[u8; 20],
    rest: &[u8],
) -> Option<(bool, Vec<u8>)> {
    if rest.len() <= TIME_LOCKED_HEAD - TLOCK_LEN {
        return None;
    }
    let (far, rest) = rest.split_at(TLOCK_LEN);
    let (w, body) = rest.split_at(16);
    let mut b = WRAP_TAG.to_vec();
    b.extend_from_slice(shared);
    b.extend_from_slice(tau);
    key_context(
        &mut b, ephemeral, lock, far, rho, rho_far, round_id, attempt, observer,
    );
    let wrap = keccak256(&b);
    let mut kappa = [0u8; 16];
    for (i, k) in kappa.iter_mut().enumerate() {
        *k = w[i] ^ wrap.0[i];
    }
    let far_ok = beacon
        .lock(rho_far, &kappa, &far_sigma(&kappa, rho_far))
        .is_ok_and(|again| again[..] == far[..]);
    let mut b = KEY_TAG_V3.to_vec();
    b.extend_from_slice(&kappa);
    key_context(
        &mut b, ephemeral, lock, far, rho, rho_far, round_id, attempt, observer,
    );
    Some((far_ok, stream(&keccak256(&b), body)))
}

fn stream(key: &Hash32, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for (i, chunk) in data.chunks(32).enumerate() {
        let mut s = STREAM_TAG.to_vec();
        s.extend_from_slice(&key.0);
        s.extend_from_slice(&u32::try_from(i).unwrap_or(u32::MAX).to_be_bytes());
        let ks = keccak256(&s);
        out.extend(chunk.iter().zip(ks.0.iter()).map(|(d, k)| d ^ k));
    }
    out
}

/// A time-locked dismissal's check (mirror of
/// `timelock::check_timelocked_dismissal` after the DLEQ proof): `Ok((plain,
/// may_be_a_reveal))` with the dismissed body and whether its far copy is the
/// unwrapped body key's (only then can the body's validity make the dismissal
/// false; ROADMAP C52), `Err` with why it is refused.
#[allow(clippy::too_many_arguments)]
pub fn check_timelocked(
    beacon: &onyx_tlock::BeaconKey,
    rho: u64,
    rho_far: u64,
    signature: &[u8],
    opening: Option<([u8; 16], [u8; 16])>,
    shared: &Point,
    ephemeral: &Point,
    round_id: &Hash32,
    attempt: u32,
    observer: &[u8; 20],
    ciphertext: &[u8],
) -> Result<(Vec<u8>, bool), &'static str> {
    let split = (ciphertext.len() > TLOCK_LEN).then(|| ciphertext.split_at(TLOCK_LEN));
    match (opening, split) {
        (Some((sigma, tau)), Some((lock, rest))) => {
            // Exact re-encryption: the opening locks to the post's bytes.
            match beacon.lock(rho, &tau, &sigma) {
                Ok(again) if again[..] == lock[..] => {}
                _ => return Err("its time-lock opening does not re-encrypt to the post"),
            }
            Ok(
                match decrypt_timelocked(
                    beacon, shared, ephemeral, lock, &tau, rho, rho_far, round_id, attempt,
                    observer, rest,
                ) {
                    // Too short for the far copy: garbage on its face.
                    None => (Vec::new(), false),
                    Some((far_ok, plain)) => (plain, far_ok),
                },
            )
        }
        (Some(_), None) => Err("an opening for a post too short to hold a time-lock"),
        (None, None) => Ok((Vec::new(), false)),
        (None, Some((lock, _))) => {
            // `open_canonical`: an opening that re-encrypts means the
            // coordinator owed it.
            if let Ok(o) = beacon.open(rho, signature, lock) {
                if beacon
                    .lock(rho, &o.msg, &o.sigma)
                    .is_ok_and(|again| again[..] == lock[..])
                {
                    return Err("its time-lock opens: the dismissal owed the opening");
                }
            }
            Ok((Vec::new(), false))
        }
    }
}

/// Mirror of `sealed::sealed_len`.
pub fn sealed_len(spec: &ObservationSpec) -> usize {
    let base = 1 + 8 + 32 + 32 + 65;
    match spec.kind {
        ObservationKind::Numeric { .. } => base + 16,
        ObservationKind::Exact { max_len } => base + 4 + usize::from(max_len),
    }
}

/// Mirror of `sealed::valid_reveal`: whether `plain` is `observer`'s signed
/// reveal for this round, opening `commit` and passing the feed's checks.
pub fn is_valid_reveal(
    plain: &[u8],
    observer: &[u8; 20],
    spec: &ObservationSpec,
    round_id: &Hash32,
    attempt: u32,
    commit: Option<Hash32>,
) -> bool {
    #[derive(borsh::BorshDeserialize)]
    struct Plain {
        observation: onyx_core::Observation,
        salt: Hash32,
        sig: [u8; 65],
    }
    if plain.len() != sealed_len(spec) {
        return false;
    }
    let mut rest = plain;
    let Ok(p) = <Plain as borsh::BorshDeserialize>::deserialize(&mut rest) else {
        return false;
    };
    let Some(commit) = commit else {
        return false;
    };
    rest.iter().all(|b| *b == 0)
        && observation_commit(round_id, observer, &p.observation, &p.salt) == commit
        && onyx_labels::attest::eip191_recover(
            crate::level2_reveal_message(round_id, attempt, observer, &p.observation, &p.salt)
                .as_bytes(),
            &p.sig,
        ) == Some(*observer)
        && p.observation.check(spec).is_ok()
}

/// Mirror of `sealed::posted_from_returns` (strict ABI): the ephemeral key,
/// ciphertext, key proof and block time of a filled slot.
pub fn posted_from_returns(
    reveal_ret: &[u8],
    proof_ret: &[u8],
) -> Option<(Point, Vec<u8>, [u8; 52], u64)> {
    let small = |w: &[u8]| -> Option<u64> {
        (w.len() == 32 && w[..24].iter().all(|b| *b == 0))
            .then(|| u64::from_be_bytes(w[24..].try_into().unwrap_or([0; 8])))
    };
    if reveal_ret.len() < 160 || small(&reveal_ret[96..128]) != Some(128) {
        return None;
    }
    let at = small(&reveal_ret[64..96])?;
    let len = usize::try_from(small(&reveal_ret[128..160])?).ok()?;
    if at == 0
        || len == 0
        || len > MAX_SEALED_BYTES
        || reveal_ret.len() != 160 + len.div_ceil(32) * 32
        || reveal_ret[160 + len..].iter().any(|b| *b != 0)
        || proof_ret.len() != 128
        || small(&proof_ret[..32]) != Some(32)
        || small(&proof_ret[32..64]) != Some(52)
        || proof_ret[116..].iter().any(|b| *b != 0)
    {
        return None;
    }
    Some((
        reveal_ret[..64].try_into().ok()?,
        reveal_ret[160..160 + len].to_vec(),
        proof_ret[64..116].try_into().ok()?,
        at,
    ))
}

/// Calldata of `drawTimeLocked(fact, attempt)` (ROADMAP C51).
pub fn draw_time_locked_calldata(registry_fact: &Hash32, attempt: u32) -> Vec<u8> {
    let mut d = keccak256(b"drawTimeLocked(bytes32,uint32)").0[..4].to_vec();
    d.extend_from_slice(&registry_fact.0);
    d.extend_from_slice(&word(u64::from(attempt)));
    d
}

fn word(v: u64) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[24..].copy_from_slice(&v.to_be_bytes());
    w
}

fn push_bytes(d: &mut Vec<u8>, b: &[u8]) {
    d.extend_from_slice(&word(b.len() as u64));
    d.extend_from_slice(b);
    d.resize(d.len() + (32 - b.len() % 32) % 32, 0);
}

/// One drawn observer of the query: address, served commit (zero when none),
/// status (0 not revealed, 1 revealed, 2 dismissed).
pub type Entry = ([u8; 20], Hash32, u8);

/// The `revealLogComplete` / `attemptMatches` arguments (mirror of
/// `sealed::RevealLogQuery`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogQuery {
    /// `OnyxRegistry.factKeyOf`.
    pub registry_fact: Hash32,
    /// The draw attempt.
    pub attempt: u32,
    /// The recomputed round id.
    pub round_id: Hash32,
    /// The window start.
    pub commit_open: u64,
    /// The config's commit close.
    pub commit_close: u64,
    /// The config's reveal close.
    pub reveal_close: u64,
    /// Every drawn observer, ascending.
    pub entries: Vec<Entry>,
}

impl LogQuery {
    /// `entries` packed (20 + 32 + 1 bytes each).
    pub fn packed(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(self.entries.len() * 53);
        for (a, c, s) in &self.entries {
            b.extend_from_slice(a);
            b.extend_from_slice(&c.0);
            b.push(*s);
        }
        b
    }

    fn head(&self, sig: &[u8]) -> Vec<u8> {
        let mut d = keccak256(sig).0[..4].to_vec();
        d.extend_from_slice(&self.registry_fact.0);
        d.extend_from_slice(&word(u64::from(self.attempt)));
        d.extend_from_slice(&self.round_id.0);
        d.extend_from_slice(&word(self.commit_open));
        d.extend_from_slice(&word(self.commit_close));
        d.extend_from_slice(&word(self.reveal_close));
        d
    }

    /// Calldata of `revealLogComplete`.
    pub fn reveal_log_calldata(&self) -> Vec<u8> {
        let mut d =
            self.head(b"revealLogComplete(bytes32,uint32,bytes32,uint64,uint64,uint64,bytes)");
        d.extend_from_slice(&word(7 * 32));
        push_bytes(&mut d, &self.packed());
        d
    }

    /// Calldata of `attemptMatches` with `result`.
    pub fn matches_calldata(&self, result: &Hash32) -> Vec<u8> {
        let mut d =
            self.head(b"attemptMatches(bytes32,uint32,bytes32,uint64,uint64,uint64,bytes32,bytes)");
        d.extend_from_slice(&result.0);
        d.extend_from_slice(&word(8 * 32));
        push_bytes(&mut d, &self.packed());
        d
    }

    /// `attemptDigest` with `result`.
    pub fn digest(&self, result: &Hash32) -> Hash32 {
        let mut b = RECORD_TAG.to_vec();
        b.extend_from_slice(&self.round_id.0);
        b.extend_from_slice(&self.commit_open.to_be_bytes());
        b.extend_from_slice(&self.commit_close.to_be_bytes());
        b.extend_from_slice(&self.reveal_close.to_be_bytes());
        b.extend_from_slice(&result.0);
        b.extend_from_slice(&keccak256(&self.packed()).0);
        keccak256(&b)
    }
}

/// The round a `sealed_reveals` section belongs to, as the page rebuilt it.
pub struct Round<'a> {
    /// The canonical feed.
    pub spec: &'a ObservationSpec,
    /// The far round the page derives from the pinned template ([`far_round`],
    /// ROADMAP C52); `None` when it has none (a time-locked section then fails).
    pub far_round: Option<u64>,
    /// The recomputed round id.
    pub round_id: Hash32,
    /// The draw attempt.
    pub attempt: u32,
    /// The registry's fact key, from the pinned template.
    pub registry_fact: Hash32,
    /// The config's commit and reveal close.
    pub window: (u64, u64),
    /// The drawn observers (checked to be the draw), ascending.
    pub observers: &'a [[u8; 20]],
    /// Each observer's signed commit, as checked.
    pub commits: &'a BTreeMap<[u8; 20], Hash32>,
    /// The observers whose reveals the round serves (`None`: a round served
    /// sealed; the section's own `revealed` list stands for them, pinned by
    /// the record).
    pub revealed: Option<&'a BTreeSet<[u8; 20]>>,
    /// The round's result hash.
    pub result: Hash32,
    /// Settled a value (recorded with `recordObservation`, no attempt record).
    pub value: bool,
}

/// Check a round's `sealed_reveals` section `v` (see the module docs), pushing
/// each problem; returns the summary the page shows (posts, dismissals and the
/// calls a node holder asks). Absent (`null`): a draw without a reveal key, as
/// every recorded rehearsal round; nothing to check.
pub fn check(
    v: &serde_json::Value,
    r: &Round<'_>,
    problems: &mut Vec<String>,
) -> serde_json::Value {
    if v.is_null() {
        return serde_json::Value::Null;
    }
    let s = |x: &serde_json::Value| x.as_str().unwrap_or_default().to_string();
    let hexn = |x: &serde_json::Value, n: usize| -> Option<Vec<u8>> {
        let b = hex::decode(s(x).strip_prefix("0x")?).ok()?;
        (n == 0 || b.len() == n).then_some(b)
    };
    let mut bad = |m: String| problems.push(format!("sealed reveals: {m}"));
    let key: Option<Point> = hexn(&v["reveal_key"], 64).and_then(|b| b.try_into().ok());
    let Some(key) = key.filter(|k| from_point(k).is_some()) else {
        bad("no valid reveal key".into());
        return serde_json::Value::Null;
    };
    let (cc, rc) = r.window;
    let empty = vec![];
    let mut posts: BTreeMap<[u8; 20], (Point, Vec<u8>)> = BTreeMap::new();
    let mut prev: Option<[u8; 20]> = None;
    for p in v["posts"].as_array().unwrap_or(&empty) {
        let (Some(o), Some(e), Some(kp), Some(ct)) = (
            hexn(&p["observer"], 20).and_then(|b| <[u8; 20]>::try_from(b).ok()),
            hexn(&p["ephemeral"], 64).and_then(|b| <Point>::try_from(b).ok()),
            hexn(&p["key_proof"], 52).and_then(|b| <[u8; 52]>::try_from(b).ok()),
            hexn(&p["ciphertext"], 0),
        ) else {
            bad("a malformed post".into());
            continue;
        };
        let at = p["posted_at"].as_u64().unwrap_or(0);
        if prev.is_some_and(|q| q >= o) || !r.observers.contains(&o) {
            bad(format!(
                "post 0x{} is not a drawn observer's, in order",
                hex::encode(o)
            ));
        }
        prev = Some(o);
        if at <= cc || at > rc {
            bad(format!(
                "post 0x{} lies outside the reveal phase",
                hex::encode(o)
            ));
        }
        if ct.is_empty() || ct.len() > MAX_SEALED_BYTES {
            bad(format!(
                "post 0x{}: a ciphertext the registry never takes",
                hex::encode(o)
            ));
        }
        if !key_proof_valid(&e, &r.round_id, r.attempt, &o, &kp) {
            bad(format!("post 0x{}: its key proof fails", hex::encode(o)));
        }
        posts.insert(o, (e, ct));
    }
    // ROADMAP C51: the time-lock read of a time-locked draw.
    let beacon = onyx_tlock::BeaconKey::quicknet().ok();
    let tl = &v["time_lock"];
    let time_locked = !tl.is_null();
    let mut lock: Option<(u64, Vec<u8>)> = None;
    let mut openings: BTreeMap<[u8; 20], ([u8; 16], [u8; 16])> = BTreeMap::new();
    let far = tl["far_round"].as_u64();
    if time_locked {
        let rho = tl["lock_round"].as_u64();
        let sig = hexn(&tl["beacon"], 48);
        match (rho, sig, &beacon) {
            (Some(n), Some(sig), Some(b)) if Some(n) == lock_round(rc) && b.verify(n, &sig) => {
                lock = Some((n, sig));
            }
            (Some(n), _, _) if Some(n) != lock_round(rc) => {
                bad("the time-lock names another lock round than the window's".into());
            }
            _ => bad("the lock round's signature is not drand quicknet's".into()),
        }
        // ROADMAP C52: the far round is the page's own, from the template.
        if far != r.far_round || !matches!((far, rho), (Some(f), Some(n)) if f > n) {
            bad("the time-lock names another far round than the template's".into());
        }
        let mut prev: Option<[u8; 20]> = None;
        for o in tl["openings"].as_array().unwrap_or(&empty) {
            let (Some(a), Some(sg), Some(tau)) = (
                hexn(&o["observer"], 20).and_then(|b| <[u8; 20]>::try_from(b).ok()),
                hexn(&o["sigma"], 16).and_then(|b| <[u8; 16]>::try_from(b).ok()),
                hexn(&o["tau"], 16).and_then(|b| <[u8; 16]>::try_from(b).ok()),
            ) else {
                bad("a malformed time-lock opening".into());
                continue;
            };
            if prev.is_some_and(|q| q >= a) {
                bad("time-lock openings out of order".into());
            }
            prev = Some(a);
            openings.insert(a, (sg, tau));
        }
        for (o, (_, ct)) in &posts {
            if ct.len() <= TIME_LOCKED_HEAD {
                bad(format!(
                    "post 0x{}: a time-locked draw's post without a time-lock and far copy",
                    hex::encode(o)
                ));
            }
        }
    }
    let revealed: BTreeSet<[u8; 20]> = match r.revealed {
        Some(set) => set.clone(),
        None => v["revealed"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .filter_map(|x| hexn(x, 20).and_then(|b| <[u8; 20]>::try_from(b).ok()))
            .filter(|o| r.observers.contains(o))
            .collect(),
    };
    let mut dismissed = BTreeSet::new();
    let mut prev: Option<[u8; 20]> = None;
    for d in v["dismissals"].as_array().unwrap_or(&empty) {
        let (Some(o), Some(sh), Some(pf)) = (
            hexn(&d["observer"], 20).and_then(|b| <[u8; 20]>::try_from(b).ok()),
            hexn(&d["shared"], 64).and_then(|b| <Point>::try_from(b).ok()),
            hexn(&d["proof"], 64).and_then(|b| <[u8; 64]>::try_from(b).ok()),
        ) else {
            bad("a malformed dismissal".into());
            continue;
        };
        if prev.is_some_and(|q| q >= o) {
            bad("dismissals out of order".into());
        }
        prev = Some(o);
        let Some((e, ct)) = posts.get(&o) else {
            bad(format!(
                "a dismissal of 0x{} without its post",
                hex::encode(o)
            ));
            continue;
        };
        if revealed.contains(&o) {
            bad(format!(
                "a dismissal of 0x{}, a revealed observer",
                hex::encode(o)
            ));
        }
        if !dismissal_proof_checks(&key, e, &r.round_id, r.attempt, &o, &sh, &pf) {
            bad(format!(
                "the dismissal of 0x{} does not check",
                hex::encode(o)
            ));
            continue;
        }
        let (plain, may_be_reveal) = if time_locked {
            let (Some((rho, sig)), Some(b), Some(rho_far)) = (&lock, &beacon, r.far_round) else {
                bad(format!(
                    "the dismissal of 0x{} cannot be checked without the lock and far rounds",
                    hex::encode(o)
                ));
                continue;
            };
            match check_timelocked(
                b,
                *rho,
                rho_far,
                sig,
                openings.get(&o).copied(),
                &sh,
                e,
                &r.round_id,
                r.attempt,
                &o,
                ct,
            ) {
                Ok(p) => p,
                Err(why) => {
                    bad(format!("the dismissal of 0x{}: {why}", hex::encode(o)));
                    continue;
                }
            }
        } else {
            (decrypt(&sh, e, &r.round_id, r.attempt, &o, ct), true)
        };
        if may_be_reveal
            && is_valid_reveal(
                &plain,
                &o,
                r.spec,
                &r.round_id,
                r.attempt,
                r.commits.get(&o).copied(),
            )
        {
            bad(format!(
                "the dismissal of 0x{} is false: its post is a valid reveal",
                hex::encode(o)
            ));
        }
        dismissed.insert(o);
    }
    if posts
        .keys()
        .any(|o| !revealed.contains(o) && !dismissed.contains(o))
    {
        bad("a post neither revealed nor dismissed".into());
    }
    if openings.keys().any(|o| !dismissed.contains(o)) {
        bad("a time-lock opening of no dismissal".into());
    }
    // Review C11-R1-3: a keyed draw is sealed only.
    if revealed.iter().any(|o| !posts.contains_key(o)) {
        bad("a revealed observer without an in-phase sealed post".into());
    }
    // Review C51-R2-1: a time-locked draw's revealed posts each hold a
    // time-lock that opens canonically under the verified lock-round
    // signature (the coordinator takes a reveal only through it).
    if let (Some((rho, sig)), Some(b)) = (&lock, &beacon) {
        for o in &revealed {
            let Some((_, ct)) = posts.get(o) else {
                continue;
            };
            let opens = ct.len() > TLOCK_LEN && {
                let l = &ct[..TLOCK_LEN];
                b.open(*rho, sig, l).is_ok_and(|op| {
                    b.lock(*rho, &op.msg, &op.sigma)
                        .is_ok_and(|again| again[..] == l[..])
                })
            };
            if !opens {
                bad(format!(
                    "revealed post 0x{}: its time-lock does not open under the lock round",
                    hex::encode(o)
                ));
            }
        }
    }
    let q = LogQuery {
        registry_fact: r.registry_fact,
        attempt: r.attempt,
        round_id: r.round_id,
        commit_open: r.spec.window_start,
        commit_close: cc,
        reveal_close: rc,
        entries: r
            .observers
            .iter()
            .map(|o| {
                let st = if revealed.contains(o) {
                    1
                } else if dismissed.contains(o) {
                    2
                } else {
                    0
                };
                (*o, r.commits.get(o).copied().unwrap_or(Hash32::ZERO), st)
            })
            .collect(),
    };
    let record = hexn(&v["record"], 32).and_then(|b| <[u8; 32]>::try_from(b).ok());
    match (r.value, record) {
        (true, Some(_)) => bad("a value round with an attempt record".into()),
        (false, None) => bad("a round without a value has no record (REVLOG-5)".into()),
        (false, Some(d)) if Hash32(d) != q.digest(&r.result) => {
            bad("the record is not this round's digest".into())
        }
        _ => {}
    }
    // The chain's answers, recorded with the round: each for exactly the
    // call the page built, and true.
    let reveal_log = q.reveal_log_calldata();
    let matches = (!r.value).then(|| q.matches_calldata(&r.result));
    let answered = |c: &serde_json::Value, want: &[u8]| -> bool {
        hexn(&c["calldata"], 0).as_deref() == Some(want) && c["answer"] == json!(true)
    };
    let chain = &v["chain"];
    if hexn(&chain["draw_reveal_key"], 64).as_deref() != Some(&key[..]) {
        bad("the chain's drawRevealKey is not the round's reveal key".into());
    }
    // ROADMAP C51: the chain's drawTimeLocked, recorded for exactly the
    // page's call, says what the section says.
    let tl_call = draw_time_locked_calldata(&r.registry_fact, r.attempt);
    let dtl = &chain["draw_time_locked"];
    if hexn(&dtl["calldata"], 0).as_deref() != Some(&tl_call[..])
        || dtl["answer"] != json!(time_locked)
    {
        bad("no recorded drawTimeLocked for the page's call that matches the section".into());
    }
    if !answered(&chain["reveal_log_complete"], &reveal_log) {
        bad("no true revealLogComplete for the page's own query".into());
    }
    if let Some(m) = &matches {
        if !answered(&chain["attempt_matches"], m) {
            bad("no true attemptMatches for the page's own query".into());
        }
    }
    // Every served post is the chain's (`sealedRevealOf`, `sealedKeyProofOf`
    // as recorded): a server's own garbage under an observer's name, with an
    // honest dismissal of it, is refused (THREATS C11-4).
    let recorded: BTreeMap<[u8; 20], (Vec<u8>, Vec<u8>)> = chain["posts"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|p| {
            Some((
                hexn(&p["observer"], 20).and_then(|b| <[u8; 20]>::try_from(b).ok())?,
                (
                    hexn(&p["sealed_reveal_of"], 0)?,
                    hexn(&p["sealed_key_proof_of"], 0)?,
                ),
            ))
        })
        .collect();
    for (o, (e, ct)) in &posts {
        let ok = recorded
            .get(o)
            .and_then(|(rv, kp)| posted_from_returns(rv, kp))
            .is_some_and(|(ce, cct, ckp, at)| {
                ce == *e
                    && cct == *ct
                    && v["posts"].as_array().unwrap_or(&empty).iter().any(|p| {
                        hexn(&p["observer"], 20).as_deref() == Some(&o[..])
                            && hexn(&p["key_proof"], 52).as_deref() == Some(&ckp[..])
                            && p["posted_at"].as_u64() == Some(at)
                    })
            });
        if !ok {
            bad(format!("post 0x{} is not the chain's", hex::encode(o)));
        }
    }
    json!({
        "posts": posts.len(),
        "dismissed": dismissed.len(),
        "time_locked": time_locked,
        "lock_round": lock.as_ref().map(|(n, _)| *n),
        "far_round": time_locked.then_some(far).flatten(),
        "draw_time_locked": format!("0x{}", hex::encode(&tl_call)),
        "reveal_log_complete": format!("0x{}", hex::encode(&reveal_log)),
        "attempt_matches": matches.map(|m| format!("0x{}", hex::encode(m))),
    })
}
