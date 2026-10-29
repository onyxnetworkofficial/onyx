//! ROADMAP C12 (ADR-0027 amendment 12): the time-lock composition with a MOCK
//! beacon, and the adversarial cases of THREATS C12-* (early decryption, drand
//! round substitution, wrong chain, a beacon that never arrives, malleability
//! across the two layers). The mock is no cryptography: its "pairing" is a hash
//! of the round's signature, which its `lock` computes from a master secret it
//! holds. It models the INTERFACE a real tlock gives (anyone locks to a future
//! round; only the round's signature opens; a wrong round or chain does not
//! open; encryption is deterministic given `sigma`), never its security.

use super::*;
use crate::observe::test_keys;
use crate::sealed::{open_posted, post_signed, seal, Opened};
use onyx_core::claim::{MetricId, Subject};
use onyx_core::{
    observation_commit, Answer, DataRights, ObservationKind, Redistribution, SourceId,
};
use std::cell::Cell;

/// The mock beacon: a chain whose round signatures are a keyed hash of a fixed
/// test master secret, released only once the mock clock reaches the round.
struct MockBeacon {
    chain: BeaconChain,
    master: [u8; 32],
    now: Cell<u64>,
    /// Accept a time-lock whose first byte differs (a stand-in for an
    /// implementation that takes a non-canonical point encoding, C12-R1-2).
    sloppy: bool,
}

fn expand(seed: &[u8], n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    let mut i = 0u32;
    while out.len() < n {
        let mut b = seed.to_vec();
        b.extend_from_slice(&i.to_be_bytes());
        out.extend_from_slice(&keccak256(&b).0);
        i += 1;
    }
    out.truncate(n);
    out
}

fn h16(tag: &[u8], parts: &[&[u8]]) -> [u8; 16] {
    let mut b = tag.to_vec();
    for p in parts {
        b.extend_from_slice(p);
    }
    let mut o = [0u8; 16];
    o.copy_from_slice(&keccak256(&b).0[..16]);
    o
}

fn xor16(a: &[u8], b: &[u8; 16]) -> [u8; 16] {
    let mut o = [0u8; 16];
    for i in 0..16 {
        o[i] = a[i] ^ b[i];
    }
    o
}

impl MockBeacon {
    fn new(master: u8, chain_hash: [u8; 32]) -> Self {
        MockBeacon {
            chain: BeaconChain {
                chain_hash,
                ..BeaconChain::quicknet()
            },
            master: [master; 32],
            now: Cell::new(0),
            sloppy: false,
        }
    }

    /// The pinned chain's mock (the quicknet chain hash and clock).
    fn pinned() -> Self {
        Self::new(0x11, QUICKNET_CHAIN_HASH)
    }

    fn sig_of(&self, round: u64) -> Vec<u8> {
        let mut b = b"mock-sig".to_vec();
        b.extend_from_slice(&self.master);
        b.extend_from_slice(&self.chain.chain_hash);
        b.extend_from_slice(&round.to_be_bytes());
        expand(&b, 48)
    }

    /// The round's signature if the mock clock has reached it.
    fn signature(&self, round: u64) -> Option<Vec<u8>> {
        (self.chain.round_time(round)? <= self.now.get()).then(|| self.sig_of(round))
    }

    fn u_of(sigma: &[u8; 16], secret: &[u8; 16]) -> Vec<u8> {
        let r = h16(b"IBE-H3", &[sigma, secret]);
        expand(&[b"mock-U".as_slice(), &r].concat(), 96)
    }

    fn mask(sig: &[u8], u: &[u8]) -> [u8; 16] {
        h16(b"IBE-H2", &[b"mock-pair", sig, u])
    }
}

impl TimeLock for MockBeacon {
    fn chain(&self) -> &BeaconChain {
        &self.chain
    }

    fn lock(&self, round: u64, secret: &[u8; 16], sigma: &[u8; 16]) -> Option<Tlock> {
        let u = Self::u_of(sigma, secret);
        // The mock's "pairing" e(r·Qid, pk) is a hash of the round's signature,
        // which only this mock can compute ahead of the round.
        // The "point" is bytes 1..96; byte 0 stands for an encoding detail that
        // a strict open requires and a sloppy one ignores.
        let v = xor16(sigma, &Self::mask(&self.sig_of(round), &u[1..]));
        let w = xor16(secret, &h16(b"IBE-H4", &[sigma]));
        let mut t = [0u8; TLOCK_LEN];
        t[..96].copy_from_slice(&u);
        t[96..112].copy_from_slice(&v);
        t[112..].copy_from_slice(&w);
        Some(t)
    }

    fn verify_signature(&self, round: u64, signature: &[u8]) -> bool {
        signature == self.sig_of(round).as_slice()
    }

    fn open(&self, _round: u64, signature: &[u8], tl: &Tlock) -> Option<TlockOpening> {
        if signature.len() != 48 {
            return None;
        }
        let sigma = xor16(&tl[96..112], &Self::mask(signature, &tl[1..96]));
        let secret = xor16(&tl[112..], &h16(b"IBE-H4", &[&sigma]));
        // The Fujisaki-Okamoto check: U must be the one (sigma, secret) gives.
        let u = Self::u_of(&sigma, &secret);
        let from = usize::from(self.sloppy);
        (u[from..] == tl[from..96]).then_some(TlockOpening { sigma, secret })
    }
}

const REVEAL_SECRET: [u8; 32] = [0x42; 32];
/// A reveal close well after quicknet's genesis (2026-10-17 12:00 UTC).
const REVEAL_CLOSE: u64 = 1_792_238_400;
const ATTEMPT: u32 = 0;
/// The observer's clock when it seals: inside the reveal phase.
const SEAL_NOW: u64 = REVEAL_CLOSE - 100;

fn spec() -> ObservationSpec {
    ObservationSpec {
        feed: "example.page.usd_per_unit".into(),
        metric: MetricId("example.page.usd_per_unit".into()),
        subject: Subject::Entity("entity:alpha".into()),
        fetcher: keccak256(b"fetcher"),
        url: "https://example.org/page.json".into(),
        selector: "/data/usd_per_unit".into(),
        kind: ObservationKind::Numeric {
            scale: 2,
            tolerance: 50,
        },
        window_start: 1_000,
        window_end: 1_050,
        min_observers: 3,
        rights: DataRights {
            license: "CC0-1.0".into(),
            redistribution: Redistribution::Allowed,
            source: SourceId("example.page".into()),
            attribution: None,
            terms: String::new(),
        },
    }
}

fn obs(v: i128) -> Observation {
    Observation {
        answer: Answer::Number(v),
        observed_at: 1_010,
        page_hash: keccak256(b"page"),
    }
}

fn round_id() -> Hash32 {
    keccak256(b"c12 round")
}

fn key() -> Point {
    public_point(&REVEAL_SECRET).expect("reveal key")
}

fn secrets(i: u8) -> LockSecrets {
    LockSecrets {
        ephemeral: keccak256(&[b'r', i]).0,
        tau: h16(b"tau", &[&[i]]),
        sigma: h16(b"sigma", &[&[i]]),
        kappa: h16(b"kappa", &[&[i]]),
    }
}

/// Observer `i`: its keys, salt and commit to `v`.
struct Obs {
    secret: [u8; 32],
    address: MinerAddress,
    salt: Hash32,
    commit: Hash32,
}

fn observer(i: u8, v: i128) -> Obs {
    let (secret, address) = test_keys::ordered(i);
    let salt = keccak256(&[b's', i]);
    let commit = observation_commit(&round_id(), &address, &obs(v), &salt);
    Obs {
        secret,
        address,
        salt,
        commit,
    }
}

fn ctx<'a>(o: &Obs, s: &'a ObservationSpec) -> RevealContext<'a> {
    RevealContext {
        round_id: round_id(),
        attempt: ATTEMPT,
        spec: s,
        commit: Some(o.commit),
    }
}

/// The rounds of a window closing at `rc` whose fact unseals a day later (the
/// earliest far date `LockRounds::for_window` allows).
fn rounds_for(rc: u64) -> LockRounds {
    LockRounds::for_window(&BeaconChain::quicknet(), rc, rc + UNSEAL_MARGIN_SECS).expect("rounds")
}

/// The rounds of the test window.
fn rounds() -> LockRounds {
    rounds_for(REVEAL_CLOSE)
}

/// The far round of the test window.
fn far() -> u64 {
    rounds().far()
}

/// A clock 100 s before the reveal close whose lock round is `r`'s.
fn seal_now(r: LockRounds) -> u64 {
    BeaconChain::quicknet().round_time(r.lock()).expect("t") - LOCK_MARGIN_SECS - 100
}

fn sealed_by<T: TimeLock>(tl: &T, i: u8, v: i128, rounds: LockRounds) -> (Obs, SealedPost) {
    let o = observer(i, v);
    let p = seal_timelocked(
        tl,
        &o.secret,
        &secrets(i),
        &key(),
        &round_id(),
        ATTEMPT,
        rounds,
        seal_now(rounds),
        &spec(),
        &obs(v),
        &o.salt,
    )
    .expect("seal");
    (o, p)
}

fn posted(p: &SealedPost) -> PostedSealed {
    PostedSealed {
        ephemeral: p.ephemeral,
        key_proof: p.key_proof,
        posted_at: REVEAL_CLOSE - 10,
        ciphertext: p.ciphertext.clone(),
    }
}

fn rho() -> u64 {
    lock_round(&BeaconChain::quicknet(), REVEAL_CLOSE).expect("round")
}

/// The pinned mock with its clock at the lock round.
fn at_round() -> MockBeacon {
    let b = MockBeacon::pinned();
    b.now
        .set(BeaconChain::quicknet().round_time(rho()).expect("time"));
    b
}

fn open<T: TimeLock>(
    tl: &T,
    sig: Option<&[u8]>,
    o: &Obs,
    p: &PostedSealed,
) -> Result<TimedOpened, TimeLockError> {
    let s = spec();
    open_timelocked(
        tl,
        &REVEAL_SECRET,
        sig,
        &o.address,
        p,
        &ctx(o, &s),
        rounds(),
    )
}

fn check<T: TimeLock>(
    tl: &T,
    sig: Option<&[u8]>,
    o: &Obs,
    p: &PostedSealed,
    d: &TimedDismissal,
) -> Result<Vec<u8>, TimeLockError> {
    let s = spec();
    check_timelocked_dismissal(tl, &key(), &o.address, p, &ctx(o, &s), rounds(), sig, d)
}

/// The lock round is the FIRST quicknet round published at or after
/// `reveal_close + LOCK_MARGIN_SECS`, never one before it (checked against
/// the round drand served on 2026-10-17: 33,152,398 at unix 1,792,260,560).
#[test]
fn c12_the_lock_round_is_the_first_round_past_reveal_close_and_the_margin() {
    let q = BeaconChain::quicknet();
    assert_eq!(q.round_time(1), Some(QUICKNET_GENESIS));
    assert_eq!(q.round_time(0), None);
    assert_eq!(q.first_round_at_or_after(0), Some(1));
    assert_eq!(q.first_round_at_or_after(QUICKNET_GENESIS), Some(1));
    assert_eq!(q.first_round_at_or_after(QUICKNET_GENESIS + 1), Some(2));
    assert_eq!(q.first_round_at_or_after(QUICKNET_GENESIS + 3), Some(2));
    // drand's latest round at 1,792,260,560 was 33,152,398 (published at or before it).
    let t = q.round_time(33_152_398).expect("t");
    assert!(t <= 1_792_260_560 && 1_792_260_560 < t + QUICKNET_PERIOD);
    for close in [
        REVEAL_CLOSE,
        REVEAL_CLOSE + 1,
        REVEAL_CLOSE + 2,
        QUICKNET_GENESIS,
        1,
    ] {
        let r = lock_round(&q, close).expect("round");
        let rt = q.round_time(r).expect("time");
        assert!(
            rt >= close + LOCK_MARGIN_SECS,
            "round {r} before the margin"
        );
        if r > 1 {
            assert!(q.round_time(r - 1).expect("t") < close + LOCK_MARGIN_SECS);
        }
    }
    assert_eq!(lock_round(&q, u64::MAX), None);
    let broken = BeaconChain { period: 0, ..q };
    assert_eq!(broken.first_round_at_or_after(5), None);
}

/// A time-locked numeric reveal is 272 bytes longer (426: the time-lock, and
/// since ROADMAP C52 the far copy and the wrapped body key) and settles once the
/// lock round's signature exists; the observer's signature covers both
/// time-locks.
#[test]
fn c12_a_timelocked_post_opens_with_the_rounds_signature() {
    let b = at_round();
    let (o, p) = sealed_by(&b, 1, 100, rounds());
    assert_eq!(
        p.ciphertext.len(),
        TIME_LOCKED_HEAD_BYTES + sealed_len(&spec())
    );
    assert_eq!(p.ciphertext.len(), 426);
    assert!(post_signed(&p, &round_id(), ATTEMPT));
    let sig = b.signature(rho()).expect("published");
    match open(&b, Some(&sig), &o, &posted(&p)).expect("open") {
        TimedOpened::Reveal((a, ob, salt, _)) => {
            assert_eq!((a, ob, salt), (o.address, obs(100), o.salt));
        }
        TimedOpened::Dismissed(_) => panic!("a valid post dismissed"),
    }
    // Random secrets seal too, and open the same way.
    let p2 = seal_timelocked_random(
        &b,
        &o.secret,
        &key(),
        &round_id(),
        ATTEMPT,
        rounds(),
        SEAL_NOW,
        &spec(),
        &obs(100),
        &o.salt,
    )
    .expect("seal");
    assert!(matches!(
        open(&b, Some(&sig), &o, &posted(&p2)),
        Ok(TimedOpened::Reveal(_))
    ));
}

/// THREATS C12-1 (closes C4-7 in the composition): before the lock round
/// nobody opens a post. The coordinator holds the reveal key, yet its ECDH
/// alone (amendment 10's path) opens nothing: the body decrypts to garbage.
/// The signature of an EARLIER round (already published) opens nothing either.
#[test]
fn c12_1_the_coordinator_opens_nothing_before_the_lock_round() {
    let b = MockBeacon::pinned();
    let q = BeaconChain::quicknet();
    // The clock just before the lock round: reveal close passed, round not yet out.
    b.now.set(q.round_time(rho()).expect("t") - 1);
    assert!(b.now.get() >= REVEAL_CLOSE);
    let (o, p) = sealed_by(&b, 1, 100, rounds());
    assert_eq!(b.signature(rho()), None);
    assert_eq!(
        open(&b, b.signature(rho()).as_deref(), &o, &posted(&p)),
        Err(TimeLockError::BeaconMissing(rho()))
    );
    // Amendment 10's opening with the reveal key only: garbage, never the reveal.
    let s = spec();
    let early = open_posted(&REVEAL_SECRET, &o.address, &posted(&p), &ctx(&o, &s));
    assert!(matches!(early, Ok(Opened::Dismissed(_))), "{early:?}");
    // Even skipping the time-lock bytes, the ECDH key alone opens nothing.
    let shared = shared_point(&REVEAL_SECRET, &p.ephemeral).expect("S");
    let guess = crate::sealed::post_key(&shared, &p.ephemeral, &round_id(), ATTEMPT, &o.address);
    let body = stream_xor(&guess, &p.ciphertext[TIME_LOCKED_HEAD_BYTES..]);
    assert!(valid_reveal(&body, &o.address, &ctx(&o, &s)).is_none());
    // Everything the coordinator holds before the round (S, R, both
    // time-locks, the wrapped key, the rounds, the chain) but `tau`: a guessed
    // `tau` unwraps a wrong body key and opens nothing, so the key really needs
    // the time-locked secret (break-the-code: dropping `tau` from the wrap).
    let lock: Tlock = p.ciphertext[..TLOCK_LEN].try_into().expect("tl");
    let far_lock: Tlock = p.ciphertext[TLOCK_LEN..2 * TLOCK_LEN]
        .try_into()
        .expect("tf");
    let mut w = [0u8; 16];
    w.copy_from_slice(&p.ciphertext[2 * TLOCK_LEN..TIME_LOCKED_HEAD_BYTES]);
    for guess_tau in [[0u8; 16], [0xff; 16], secrets(2).tau] {
        let wrap = wrap_key(
            &shared,
            &guess_tau,
            &p.ephemeral,
            &lock,
            &far_lock,
            rounds(),
            &QUICKNET_CHAIN_HASH,
            &round_id(),
            ATTEMPT,
            &o.address,
        );
        let kappa = xor16(&w, &wrap);
        assert_ne!(kappa, secrets(1).kappa);
        let k = body_key(
            &kappa,
            &p.ephemeral,
            &lock,
            &far_lock,
            rounds(),
            &QUICKNET_CHAIN_HASH,
            &round_id(),
            ATTEMPT,
            &o.address,
        );
        let body = stream_xor(&k, &p.ciphertext[TIME_LOCKED_HEAD_BYTES..]);
        assert!(valid_reveal(&body, &o.address, &ctx(&o, &s)).is_none());
    }
    // The far copy is no way in before the far round either: the mock gives
    // no far signature yet, so `unseal_far` refuses to guess.
    assert_eq!(b.signature(far()), None);
    assert_eq!(
        unseal_far(&b, None, &o.address, &posted(&p), &ctx(&o, &s), rounds()),
        Err(TimeLockError::BeaconMissing(far()))
    );
    // An earlier round's signature is published but is not the lock round's.
    let earlier = b.signature(rho() - 1).expect("published");
    assert_eq!(
        open(&b, Some(&earlier), &o, &posted(&p)),
        Err(TimeLockError::BadBeacon(rho()))
    );
    assert_eq!(b.open(rho() - 1, &earlier, &lock), None);
}

/// THREATS C12-2: drand round substitution. (a) An observer locking to an
/// earlier round (to let a colluding coordinator read it early) only spoils its
/// own post: it does not open under the lock round, is dismissed without an
/// opening, and every client agrees given the round's signature. (b) A
/// coordinator cannot show "does not open" for an honest post (false
/// dismissal), nor pass another round's signature as the lock round's, nor
/// make up an opening.
#[test]
fn c12_2_round_substitution_spoils_only_the_substituters_own_post() {
    let b = at_round();
    let sig = b.signature(rho()).expect("published");
    // (a) Locked to the round of a reveal close 10 minutes earlier.
    let (o, early) = sealed_by(&b, 1, 100, rounds_for(REVEAL_CLOSE - 600));
    let pe = posted(&early);
    let TimedOpened::Dismissed(d) = open(&b, Some(&sig), &o, &pe).expect("open") else {
        panic!("a post locked to another round was taken");
    };
    assert_eq!(d.opening, None);
    assert_eq!(check(&b, Some(&sig), &o, &pe, &d), Ok(Vec::new()));
    assert_eq!(
        check(&b, None, &o, &pe, &d),
        Err(TimeLockError::BeaconMissing(rho()))
    );
    // (b) An honest post "dismissed" as not opening: refused.
    let (h, honest) = sealed_by(&b, 2, 100, rounds());
    let ph = posted(&honest);
    let fake = TimedDismissal {
        dismissal: dismiss(&REVEAL_SECRET, &honest, &round_id(), ATTEMPT).expect("dleq"),
        opening: None,
    };
    assert_eq!(
        check(&b, Some(&sig), &h, &ph, &fake),
        Err(SealError::FalseDismissal.into())
    );
    // Another round's signature passed as the lock round's: refused.
    let other = b.signature(rho() - 1).expect("published");
    assert_eq!(
        check(&b, Some(&other), &h, &ph, &fake),
        Err(TimeLockError::BadBeacon(rho()))
    );
    // A made-up opening does not re-encrypt to the post's time-lock.
    let made_up = TimedDismissal {
        opening: Some(TlockOpening {
            sigma: [1; 16],
            secret: [2; 16],
        }),
        ..fake
    };
    assert_eq!(
        check(&b, None, &h, &ph, &made_up),
        Err(TimeLockError::BadOpening)
    );
    // The TRUE opening of an honest post opens a valid reveal: a false dismissal.
    let lock: Tlock = honest.ciphertext[..TLOCK_LEN].try_into().expect("tl");
    let true_opening = TimedDismissal {
        opening: b.open(rho(), &sig, &lock),
        ..fake
    };
    assert!(true_opening.opening.is_some());
    assert_eq!(
        check(&b, None, &h, &ph, &true_opening),
        Err(SealError::FalseDismissal.into())
    );
}

/// THREATS C12-3: a wrong chain. A post locked to another beacon (another key,
/// another chain hash: a chain the coordinator might run) does not open under
/// the pinned chain and is dismissed; that chain's signatures are refused as
/// the pinned chain's. The pinned key is a constant, never an argument.
#[test]
fn c12_3_a_post_locked_to_another_chain_is_garbage() {
    let pinned = at_round();
    let rogue = MockBeacon::new(0x66, keccak256(b"another chain").0);
    rogue.now.set(pinned.now.get());
    let (o, p) = sealed_by(&rogue, 1, 100, rounds());
    let sig = pinned.signature(rho()).expect("published");
    let pp = posted(&p);
    let TimedOpened::Dismissed(d) = open(&pinned, Some(&sig), &o, &pp).expect("open") else {
        panic!("a post locked to another chain was taken");
    };
    assert_eq!(d.opening, None);
    assert_eq!(check(&pinned, Some(&sig), &o, &pp, &d), Ok(Vec::new()));
    let rogue_sig = rogue.signature(rho()).expect("rogue round");
    assert_eq!(
        open(&pinned, Some(&rogue_sig), &o, &pp),
        Err(TimeLockError::BadBeacon(rho()))
    );
    // Same master, other chain hash: the key itself binds the chain hash too.
    let twin = MockBeacon::new(0x11, keccak256(b"another chain").0);
    twin.now.set(pinned.now.get());
    let (o2, p2) = sealed_by(&twin, 2, 100, rounds());
    // The MOCK's signatures hash in the chain hash, so the time-lock layer
    // rejects it (no opening). Real IBE binds only the key and the round: with
    // the same key the time-lock would open and the chain hash in `k` would
    // reject the body instead (C12-R1-6; C50 swaps this expectation).
    let Ok(TimedOpened::Dismissed(d2)) = open(&pinned, Some(&sig), &o2, &posted(&p2)) else {
        panic!("a post locked to a twin chain was taken");
    };
    assert_eq!(d2.opening, None);
    assert_eq!(QUICKNET_PUBLIC_KEY_HEX.len(), 192);
}

/// THREATS C12-4: the beacon never arrives (drand down or late). Nothing opens
/// and nothing is dismissed: there is no fallback path, by design. A late
/// round loses nothing: the posts open when it comes.
#[test]
fn c12_4_without_the_beacon_nothing_opens_and_a_late_one_loses_nothing() {
    let b = MockBeacon::pinned();
    let q = BeaconChain::quicknet();
    b.now.set(REVEAL_CLOSE);
    let posts: Vec<_> = (1..=3).map(|i| sealed_by(&b, i, 100, rounds())).collect();
    for (o, p) in &posts {
        assert_eq!(
            open(&b, None, o, &posted(p)),
            Err(TimeLockError::BeaconMissing(rho()))
        );
    }
    // A day late: the same round's signature opens every post.
    b.now.set(q.round_time(rho()).expect("t") + 86_400);
    let sig = b.signature(rho()).expect("published late");
    for (o, p) in &posts {
        assert!(matches!(
            open(&b, Some(&sig), o, &posted(p)),
            Ok(TimedOpened::Reveal(_))
        ));
    }
}

/// THREATS C12-5: malleability across the two layers. A third party cannot
/// change any byte (the observer's signature covers the whole ciphertext). A
/// node that serves changed bytes under a valid key proof (C11-R1-1's class)
/// never turns a post into another VALID reveal: a changed time-lock does not
/// open (Fujisaki-Okamoto), a changed body opens to garbage; both are dismissed
/// and the dismissal checks against the SAME changed bytes, so a client fed the
/// same lie accepts it (C11-R1-1's class, review C12-R1-5: TODO C31, C34),
/// while against the real post the same dismissal is refused as false. Copying
/// another post's time-lock gains nothing.
#[test]
fn c12_5_no_byte_of_either_layer_can_be_changed_into_another_reveal() {
    let b = at_round();
    let sig = b.signature(rho()).expect("published");
    let (o, p) = sealed_by(&b, 1, 100, rounds());
    // The near time-lock (0..128), the far copy (128..256), the wrapped body
    // key (256..272) and the body (272..426, ROADMAP C52).
    for i in [
        0, 47, 95, 96, 111, 112, 127, 128, 200, 255, 256, 271, 272, 300, 425,
    ] {
        let mut q = p.clone();
        q.ciphertext[i] ^= 0x01;
        assert!(!post_signed(&q, &round_id(), ATTEMPT), "byte {i}");
        let pq = posted(&q);
        let TimedOpened::Dismissed(d) = open(&b, Some(&sig), &o, &pq).expect("open") else {
            panic!("byte {i}: a changed post was taken as a reveal");
        };
        assert_eq!(d.opening.is_none(), i < TLOCK_LEN, "byte {i}");
        assert!(check(&b, Some(&sig), &o, &pq, &d).is_ok(), "byte {i}");
        assert_eq!(
            check(&b, Some(&sig), &o, &posted(&p), &d),
            Err(SealError::FalseDismissal.into()),
            "byte {i}: a dismissal of changed bytes passed against the real post"
        );
    }
    // Observer 2 posts observer 1's time-lock in front of its own body.
    let (o2, p2) = sealed_by(&b, 2, 100, rounds());
    let mut mixed = p2.clone();
    mixed.ciphertext[..TLOCK_LEN].copy_from_slice(&p.ciphertext[..TLOCK_LEN]);
    let pm = posted(&mixed);
    let TimedOpened::Dismissed(d) = open(&b, Some(&sig), &o2, &pm).expect("open") else {
        panic!("a borrowed time-lock was taken");
    };
    // Its dismissal shows observer 1's tau, which the round made public anyway,
    // and not observer 1's value.
    assert_eq!(d.opening.map(|x| x.secret), Some(secrets(1).tau));
    let plain = check(&b, None, &o2, &pm, &d).expect("checks");
    assert!(valid_reveal(&plain, &o.address, &ctx(&o, &spec())).is_none());
}

/// Garbage behind a valid time-lock is dismissed with its opening, which any
/// client checks WITHOUT the beacon signature (re-encryption); a dismissal of
/// a valid post is refused; a tampered DLEQ proof is refused; a post too short
/// to hold a time-lock is garbage on its face.
#[test]
fn c12_garbage_is_dismissed_with_an_opening_anyone_checks() {
    let b = at_round();
    let sig = b.signature(rho()).expect("published");
    let o = observer(3, 100);
    let junk = seal_bytes_timelocked(
        &b,
        &o.secret,
        &o.address,
        &secrets(3),
        &key(),
        &round_id(),
        ATTEMPT,
        rounds(),
        SEAL_NOW,
        b"not a reveal",
    )
    .expect("seal");
    let pj = posted(&junk);
    let TimedOpened::Dismissed(d) = open(&b, Some(&sig), &o, &pj).expect("open") else {
        panic!("junk taken");
    };
    assert_eq!(d.opening.map(|x| x.secret), Some(secrets(3).tau));
    assert_eq!(check(&b, None, &o, &pj, &d), Ok(b"not a reveal".to_vec()));
    let mut bad = d;
    bad.dismissal.proof[40] ^= 1;
    assert_eq!(
        check(&b, None, &o, &pj, &bad),
        Err(SealError::BadProof.into())
    );
    // A post of amendment 10's shape (no time-lock) and too short to hold one.
    let short = seal(
        &o.secret,
        &secrets(3).ephemeral,
        &key(),
        &round_id(),
        ATTEMPT,
        &spec(),
        &obs(100),
        &o.salt,
    )
    .expect("seal");
    let ps = PostedSealed {
        ciphertext: short.ciphertext[..TLOCK_LEN].to_vec(),
        ..posted(&short)
    };
    let TimedOpened::Dismissed(d) = open(&b, Some(&sig), &o, &ps).expect("open") else {
        panic!("short post taken");
    };
    assert_eq!(check(&b, None, &o, &ps, &d), Ok(Vec::new()));
    // A post of amendment 10's shape at full length is garbage under this layer.
    assert!(matches!(
        open(&b, Some(&sig), &o, &posted(&short)),
        Ok(TimedOpened::Dismissed(_))
    ));
}

/// Sizes: the time-lock costs 128 bytes per post, the far copy and the wrapped
/// body key 144 more (ROADMAP C52). ROADMAP C51 raised the registry's cap from
/// 1,166 to 1,294 bytes and C52 to 1,438 (THREATS C12-7, C52-6), so every
/// exact feed up to the largest value (`max_len` 1,024) seals time-locked with
/// its far copy; past the cap is `TooLong`, never a silent fallback.
#[test]
fn c12_sizes_against_the_registry_cap() {
    let b = at_round();
    let o = observer(4, 100);
    let fits = vec![1u8; MAX_SEALED_BYTES - TIME_LOCKED_HEAD_BYTES];
    let seal_n = |plain: &[u8]| {
        seal_bytes_timelocked(
            &b,
            &o.secret,
            &o.address,
            &secrets(4),
            &key(),
            &round_id(),
            ATTEMPT,
            rounds(),
            SEAL_NOW,
            plain,
        )
    };
    assert_eq!(
        seal_n(&fits).expect("fits").ciphertext.len(),
        MAX_SEALED_BYTES
    );
    assert_eq!(
        seal_n(&[1u8; MAX_SEALED_BYTES - TIME_LOCKED_HEAD_BYTES + 1]),
        Err(SealError::TooLong.into())
    );
    let exact = |max_len| ObservationSpec {
        kind: ObservationKind::Exact { max_len },
        ..spec()
    };
    assert_eq!(MAX_SEALED_BYTES, 1_438);
    assert_eq!(TIME_LOCKED_HEAD_BYTES, 272);
    assert_eq!(crate::sealed::FAR_COPY_BYTES, 144);
    assert_eq!(
        TIME_LOCKED_HEAD_BYTES + sealed_len(&exact(1_024)),
        MAX_SEALED_BYTES,
        "the largest exact answer, sealed, time-locked and far-copied, is the cap"
    );
    assert_eq!(
        crate::sealed::MAX_PLAIN_BYTES + TIME_LOCKED_HEAD_BYTES,
        MAX_SEALED_BYTES
    );
    // An empty body is no post.
    assert_eq!(seal_n(&[]), Err(SealError::TooLong.into()));
}

/// Opt-in, needs the network (never in check-all): drand quicknet's public
/// chain info matches the pinned constants, and its latest round agrees with
/// the round arithmetic. `ONYX_DRAND_LIVE=1 cargo test -p onyx-coordinator
/// timelock::tests::c12_live -- --ignored`. Reads api.drand.sh (no account).
#[test]
#[ignore = "network: ONYX_DRAND_LIVE=1, reads api.drand.sh"]
fn c12_live_quicknet_matches_the_pinned_chain() {
    if std::env::var("ONYX_DRAND_LIVE").as_deref() != Ok("1") {
        eprintln!("ONYX_DRAND_LIVE is not 1: skipped");
        return;
    }
    let base = format!("https://api.drand.sh/{}", hex::encode(QUICKNET_CHAIN_HASH));
    let get = |path: &str| -> serde_json::Value {
        let out = std::process::Command::new("curl")
            .args(["-fsS", "-A", "onyx-drand-check", "--max-time", "15"])
            .arg(format!("{base}/{path}"))
            .output()
            .expect("curl");
        assert!(out.status.success(), "curl {path} failed");
        serde_json::from_slice(&out.stdout).expect("json")
    };
    let info = get("info");
    assert_eq!(info["hash"], hex::encode(QUICKNET_CHAIN_HASH));
    assert_eq!(info["public_key"], QUICKNET_PUBLIC_KEY_HEX);
    assert_eq!(info["genesis_time"], QUICKNET_GENESIS);
    assert_eq!(info["period"], QUICKNET_PERIOD);
    assert_eq!(info["schemeID"], "bls-unchained-g1-rfc9380");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let latest = get("public/latest");
    let round = latest["round"].as_u64().expect("round");
    let q = BeaconChain::quicknet();
    let t = q.round_time(round).expect("t");
    // Published, and not more than a few rounds behind this machine's clock.
    assert!(t <= now + 5, "round {round} from the future");
    assert!(now < t + 30, "round {round} stale: drand late or clock off");
    assert_eq!(latest["signature"].as_str().map(str::len), Some(96));
}

/// Review C12-R1-2: a time-lock implementation that accepts a non-canonical
/// encoding (here: a mock that ignores the first byte) would open a griefer's
/// time-lock for the coordinator while a client's re-encryption fails, so no
/// dismissal of it would ever be accepted (a veto of every round the griefer
/// is drawn into). `open_canonical` keeps an opening only when it re-encrypts
/// byte for byte, on both sides, so the post is dismissed without an opening
/// and the dismissal checks.
#[test]
fn c12_r1_2_a_non_canonical_time_lock_is_dismissed_and_the_dismissal_checks() {
    let mut b = at_round();
    b.sloppy = true;
    let sig = b.signature(rho()).expect("published");
    let o = observer(5, 100);
    let honest = seal_bytes_timelocked(
        &b,
        &o.secret,
        &o.address,
        &secrets(5),
        &key(),
        &round_id(),
        ATTEMPT,
        rounds(),
        SEAL_NOW,
        b"junk behind a non-canonical time-lock",
    )
    .expect("seal");
    let mut bad = posted(&honest);
    bad.ciphertext[0] ^= 0x01;
    let lock: Tlock = bad.ciphertext[..TLOCK_LEN].try_into().expect("tl");
    let raw = b
        .open(rho(), &sig, &lock)
        .expect("the sloppy open takes it");
    assert_ne!(b.lock(rho(), &raw.secret, &raw.sigma), Some(lock));
    assert_eq!(open_canonical(&b, rho(), &sig, &lock), None);
    let Ok(TimedOpened::Dismissed(d)) = open(&b, Some(&sig), &o, &bad) else {
        panic!("a non-canonical time-lock was taken");
    };
    assert_eq!(d.opening, None);
    assert_eq!(check(&b, Some(&sig), &o, &bad, &d), Ok(Vec::new()));
    // The opening the sloppy open gives would be refused by a client.
    let with_raw = TimedDismissal {
        opening: Some(raw),
        ..d
    };
    assert_eq!(
        check(&b, None, &o, &bad, &with_raw),
        Err(TimeLockError::BadOpening)
    );
}

/// Review C12-R1-4: an observer seals only while the lock round is more than
/// `SEAL_SLACK_SECS` away by its own clock, so a coordinator that serves it an
/// earlier `reveal_close` (a round already out) gets no post it can read at
/// once.
#[test]
fn c12_r1_4_an_observer_never_locks_to_a_round_that_is_out() {
    let b = at_round();
    let o = observer(6, 100);
    let q = BeaconChain::quicknet();
    let due = q.round_time(rho()).expect("t");
    let seal_at = |close: u64, now: u64| {
        seal_timelocked(
            &b,
            &o.secret,
            &secrets(6),
            &key(),
            &round_id(),
            ATTEMPT,
            rounds_for(close),
            now,
            &spec(),
            &obs(100),
            &o.salt,
        )
    };
    assert!(seal_at(REVEAL_CLOSE, due - SEAL_SLACK_SECS - 1).is_ok());
    assert_eq!(
        seal_at(REVEAL_CLOSE, due - SEAL_SLACK_SECS),
        Err(TimeLockError::RoundTooSoon(rho()))
    );
    // An hour-earlier reveal close served by a lying coordinator.
    let early = lock_round(&q, REVEAL_CLOSE - 3_600).expect("round");
    assert_eq!(
        seal_at(REVEAL_CLOSE - 3_600, SEAL_NOW),
        Err(TimeLockError::RoundTooSoon(early))
    );
}

/// Review C12-R2, extended for ROADMAP C52: every part of both key
/// transcripts changes its key, so a post's body key unwraps only under the
/// exact shared point, locked secret, ephemeral key, both time-locks, both
/// rounds, chain, round id, attempt and observer it was sealed for, and its
/// body opens only under the exact body key and the same context
/// (break-the-code: leaving any one out of `wrap_key` or `body_key` makes two
/// of these keys equal). The registry address and the EVM chain id are NOT in
/// them; `round_id` binds the deployment chain only through the draw's beacon
/// block hash (THREATS C12-R2-3, TODO C31).
#[test]
fn c12_r2_every_part_of_the_key_transcript_changes_the_key() {
    let s1 = public_point(&[0x01; 32]).expect("point");
    let s2 = public_point(&[0x02; 32]).expect("point");
    let r1 = public_point(&[0x03; 32]).expect("point");
    let r2 = public_point(&[0x04; 32]).expect("point");
    let tl1 = [0x05u8; TLOCK_LEN];
    let mut tl2 = tl1;
    tl2[TLOCK_LEN - 1] ^= 1;
    let rs = rounds();
    let later = LockRounds::new(rs.lock(), rs.far() + 1).expect("rounds");
    let sooner = LockRounds::new(rs.lock() + 1, rs.far()).expect("rounds");
    // (secret: S for the wrap, kappa padded for the body; tau; R; TL; TF;
    // rounds; chain; round; attempt; observer)
    type T = (
        Point,
        [u8; 16],
        Point,
        Tlock,
        Tlock,
        LockRounds,
        [u8; 32],
        Hash32,
        u32,
        MinerAddress,
    );
    let base: T = (
        s1,
        [0x06u8; 16],
        r1,
        tl1,
        [0x08u8; TLOCK_LEN],
        rs,
        QUICKNET_CHAIN_HASH,
        round_id(),
        ATTEMPT,
        [0x07u8; 20],
    );
    let mut variants: Vec<T> = vec![base];
    let mut v = base;
    v.0 = s2;
    variants.push(v);
    let mut v = base;
    v.1[15] ^= 1;
    variants.push(v);
    let mut v = base;
    v.2 = r2;
    variants.push(v);
    let mut v = base;
    v.3 = tl2;
    variants.push(v);
    let mut v = base;
    v.4[0] ^= 1;
    variants.push(v);
    let mut v = base;
    v.5 = later;
    variants.push(v);
    let mut v = base;
    v.5 = sooner;
    variants.push(v);
    let mut v = base;
    v.6[0] ^= 1;
    variants.push(v);
    let mut v = base;
    v.7 = keccak256(b"another round");
    variants.push(v);
    let mut v = base;
    v.8 += 1;
    variants.push(v);
    let mut v = base;
    v.9[19] ^= 1;
    variants.push(v);
    let wrap = |t: &T| wrap_key(&t.0, &t.1, &t.2, &t.3, &t.4, t.5, &t.6, &t.7, t.8, &t.9);
    // The body key has no `S` and no `tau`: its secret is kappa (here the
    // first 16 bytes of the variant's `S` slot stand in for it).
    let body = |t: &T| {
        let mut kappa = [0u8; 16];
        kappa.copy_from_slice(&t.0[..16]);
        body_key(&kappa, &t.2, &t.3, &t.4, t.5, &t.6, &t.7, t.8, &t.9)
    };
    let wraps: Vec<Vec<u8>> = variants.iter().map(|t| wrap(t).to_vec()).collect();
    // tau (variant 2) is no part of the body key.
    let bodies: Vec<Vec<u8>> = variants
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 2)
        .map(|(_, t)| body(t).0.to_vec())
        .collect();
    for keys in [&wraps, &bodies] {
        for i in 0..keys.len() {
            for j in i + 1..keys.len() {
                assert_ne!(keys[i], keys[j], "parts {i} and {j} give one key");
            }
        }
    }
    assert_eq!(body(&variants[0]), body(&variants[2]));
    assert_ne!(
        wrap(&base).to_vec(),
        body(&base).0[..16].to_vec(),
        "wrap and body keys are domain-separated"
    );
}

// ROADMAP C50: the same composition with the REAL scheme (`QuicknetTimeLock`,
// drand/tlock's IBE on the pinned quicknet key) and real published quicknet
// signatures (fixtures/tlock/drand), so the properties the mock only modelled
// (C12-R1-6) hold for curve points, the GT bytes and drand's H3.

/// A published quicknet round and its signature (api.drand.sh, no account).
fn published(json: &str) -> (u64, Vec<u8>) {
    let v: serde_json::Value = serde_json::from_str(json).expect("beacon json");
    let round = v["round"].as_u64().expect("round");
    let sig = hex::decode(v["signature"].as_str().expect("signature")).expect("hex");
    (round, sig)
}

fn real_round() -> (u64, Vec<u8>) {
    published(include_str!(
        "../../../fixtures/tlock/drand/quicknet-round-33154000.json"
    ))
}

fn other_round() -> (u64, Vec<u8>) {
    published(include_str!(
        "../../../fixtures/tlock/drand/quicknet-round-746511.json"
    ))
}

/// A reveal close whose lock round is exactly `round`.
fn close_for(round: u64) -> u64 {
    BeaconChain::quicknet().round_time(round).expect("time") - LOCK_MARGIN_SECS
}

/// Rounds locking to `round`, with a far round a day of rounds later (the
/// real scheme locks to any future round; these tests open only the lock).
fn real_rounds(round: u64) -> LockRounds {
    LockRounds::new(round, round + 28_800).expect("rounds")
}

fn open_at<T: TimeLock>(
    tl: &T,
    sig: Option<&[u8]>,
    o: &Obs,
    p: &PostedSealed,
    rs: LockRounds,
) -> Result<TimedOpened, TimeLockError> {
    let s = spec();
    open_timelocked(tl, &REVEAL_SECRET, sig, &o.address, p, &ctx(o, &s), rs)
}

fn check_at<T: TimeLock>(
    tl: &T,
    sig: Option<&[u8]>,
    o: &Obs,
    p: &PostedSealed,
    d: &TimedDismissal,
    rs: LockRounds,
) -> Result<Vec<u8>, TimeLockError> {
    let s = spec();
    check_timelocked_dismissal(tl, &key(), &o.address, p, &ctx(o, &s), rs, sig, d)
}

#[test]
fn c50_the_pinned_key_is_onyx_tlocks_key() {
    assert_eq!(
        hex::decode(QUICKNET_PUBLIC_KEY_HEX).expect("hex"),
        onyx_tlock::QUICKNET_PUBLIC_KEY.to_vec()
    );
    assert_eq!(QUICKNET_CHAIN_HASH, onyx_tlock::QUICKNET_CHAIN_HASH);
    let q = QuicknetTimeLock::pinned().expect("pinned");
    assert_eq!(q.chain(), &BeaconChain::quicknet());
    let (round, sig) = real_round();
    assert!(q.verify_signature(round, &sig));
    assert!(!q.verify_signature(round + 1, &sig));
}

#[test]
fn c50_r1_1_quicknet_open_checks_the_signature_itself() {
    // Review C50-R1-1: every caller verifies the beacon first today
    // (`beacon_checked`), so `QuicknetTimeLock::open` must not lean on that.
    // A time-lock made for quicknet-t (same scheme, another key) opens under
    // quicknet-t's real signature of its round; through the pinned quicknet
    // layer it must not, even though the Fujisaki-Okamoto check alone would
    // pass (break-the-code: `open` without the signature check fails here).
    let info: serde_json::Value = serde_json::from_str(include_str!(
        "../../../fixtures/tlock/drand/quicknet-t-info.json"
    ))
    .expect("info json");
    let key_t = onyx_tlock::BeaconKey::from_compressed(
        &hex::decode(info["public_key"].as_str().expect("key")).expect("hex"),
    )
    .expect("quicknet-t key");
    let (round, sig) = published(include_str!(
        "../../../fixtures/tlock/drand/quicknet-t-round-1624910.json"
    ));
    let s = secrets(1);
    let tl = key_t.lock(round, &s.tau, &s.sigma).expect("lock");
    let o = key_t
        .open(round, &sig, &tl)
        .expect("opens on its own chain");
    assert_eq!((o.msg, o.sigma), (s.tau, s.sigma));
    let q = QuicknetTimeLock::pinned().expect("pinned");
    assert!(!q.verify_signature(round, &sig));
    assert_eq!(q.open(round, &sig, &tl), None);
    assert_eq!(open_canonical(&q, round, &sig, &tl), None);
}

#[test]
fn c50_a_real_time_locked_post_opens_only_with_its_rounds_published_signature() {
    let q = QuicknetTimeLock::pinned().expect("pinned");
    let (round, sig) = real_round();
    let (_, other) = other_round();
    let rc = close_for(round);
    assert_eq!(lock_round(q.chain(), rc), Some(round));
    let rc = real_rounds(round);
    let (o, p) = sealed_by(&q, 1, 4_200, rc);
    let pp = posted(&p);
    // C12-1 with real IBE: nothing before the round, nothing with another
    // round's real signature.
    assert_eq!(
        open_at(&q, None, &o, &pp, rc),
        Err(TimeLockError::BeaconMissing(round))
    );
    assert_eq!(
        open_at(&q, Some(&other), &o, &pp, rc),
        Err(TimeLockError::BadBeacon(round))
    );
    let Ok(TimedOpened::Reveal(r)) = open_at(&q, Some(&sig), &o, &pp, rc) else {
        panic!("the real signature opens the post");
    };
    assert_eq!(r.1, obs(4_200));
    // The time-lock itself opens to the observer's (sigma, tau), and only
    // under its own round.
    let mut tl = [0u8; TLOCK_LEN];
    tl.copy_from_slice(&p.ciphertext[..TLOCK_LEN]);
    let opening = open_canonical(&q, round, &sig, &tl).expect("opens");
    assert_eq!(opening.secret, secrets(1).tau);
    assert_eq!(opening.sigma, secrets(1).sigma);
    let (r2, s2) = other_round();
    assert_eq!(open_canonical(&q, r2, &s2, &tl), None);
}

#[test]
fn c50_real_garbage_is_dismissed_with_an_opening_any_client_checks_without_the_signature() {
    let q = QuicknetTimeLock::pinned().expect("pinned");
    let (round, sig) = real_round();
    let rc = real_rounds(round);
    let o = observer(2, 0);
    let p = seal_bytes_timelocked(
        &q,
        &o.secret,
        &o.address,
        &secrets(2),
        &key(),
        &round_id(),
        ATTEMPT,
        rc,
        seal_now(rc),
        &[0xee; 40],
    )
    .expect("seal garbage");
    let pp = posted(&p);
    let Ok(TimedOpened::Dismissed(d)) = open_at(&q, Some(&sig), &o, &pp, rc) else {
        panic!("garbage is dismissed");
    };
    let opening = d
        .opening
        .expect("the time-lock opens, so the opening is owed");
    assert_eq!(opening.secret, secrets(2).tau);
    // A client checks by re-encryption alone (no beacon signature).
    assert_eq!(
        check_at(&q, None, &o, &pp, &d, rc).expect("checks"),
        vec![0xee; 40]
    );
    // A made-up opening is refused.
    let mut bad = d;
    if let Some(x) = bad.opening.as_mut() {
        x.sigma[0] ^= 1;
    }
    assert_eq!(
        check_at(&q, None, &o, &pp, &bad, rc),
        Err(TimeLockError::BadOpening)
    );
    // Withholding the opening of a time-lock that opens is a false dismissal.
    let withheld = TimedDismissal { opening: None, ..d };
    assert_eq!(
        check_at(&q, Some(&sig), &o, &pp, &withheld, rc),
        Err(SealError::FalseDismissal.into())
    );
}

#[test]
fn c50_a_tampered_real_time_lock_never_opens_and_its_dismissal_needs_the_signature() {
    let q = QuicknetTimeLock::pinned().expect("pinned");
    let (round, sig) = real_round();
    let rc = real_rounds(round);
    let (o, p) = sealed_by(&q, 3, 1_234, rc);
    for at in [0usize, 50, 95, 96, 111, 112, 127] {
        let mut pp = posted(&p);
        pp.ciphertext[at] ^= 0x01;
        let Ok(TimedOpened::Dismissed(d)) = open_at(&q, Some(&sig), &o, &pp, rc) else {
            panic!("byte {at}: a tampered time-lock is dismissed");
        };
        assert_eq!(d.opening, None, "byte {at}: it does not open");
        assert_eq!(
            check_at(&q, None, &o, &pp, &d, rc),
            Err(TimeLockError::BeaconMissing(round))
        );
        assert_eq!(check_at(&q, Some(&sig), &o, &pp, &d, rc), Ok(Vec::new()));
        // Against the real post, that dismissal is false (C12-R1-5).
        assert_eq!(
            check_at(&q, Some(&sig), &o, &posted(&p), &d, rc),
            Err(SealError::FalseDismissal.into())
        );
    }
}

// ROADMAP C51: the beacon sources the coordinator reads, and its wait for the
// lock round (`ChainWait::lock_round_signature`). Signatures from published
// rounds (fixtures/tlock/drand/quicknet-c51-rounds.json); no network.

const C51_ROUNDS: &str = include_str!("../../../fixtures/tlock/drand/quicknet-c51-rounds.json");

fn c51_fixture() -> FixtureBeacon {
    FixtureBeacon::from_json(C51_ROUNDS).expect("fixture")
}

#[test]
fn c51_a_beacon_source_is_never_trusted() {
    let q = QuicknetTimeLock::pinned().expect("pinned");
    let f = c51_fixture();
    let r = f.rounds().next().expect("a round");
    let sig = f.signature(r).expect("read").expect("held");
    assert_eq!(
        verified_signature(&q, &f, r).expect("read"),
        Some(sig.clone())
    );
    // Held for another round number, forged, or truncated: never returned.
    let moved = FixtureBeacon::of([(r + 1, sig.clone())]);
    assert_eq!(verified_signature(&q, &moved, r + 1).expect("read"), None);
    let mut bad = sig.clone();
    bad[10] ^= 1;
    assert_eq!(
        verified_signature(&q, &FixtureBeacon::of([(r, bad)]), r).expect("read"),
        None
    );
    assert_eq!(
        verified_signature(&q, &FixtureBeacon::of([(r, sig[..47].to_vec())]), r).expect("read"),
        None
    );
    assert_eq!(
        verified_signature(&q, &FixtureBeacon::default(), r).expect("read"),
        None
    );
    // A file for another chain, a duplicated round, a non-hex signature.
    assert!(FixtureBeacon::from_json(&C51_ROUNDS.replace(
        &hex::encode(QUICKNET_CHAIN_HASH),
        &hex::encode([0x11u8; 32])
    ))
    .is_err());
    let mut v: serde_json::Value = serde_json::from_str(C51_ROUNDS).expect("json");
    let first = v["rounds"][0].clone();
    v["rounds"].as_array_mut().expect("rounds").push(first);
    assert!(FixtureBeacon::from_json(&v.to_string()).is_err());
    let mut v: serde_json::Value = serde_json::from_str(C51_ROUNDS).expect("json");
    v["rounds"][0]["signature"] = "zz".into();
    assert!(FixtureBeacon::from_json(&v.to_string()).is_err());
}

#[test]
fn c51_the_coordinator_waits_for_a_verified_lock_round_and_nothing_else() {
    use crate::observe::{ChainWait, SimulatedRegistry};
    let f = c51_fixture();
    // A reveal close whose lock round the fixture holds.
    let rho = f.rounds().nth(3).expect("a round");
    let rc = close_for(rho);
    let mut reg = SimulatedRegistry::new();
    let w = ChainWait::NOW;
    let e = w.lock_round_signature(&reg, rc).expect_err("no source");
    assert!(e.contains("waits") && e.contains("--beacon"), "{e}");
    reg.beacon = Some(FixtureBeacon::of([(
        rho,
        f.signature(rho + 1).expect("r").expect("h"),
    )]));
    let e = w
        .lock_round_signature(&reg, rc)
        .expect_err("another round's");
    assert!(
        e.contains("not the pinned chain's") && e.contains("waits"),
        "{e}"
    );
    reg.beacon = Some(FixtureBeacon::of([(
        rho - 1,
        f.signature(rho - 1).expect("r").expect("h"),
    )]));
    let e = w.lock_round_signature(&reg, rc).expect_err("not yet");
    assert!(e.contains("not published yet"), "{e}");
    reg.beacon = Some(f.clone());
    assert_eq!(
        w.lock_round_signature(&reg, rc).expect("published"),
        (rho, f.signature(rho).expect("r").expect("h"))
    );
    // One second earlier is the previous round's window: it needs round rho
    // only if rho is the first at or after rc - 1 + 60.
    assert_eq!(
        lock_round(&BeaconChain::quicknet(), rc),
        Some(rho),
        "close_for gives the round's own reveal close"
    );
}

/// Opt-in, needs the network (never in check-all): the coordinator's live
/// beacon source (`--beacon drand`: api.drand.sh and its sibling relays, no
/// account, a generic User-Agent) returns the PUBLISHED signature of a fixture
/// round, which verifies against the pinned key; a round far in the future is
/// not published (`None`: the round would wait).
/// `ONYX_DRAND_LIVE=1 cargo test -p onyx-coordinator c51_live -- --ignored`.
#[test]
#[ignore = "network: ONYX_DRAND_LIVE=1, reads api.drand.sh"]
fn c51_live_drand_serves_the_published_lock_round() {
    if std::env::var("ONYX_DRAND_LIVE").as_deref() != Ok("1") {
        eprintln!("ONYX_DRAND_LIVE is not 1: skipped");
        return;
    }
    let q = QuicknetTimeLock::pinned().expect("pinned");
    let f = c51_fixture();
    let live = DrandHttp::default();
    for r in f.rounds().take(3) {
        let got = verified_signature(&q, &live, r)
            .expect("relays")
            .expect("published");
        assert_eq!(Some(got), f.signature(r).expect("read"), "round {r}");
    }
    assert_eq!(live.signature(u64::MAX / 4).expect("relays"), None);
}

/// Review C51-R1-2: a relay answering the right round with a forged or
/// another round's signature is skipped, so one lying relay cannot stall
/// every time-locked round while another relay is honest.
#[test]
fn c51_r1_2_a_lying_relay_is_skipped_for_the_next() {
    let f = c51_fixture();
    let r = f.rounds().nth(2).expect("a round");
    let good = f.signature(r).expect("r").expect("h");
    let other = f.signature(r + 1).expect("r").expect("h");
    let mut forged = good.clone();
    forged[3] ^= 1;
    assert_eq!(
        first_verified(r, [forged.clone(), other.clone(), good.clone()]),
        Some(good.clone())
    );
    assert_eq!(first_verified(r, [forged, other, vec![]]), None);
}

// ROADMAP C52 (ADR-0027 amendment 15): the far-date unseal copy. A second
// time-lock of the body key to the fact's far round, so every round unseals
// without the coordinator (THREATS REVLOG-5, C52-*).

fn rights() -> DataRights {
    spec().rights
}

fn dated_template() -> FeedTemplate {
    FeedTemplate {
        feed: "example.rate".into(),
        metric: MetricId("example.rate".into()),
        fetcher: keccak256(b"fetcher"),
        url_template: "https://example.org/{date}.json".into(),
        selector: "/rate".into(),
        kind: ObservationKind::Numeric {
            scale: 2,
            tolerance: 0,
        },
        window_secs: 300,
        min_observers: 3,
        rights: rights(),
        subject: onyx_core::SubjectRule::Dated {
            prefix: "example.rate".into(),
            lag: onyx_core::DateLag {
                min_business_days: 1,
                max_calendar_days: 10,
                first_day: day("2026-01-01"),
                last_day: day("2035-12-31"),
                holidays: vec![],
            },
        },
    }
}

fn day(d: &str) -> i64 {
    onyx_core::day_number(d).expect("date")
}

fn at(d: &str) -> u64 {
    u64::try_from(day(d)).expect("after 1970") * 86_400
}

/// THREATS C52-1: the far round is a function of the registered template and
/// the spec alone (the fact's last observable day), the same for every round
/// of the fact whatever its window or reveal phase, after every possible
/// reveal close by at least a day, and refused when a window's reveal close
/// could reach it (`FarTooSoon`) or it lies past the horizon
/// (`FarBeyondHorizon`).
#[test]
fn c52_1_the_far_round_is_the_facts_and_anyone_recomputes_it() {
    let q = BeaconChain::quicknet();
    let t = dated_template();
    let s = Subject::Aggregate("example.rate:2026-10-08".into());
    // Last observable day 2026-10-18: the latest window starts 2026-10-18
    // 23:59:59; its latest reveal close is 300 + 600 + 86,400 s later; the far
    // date a day after that.
    let latest = at("2026-10-19") - 1 + 300 + 600 + 86_400;
    let want = q
        .first_round_at_or_after(latest + UNSEAL_MARGIN_SECS)
        .expect("round");
    let mut seen = 0;
    for d in day("2026-10-08")..=day("2026-10-20") {
        for off in [0, 3_600, 86_399] {
            let ws = u64::try_from(d).expect("pos") * 86_400 + off;
            let Ok(sp) = t.instantiate(&s, ws) else {
                continue;
            };
            assert_eq!(latest_reveal_close(&t, &sp), Some(latest));
            for rc in [sp.window_end + 660, sp.window_end + 600 + MAX_REVEAL_SECS] {
                let r = LockRounds::of_round(&q, &t, &sp, rc).expect("rounds");
                assert_eq!(r.far(), want, "one far round for the whole fact");
                assert!(r.far() > r.lock());
                assert_eq!(Some(r.lock()), lock_round(&q, rc));
                let far_at = q.round_time(r.far()).expect("t");
                assert!(far_at >= rc + UNSEAL_MARGIN_SECS, "a day past this close");
                seen += 1;
            }
        }
    }
    assert!(seen >= 18, "windows on the allowed days: {seen}");
    // A reveal close later than any config allows reaches the far date.
    let last = t
        .instantiate(&s, at("2026-10-19") - 1)
        .expect("last window");
    assert_eq!(
        LockRounds::of_round(&q, &t, &last, latest + 1),
        Err(TimeLockError::FarTooSoon(latest + UNSEAL_MARGIN_SECS))
    );
    // A fixed subject: its fact is the window, so it unseals a day after the
    // window's latest reveal close.
    let mut f = dated_template();
    let fs = Subject::Entity("entity:alpha".into());
    f.subject = onyx_core::SubjectRule::Fixed(fs.clone());
    f.url_template = "https://example.org/a.json".into();
    let fsp = f.instantiate(&fs, 1_792_238_100).expect("on the grid");
    assert_eq!(
        unseal_time(&f, &fsp),
        Some(fsp.window_end + 600 + 86_400 + UNSEAL_MARGIN_SECS)
    );
    // A period subject: EDGAR's 110..=730 days fits the horizon; 2,000 days
    // does not (seal and intake refuse; the template cannot run time-locked).
    let mut p = dated_template();
    p.url_template = "https://example.org/doc.json".into();
    p.selector = "end={date}".into();
    p.subject = onyx_core::SubjectRule::Period {
        prefix: "co.x".into(),
        min_days: 110,
        max_days: 730,
    };
    let ps = Subject::Aggregate("co.x:2026-06-30".into());
    let first = p
        .instantiate(
            &ps,
            u64::try_from(day("2026-06-30") + 110).expect("pos") * 86_400,
        )
        .expect("earliest window");
    let r = LockRounds::of_round(&q, &p, &first, first.window_end + 660).expect("fits");
    let delay = q.round_time(r.far()).expect("t") - (first.window_end + 660);
    assert!(delay < 623 * 86_400 && delay > 621 * 86_400, "{delay}");
    p.subject = onyx_core::SubjectRule::Period {
        prefix: "co.x".into(),
        min_days: 110,
        max_days: 2_000,
    };
    let first = p
        .instantiate(
            &ps,
            u64::try_from(day("2026-06-30") + 110).expect("pos") * 86_400,
        )
        .expect("earliest window");
    assert!(matches!(
        LockRounds::of_round(&q, &p, &first, first.window_end + 660),
        Err(TimeLockError::FarBeyondHorizon(_))
    ));
    // A daily subject (NS3): the USGS feed's 100..=120 days unseals about
    // 23 days after its earliest window (20 days, the last day itself and the
    // margin), well inside the horizon.
    let mut d = dated_template();
    d.url_template = "https://example.org/c?s={date}T00:00:00&e={date}T23:59:59.999".into();
    d.subject = onyx_core::SubjectRule::Daily {
        prefix: "quakes.m5".into(),
        min_days: 100,
        max_days: 120,
    };
    let ds = Subject::Aggregate("quakes.m5:2026-04-02".into());
    let first = d
        .instantiate(
            &ds,
            u64::try_from(day("2026-04-02") + 100).expect("pos") * 86_400,
        )
        .expect("earliest window");
    assert!(first.url.contains("s=2026-04-02T00") && first.url.contains("e=2026-04-02T23"));
    let r = LockRounds::of_round(&q, &d, &first, first.window_end + 660).expect("fits");
    let delay = q.round_time(r.far()).expect("t") - (first.window_end + 660);
    assert!(delay > 22 * 86_400 && delay < 24 * 86_400, "{delay}");
    // Review C52-R2-2: the horizon's and the margin's exact edges. A far date
    // exactly `MAX_UNSEAL_DELAY_SECS` after the reveal close is taken, a
    // second more is not; exactly `UNSEAL_MARGIN_SECS` is taken, a second
    // less is not.
    let rc = 1_800_000_000;
    assert!(LockRounds::for_window(&q, rc, rc + MAX_UNSEAL_DELAY_SECS).is_ok());
    assert_eq!(
        LockRounds::for_window(&q, rc, rc + MAX_UNSEAL_DELAY_SECS + 1),
        Err(TimeLockError::FarBeyondHorizon(
            rc + MAX_UNSEAL_DELAY_SECS + 1
        ))
    );
    assert!(LockRounds::for_window(&q, rc, rc + UNSEAL_MARGIN_SECS).is_ok());
    assert_eq!(
        LockRounds::for_window(&q, rc, rc + UNSEAL_MARGIN_SECS - 1),
        Err(TimeLockError::FarTooSoon(rc + UNSEAL_MARGIN_SECS - 1))
    );
    // A spec that is not the template's has no far date (fails closed).
    let mut other = t.instantiate(&s, at("2026-10-10")).expect("spec");
    other.url = "https://example.org/other.json".into();
    assert_eq!(unseal_time(&t, &other), None);
    assert_eq!(
        LockRounds::of_round(&q, &t, &other, other.window_end + 660),
        Err(TimeLockError::NoUnsealTime)
    );
    assert_eq!(
        LockRounds::new(7, 7),
        Err(TimeLockError::FarNotAfterLock { lock: 7, far: 7 })
    );
    // Review C52-R1-2: the far copy redistributes every value for free, so a
    // feed whose rights restrict redistribution runs no time-locked draw.
    let mut restricted = dated_template();
    restricted.rights.redistribution = onyx_core::Redistribution::Restricted;
    let rs = restricted.instantiate(&s, at("2026-10-10")).expect("spec");
    assert_eq!(
        LockRounds::of_round(&q, &restricted, &rs, rs.window_end + 660),
        Err(TimeLockError::NotRedistributable)
    );
}

/// The parts of observer `i`'s honest post under the mock and `rounds()`:
/// (near time-lock, far copy, wrapped key, body, R).
fn honest_parts(i: u8, v: i128) -> (Obs, Tlock, Tlock, [u8; 16], Vec<u8>, Point) {
    let b = MockBeacon::pinned();
    let o = observer(i, v);
    let s = secrets(i);
    let rs = rounds();
    let lock = b.lock(rs.lock(), &s.tau, &s.sigma).expect("lock");
    let far = far_copy(&b, rs.far(), &s.kappa).expect("far");
    let eph = public_point(&s.ephemeral).expect("R");
    let shared = shared_point(&s.ephemeral, &key()).expect("S");
    let wrap = wrap_key(
        &shared,
        &s.tau,
        &eph,
        &lock,
        &far,
        rs,
        &QUICKNET_CHAIN_HASH,
        &round_id(),
        ATTEMPT,
        &o.address,
    );
    let sig = crate::observe::sign_reveal(&o.secret, &round_id(), ATTEMPT, &obs(v), &o.salt)
        .expect("sig");
    let mut plain = borsh::to_vec(&Plain {
        observation: obs(v),
        salt: o.salt,
        sig,
    })
    .expect("borsh");
    plain.resize(sealed_len(&spec()), 0);
    (o, lock, far, xor16(&wrap, &s.kappa), plain, eph)
}

/// Seal `(lock, far, w)` in front of `plain` encrypted under `kappa`'s body
/// key, as observer `o` with secrets `i`.
#[allow(clippy::too_many_arguments)]
fn post_of(
    o: &Obs,
    i: u8,
    eph: &Point,
    lock: &Tlock,
    far: &Tlock,
    w: &[u8; 16],
    kappa: &[u8; 16],
    plain: &[u8],
) -> PostedSealed {
    let k = body_key(
        kappa,
        eph,
        lock,
        far,
        rounds(),
        &QUICKNET_CHAIN_HASH,
        &round_id(),
        ATTEMPT,
        &o.address,
    );
    let p = seal_parts(
        &o.secret,
        &o.address,
        &secrets(i).ephemeral,
        eph,
        &round_id(),
        ATTEMPT,
        &[lock, far, w, &stream_xor(&k, plain)],
    )
    .expect("seal");
    assert!(post_signed(&p, &round_id(), ATTEMPT));
    posted(&p)
}

/// The mock's clock at the far round, so it hands out the far signature too.
fn at_far() -> MockBeacon {
    let b = MockBeacon::pinned();
    b.now
        .set(BeaconChain::quicknet().round_time(far()).expect("t"));
    b
}

/// THREATS C52-2 (REVLOG-5's residual): once the far round is out, ANYONE
/// unseals a post with the far round's signature alone (no reveal key, no
/// `S`, no `tau`) to the very reveal the coordinator took; before it the far
/// path opens nothing (`BeaconMissing`), and another round's signature is
/// refused (`BadBeacon`), never tried.
#[test]
fn c52_2_a_post_unseals_at_the_far_round_without_the_coordinator() {
    let b = at_far();
    let lock_sig = b.signature(rho()).expect("published");
    let far_sig = b.signature(far()).expect("published");
    let s = spec();
    let (o, p) = sealed_by(&b, 1, 4_242, rounds());
    let pp = posted(&p);
    let Ok(TimedOpened::Reveal(took)) = open(&b, Some(&lock_sig), &o, &pp) else {
        panic!("the coordinator takes the honest post");
    };
    let unsealed = unseal_far(&b, Some(&far_sig), &o.address, &pp, &ctx(&o, &s), rounds());
    assert_eq!(unsealed, Ok(Unsealed::Reveal(took)));
    assert_eq!(
        unseal_far(&b, None, &o.address, &pp, &ctx(&o, &s), rounds()),
        Err(TimeLockError::BeaconMissing(far()))
    );
    assert_eq!(
        unseal_far(&b, Some(&lock_sig), &o.address, &pp, &ctx(&o, &s), rounds()),
        Err(TimeLockError::BadBeacon(far()))
    );
    // A second before the far round the mock has no far signature to give.
    let early = MockBeacon::pinned();
    early
        .now
        .set(BeaconChain::quicknet().round_time(far()).expect("t") - 1);
    assert_eq!(early.signature(far()), None);
    // Garbage unseals as garbage; an amendment 10 post or a post too short
    // for the far copy unseals to nothing.
    let junk = seal_bytes_timelocked(
        &b,
        &o.secret,
        &o.address,
        &secrets(1),
        &key(),
        &round_id(),
        ATTEMPT,
        rounds(),
        SEAL_NOW,
        b"junk",
    )
    .expect("seal");
    assert_eq!(
        unseal_far(
            &b,
            Some(&far_sig),
            &o.address,
            &posted(&junk),
            &ctx(&o, &s),
            rounds()
        ),
        Ok(Unsealed::Garbage(b"junk".to_vec()))
    );
    let plain10 = seal(
        &o.secret,
        &secrets(1).ephemeral,
        &key(),
        &round_id(),
        ATTEMPT,
        &s,
        &obs(4_242),
        &o.salt,
    )
    .expect("seal");
    assert_eq!(
        unseal_far(
            &b,
            Some(&far_sig),
            &o.address,
            &posted(&plain10),
            &ctx(&o, &s),
            rounds()
        ),
        Ok(Unsealed::Garbage(Vec::new()))
    );
    // A wrong commit (another round's context) never unseals as a reveal.
    let mut other = ctx(&o, &s);
    other.commit = Some(keccak256(b"another commit"));
    assert!(matches!(
        unseal_far(&b, Some(&far_sig), &o.address, &pp, &other, rounds()),
        Ok(Unsealed::Garbage(_))
    ));
}

/// THREATS C52-3: the coordinator REFUSES at intake a post whose far copy
/// does not carry the body key it unwraps (a copy of another key, the right
/// key under a sigma that is not `far_sigma`, a copy locked to the LOCK round
/// so the value would be public right after reveal close, junk), even when its
/// body is the observer's valid reveal: it is dismissed with its opening, and
/// every client accepts that dismissal (the far copy re-encrypts wrongly). It
/// is checked by exact re-encryption at intake, BEFORE the far round exists
/// (the far copy's sigma is a function of the key), so no post the
/// coordinator takes can fail to unseal later.
#[test]
fn c52_3_a_far_copy_that_does_not_carry_the_body_key_is_dismissed_at_intake() {
    let b = at_far();
    let lock_sig = b.signature(rho()).expect("published");
    let far_sig = b.signature(far()).expect("published");
    let s = spec();
    let i = 7;
    let (o, lock, far_ok, w, plain, eph) = honest_parts(i, 900);
    let kappa = secrets(i).kappa;
    // The honest parts assemble to a post the coordinator takes.
    let honest = post_of(&o, i, &eph, &lock, &far_ok, &w, &kappa, &plain);
    assert!(matches!(
        open(&b, Some(&lock_sig), &o, &honest),
        Ok(TimedOpened::Reveal(_))
    ));
    let other_kappa = [0x99u8; 16];
    let bad_fars: Vec<(&str, Tlock)> = vec![
        (
            "another key",
            far_copy(&b, far(), &other_kappa).expect("far"),
        ),
        (
            "a random sigma",
            b.lock(far(), &kappa, &[0x55; 16]).expect("far"),
        ),
        (
            "locked to the lock round",
            b.lock(rho(), &kappa, &far_sigma(&kappa, rho()))
                .expect("far"),
        ),
        ("junk", [0x5a; TLOCK_LEN]),
    ];
    for (why, bad) in &bad_fars {
        // The wrap must be over THIS post's far copy (both time-locks are in
        // the transcript), so recompute it: only the far copy is wrong.
        let shared = shared_point(&secrets(i).ephemeral, &key()).expect("S");
        let wrap = wrap_key(
            &shared,
            &secrets(i).tau,
            &eph,
            &lock,
            bad,
            rounds(),
            &QUICKNET_CHAIN_HASH,
            &round_id(),
            ATTEMPT,
            &o.address,
        );
        let pb = post_of(
            &o,
            i,
            &eph,
            &lock,
            bad,
            &xor16(&wrap, &kappa),
            &kappa,
            &plain,
        );
        let Ok(TimedOpened::Dismissed(d)) = open(&b, Some(&lock_sig), &o, &pb) else {
            panic!("{why}: a post with a bad far copy was taken");
        };
        assert_eq!(d.opening.map(|x| x.secret), Some(secrets(i).tau), "{why}");
        // The client accepts the dismissal although the body IS a valid
        // reveal: the far copy does not re-encrypt from the unwrapped key.
        let shown = check(&b, None, &o, &pb, &d).expect(why);
        assert!(
            valid_reveal(&shown, &o.address, &ctx(&o, &s)).is_some(),
            "{why}: the dismissed body is the observer's own valid reveal"
        );
        // At the far round it does not unseal as that reveal either.
        assert!(
            matches!(
                unseal_far(&b, Some(&far_sig), &o.address, &pb, &ctx(&o, &s), rounds()),
                Ok(Unsealed::Garbage(_))
            ),
            "{why}"
        );
    }
    // A wrong wrap with a right far copy and body: the coordinator cannot
    // reach the key (dismissed, the client agrees); the far round unseals it
    // (the poster's own loss: its value is published either way).
    let mut wrong_w = w;
    wrong_w[0] ^= 1;
    let pw = post_of(&o, i, &eph, &lock, &far_ok, &wrong_w, &kappa, &plain);
    let Ok(TimedOpened::Dismissed(d)) = open(&b, Some(&lock_sig), &o, &pw) else {
        panic!("a post with a wrong wrap was taken");
    };
    assert!(check(&b, None, &o, &pw, &d).is_ok());
    assert!(matches!(
        unseal_far(&b, Some(&far_sig), &o.address, &pw, &ctx(&o, &s), rounds()),
        Ok(Unsealed::Reveal(_))
    ));
    // A coordinator cannot dismiss the honest post on a far-copy pretext:
    // with the true opening the client re-encrypts the far copy, finds it
    // right and the body valid: a false dismissal.
    let fake = TimedDismissal {
        dismissal: dismiss(
            &REVEAL_SECRET,
            &SealedPost::from_posted(&o.address, &honest),
            &round_id(),
            ATTEMPT,
        )
        .expect("dleq"),
        opening: b.open(rho(), &lock_sig, &lock),
    };
    assert_eq!(
        check(&b, None, &o, &honest, &fake),
        Err(SealError::FalseDismissal.into())
    );
    // Nor by naming another far round for the window: the far copy then
    // fails for EVERY post, honest ones included, which a client with the
    // template's far round refuses as false.
    let shifted = LockRounds::new(rho(), far() + 1).expect("rounds");
    let Ok(TimedOpened::Dismissed(d2)) = open_timelocked(
        &b,
        &REVEAL_SECRET,
        Some(&lock_sig),
        &o.address,
        &honest,
        &ctx(&o, &s),
        shifted,
    ) else {
        panic!("under another far round the honest post fails its far copy");
    };
    assert_eq!(
        check(&b, None, &o, &honest, &d2),
        Err(SealError::FalseDismissal.into())
    );
}

/// A post whose near time-lock opens but which is too short to hold the far
/// copy, the wrapped key and a body byte is garbage on its face: dismissed
/// with its opening, checked, and it unseals to nothing (the registry refuses
/// such a post in a time-locked draw anyway, `TIME_LOCKED_HEAD_BYTES`).
#[test]
fn c52_a_post_too_short_for_the_far_copy_is_garbage() {
    let b = at_far();
    let lock_sig = b.signature(rho()).expect("published");
    let far_sig = b.signature(far()).expect("published");
    let s = spec();
    let i = 8;
    let (o, lock, far_ok, w, _, eph) = honest_parts(i, 1);
    for extra in [1usize, 100, 144] {
        let mut ct = lock.to_vec();
        ct.extend_from_slice(&far_ok[..extra.min(TLOCK_LEN)]);
        if extra > TLOCK_LEN {
            ct.extend_from_slice(&w[..extra - TLOCK_LEN]);
        }
        let key_proof =
            key_proof(&secrets(i).ephemeral, &round_id(), ATTEMPT, &o.address).expect("proof");
        let pp = PostedSealed {
            ephemeral: eph,
            key_proof,
            posted_at: REVEAL_CLOSE - 10,
            ciphertext: ct,
        };
        let Ok(TimedOpened::Dismissed(d)) = open(&b, Some(&lock_sig), &o, &pp) else {
            panic!("{extra}: a short post was taken");
        };
        assert!(d.opening.is_some(), "{extra}: its near time-lock opens");
        assert_eq!(check(&b, None, &o, &pp, &d), Ok(Vec::new()), "{extra}");
        assert_eq!(
            unseal_far(&b, Some(&far_sig), &o.address, &pp, &ctx(&o, &s), rounds()),
            Ok(Unsealed::Garbage(Vec::new()))
        );
    }
}

/// The far copy with the REAL scheme and REAL published quicknet signatures:
/// a post locked to round 33,152,429 (lock) and 33,154,000 (far) opens for the
/// coordinator with the lock round's signature and unseals for anyone with the
/// far round's, to the same reveal; a changed far-copy byte is dismissed at
/// intake (re-encryption) and unseals as garbage (Fujisaki-Okamoto).
#[test]
fn c52_real_far_copy_opens_only_with_the_far_rounds_published_signature() {
    let q = QuicknetTimeLock::pinned().expect("pinned");
    let f = c51_fixture();
    let lock = 33_152_429;
    let lock_sig = f.signature(lock).expect("read").expect("held");
    let (far_round, far_sig) = real_round();
    let rs = LockRounds::new(lock, far_round).expect("rounds");
    let s = spec();
    let (o, p) = sealed_by(&q, 2, 7_777, rs);
    let pp = posted(&p);
    let Ok(TimedOpened::Reveal(took)) = open_at(&q, Some(&lock_sig), &o, &pp, rs) else {
        panic!("the real lock signature opens the post");
    };
    assert_eq!(took.1, obs(7_777));
    assert_eq!(
        unseal_far(&q, Some(&far_sig), &o.address, &pp, &ctx(&o, &s), rs),
        Ok(Unsealed::Reveal(took))
    );
    assert_eq!(
        unseal_far(&q, Some(&lock_sig), &o.address, &pp, &ctx(&o, &s), rs),
        Err(TimeLockError::BadBeacon(far_round))
    );
    // The far copy is drand/tlock's own ciphertext of the body key: it opens
    // under the published far signature to (far_sigma(kappa), kappa).
    let tf: Tlock = p.ciphertext[TLOCK_LEN..2 * TLOCK_LEN]
        .try_into()
        .expect("tf");
    let o2 = open_canonical(&q, far_round, &far_sig, &tf).expect("opens");
    assert_eq!(o2.secret, secrets(2).kappa);
    assert_eq!(o2.sigma, far_sigma(&secrets(2).kappa, far_round));
    for at in [
        TLOCK_LEN,
        TLOCK_LEN + 50,
        TLOCK_LEN + 100,
        2 * TLOCK_LEN - 1,
    ] {
        let mut bad = pp.clone();
        bad.ciphertext[at] ^= 1;
        let Ok(TimedOpened::Dismissed(d)) = open_at(&q, Some(&lock_sig), &o, &bad, rs) else {
            panic!("byte {at}: a changed far copy was taken");
        };
        assert!(d.opening.is_some(), "byte {at}: the near time-lock opens");
        assert!(check_at(&q, None, &o, &bad, &d, rs).is_ok(), "byte {at}");
        assert!(matches!(
            unseal_far(&q, Some(&far_sig), &o.address, &bad, &ctx(&o, &s), rs),
            Ok(Unsealed::Garbage(_))
        ));
    }
}

const FAR_DATE: &str = include_str!("../../../fixtures/tlock/onyx-far-date.json");

/// The far-copy vectors (`fixtures/tlock/onyx-far-date.json`), the same file
/// `onyx-tlock`'s known-answer checks run natively and on wasm32
/// (`scripts/check-tlock-wasm.sh`): each body key, its `far_sigma` and its
/// far copy locked to a published quicknet round, which opens under the
/// round's real signature. This pins the coordinator's derivation to the
/// bytes both targets check. Regenerate (only with an ADR change):
/// `ONYX_BLESS=1 cargo test -p onyx-coordinator c52_far_date_vectors`.
#[test]
fn c52_far_date_vectors_match_the_fixture() {
    let q = QuicknetTimeLock::pinned().expect("pinned");
    let beacons: [(&str, &str); 5] = [
        (
            "drand/quicknet-round-1.json",
            include_str!("../../../fixtures/tlock/drand/quicknet-round-1.json"),
        ),
        (
            "drand/quicknet-round-2.json",
            include_str!("../../../fixtures/tlock/drand/quicknet-round-2.json"),
        ),
        (
            "drand/quicknet-round-1000.json",
            include_str!("../../../fixtures/tlock/drand/quicknet-round-1000.json"),
        ),
        (
            "drand/quicknet-round-746511.json",
            include_str!("../../../fixtures/tlock/drand/quicknet-round-746511.json"),
        ),
        (
            "drand/quicknet-round-33154000.json",
            include_str!("../../../fixtures/tlock/drand/quicknet-round-33154000.json"),
        ),
    ];
    let kappas: [[u8; 16]; 5] = [
        [0x31; 16],
        h16(b"far kappa", &[b"2"]),
        {
            let mut k = [0x33; 16];
            k[12..].fill(0);
            k
        },
        [0u8; 16],
        [0xff; 16],
    ];
    let mut vectors = vec![];
    for ((file, json), kappa) in beacons.iter().zip(kappas) {
        let (round, sig) = published(json);
        let sigma = far_sigma(&kappa, round);
        let ct = far_copy(&q, round, &kappa).expect("lock");
        let o = open_canonical(&q, round, &sig, &ct).expect("opens");
        assert_eq!((o.secret, o.sigma), (kappa, sigma), "round {round}");
        vectors.push(serde_json::json!({
            "beacon": file,
            "round": round,
            "kappa": hex::encode(kappa),
            "sigma": hex::encode(sigma),
            "ciphertext": hex::encode(ct),
        }));
    }
    let made = serde_json::json!({
        "about": "Made by onyx-coordinator (ROADMAP C52, ADR-0027 amendment 15), NOT by drand/tlock: far copies of a body key kappa, tlock(round, kappa, sigma) with sigma = keccak256(\"onyx-sealed-far-sigma-v1\" || kappa || round as 8 bytes big endian)[..16], each locked to a published quicknet round in fixtures/tlock/drand and opened with its real signature. Cross-target consistency vectors (native and wasm32), not interop vectors. Regenerate: ONYX_BLESS=1 cargo test -p onyx-coordinator c52_far_date_vectors.",
        "vectors": vectors,
    });
    let text = format!("{}\n", serde_json::to_string_pretty(&made).expect("json"));
    if std::env::var("ONYX_BLESS").as_deref() == Ok("1") {
        std::fs::write(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/tlock/onyx-far-date.json"
            ),
            &text,
        )
        .expect("bless");
        return;
    }
    assert_eq!(FAR_DATE, text, "fixtures/tlock/onyx-far-date.json is stale");
}

/// THREATS C52-2 and REVLOG-5 at round level: a sealed round of five drawn
/// observers (three honest, one posting garbage, one posting a valid body
/// behind a bad far copy, one withholding) is opened by the coordinator at
/// intake; at the far round ANYONE, holding only the posts and the far
/// round's signature, unseals every post (`unseal_posts`) and recomputes the
/// round's revealed set and attempt record: it is the coordinator's. A
/// coordinator that listed a garbage or withheld observer as revealed, or left
/// an honest one out, served a record that the unsealed posts contradict.
#[test]
fn c52_4_a_sealed_round_unseals_without_the_coordinator_and_a_false_record_is_caught() {
    use crate::observe::{ObserveConfig, Observer};
    use crate::sealed::reveal_log_query;
    use std::collections::{BTreeMap, BTreeSet};
    let b = at_far();
    let lock_sig = b.signature(rho()).expect("published");
    let far_sig = b.signature(far()).expect("published");
    let s = spec();
    // Observers 1..=7: 1-3 honest, 4 garbage, 5 a bad far copy, 6 withholds,
    // 7 a valid far copy and body behind a near time-lock that does not open
    // (review C52-R1-1: rightly dismissed, yet it unseals).
    let mut posts = vec![];
    let mut commits = BTreeMap::new();
    let mut observers = vec![];
    for i in 1..=7u8 {
        let o = observer(i, 100);
        commits.insert(o.address, o.commit);
        observers.push(Observer {
            address: o.address,
            bond: 1_000,
        });
        let p = match i {
            1..=3 => Some(sealed_by(&b, i, 100, rounds()).1),
            4 => Some(
                seal_bytes_timelocked(
                    &b,
                    &o.secret,
                    &o.address,
                    &secrets(i),
                    &key(),
                    &round_id(),
                    ATTEMPT,
                    rounds(),
                    SEAL_NOW,
                    &[0x44; 154],
                )
                .expect("seal"),
            ),
            5 => {
                let (o5, lock, _, _, plain, eph) = honest_parts(i, 100);
                let bad = far_copy(&b, far(), &[0x55; 16]).expect("far");
                let shared = shared_point(&secrets(i).ephemeral, &key()).expect("S");
                let wrap = wrap_key(
                    &shared,
                    &secrets(i).tau,
                    &eph,
                    &lock,
                    &bad,
                    rounds(),
                    &QUICKNET_CHAIN_HASH,
                    &round_id(),
                    ATTEMPT,
                    &o5.address,
                );
                let pp = post_of(
                    &o5,
                    i,
                    &eph,
                    &lock,
                    &bad,
                    &xor16(&wrap, &secrets(i).kappa),
                    &secrets(i).kappa,
                    &plain,
                );
                posts.push((o5.address, pp));
                None
            }
            7 => {
                let (o7, _, far_ok, _, plain, eph) = honest_parts(i, 100);
                let junk = [0x5a; TLOCK_LEN];
                let shared = shared_point(&secrets(i).ephemeral, &key()).expect("S");
                let wrap = wrap_key(
                    &shared,
                    &secrets(i).tau,
                    &eph,
                    &junk,
                    &far_ok,
                    rounds(),
                    &QUICKNET_CHAIN_HASH,
                    &round_id(),
                    ATTEMPT,
                    &o7.address,
                );
                let pp = post_of(
                    &o7,
                    i,
                    &eph,
                    &junk,
                    &far_ok,
                    &xor16(&wrap, &secrets(i).kappa),
                    &secrets(i).kappa,
                    &plain,
                );
                posts.push((o7.address, pp));
                None
            }
            _ => None,
        };
        if let Some(p) = p {
            posts.push((o.address, posted(&p)));
        }
    }
    posts.sort_by_key(|(a, _)| *a);
    observers.sort_by_key(|o| o.address);
    // The coordinator's intake.
    let mut revealed = BTreeSet::new();
    let mut dismissed = BTreeSet::new();
    for (a, p) in &posts {
        let c = RevealContext {
            round_id: round_id(),
            attempt: ATTEMPT,
            spec: &s,
            commit: commits.get(a).copied(),
        };
        match open_timelocked(&b, &REVEAL_SECRET, Some(&lock_sig), a, p, &c, rounds())
            .expect("open")
        {
            TimedOpened::Reveal(_) => {
                revealed.insert(*a);
            }
            TimedOpened::Dismissed(d) => {
                check_timelocked_dismissal(&b, &key(), a, p, &c, rounds(), Some(&lock_sig), &d)
                    .expect("every dismissal checks");
                dismissed.insert(*a);
            }
        }
    }
    assert_eq!((revealed.len(), dismissed.len()), (3, 3));
    let cfg = ObserveConfig {
        commit_close: REVEAL_CLOSE - 600,
        reveal_close: REVEAL_CLOSE,
        reward: 1,
        outlier_slash: 2,
        miss_slash: 1,
    };
    let signed: Vec<(MinerAddress, Hash32, crate::observe::ObserverSig)> =
        commits.iter().map(|(a, h)| (*a, *h, [0u8; 65])).collect();
    let fact = keccak256(b"c52 fact");
    let result = keccak256(b"c52 result");
    let record = |rev: &BTreeSet<MinerAddress>, dis: &BTreeSet<MinerAddress>| {
        reveal_log_query(
            fact,
            &s,
            &cfg,
            ATTEMPT,
            round_id(),
            &observers,
            &signed,
            rev,
            dis,
        )
        .digest(&result)
    };
    let recorded = record(&revealed, &dismissed);
    // The far round: anyone, with the posts and the far signature only.
    let unsealed = unseal_posts(
        &b,
        &far_sig,
        &s,
        &round_id(),
        ATTEMPT,
        &commits,
        &posts,
        rounds(),
    )
    .expect("unseal");
    let unsealed_ok: BTreeSet<MinerAddress> = unsealed
        .iter()
        .filter_map(|(a, r)| r.as_ref().map(|_| *a))
        .collect();
    // The rule (review C52-R1-1): every revealed post unseals, and every post
    // that unseals is revealed OR dismissed with a proof that checks (observer
    // 7: its near time-lock does not open, so it was rightly dismissed).
    let seventh = observer(7, 100).address;
    assert!(revealed.is_subset(&unsealed_ok));
    assert!(unsealed_ok
        .iter()
        .all(|a| revealed.contains(a) || dismissed.contains(a)));
    assert_eq!(
        unsealed_ok
            .difference(&revealed)
            .copied()
            .collect::<Vec<_>>(),
        vec![seventh],
        "only the rightly dismissed seventh post unseals beyond the revealed set"
    );
    let mine: BTreeSet<MinerAddress> = unsealed_ok
        .iter()
        .filter(|a| !dismissed.contains(*a))
        .copied()
        .collect();
    assert_eq!(mine, revealed, "the unsealed posts give the revealed set");
    let others: BTreeSet<MinerAddress> = posts
        .iter()
        .map(|(a, _)| *a)
        .filter(|a| !mine.contains(a))
        .collect();
    assert_eq!(record(&mine, &others), recorded, "and the recorded digest");
    for (a, r) in &unsealed {
        if let Some((who, ob, salt, _)) = r {
            assert_eq!(*who, *a);
            assert_eq!(*ob, obs(100));
            assert_eq!(
                Some(observation_commit(&round_id(), a, ob, salt)),
                commits.get(a).copied()
            );
        }
    }
    // The coordinator's lies, each contradicted by the unsealed posts.
    let garbage = observer(4, 100).address;
    let withheld = observer(6, 100).address;
    let honest = observer(1, 100).address;
    for (why, lie) in [
        ("garbage listed as revealed", {
            let mut r = revealed.clone();
            r.insert(garbage);
            r
        }),
        ("a withheld observer listed as revealed", {
            let mut r = revealed.clone();
            r.insert(withheld);
            r
        }),
        ("an honest reveal left out", {
            let mut r = revealed.clone();
            r.remove(&honest);
            r
        }),
    ] {
        assert_ne!(record(&lie, &dismissed), recorded, "{why}: another digest");
        // Checked against the unsealed posts alone: some listed observer does
        // not unseal, or some unsealed post is neither listed nor dismissed.
        let caught = !lie.is_subset(&unsealed_ok)
            || unsealed_ok
                .iter()
                .any(|a| !lie.contains(a) && !dismissed.contains(a));
        assert!(caught, "{why}: the unsealed posts contradict it");
    }
    // A wrong far signature unseals nothing (refused, never guessed).
    assert_eq!(
        unseal_posts(
            &b,
            &lock_sig,
            &s,
            &round_id(),
            ATTEMPT,
            &commits,
            &posts,
            rounds()
        ),
        Err(TimeLockError::BadBeacon(far()))
    );
}
