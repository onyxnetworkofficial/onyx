//! Sealed reveal posts (ADR-0027 amendment 10, ROADMAP C4; THREATS REVLOG-1,
//! REVLOG-5).
//!
//! An observer posts its signed reveal ON CHAIN (`OnyxRegistry.postSealedReveal`),
//! encrypted to the draw's reveal key, a key only the coordinator holds. The post
//! is the receipt: the chain, not the coordinator, says a reveal arrived, and a
//! round must account for every post in its reveal phase, as a reveal or as
//! garbage DISMISSED with a proof anyone can check (`OnyxRegistry.revealLogComplete`).
//!
//! - Encryption: ECIES (SEC 1 §5.1) on secp256k1 with Keccak-256 as KDF and stream
//!   cipher. `S = r·K`, `k = keccak256("onyx-sealed-reveal-key-v1" ‖ S ‖ R ‖ round ‖
//!   attempt ‖ observer)`, ciphertext = plaintext XOR `keccak256(tag ‖ k ‖ i)`. No MAC:
//!   the plaintext must be the observer's own signed reveal opening its commit, and the
//!   post is signed by the observer.
//! - Per post, the observer's signature (only it fills its slot) and a Schnorr proof of
//!   knowledge of the ephemeral secret `r`, bound to round, attempt and observer
//!   ([`key_proof`]; the registry checks it with one `ecrecover`). So nobody posts an `R`
//!   it does not hold (another observer's, or a point related to it), and a dismissal
//!   reveals only an `S` its poster could compute itself. Review C4-R1-1: an ECDSA
//!   signature by `R` is NOT such a proof (anyone can make a signature that recovers to a
//!   point whose secret nobody knows), and with it a dismissal was a decryption oracle.
//! - Dismissal: `S` with a Chaum-Pedersen proof (RFC 9497 §2.2 form) that
//!   `log_G(K) = log_R(S)`; the proof fixes `S`, so it fixes the decryption, and a valid
//!   reveal can never be dismissed.
//! - Attempt records (REVLOG-5): the digest the registry keeps for a round before it is
//!   served (`attempt_digest`), over every drawn observer's commit and status.
//!
//! [`SimulatedSealedLog`] mirrors the registry's rules for tests and the Rust <->
//! Solidity vectors (`fixtures/registry/sealed-reveals.json`).

use crate::observe::{
    random_salt, reveal_signed, sign_reveal, transcript_log_key, ObserverSig, SignedReveal,
    SimulatedTranscriptLog,
};
use borsh::{BorshDeserialize, BorshSerialize};
use onyx_core::{
    keccak256, observation_commit, Hash32, MinerAddress, Observation, ObservationKind,
    ObservationSpec,
};
use onyx_labels::attest::{eip191_recover, eip191_sign_with, secret_address};
use secp256k1::{PublicKey, Scalar, Secp256k1, SecretKey};
use std::collections::BTreeMap;

/// An uncompressed secp256k1 point without its `0x04` prefix: `x ‖ y`, 32 bytes
/// each, big endian (the registry's `(bytes32 x, bytes32 y)`).
pub type Point = [u8; 64];

/// The longest padded plaintext of a sealed reveal: the sealed length of an
/// exact answer of `MAX_VALUE_BYTES` (review C4-R2-2), 1,166 bytes.
pub const MAX_PLAIN_BYTES: usize = PLAIN_BASE + 4 + onyx_core::value::MAX_VALUE_BYTES;

/// Bytes of the drand time-lock that leads a time-locked post (ADR-0027
/// amendments 12 and 14; `timelock::TLOCK_LEN`, the registry's
/// `TIME_LOCK_BYTES`).
pub const TIME_LOCK_BYTES: usize = 96 + 16 + 16;

/// Bytes of the far-date unseal copy that follows it (ADR-0027 amendment 15,
/// ROADMAP C52; the registry's `FAR_COPY_BYTES`): a second time-lock of the
/// body key to the far round (128) and the body key wrapped for the
/// coordinator (16).
pub const FAR_COPY_BYTES: usize = TIME_LOCK_BYTES + 16;

/// A post of a time-locked draw must be longer than this (the registry's
/// `TIME_LOCK_BYTES + FAR_COPY_BYTES`, 272): both time-locks and the wrapped
/// key, then at least one body byte.
pub const TIME_LOCKED_HEAD_BYTES: usize = TIME_LOCK_BYTES + FAR_COPY_BYTES;

/// Most ciphertext bytes the registry takes (`MAX_SEALED_BYTES`, ROADMAP C51,
/// C52): the longest plaintext sealed, time-locked AND with its far-date copy,
/// 1,438 bytes (THREATS C12-7, C52-6).
pub const MAX_SEALED_BYTES: usize = MAX_PLAIN_BYTES + TIME_LOCKED_HEAD_BYTES;

/// Borsh bytes of a [`Plain`] besides its answer's payload: the answer's tag (1),
/// `observed_at` (8), `page_hash` (32), salt (32), signature (65).
const PLAIN_BASE: usize = 1 + 8 + 32 + 32 + 65;

/// The ONE plaintext length every sealed reveal of `spec` has (review C4-R2-1):
/// a numeric feed's value or `Unavailable`, an exact feed's answer of any length
/// up to `max_len`, all padded with zeros to it, so a ciphertext's length shows
/// nothing about its answer.
pub fn sealed_len(spec: &ObservationSpec) -> usize {
    match spec.kind {
        ObservationKind::Numeric { .. } => PLAIN_BASE + 16,
        ObservationKind::Exact { max_len } => PLAIN_BASE + 4 + usize::from(max_len),
    }
}

const KEY_TAG: &[u8] = b"onyx-sealed-reveal-key-v1";
const STREAM_TAG: &[u8] = b"onyx-sealed-reveal-stream-v1";
const DLEQ_TAG: &[u8] = b"onyx-sealed-dleq-v1";
const DLEQ_NONCE_TAG: &[u8] = b"onyx-sealed-dleq-nonce-v1";
const RECORD_TAG: &[u8] = b"onyx-attempt-record-v1";
const POK_TAG: &[u8] = b"onyx-sealed-key-pok-v1";
const POK_NONCE_TAG: &[u8] = b"onyx-sealed-key-pok-nonce-v1";
/// The secp256k1 group order, big endian.
const ORDER: [u8; 32] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFE,
    0xBA, 0xAE, 0xDC, 0xE6, 0xAF, 0x48, 0xA0, 0x3B, 0xBF, 0xD2, 0x5E, 0x8C, 0xD0, 0x36, 0x41, 0x41,
];

/// A Schnorr proof of knowledge of an ephemeral secret: `addr(T) (20) ‖ z (32)`.
pub type KeyProof = [u8; 52];

/// A sealed reveal as the observer posts it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedPost {
    /// The posting observer (its slot).
    pub observer: MinerAddress,
    /// The ephemeral public key `R = r·G`.
    pub ephemeral: Point,
    /// The encrypted plaintext ([`Plain`] in Borsh).
    pub ciphertext: Vec<u8>,
    /// Schnorr proof that the poster knows the ephemeral secret ([`key_proof`]), bound to
    /// round, attempt and observer (review C4-R1-1: an ECDSA signature is no such proof).
    pub key_proof: KeyProof,
    /// The observer's EIP-191 signature of [`sealed_reveal_message`].
    pub sig: ObserverSig,
}

/// What a sealed post encrypts: the observer's signed reveal (amendment 7).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Plain {
    /// The observation.
    pub observation: Observation,
    /// The commit salt.
    pub salt: Hash32,
    /// The observer's reveal signature (`observation_reveal_message`).
    pub sig: ObserverSig,
}

/// A post the coordinator could not take as a reveal, with the proof: the shared
/// point `S = k_coord·R` and a Chaum-Pedersen proof `c ‖ z` that `log_G(K) =
/// log_R(S)`. Anyone holding the post decrypts it with `S` and sees the garbage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Dismissal {
    /// `S`.
    pub shared: Point,
    /// `c ‖ z`, two scalars, big endian.
    pub proof: [u8; 64],
}

/// What [`open_post`] made of a post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opened {
    /// The observer's valid reveal: signed, opening its commit, passing the feed.
    Reveal(SignedReveal),
    /// Garbage, with the proof.
    Dismissed(Dismissal),
}

/// The round a post belongs to, as the coordinator and a client know it.
#[derive(Clone, Copy, Debug)]
pub struct RevealContext<'a> {
    /// The round id.
    pub round_id: Hash32,
    /// The draw attempt.
    pub attempt: u32,
    /// The feed (a reveal must pass `Observation::check`).
    pub spec: &'a ObservationSpec,
    /// The observer's commit in the round, if any (a reveal must open it).
    pub commit: Option<Hash32>,
}

/// Why a post could not be sealed, opened or checked.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SealError {
    /// A secret is not a valid secp256k1 scalar.
    #[error("not a valid secret key")]
    BadSecret,
    /// A point is not on secp256k1.
    #[error("not a point of secp256k1")]
    BadPoint,
    /// The plaintext is longer than the registry takes.
    #[error("sealed reveal longer than {MAX_SEALED_BYTES} bytes")]
    TooLong,
    /// The OS random source failed.
    #[error("random source: {0}")]
    Random(String),
    /// The post's signatures do not check.
    #[error("post not signed by its observer and ephemeral key")]
    BadPost,
    /// The dismissal proof does not check.
    #[error("dismissal proof does not check")]
    BadProof,
    /// The dismissed post decrypts to a valid reveal: the dismissal is false.
    #[error("dismissed post holds a valid reveal")]
    FalseDismissal,
}

fn secp() -> Secp256k1<secp256k1::All> {
    Secp256k1::new()
}

fn to_point(pk: &PublicKey) -> Point {
    let u = pk.serialize_uncompressed();
    let mut p = [0u8; 64];
    p.copy_from_slice(&u[1..]);
    p
}

fn from_point(p: &Point) -> Result<PublicKey, SealError> {
    let mut u = [0u8; 65];
    u[0] = 4;
    u[1..].copy_from_slice(p);
    PublicKey::from_slice(&u).map_err(|_| SealError::BadPoint)
}

fn secret(s: &[u8; 32]) -> Result<SecretKey, SealError> {
    SecretKey::from_slice(s).map_err(|_| SealError::BadSecret)
}

/// The public point of a 32-byte secret (`setRevealKey`'s argument).
pub fn public_point(s: &[u8; 32]) -> Result<Point, SealError> {
    Ok(to_point(&secret(s)?.public_key(&secp())))
}

/// The address of a point: `keccak256(x ‖ y)[12..]`, as the registry computes it.
pub fn point_address(p: &Point) -> [u8; 20] {
    let mut a = [0u8; 20];
    a.copy_from_slice(&keccak256(p).0[12..]);
    a
}

/// `v mod n` for a 32-byte big-endian `v` (one subtraction suffices: `v < 2n`).
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

/// The challenge of a key proof (`OnyxRegistry.keyProofChallenge`):
/// `keccak256("onyx-sealed-key-pok-v1" ‖ R.x ‖ R.y ‖ addr(T) ‖ round ‖ attempt ‖
/// observer) mod n`. `None` when it is zero.
pub fn key_proof_challenge(
    ephemeral: &Point,
    t: &[u8; 20],
    round_id: &Hash32,
    attempt: u32,
    observer: &MinerAddress,
) -> Option<Scalar> {
    let mut b = POK_TAG.to_vec();
    b.extend_from_slice(ephemeral);
    b.extend_from_slice(t);
    b.extend_from_slice(&round_id.0);
    b.extend_from_slice(&attempt.to_be_bytes());
    b.extend_from_slice(observer);
    let c = Scalar::from_be_bytes(reduce(keccak256(&b).0)).ok()?;
    (c != Scalar::ZERO).then_some(c)
}

/// Whether `R.x < n`: the registry checks a key proof with `ecrecover`, whose `r`
/// argument must be below the group order.
fn x_below_order(p: &Point) -> bool {
    p[..32] < ORDER[..]
}

/// The Schnorr proof that the holder of `ephemeral_secret` knows it, for this
/// round, attempt and observer: `T = t·G`, `c = H(R, addr(T), …)`, `z = t + c·r`.
/// The nonce is derived from the secret and the context.
pub fn key_proof(
    ephemeral_secret: &[u8; 32],
    round_id: &Hash32,
    attempt: u32,
    observer: &MinerAddress,
) -> Result<KeyProof, SealError> {
    let ctx = secp();
    let r = secret(ephemeral_secret)?;
    let eph = to_point(&r.public_key(&ctx));
    if !x_below_order(&eph) {
        return Err(SealError::BadSecret);
    }
    let mut nb = POK_NONCE_TAG.to_vec();
    nb.extend_from_slice(ephemeral_secret);
    nb.extend_from_slice(&round_id.0);
    nb.extend_from_slice(&attempt.to_be_bytes());
    nb.extend_from_slice(observer);
    let t = SecretKey::from_slice(&scalar_of(keccak256(&nb)).to_be_bytes())
        .map_err(|_| SealError::BadSecret)?;
    let t_addr = point_address(&to_point(&t.public_key(&ctx)));
    let c = key_proof_challenge(&eph, &t_addr, round_id, attempt, observer)
        .ok_or(SealError::BadSecret)?;
    let cr = r.mul_tweak(&c).map_err(|_| SealError::BadSecret)?;
    let z = t
        .add_tweak(&Scalar::from(cr))
        .map_err(|_| SealError::BadSecret)?;
    let mut out = [0u8; 52];
    out[..20].copy_from_slice(&t_addr);
    out[20..].copy_from_slice(&z.secret_bytes());
    Ok(out)
}

/// Whether `proof` proves knowledge of the secret of `ephemeral` for this
/// context (`OnyxRegistry.keyProofValid`): `addr(z·G − c·R) = addr(T)`.
pub fn key_proof_valid(
    ephemeral: &Point,
    round_id: &Hash32,
    attempt: u32,
    observer: &MinerAddress,
    proof: &KeyProof,
) -> bool {
    let check = || -> Option<bool> {
        let r = from_point(ephemeral).ok()?;
        if !x_below_order(ephemeral) {
            return Some(false);
        }
        let mut t = [0u8; 20];
        t.copy_from_slice(&proof[..20]);
        if t == [0u8; 20] {
            return Some(false);
        }
        let z = SecretKey::from_slice(&proof[20..]).ok()?;
        let c = key_proof_challenge(ephemeral, &t, round_id, attempt, observer)?;
        let tp = lin(&z, &generator()?, &c, &r)?;
        Some(point_address(&to_point(&tp)) == t)
    };
    check().unwrap_or(false)
}

fn generator() -> Option<PublicKey> {
    let mut one = [0u8; 32];
    one[31] = 1;
    Some(SecretKey::from_slice(&one).ok()?.public_key(&secp()))
}

/// What the observer signs to post (`OnyxRegistry.sealedRevealMessage`).
pub fn sealed_reveal_message(
    round_id: &Hash32,
    attempt: u32,
    observer: &MinerAddress,
    ephemeral: &Point,
    ciphertext_hash: &Hash32,
) -> String {
    format!(
        "Onyx observe sealed reveal v1\nround: {round_id}\nattempt: {attempt}\nobserver: 0x{}\nkey: 0x{}\nciphertext: {ciphertext_hash}",
        hex::encode(observer),
        hex::encode(ephemeral)
    )
}

/// The symmetric key of a post from its shared point `S`.
pub fn post_key(
    shared: &Point,
    ephemeral: &Point,
    round_id: &Hash32,
    attempt: u32,
    observer: &MinerAddress,
) -> Hash32 {
    let mut b = KEY_TAG.to_vec();
    b.extend_from_slice(shared);
    b.extend_from_slice(ephemeral);
    b.extend_from_slice(&round_id.0);
    b.extend_from_slice(&attempt.to_be_bytes());
    b.extend_from_slice(observer);
    keccak256(&b)
}

/// `data` XOR the Keccak stream of `key` (its own inverse).
pub fn stream_xor(key: &Hash32, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for (i, chunk) in data.chunks(32).enumerate() {
        let mut b = STREAM_TAG.to_vec();
        b.extend_from_slice(&key.0);
        b.extend_from_slice(&u32::try_from(i).unwrap_or(u32::MAX).to_be_bytes());
        let ks = keccak256(&b);
        out.extend(chunk.iter().zip(ks.0.iter()).map(|(d, k)| d ^ k));
    }
    out
}

fn ecdh(s: &SecretKey, p: &PublicKey) -> Result<PublicKey, SealError> {
    p.mul_tweak(&secp(), &Scalar::from(*s))
        .map_err(|_| SealError::BadPoint)
}

/// The shared point `S = s·P` of a secret and a point (the observer's `r·K`,
/// the coordinator's `k·R`); used by the time-lock layer (`crate::timelock`).
pub fn shared_point(s: &[u8; 32], p: &Point) -> Result<Point, SealError> {
    Ok(to_point(&ecdh(&secret(s)?, &from_point(p)?)?))
}

/// Seal `observation` (with `salt`) for `round_id`/`attempt` of the feed
/// `spec` to `reveal_key`, with the ephemeral secret `ephemeral` (tests pass a
/// fixed one; observers use [`seal_random`]). Signs the reveal, proves the
/// ephemeral key and signs the post. The plaintext is padded with zeros to
/// [`sealed_len`].
#[allow(clippy::too_many_arguments)]
pub fn seal(
    observer_secret: &[u8; 32],
    ephemeral: &[u8; 32],
    reveal_key: &Point,
    round_id: &Hash32,
    attempt: u32,
    spec: &ObservationSpec,
    observation: &Observation,
    salt: &Hash32,
) -> Result<SealedPost, SealError> {
    let observer = secret_address(observer_secret).ok_or(SealError::BadSecret)?;
    let sig = sign_reveal(observer_secret, round_id, attempt, observation, salt)
        .ok_or(SealError::BadSecret)?;
    let plain = Plain {
        observation: observation.clone(),
        salt: *salt,
        sig,
    };
    let mut bytes = borsh::to_vec(&plain).map_err(|_| SealError::TooLong)?;
    let len = sealed_len(spec);
    if bytes.len() > len {
        return Err(SealError::TooLong);
    }
    bytes.resize(len, 0);
    seal_bytes(
        observer_secret,
        &observer,
        ephemeral,
        reveal_key,
        round_id,
        attempt,
        &bytes,
    )
}

/// Seal arbitrary bytes (what a griefing observer posts; tests).
pub fn seal_bytes(
    observer_secret: &[u8; 32],
    observer: &MinerAddress,
    ephemeral: &[u8; 32],
    reveal_key: &Point,
    round_id: &Hash32,
    attempt: u32,
    plain: &[u8],
) -> Result<SealedPost, SealError> {
    if plain.is_empty() || plain.len() > MAX_SEALED_BYTES {
        return Err(SealError::TooLong);
    }
    let r = secret(ephemeral)?;
    let eph = to_point(&r.public_key(&secp()));
    let shared = to_point(&ecdh(&r, &from_point(reveal_key)?)?);
    let ciphertext = stream_xor(&post_key(&shared, &eph, round_id, attempt, observer), plain);
    let key_proof = key_proof(ephemeral, round_id, attempt, observer)?;
    let sig = eip191_sign_with(
        observer_secret,
        sealed_reveal_message(round_id, attempt, observer, &eph, &keccak256(&ciphertext))
            .as_bytes(),
    )
    .ok_or(SealError::BadSecret)?;
    Ok(SealedPost {
        observer: *observer,
        ephemeral: eph,
        ciphertext,
        key_proof,
        sig,
    })
}

/// [`seal`] with a fresh ephemeral secret from the OS.
#[allow(clippy::too_many_arguments)]
pub fn seal_random(
    observer_secret: &[u8; 32],
    reveal_key: &Point,
    round_id: &Hash32,
    attempt: u32,
    spec: &ObservationSpec,
    observation: &Observation,
    salt: &Hash32,
) -> Result<SealedPost, SealError> {
    loop {
        let r = random_salt().map_err(|e| SealError::Random(e.to_string()))?;
        let ok = public_point(&r.0).is_ok_and(|p| x_below_order(&p));
        if ok {
            return seal(
                observer_secret,
                &r.0,
                reveal_key,
                round_id,
                attempt,
                spec,
                observation,
                salt,
            );
        }
    }
}

/// Whether the observer's signature and the key proof of `post` check (the
/// registry's `postSealedReveal`).
pub fn post_signed(post: &SealedPost, round_id: &Hash32, attempt: u32) -> bool {
    let msg = sealed_reveal_message(
        round_id,
        attempt,
        &post.observer,
        &post.ephemeral,
        &keccak256(&post.ciphertext),
    );
    eip191_recover(msg.as_bytes(), &post.sig) == Some(post.observer)
        && key_proof_valid(
            &post.ephemeral,
            round_id,
            attempt,
            &post.observer,
            &post.key_proof,
        )
}

/// Decrypt `post` with its shared point.
pub fn decrypt_with(shared: &Point, post: &SealedPost, round_id: &Hash32, attempt: u32) -> Vec<u8> {
    stream_xor(
        &post_key(shared, &post.ephemeral, round_id, attempt, &post.observer),
        &post.ciphertext,
    )
}

/// The reveal a plaintext holds, if it is a valid one for `observer` in `ctx`:
/// [`sealed_len`] bytes, Borsh of a [`Plain`] followed by zeros only, the
/// observer's reveal signature, opening its commit, passing the feed's checks.
/// Anything else is garbage.
pub fn valid_reveal(
    plain: &[u8],
    observer: &MinerAddress,
    ctx: &RevealContext<'_>,
) -> Option<SignedReveal> {
    if plain.len() != sealed_len(ctx.spec) {
        return None;
    }
    let mut rest = plain;
    let p = Plain::deserialize(&mut rest).ok()?;
    if rest.iter().any(|b| *b != 0) {
        return None;
    }
    let commit = ctx.commit?;
    if observation_commit(&ctx.round_id, observer, &p.observation, &p.salt) != commit
        || !reveal_signed(
            &ctx.round_id,
            ctx.attempt,
            observer,
            &p.observation,
            &p.salt,
            &p.sig,
        )
        || p.observation.check(ctx.spec).is_err()
    {
        return None;
    }
    Some((*observer, p.observation, p.salt, p.sig))
}

/// The coordinator opens a post with its reveal secret: the reveal, or a
/// dismissal with its proof. Refuses a post whose signatures do not check (the
/// registry never holds one).
pub fn open_post(
    reveal_secret: &[u8; 32],
    post: &SealedPost,
    ctx: &RevealContext<'_>,
) -> Result<Opened, SealError> {
    if !post_signed(post, &ctx.round_id, ctx.attempt) {
        return Err(SealError::BadPost);
    }
    open_checked(reveal_secret, post, ctx)
}

/// [`open_post`] for a post read from the registry's state (`sealedRevealOf`,
/// `sealedKeyProofOf`), which keeps no observer signature: the registry checked
/// it when it took the post (review C4-R1-3), so the coordinator opens what the
/// chain holds. The key proof IS re-checked here (review C4-R3-2): a dismissal publishes
/// `S = k·R`, so dismissing an `R` its poster does not hold (a node that lies
/// about chain state, or a post that never passed the registry) would be a
/// Diffie-Hellman oracle on the reveal key; such a post is `BadPost`, never opened.
pub fn open_posted(
    reveal_secret: &[u8; 32],
    observer: &MinerAddress,
    posted: &PostedSealed,
    ctx: &RevealContext<'_>,
) -> Result<Opened, SealError> {
    let post = SealedPost::from_posted(observer, posted);
    if !key_proof_valid(
        &post.ephemeral,
        &ctx.round_id,
        ctx.attempt,
        observer,
        &post.key_proof,
    ) {
        return Err(SealError::BadPost);
    }
    open_checked(reveal_secret, &post, ctx)
}

fn open_checked(
    reveal_secret: &[u8; 32],
    post: &SealedPost,
    ctx: &RevealContext<'_>,
) -> Result<Opened, SealError> {
    let k = secret(reveal_secret)?;
    let shared = to_point(&ecdh(&k, &from_point(&post.ephemeral)?)?);
    let plain = decrypt_with(&shared, post, &ctx.round_id, ctx.attempt);
    match valid_reveal(&plain, &post.observer, ctx) {
        Some(r) => Ok(Opened::Reveal(r)),
        None => Ok(Opened::Dismissed(dismiss(
            reveal_secret,
            post,
            &ctx.round_id,
            ctx.attempt,
        )?)),
    }
}

impl SealedPost {
    /// A post as the registry's state holds it (no observer signature; the
    /// registry checked it on the way in; the key proof is kept, C4-R3-2).
    pub fn from_posted(observer: &MinerAddress, posted: &PostedSealed) -> Self {
        SealedPost {
            observer: *observer,
            ephemeral: posted.ephemeral,
            ciphertext: posted.ciphertext.clone(),
            key_proof: posted.key_proof,
            sig: [0u8; 65],
        }
    }
}

fn scalar_of(mut h: Hash32) -> Scalar {
    // Below the group order with probability 1 - 2^-128 per try.
    loop {
        if let Ok(s) = Scalar::from_be_bytes(h.0) {
            if s != Scalar::ZERO {
                return s;
            }
        }
        h = keccak256(&h.0);
    }
}

#[allow(clippy::too_many_arguments)]
fn challenge(
    key: &Point,
    eph: &Point,
    shared: &Point,
    t1: &Point,
    t2: &Point,
    round_id: &Hash32,
    attempt: u32,
    observer: &MinerAddress,
) -> Scalar {
    let mut b = DLEQ_TAG.to_vec();
    for p in [key, eph, shared, t1, t2] {
        b.extend_from_slice(p);
    }
    b.extend_from_slice(&round_id.0);
    b.extend_from_slice(&attempt.to_be_bytes());
    b.extend_from_slice(observer);
    scalar_of(keccak256(&b))
}

/// The dismissal of `post`: `S` and the Chaum-Pedersen proof. The nonce is
/// derived from the secret and the post (deterministic, never reused for
/// another statement).
pub fn dismiss(
    reveal_secret: &[u8; 32],
    post: &SealedPost,
    round_id: &Hash32,
    attempt: u32,
) -> Result<Dismissal, SealError> {
    let ctx = secp();
    let x = secret(reveal_secret)?;
    let key = to_point(&x.public_key(&ctx));
    let r = from_point(&post.ephemeral)?;
    let shared_pk = ecdh(&x, &r)?;
    let shared = to_point(&shared_pk);
    let mut nb = DLEQ_NONCE_TAG.to_vec();
    nb.extend_from_slice(reveal_secret);
    nb.extend_from_slice(&post.ephemeral);
    nb.extend_from_slice(&round_id.0);
    nb.extend_from_slice(&attempt.to_be_bytes());
    nb.extend_from_slice(&post.observer);
    let t = SecretKey::from_slice(&scalar_of(keccak256(&nb)).to_be_bytes())
        .map_err(|_| SealError::BadSecret)?;
    let t1 = to_point(&t.public_key(&ctx));
    let t2 = to_point(&ecdh(&t, &r)?);
    let c = challenge(
        &key,
        &post.ephemeral,
        &shared,
        &t1,
        &t2,
        round_id,
        attempt,
        &post.observer,
    );
    let cx = x.mul_tweak(&c).map_err(|_| SealError::BadSecret)?;
    let z = t
        .add_tweak(&Scalar::from(cx))
        .map_err(|_| SealError::BadSecret)?;
    let mut proof = [0u8; 64];
    proof[..32].copy_from_slice(&c.to_be_bytes());
    proof[32..].copy_from_slice(&z.secret_bytes());
    Ok(Dismissal { shared, proof })
}

/// `a·P - c·Q`, `None` at infinity.
fn lin(a: &SecretKey, p: &PublicKey, c: &Scalar, q: &PublicKey) -> Option<PublicKey> {
    let ctx = secp();
    let ap = p.mul_tweak(&ctx, &Scalar::from(*a)).ok()?;
    let cq = q.mul_tweak(&ctx, c).ok()?.negate(&ctx);
    ap.combine(&cq).ok()
}

/// Whether `d` proves `S = k·R` for the reveal key `K = k·G` (the proof part only).
pub fn dismissal_proof_checks(
    reveal_key: &Point,
    post: &SealedPost,
    round_id: &Hash32,
    attempt: u32,
    d: &Dismissal,
) -> bool {
    let check = || -> Option<bool> {
        let k = from_point(reveal_key).ok()?;
        let r = from_point(&post.ephemeral).ok()?;
        let s = from_point(&d.shared).ok()?;
        let mut cb = [0u8; 32];
        cb.copy_from_slice(&d.proof[..32]);
        let c = Scalar::from_be_bytes(cb).ok()?;
        if c == Scalar::ZERO {
            return Some(false);
        }
        let z = SecretKey::from_slice(&d.proof[32..]).ok()?;
        let g = SecretKey::from_slice(&{
            let mut one = [0u8; 32];
            one[31] = 1;
            one
        })
        .ok()?
        .public_key(&secp());
        let t1 = to_point(&lin(&z, &g, &c, &k)?);
        let t2 = to_point(&lin(&z, &r, &c, &s)?);
        Some(
            challenge(
                reveal_key,
                &post.ephemeral,
                &d.shared,
                &t1,
                &t2,
                round_id,
                attempt,
                &post.observer,
            ) == c,
        )
    };
    check().unwrap_or(false)
}

/// A client's check of a dismissed post: the post is signed, the proof checks,
/// and the plaintext it opens is NOT a valid reveal (else the dismissal is a
/// false one and the round is refused).
pub fn check_dismissal(
    reveal_key: &Point,
    post: &SealedPost,
    ctx: &RevealContext<'_>,
    d: &Dismissal,
) -> Result<Vec<u8>, SealError> {
    if !post_signed(post, &ctx.round_id, ctx.attempt) {
        return Err(SealError::BadPost);
    }
    check_dismissal_unsigned(reveal_key, post, ctx, d)
}

/// [`check_dismissal`] for a post read from the registry's state at the round's
/// pinned block (`sealedRevealOf`; the registry checked its signatures).
pub fn check_dismissal_posted(
    reveal_key: &Point,
    observer: &MinerAddress,
    posted: &PostedSealed,
    ctx: &RevealContext<'_>,
    d: &Dismissal,
) -> Result<Vec<u8>, SealError> {
    let post = SealedPost::from_posted(observer, posted);
    if !key_proof_valid(
        &post.ephemeral,
        &ctx.round_id,
        ctx.attempt,
        observer,
        &post.key_proof,
    ) {
        return Err(SealError::BadPost);
    }
    check_dismissal_unsigned(reveal_key, &post, ctx, d)
}

fn check_dismissal_unsigned(
    reveal_key: &Point,
    post: &SealedPost,
    ctx: &RevealContext<'_>,
    d: &Dismissal,
) -> Result<Vec<u8>, SealError> {
    if !dismissal_proof_checks(reveal_key, post, &ctx.round_id, ctx.attempt, d) {
        return Err(SealError::BadProof);
    }
    let plain = decrypt_with(&d.shared, post, &ctx.round_id, ctx.attempt);
    if valid_reveal(&plain, &post.observer, ctx).is_some() {
        return Err(SealError::FalseDismissal);
    }
    Ok(plain)
}

/// One drawn observer's status in a round (`revealLogComplete`'s status byte).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryStatus {
    /// No reveal served.
    NotRevealed = 0,
    /// Reveal served (or, sealed, listed as revealed).
    Revealed = 1,
    /// Its sealed post is garbage; the transcript carries the dismissal.
    Dismissed = 2,
}

/// One drawn observer as `revealLogComplete` reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SealedEntry {
    /// The drawn observer.
    pub observer: MinerAddress,
    /// Its served commit, zero when none.
    pub commit: Hash32,
    /// Its status.
    pub status: EntryStatus,
}

/// The arguments of `revealLogComplete`, `recordAttempt` and `attemptMatches`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevealLogQuery {
    /// The on-chain fact key.
    pub registry_fact: Hash32,
    /// The draw attempt.
    pub attempt: u32,
    /// The round id.
    pub round_id: Hash32,
    /// The window start.
    pub commit_open: u64,
    /// `commit_close`.
    pub commit_close: u64,
    /// `reveal_close`.
    pub reveal_close: u64,
    /// Every drawn observer, ascending.
    pub entries: Vec<SealedEntry>,
}

impl RevealLogQuery {
    /// `entries` packed: 20-byte observer, 32-byte commit, 1-byte status.
    pub fn packed_entries(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(self.entries.len() * 53);
        for e in &self.entries {
            b.extend_from_slice(&e.observer);
            b.extend_from_slice(&e.commit.0);
            b.push(e.status as u8);
        }
        b
    }

    /// `attemptDigest` of this round with `result`.
    pub fn digest(&self, result: &Hash32) -> Hash32 {
        attempt_digest(
            &self.round_id,
            self.commit_open,
            self.commit_close,
            self.reveal_close,
            result,
            &self.packed_entries(),
        )
    }

    /// Calldata of `revealLogComplete(fact, attempt, round, commitOpen,
    /// commitClose, revealClose, entries)`.
    pub fn calldata(&self) -> Vec<u8> {
        let mut d =
            keccak256(b"revealLogComplete(bytes32,uint32,bytes32,uint64,uint64,uint64,bytes)").0
                [..4]
                .to_vec();
        d.extend_from_slice(&self.registry_fact.0);
        d.extend_from_slice(&word(u64::from(self.attempt)));
        d.extend_from_slice(&self.round_id.0);
        d.extend_from_slice(&word(self.commit_open));
        d.extend_from_slice(&word(self.commit_close));
        d.extend_from_slice(&word(self.reveal_close));
        d.extend_from_slice(&word(7 * 32));
        push_bytes(&mut d, &self.packed_entries());
        d
    }

    /// Calldata of `attemptMatches(fact, attempt, round, commitOpen,
    /// commitClose, revealClose, result, entries)`.
    pub fn matches_calldata(&self, result: &Hash32) -> Vec<u8> {
        let mut d =
            keccak256(b"attemptMatches(bytes32,uint32,bytes32,uint64,uint64,uint64,bytes32,bytes)")
                .0[..4]
                .to_vec();
        d.extend_from_slice(&self.registry_fact.0);
        d.extend_from_slice(&word(u64::from(self.attempt)));
        d.extend_from_slice(&self.round_id.0);
        d.extend_from_slice(&word(self.commit_open));
        d.extend_from_slice(&word(self.commit_close));
        d.extend_from_slice(&word(self.reveal_close));
        d.extend_from_slice(&result.0);
        d.extend_from_slice(&word(8 * 32));
        push_bytes(&mut d, &self.packed_entries());
        d
    }
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

/// `keccak256("onyx-attempt-record-v1" ‖ round ‖ commitOpen ‖ commitClose ‖
/// revealClose ‖ result ‖ keccak256(entries))`, the times as 8-byte big endian
/// (`OnyxRegistry.attemptDigest`).
pub fn attempt_digest(
    round_id: &Hash32,
    commit_open: u64,
    commit_close: u64,
    reveal_close: u64,
    result: &Hash32,
    packed_entries: &[u8],
) -> Hash32 {
    let mut b = RECORD_TAG.to_vec();
    b.extend_from_slice(&round_id.0);
    b.extend_from_slice(&commit_open.to_be_bytes());
    b.extend_from_slice(&commit_close.to_be_bytes());
    b.extend_from_slice(&reveal_close.to_be_bytes());
    b.extend_from_slice(&result.0);
    b.extend_from_slice(&keccak256(packed_entries).0);
    keccak256(&b)
}

// ------------------------------------------------------------ wiring (ROADMAP C11)

/// The `revealLogComplete` / `attemptMatches` query of one round as it is served
/// (ADR-0027 amendment 10 items 6 and 7, ROADMAP C11): EVERY drawn observer of
/// `observers` (the draw, ascending), its served commit (zero when none) and its
/// status: [`EntryStatus::Revealed`] when its reveal is served (or, for a round
/// served sealed, listed as revealed), [`EntryStatus::Dismissed`] when the round
/// carries its dismissal, else [`EntryStatus::NotRevealed`]. The window is the
/// round's own config: `commit_open` the window start, `commit_close` and
/// `reveal_close` from `cfg`; with the round id the caller recomputed (review
/// C4-R1-2: the chain knows none of the drawn set, the window or the round id,
/// so whoever builds this query from what a server says, rather than from the
/// draw and the config, lets a record leave an observer out).
#[allow(clippy::too_many_arguments)]
pub fn reveal_log_query(
    registry_fact: Hash32,
    spec: &ObservationSpec,
    cfg: &crate::observe::ObserveConfig,
    attempt: u32,
    round_id: Hash32,
    observers: &[crate::observe::Observer],
    commits: &[(MinerAddress, Hash32, ObserverSig)],
    revealed: &std::collections::BTreeSet<MinerAddress>,
    dismissed: &std::collections::BTreeSet<MinerAddress>,
) -> RevealLogQuery {
    let served: BTreeMap<MinerAddress, Hash32> = commits.iter().map(|(a, h, _)| (*a, *h)).collect();
    RevealLogQuery {
        registry_fact,
        attempt,
        round_id,
        commit_open: spec.window_start,
        commit_close: cfg.commit_close,
        reveal_close: cfg.reveal_close,
        entries: observers
            .iter()
            .map(|o| SealedEntry {
                observer: o.address,
                commit: served.get(&o.address).copied().unwrap_or(Hash32::ZERO),
                status: if revealed.contains(&o.address) {
                    EntryStatus::Revealed
                } else if dismissed.contains(&o.address) {
                    EntryStatus::Dismissed
                } else {
                    EntryStatus::NotRevealed
                },
            })
            .collect(),
    }
}

/// What the coordinator read of a round's sealed posts, ONCE, from one block
/// past `reveal_close` (ROADMAP C11; the single-read rule of amendment 8 item 4b,
/// now for sealed posts), and what it made of them: kept in the round and saved
/// with the round book, so a retry or a later run never reads the posts again
/// and the book keeps every dismissal.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SealedSnapshot {
    /// The block every slot was read at: at or below the chain's finalized
    /// block, its time past `reveal_close` (no post can still land in the
    /// window, and no reorg can add one).
    pub block: crate::observe::LogBlock,
    /// The draw's reveal key (`drawRevealKey`), the public point of the
    /// coordinator's reveal key.
    pub reveal_key: Point,
    /// Every drawn observer's filled slot at `block` (`sealedRevealOf` with its
    /// `sealedKeyProofOf`), ascending, in the reveal phase or not.
    pub posts: Vec<(MinerAddress, PostedSealed)>,
    /// The in-phase posts that are garbage, each with its proof, ascending.
    pub dismissals: Vec<(MinerAddress, Dismissal)>,
    /// For a time-locked draw (`drawTimeLocked`, ROADMAP C51): the lock round,
    /// its verified beacon signature and the dismissals' time-lock openings;
    /// `None` for a draw that is not time-locked (book version 6 and before).
    pub timelock: Option<crate::timelock::TimeLockRead>,
}

/// A [`SealedSnapshot`] as book version 6 saved it: no time-lock.
#[derive(BorshDeserialize)]
pub(crate) struct SealedSnapshotV6 {
    block: crate::observe::LogBlock,
    reveal_key: Point,
    posts: Vec<(MinerAddress, PostedSealed)>,
    dismissals: Vec<(MinerAddress, Dismissal)>,
}

impl From<SealedSnapshotV6> for SealedSnapshot {
    fn from(w: SealedSnapshotV6) -> Self {
        SealedSnapshot {
            block: w.block,
            reveal_key: w.reveal_key,
            posts: w.posts,
            dismissals: w.dismissals,
            timelock: None,
        }
    }
}

/// A [`SealedSnapshot`] as book version 7 saved it (ROADMAP C51): its
/// time-lock read has no far round. Only a snapshot WITHOUT a time-lock is
/// taken back ([`Self::upgrade`]): a version-7 time-locked round's posts carry
/// no far-date copy (ROADMAP C52), a format never deployed, so it is refused
/// rather than checked under rules it was not made for.
#[derive(BorshDeserialize)]
pub(crate) struct SealedSnapshotV7 {
    block: crate::observe::LogBlock,
    reveal_key: Point,
    posts: Vec<(MinerAddress, PostedSealed)>,
    dismissals: Vec<(MinerAddress, Dismissal)>,
    timelock: Option<crate::timelock::TimeLockReadV7>,
}

impl SealedSnapshotV7 {
    /// The snapshot in today's layout; `None` for a time-locked one.
    pub(crate) fn upgrade(self) -> Option<SealedSnapshot> {
        if self.timelock.is_some() {
            return None;
        }
        Some(SealedSnapshot {
            block: self.block,
            reveal_key: self.reveal_key,
            posts: self.posts,
            dismissals: self.dismissals,
            timelock: None,
        })
    }
}

impl SealedSnapshot {
    /// The posts with a block time in (`commit_close`, `reveal_close`]: the ones
    /// the round must account for (`revealLogComplete`); the others neither need
    /// a reveal nor can be dismissed (C4-5).
    pub fn in_phase(
        &self,
        commit_close: u64,
        reveal_close: u64,
    ) -> impl Iterator<Item = &(MinerAddress, PostedSealed)> {
        self.posts
            .iter()
            .filter(move |(_, p)| p.posted_at > commit_close && p.posted_at <= reveal_close)
    }
}

fn selector(sig: &[u8]) -> Vec<u8> {
    keccak256(sig).0[..4].to_vec()
}

fn word_addr(a: &MinerAddress) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a);
    w
}

/// Calldata of the public getter `OnyxRegistry.drawTimeLocked(fact, attempt)`
/// (ADR-0027 amendment 14, ROADMAP C51): whether the draw was announced with
/// time-locks required; its return is one ABI bool ([`bool_from_return`]).
pub fn draw_time_locked_calldata(registry_fact: &Hash32, attempt: u32) -> Vec<u8> {
    let mut d = selector(b"drawTimeLocked(bytes32,uint32)");
    d.extend_from_slice(&registry_fact.0);
    d.extend_from_slice(&word(u64::from(attempt)));
    d
}

/// Calldata of the public getter `OnyxRegistry.drawRevealKey(fact, attempt)`:
/// the reveal key the draw snapshotted when it was announced (zero: none, the
/// draw takes no sealed post).
pub fn draw_reveal_key_calldata(registry_fact: &Hash32, attempt: u32) -> Vec<u8> {
    let mut d = selector(b"drawRevealKey(bytes32,uint32)");
    d.extend_from_slice(&registry_fact.0);
    d.extend_from_slice(&word(u64::from(attempt)));
    d
}

fn slot_calldata(
    sig: &[u8],
    registry_fact: &Hash32,
    attempt: u32,
    observer: &MinerAddress,
    round_id: &Hash32,
) -> Vec<u8> {
    let mut d = selector(sig);
    d.extend_from_slice(&registry_fact.0);
    d.extend_from_slice(&word(u64::from(attempt)));
    d.extend_from_slice(&word_addr(observer));
    d.extend_from_slice(&round_id.0);
    d
}

/// Calldata of `OnyxRegistry.sealedRevealOf(fact, attempt, observer, round)`.
pub fn sealed_reveal_of_calldata(
    registry_fact: &Hash32,
    attempt: u32,
    observer: &MinerAddress,
    round_id: &Hash32,
) -> Vec<u8> {
    slot_calldata(
        b"sealedRevealOf(bytes32,uint32,address,bytes32)",
        registry_fact,
        attempt,
        observer,
        round_id,
    )
}

/// Calldata of `OnyxRegistry.sealedKeyProofOf(fact, attempt, observer, round)`.
pub fn sealed_key_proof_of_calldata(
    registry_fact: &Hash32,
    attempt: u32,
    observer: &MinerAddress,
    round_id: &Hash32,
) -> Vec<u8> {
    slot_calldata(
        b"sealedKeyProofOf(bytes32,uint32,address,bytes32)",
        registry_fact,
        attempt,
        observer,
        round_id,
    )
}

/// The reveal key `drawRevealKey` returned (two ABI words `x`, `y`); `None`
/// when the draw has none (both zero). Anything else malformed is an error.
pub fn reveal_key_from_return(ret: &[u8]) -> Result<Option<Point>, String> {
    let p: Point = ret
        .try_into()
        .map_err(|_| "drawRevealKey return: not two words".to_string())?;
    if p == [0u8; 64] {
        return Ok(None);
    }
    if from_point(&p).is_err() {
        return Err("drawRevealKey return: not a point of secp256k1".into());
    }
    Ok(Some(p))
}

/// The post `sealedRevealOf` and `sealedKeyProofOf` returned, exactly as the
/// registry ABI-encodes them (`(bytes32 ex, bytes32 ey, uint64 postedAt, bytes
/// ct)` and `bytes` of 52); `None` for an empty slot (all zero, no ciphertext,
/// a zero proof). Strict: any other encoding, a ciphertext over
/// [`MAX_SEALED_BYTES`] or a half-empty slot is an error, never a post.
pub fn posted_from_returns(
    reveal_ret: &[u8],
    proof_ret: &[u8],
) -> Result<Option<PostedSealed>, String> {
    let bad = |what: &str| format!("sealedRevealOf/sealedKeyProofOf return: {what}");
    let small = |w: &[u8]| -> Option<u64> {
        (w.len() == 32 && w[..24].iter().all(|b| *b == 0))
            .then(|| u64::from_be_bytes(w[24..].try_into().unwrap_or([0; 8])))
    };
    let padded = |n: usize| n.div_ceil(32) * 32;
    if reveal_ret.len() < 160 || small(&reveal_ret[96..128]) != Some(128) {
        return Err(bad("not (bytes32, bytes32, uint64, bytes)"));
    }
    let posted_at = small(&reveal_ret[64..96]).ok_or_else(|| bad("postedAt is not a uint64"))?;
    let len = small(&reveal_ret[128..160])
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| bad("ciphertext length"))?;
    if len > MAX_SEALED_BYTES
        || reveal_ret.len() != 160 + padded(len)
        || reveal_ret[160 + len..].iter().any(|b| *b != 0)
    {
        return Err(bad("ciphertext encoding"));
    }
    if proof_ret.len() != 128
        || small(&proof_ret[..32]) != Some(32)
        || small(&proof_ret[32..64]) != Some(52)
        || proof_ret[64 + 52..].iter().any(|b| *b != 0)
    {
        return Err(bad("key proof encoding"));
    }
    let mut ephemeral = [0u8; 64];
    ephemeral.copy_from_slice(&reveal_ret[..64]);
    let mut key_proof = [0u8; 52];
    key_proof.copy_from_slice(&proof_ret[64..116]);
    let ciphertext = reveal_ret[160..160 + len].to_vec();
    if posted_at == 0 {
        if ephemeral != [0u8; 64] || !ciphertext.is_empty() || key_proof != [0u8; 52] {
            return Err(bad("an empty slot with a key, a ciphertext or a proof"));
        }
        return Ok(None);
    }
    if ciphertext.is_empty() {
        return Err(bad("a post without a ciphertext"));
    }
    Ok(Some(PostedSealed {
        ephemeral,
        key_proof,
        posted_at,
        ciphertext,
    }))
}

/// One ABI `bool` word (`revealLogComplete`, `attemptMatches`).
pub fn bool_from_return(ret: &[u8], what: &str) -> Result<bool, String> {
    let w: [u8; 32] = ret
        .try_into()
        .map_err(|_| format!("{what} return: not one word"))?;
    if w[..31].iter().any(|b| *b != 0) || w[31] > 1 {
        return Err(format!("{what} return: not a bool"));
    }
    Ok(w[31] == 1)
}

/// The ABI returns of `sealedRevealOf` and `sealedKeyProofOf` for `posted`
/// (`None`: an empty slot): what the registry answers, for tests and test
/// chains.
pub fn posted_returns(posted: Option<&PostedSealed>) -> (Vec<u8>, Vec<u8>) {
    let empty = PostedSealed {
        ephemeral: [0u8; 64],
        key_proof: [0u8; 52],
        posted_at: 0,
        ciphertext: vec![],
    };
    let p = posted.unwrap_or(&empty);
    let mut r = p.ephemeral.to_vec();
    r.extend_from_slice(&word(p.posted_at));
    r.extend_from_slice(&word(128));
    push_bytes(&mut r, &p.ciphertext);
    let mut k = word(32).to_vec();
    push_bytes(&mut k, &p.key_proof);
    (r, k)
}

/// A sealed post as the registry holds it (`sealedRevealOf`, `sealedKeyProofOf`).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct PostedSealed {
    /// The ephemeral key.
    pub ephemeral: Point,
    /// The key proof the registry checked (`sealedKeyProofOf`), re-checked before
    /// any dismissal (review C4-R3-2).
    pub key_proof: KeyProof,
    /// Its block time.
    pub posted_at: u64,
    /// The ciphertext.
    pub ciphertext: Vec<u8>,
}

/// The registry's sealed-reveal rules as Rust simulates them, on top of the
/// commit and receipt log ([`SimulatedTranscriptLog`]). Same keys, same answers.
/// One difference (review C4-R1-7): the simulation has no beacons, so "the draw's
/// beacon is fixed" is modelled as "the draw is announced" (`log.signers`).
#[derive(Clone, Debug, Default)]
pub struct SimulatedSealedLog {
    /// Commits and receipts; its `signers` stand for announced draws.
    pub log: SimulatedTranscriptLog,
    /// `(fact, attempt)` -> the draw's reveal key.
    pub keys: BTreeMap<(Hash32, u32), Point>,
    /// `timeLockRequired` (ADR-0027 amendment 14, ROADMAP C51): once true the
    /// announcer snapshots every draw into [`Self::time_locked`] and refuses one
    /// without a reveal key; records of other draws are refused.
    pub time_lock_required: bool,
    /// `drawTimeLocked`: the draws announced with the mode on.
    pub time_locked: std::collections::BTreeSet<(Hash32, u32)>,
    posts: BTreeMap<Hash32, PostedSealed>,
    records: BTreeMap<(Hash32, u32), Hash32>,
}

impl SimulatedSealedLog {
    /// `postSealedReveal`, landing at block time `now`.
    pub fn post_sealed(
        &mut self,
        fact: &Hash32,
        attempt: u32,
        round_id: &Hash32,
        post: &SealedPost,
        now: u64,
    ) -> Result<(), &'static str> {
        let log = std::mem::take(&mut self.log);
        let r = self.post_sealed_with(&log, fact, attempt, round_id, post, now);
        self.log = log;
        r
    }

    /// [`Self::post_sealed`] on the commit and receipt log `log` (a registry
    /// that keeps its own, `observe::SimulatedRegistry`).
    pub fn post_sealed_with(
        &mut self,
        log: &SimulatedTranscriptLog,
        fact: &Hash32,
        attempt: u32,
        round_id: &Hash32,
        post: &SealedPost,
        now: u64,
    ) -> Result<(), &'static str> {
        if *round_id == Hash32::ZERO || post.observer == [0u8; 20] {
            return Err("ZeroValue");
        }
        if !log.signers.contains_key(&(*fact, attempt)) {
            return Err("NoDraw");
        }
        if !self.keys.contains_key(&(*fact, attempt)) {
            return Err("NoRevealKey");
        }
        if post.ciphertext.is_empty() || post.ciphertext.len() > MAX_SEALED_BYTES {
            return Err("BadCiphertext");
        }
        if post.ciphertext.len() <= TIME_LOCKED_HEAD_BYTES
            && self.time_locked.contains(&(*fact, attempt))
        {
            return Err("BadCiphertext");
        }
        let msg = sealed_reveal_message(
            round_id,
            attempt,
            &post.observer,
            &post.ephemeral,
            &keccak256(&post.ciphertext),
        );
        if eip191_recover(msg.as_bytes(), &post.sig) != Some(post.observer) {
            return Err("BadSignature");
        }
        if !key_proof_valid(
            &post.ephemeral,
            round_id,
            attempt,
            &post.observer,
            &post.key_proof,
        ) {
            return Err("BadKeyProof");
        }
        let k = transcript_log_key(fact, attempt, &post.observer, round_id);
        if self.posts.contains_key(&k) {
            return Err("AlreadyPosted");
        }
        self.posts.insert(
            k,
            PostedSealed {
                ephemeral: post.ephemeral,
                key_proof: post.key_proof,
                posted_at: now,
                ciphertext: post.ciphertext.clone(),
            },
        );
        Ok(())
    }

    /// `sealedRevealOf`.
    pub fn sealed_post(
        &self,
        fact: &Hash32,
        attempt: u32,
        observer: &MinerAddress,
        round_id: &Hash32,
    ) -> Option<&PostedSealed> {
        self.posts
            .get(&transcript_log_key(fact, attempt, observer, round_id))
    }

    /// `revealLogComplete`.
    pub fn complete(&self, q: &RevealLogQuery) -> bool {
        self.complete_with(&self.log, q)
    }

    /// [`Self::complete`] on the commit and receipt log `log`.
    pub fn complete_with(&self, log: &SimulatedTranscriptLog, q: &RevealLogQuery) -> bool {
        if !log.signers.contains_key(&(q.registry_fact, q.attempt))
            || q.commit_open > q.commit_close
            || q.commit_close >= q.reveal_close
        {
            return false;
        }
        // Strictly ascending observers (review C4-R1-2), as the registry requires.
        if !q.entries.windows(2).all(|w| w[0].observer < w[1].observer)
            || q.entries.first().is_some_and(|e| e.observer == [0u8; 20])
        {
            return false;
        }
        q.entries.iter().all(|e| {
            let posted = log.posted(&q.registry_fact, q.attempt, &e.observer, &q.round_id);
            let commit_ok = match posted {
                Some(p) if (q.commit_open..=q.commit_close).contains(&p.posted_at) => {
                    p.commit == e.commit
                }
                _ => true,
            };
            let receipt_ok = log
                .receipt(&q.registry_fact, q.attempt, &e.observer, &q.round_id)
                .is_none_or(|c| c == e.commit && e.status == EntryStatus::Revealed);
            let in_phase = self
                .sealed_post(&q.registry_fact, q.attempt, &e.observer, &q.round_id)
                .is_some_and(|p| p.posted_at > q.commit_close && p.posted_at <= q.reveal_close);
            let sealed_ok = match e.status {
                EntryStatus::NotRevealed => !in_phase,
                EntryStatus::Revealed => true,
                EntryStatus::Dismissed => in_phase,
            };
            commit_ok && receipt_ok && sealed_ok
        })
    }

    /// `recordAttempt` at block time `now`.
    pub fn record_attempt(
        &mut self,
        q: &RevealLogQuery,
        result: &Hash32,
        now: u64,
    ) -> Result<(), &'static str> {
        let log = std::mem::take(&mut self.log);
        let r = self.record_attempt_with(&log, q, result, now);
        self.log = log;
        r
    }

    /// [`Self::record_attempt`] on the commit and receipt log `log`.
    pub fn record_attempt_with(
        &mut self,
        log: &SimulatedTranscriptLog,
        q: &RevealLogQuery,
        result: &Hash32,
        now: u64,
    ) -> Result<(), &'static str> {
        if q.round_id == Hash32::ZERO || *result == Hash32::ZERO {
            return Err("ZeroValue");
        }
        if !log.signers.contains_key(&(q.registry_fact, q.attempt)) {
            return Err("NoDraw");
        }
        if self.records.contains_key(&(q.registry_fact, q.attempt)) {
            return Err("AlreadyRecorded");
        }
        if self.time_lock_required && !self.time_locked.contains(&(q.registry_fact, q.attempt)) {
            return Err("NotTimeLocked");
        }
        if q.commit_open > q.commit_close || q.commit_close >= q.reveal_close {
            return Err("BadWindow");
        }
        if now <= q.reveal_close {
            return Err("TooEarly");
        }
        if !self.complete_with(log, q) {
            return Err("IncompleteTranscript");
        }
        self.records
            .insert((q.registry_fact, q.attempt), q.digest(result));
        Ok(())
    }

    /// `attemptRecord(fact, attempt)`: the recorded digest, if any.
    pub fn attempt_record(&self, fact: &Hash32, attempt: u32) -> Option<Hash32> {
        self.records.get(&(*fact, attempt)).copied()
    }

    /// `attemptMatches`.
    pub fn attempt_matches(&self, q: &RevealLogQuery, result: &Hash32) -> bool {
        self.records
            .get(&(q.registry_fact, q.attempt))
            .is_some_and(|d| *d == q.digest(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(i: u8) -> (SealedPost, Hash32) {
        let round = keccak256(&[b'q', i]);
        let obs_secret = keccak256(&[b'o', i]).0;
        let observer = secret_address(&obs_secret).expect("key");
        let key = public_point(&keccak256(&[b'k', i]).0).expect("key");
        let p = seal_bytes(
            &obs_secret,
            &observer,
            &keccak256(&[b'e', i]).0,
            &key,
            &round,
            u32::from(i),
            &[i; 40],
        )
        .expect("seal");
        (p, round)
    }

    /// ROADMAP C11: the registry's ABI returns of a slot read back exactly
    /// (`sealedRevealOf`, `sealedKeyProofOf`, `drawRevealKey`), and anything
    /// else is an error, never a post: trailing bytes, padding that is not
    /// zero, another offset, a ciphertext over the cap, a half-empty slot, a
    /// key that is not a point.
    #[test]
    fn c11_abi_returns_round_trip_and_are_strict() {
        let (p, _) = post(5);
        let posted = PostedSealed {
            ephemeral: p.ephemeral,
            key_proof: p.key_proof,
            posted_at: 1_234,
            ciphertext: p.ciphertext.clone(),
        };
        let (r, k) = posted_returns(Some(&posted));
        assert_eq!(posted_from_returns(&r, &k), Ok(Some(posted.clone())));
        let (r0, k0) = posted_returns(None);
        assert_eq!(posted_from_returns(&r0, &k0), Ok(None));
        let bad = |r: &[u8], k: &[u8]| posted_from_returns(r, k).is_err();
        let mut x = r.clone();
        x.push(0);
        assert!(bad(&x, &k), "trailing byte");
        let mut x = r.clone();
        let last = x.len() - 1;
        x[last] = 1;
        assert!(bad(&x, &k), "non-zero padding (40-byte ciphertext)");
        let mut x = r.clone();
        x[127] = 0x60;
        assert!(bad(&x, &k), "another offset");
        let mut x = r.clone();
        x[95] ^= 0;
        x[64] = 1;
        assert!(bad(&x, &k), "postedAt over 64 bits");
        let mut x = k.clone();
        x[127] = 1;
        assert!(bad(&r, &x), "key proof padding");
        let mut x = k.clone();
        x[63] = 51;
        assert!(bad(&r, &x), "key proof length");
        // A half-empty slot: no time but a key, or a time and no ciphertext.
        let mut x = r0.clone();
        x[0] = 1;
        assert!(bad(&x, &k0));
        let empty_ct = PostedSealed {
            ciphertext: vec![],
            ..posted.clone()
        };
        let (x, y) = posted_returns(Some(&empty_ct));
        assert!(bad(&x, &y));
        let long = PostedSealed {
            ciphertext: vec![1; MAX_SEALED_BYTES + 1],
            ..posted
        };
        let (x, y) = posted_returns(Some(&long));
        assert!(bad(&x, &y), "over the registry's cap");
        // drawRevealKey.
        let key = public_point(&keccak256(b"k").0).expect("point");
        assert_eq!(reveal_key_from_return(&key), Ok(Some(key)));
        assert_eq!(reveal_key_from_return(&[0u8; 64]), Ok(None));
        let mut off = key;
        off[63] ^= 1;
        assert!(reveal_key_from_return(&off).is_err(), "not on the curve");
        assert!(reveal_key_from_return(&key[..63]).is_err());
        assert_eq!(bool_from_return(&[0u8; 32], "x"), Ok(false));
        let mut two = [0u8; 32];
        two[31] = 2;
        assert!(bool_from_return(&two, "x").is_err());
    }

    /// Over 64 keys and posts: an honest dismissal checks and opens the post's
    /// plaintext; the same proof for another round, attempt, observer or key
    /// does not (Fiat-Shamir binds the statement and its context).
    #[test]
    fn dismissal_proofs_check_and_bind_their_context() {
        for i in 0..64u8 {
            let (p, round) = post(i);
            let a = u32::from(i);
            let sk = keccak256(&[b'k', i]).0;
            let key = public_point(&sk).expect("key");
            let d = dismiss(&sk, &p, &round, a).expect("dismiss");
            assert!(dismissal_proof_checks(&key, &p, &round, a, &d), "seed {i}");
            assert_eq!(decrypt_with(&d.shared, &p, &round, a), vec![i; 40]);
            assert!(!dismissal_proof_checks(&key, &p, &round, a + 1, &d));
            assert!(!dismissal_proof_checks(&key, &p, &keccak256(b"x"), a, &d));
            let mut other = p.clone();
            other.observer[0] ^= 1;
            assert!(!dismissal_proof_checks(&key, &other, &round, a, &d));
            let wrong = public_point(&keccak256(&[b'z', i]).0).expect("key");
            assert!(!dismissal_proof_checks(&wrong, &p, &round, a, &d));
        }
    }

    /// The stream is its own inverse, differs per key, and a ciphertext shows
    /// no plaintext byte pattern (all-zero plaintext is not all-zero out).
    #[test]
    fn the_stream_round_trips_and_depends_on_the_key() {
        let k1 = keccak256(b"k1");
        let k2 = keccak256(b"k2");
        let data: Vec<u8> = (0..100u8).collect();
        assert_eq!(stream_xor(&k1, &stream_xor(&k1, &data)), data);
        assert_ne!(stream_xor(&k1, &data), stream_xor(&k2, &data));
        assert!(stream_xor(&k1, &[0u8; 64]).iter().any(|b| *b != 0));
    }

    /// The ECDH of both sides agrees (`r·K = k·R`), and a post's key depends on
    /// round, attempt and observer.
    #[test]
    fn both_sides_derive_the_same_key_and_it_binds_the_context() {
        let (p, round) = post(3);
        let sk = keccak256(&[b'k', 3]).0;
        let d = dismiss(&sk, &p, &round, 3).expect("dismiss");
        let r = secret(&keccak256(&[b'e', 3]).0).expect("r");
        let k = from_point(&public_point(&sk).expect("key")).expect("point");
        assert_eq!(to_point(&ecdh(&r, &k).expect("ecdh")), d.shared);
        let base = post_key(&d.shared, &p.ephemeral, &round, 3, &p.observer);
        assert_ne!(
            base,
            post_key(&d.shared, &p.ephemeral, &round, 4, &p.observer)
        );
        assert_ne!(
            base,
            post_key(&d.shared, &p.ephemeral, &keccak256(b"r"), 3, &p.observer)
        );
        assert_ne!(base, post_key(&d.shared, &p.ephemeral, &round, 3, &[9; 20]));
    }

    /// Review C4-R3-1: edge cases. A key proof for an off-curve `R` (the real
    /// point's x with another y of the same parity, the proof made honestly
    /// with the real secret) is refused, and such a post could be neither opened
    /// nor dismissed (`BadPoint`): the reason the registry checks `R` on the
    /// curve. A dismissal with `S` off the curve, `c` of 0 or at least n, `z` of
    /// 0, or `S` at the identity encoding does not check.
    #[test]
    fn key_and_dismissal_proof_edge_cases() {
        let (p, round) = post(5);
        let sk = keccak256(&[b'k', 5]).0;
        let key = public_point(&sk).expect("key");
        let r = keccak256(&[b'e', 5]).0;
        assert!(key_proof_valid(
            &p.ephemeral,
            &round,
            5,
            &p.observer,
            &p.key_proof
        ));
        // Off-curve R: y + 2 keeps the parity; only y and p - y are on the curve.
        let mut off = p.ephemeral;
        let mut carry = 2u16;
        for b in off[32..].iter_mut().rev() {
            let v = u16::from(*b) + carry;
            *b = u8::try_from(v & 0xFF).unwrap_or(0);
            carry = v >> 8;
        }
        assert_eq!(from_point(&off), Err(SealError::BadPoint));
        // An honest proof over the off-curve encoding: challenge over `off`, z with r.
        let ctx = secp();
        let t = secret(&keccak256(b"nonce").0).expect("t");
        let t_addr = point_address(&to_point(&t.public_key(&ctx)));
        let c = key_proof_challenge(&off, &t_addr, &round, 5, &p.observer).expect("c");
        let z = t
            .add_tweak(&Scalar::from(
                secret(&r).expect("r").mul_tweak(&c).expect("cr"),
            ))
            .expect("z");
        let mut pf = [0u8; 52];
        pf[..20].copy_from_slice(&t_addr);
        pf[20..].copy_from_slice(&z.secret_bytes());
        assert!(!key_proof_valid(&off, &round, 5, &p.observer, &pf));
        let mut off_post = p.clone();
        off_post.ephemeral = off;
        // `open_checked` derives `S` with the same `from_point` first; `dismiss` too.
        assert_eq!(
            dismiss(&sk, &off_post, &round, 5),
            Err(SealError::BadPoint),
            "an off-curve post can be neither opened nor dismissed"
        );
        // Zero z or zero T in a key proof.
        let mut zero_z = p.key_proof;
        zero_z[20..].copy_from_slice(&[0u8; 32]);
        assert!(!key_proof_valid(
            &p.ephemeral,
            &round,
            5,
            &p.observer,
            &zero_z
        ));
        let mut zero_t = p.key_proof;
        zero_t[..20].copy_from_slice(&[0u8; 20]);
        assert!(!key_proof_valid(
            &p.ephemeral,
            &round,
            5,
            &p.observer,
            &zero_t
        ));
        // Dismissal edge cases.
        let d = dismiss(&sk, &p, &round, 5).expect("dismiss");
        assert!(dismissal_proof_checks(&key, &p, &round, 5, &d));
        let mut bad = d;
        bad.shared = [0u8; 64];
        assert!(!dismissal_proof_checks(&key, &p, &round, 5, &bad));
        let mut bad = d;
        bad.shared[63] ^= 2;
        assert!(!dismissal_proof_checks(&key, &p, &round, 5, &bad));
        for c in [[0u8; 32], ORDER, [0xFF; 32]] {
            let mut bad = d;
            bad.proof[..32].copy_from_slice(&c);
            assert!(!dismissal_proof_checks(&key, &p, &round, 5, &bad));
        }
        let mut bad = d;
        bad.proof[32..].copy_from_slice(&[0u8; 32]);
        assert!(!dismissal_proof_checks(&key, &p, &round, 5, &bad));
    }

    /// Key proofs over 64 keys: valid for their own context only; `reduce`
    /// maps values at or above the order into range.
    #[test]
    fn key_proofs_check_and_bind_their_context() {
        let round = keccak256(b"round");
        for i in 0..64u8 {
            let r = keccak256(&[b'p', i]).0;
            let eph = public_point(&r).expect("point");
            let obs = [i; 20];
            let Ok(pf) = key_proof(&r, &round, 1, &obs) else {
                assert!(!x_below_order(&eph));
                continue;
            };
            assert!(key_proof_valid(&eph, &round, 1, &obs, &pf), "seed {i}");
            assert!(!key_proof_valid(&eph, &round, 2, &obs, &pf));
            assert!(!key_proof_valid(&eph, &keccak256(b"x"), 1, &obs, &pf));
            assert!(!key_proof_valid(&eph, &round, 1, &[i ^ 1; 20], &pf));
            let mut bad = pf;
            bad[40] ^= 1;
            assert!(!key_proof_valid(&eph, &round, 1, &obs, &bad));
        }
        assert_eq!(reduce(ORDER), [0u8; 32]);
        let mut n1 = ORDER;
        n1[31] += 1;
        let mut one = [0u8; 32];
        one[31] = 1;
        assert_eq!(reduce(n1), one);
        let max = reduce([0xFF; 32]);
        assert!(max < ORDER);
        assert_eq!(max[..15], [0u8; 15]);
    }
}
