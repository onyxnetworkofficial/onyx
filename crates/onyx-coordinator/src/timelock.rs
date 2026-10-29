//! Time-lock layer for sealed reveals (ADR-0027 amendments 12 and 13, ROADMAP
//! C12 and C50; THREATS C4-7, C12-*, C50-*). The real scheme is
//! [`QuicknetTimeLock`] (drand/tlock's IBE for quicknet, `onyx_tlock` on
//! `blst`, ROADMAP C50); the tests' mock beacon is `#[cfg(test)]` only. WIRED
//! by ROADMAP C51 (ADR-0027 amendment 14): a draw announced while the registry
//! requires time-locks (`drawTimeLocked`) is sealed with [`seal_timelocked`]
//! by the observer, opened by the coordinator only with the lock round's
//! signature from a [`BeaconSource`] verified against the pinned key, and its
//! dismissals carry the openings clients re-encrypt.
//!
//! Amendment 10 (C4) seals each reveal to the coordinator's reveal key, so the
//! coordinator can open every post as it lands, before `reveal_close` (C4-7, the
//! last-revealer problem). This layer makes the post's body key reachable only
//! through BOTH the coordinator's ECDH secret AND a 16-byte secret `tau` the
//! observer time-locks to a drand quicknet round at `reveal_close` (tlock,
//! Boneh-Franklin IBE where the identity is the round and the beacon's
//! threshold BLS signature of that round is the decryption key), or through a
//! second time-lock of the body key itself to the FAR round, the fact's unseal
//! date (ROADMAP C52, ADR-0027 amendment 15: every round unseals without the
//! coordinator, THREATS REVLOG-5):
//!
//! ```text
//! rho      = lock_round(reveal_close)          first quicknet round at or after
//!                                              reveal_close + LOCK_MARGIN_SECS
//! rho_far  = first round at or after unseal_time(template, spec): the fact's
//!            latest possible reveal close (its last observable day) + 1 day
//! TL       = tlock(rho, tau, sigma)            128 bytes: U (G2, 96) ‖ V (16) ‖ W (16)
//! TF       = tlock(rho_far, kappa, far_sigma(kappa))   128 bytes, deterministic in kappa
//! S        = r·K                               as in amendment 10
//! w        = kappa XOR wrap_key(S, R, TL, TF, tau, rho, rho_far, chain, round, attempt, observer)
//! k        = body_key(kappa, R, TL, TF, rho, rho_far, chain, round, attempt, observer)
//! post     = TL ‖ TF ‖ w ‖ (padded plain XOR keccak stream of k)
//! ```
//!
//! Before round `rho` nobody, the coordinator included, holds `tau`, and
//! before `rho_far` nobody holds `kappa` by the far path, so nobody opens a
//! post (closes C4-7 under drand's threshold assumption). After `rho` anyone
//! opens `TL`, but `kappa` still needs `S`, which only the coordinator (and
//! the observer) holds: values stay private until `rho_far`, when anyone opens
//! `TF`, reads `kappa` and every value of the round ([`unseal_far`]). The far
//! copy's IBE randomness is `far_sigma(kappa)` (encrypt-with-hash, Bellare,
//! Boldyreva and O'Neill 2007, for a fresh 128-bit secret), so the coordinator
//! checks the far copy at intake by exact re-encryption once it holds `kappa`
//! and dismisses a post whose copy differs: a post it takes always unseals.
//! A dismissal is amendment 10's `S` and DLEQ proof plus the time-lock opening
//! `(sigma, tau)`; anyone re-encrypts `tau` with `sigma` to round `rho`
//! (encryption is deterministic given them) and compares with `TL`, which
//! needs no beacon signature (but, with the real scheme, one pairing per
//! round), then unwraps `kappa` and re-encrypts the far copy. Only a post
//! whose `TL` does not open at all needs the round's signature to check.
//!
//! There is NO path that opens a post without the lock round's beacon
//! signature (or, later, the far round's): if drand is late the round waits;
//! if it never comes the posts stay sealed. A coordinator-only fallback copy
//! would bring the early view back (C12-4).
//!
//! The [`TimeLock`] trait is what a tlock implementation provides. No crate was
//! acceptable (RESEARCH.md "Time-lock crates", ADR-0027 amendment 12), so C50
//! wrote one: [`QuicknetTimeLock`], interop-tested against drand/tlock's Go
//! and tlock-js vectors and published quicknet rounds (`fixtures/tlock`). The
//! MOCK beacon (fixed test keys, `#[cfg(test)]`) stays for the composition
//! tests that need a beacon on a test clock; the `c50_*` and `c52_*` tests run
//! the same composition with the real scheme and real quicknet signatures.

use crate::observe::{
    random_salt, sign_reveal, SignedReveal, MAX_COMMIT_GRACE_SECS, MAX_REVEAL_SECS,
};
use crate::sealed::{
    dismiss, dismissal_proof_checks, key_proof, key_proof_valid, public_point, sealed_len,
    sealed_reveal_message, shared_point, stream_xor, valid_reveal, Dismissal, Plain, Point,
    PostedSealed, RevealContext, SealError, SealedPost, MAX_SEALED_BYTES, TIME_LOCKED_HEAD_BYTES,
};
use onyx_core::{keccak256, FeedTemplate, Hash32, MinerAddress, Observation, ObservationSpec};
use onyx_labels::attest::{eip191_sign_with, secret_address};

/// drand quicknet's chain hash (read from
/// <https://api.drand.sh/52db9ba70e0cc0f6eaf7803dd07447a1f5477735fd3f661792ba94600c84e971/info>
/// on 2026-10-17; scheme `bls-unchained-g1-rfc9380`: signatures on G1, the
/// group public key on G2).
pub const QUICKNET_CHAIN_HASH: [u8; 32] = onyx_tlock::QUICKNET_CHAIN_HASH;

/// drand quicknet's group public key (G2, compressed, 96 bytes), from the same
/// chain info. Pinned in code: a key served by the coordinator or set on chain
/// could be one the coordinator holds (THREATS C12-3). The bytes
/// [`QuicknetTimeLock`] uses are `onyx_tlock::QUICKNET_PUBLIC_KEY`; a test
/// pins the two equal.
pub const QUICKNET_PUBLIC_KEY_HEX: &str = "83cf0f2896adee7eb8b5f01fcad3912212c437e0073e911fb90022d3e760183c8c4b450b6a0a6c3ac6a5776a2d1064510d1fec758c921cc22b0e17e63aaf4bcb5ed66304de9cf809bd274ca73bab4af5a6e9c76a4bc09e76eae8991ef5ece45a";

/// drand quicknet's genesis time (unix seconds) and period.
pub const QUICKNET_GENESIS: u64 = 1_692_803_367;
/// Seconds between quicknet rounds.
pub const QUICKNET_PERIOD: u64 = 3;

/// How far past `reveal_close` the lock round lies. Posts are timed by BLOCK
/// time, drand by wall-clock time: if the chain's clock lags the wall clock by
/// more than this, the round arrives while posts can still land and the
/// coordinator regains an early view of those posts. Block time can lag without
/// bound: an OP-stack sequencer back from an outage builds the missed blocks at
/// past timestamps (review C12-R1-1, THREATS C12-6; the rule is C53).
pub const LOCK_MARGIN_SECS: u64 = 60;

/// Bytes of a time-lock ciphertext: `U` (a G2 point, 96) ‖ `V` (16) ‖ `W` (16),
/// drand/tlock's layout for a 16-byte message.
pub const TLOCK_LEN: usize = crate::sealed::TIME_LOCK_BYTES;

/// A time-lock ciphertext.
pub type Tlock = [u8; TLOCK_LEN];

const KEY_TAG_V3: &[u8] = b"onyx-sealed-reveal-key-v3";
const WRAP_TAG: &[u8] = b"onyx-sealed-far-wrap-v1";
const FAR_SIGMA_TAG: &[u8] = b"onyx-sealed-far-sigma-v1";

/// How long after a fact's latest possible reveal close its far round lies
/// (ROADMAP C52): a day, so that no round of the fact can still take posts
/// when the far round's signature, and with it every value, becomes public,
/// even with block time lagging the wall clock by up to a day (THREATS C12-6,
/// C52-4).
pub const UNSEAL_MARGIN_SECS: u64 = 86_400;

/// The longest a far round may lie after a round's reveal close: 1,096 days
/// (three years). drand quicknet publishes no end date, and a far round past
/// its life never comes; a template whose unseal date lies further out (none
/// today: EDGAR's period subjects, observable 110 to 730 days after the
/// period end, unseal at most about 623 days after their earliest round) cannot run time-locked draws (`FarBeyondHorizon`
/// at seal and at intake). THREATS C52-5.
pub const MAX_UNSEAL_DELAY_SECS: u64 = 1_096 * 86_400;

/// A drand chain: what an observer locks to and a client opens with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BeaconChain {
    /// The chain hash.
    pub chain_hash: [u8; 32],
    /// Genesis time, unix seconds.
    pub genesis: u64,
    /// Seconds per round.
    pub period: u64,
}

impl BeaconChain {
    /// drand quicknet, pinned.
    pub fn quicknet() -> Self {
        BeaconChain {
            chain_hash: QUICKNET_CHAIN_HASH,
            genesis: QUICKNET_GENESIS,
            period: QUICKNET_PERIOD,
        }
    }

    /// When round `round` (1-based) is published: `genesis + (round - 1)·period`.
    pub fn round_time(&self, round: u64) -> Option<u64> {
        round
            .checked_sub(1)?
            .checked_mul(self.period)?
            .checked_add(self.genesis)
    }

    /// The first round published at or after `t` (never one before it).
    pub fn first_round_at_or_after(&self, t: u64) -> Option<u64> {
        if self.period == 0 {
            return None;
        }
        if t <= self.genesis {
            return Some(1);
        }
        let since = t - self.genesis;
        since.div_ceil(self.period).checked_add(1)
    }
}

/// The round every post of a round is locked to: the first one published at
/// or after `reveal_close + LOCK_MARGIN_SECS`. A function of the window alone,
/// which the attempt record binds (`revealClose`), so no extra field names it.
pub fn lock_round(chain: &BeaconChain, reveal_close: u64) -> Option<u64> {
    chain.first_round_at_or_after(reveal_close.checked_add(LOCK_MARGIN_SECS)?)
}

/// The latest `reveal_close` any round of `spec`'s FACT can have (ROADMAP
/// C52): its latest window start under `template` (the subject's last
/// observable day for a dated or period subject, the window itself for a fixed
/// one, [`FeedTemplate::latest_window_start`]) plus the window and the longest
/// commit grace and reveal phase a config may take ([`MAX_COMMIT_GRACE_SECS`],
/// [`MAX_REVEAL_SECS`]). `None` when `spec` is not `template` instantiated.
pub fn latest_reveal_close(template: &FeedTemplate, spec: &ObservationSpec) -> Option<u64> {
    template.matches(spec).ok()?;
    template
        .latest_window_start(&spec.subject, spec.window_start)?
        .checked_add(template.window_secs)?
        .checked_add(MAX_COMMIT_GRACE_SECS)?
        .checked_add(MAX_REVEAL_SECS)
}

/// When every round of `spec`'s fact unseals (ROADMAP C52, ADR-0027
/// amendment 15): [`latest_reveal_close`] plus [`UNSEAL_MARGIN_SECS`]. A pure
/// function of the registered template and the spec, so the observer, the
/// coordinator and every client recompute it; the far round is the first
/// quicknet round at or after it ([`LockRounds::of_round`]).
pub fn unseal_time(template: &FeedTemplate, spec: &ObservationSpec) -> Option<u64> {
    latest_reveal_close(template, spec)?.checked_add(UNSEAL_MARGIN_SECS)
}

/// The two quicknet rounds a time-locked post is locked to (ROADMAP C51,
/// C52): `lock`, the round at the window's reveal close ([`lock_round`]),
/// whose signature opens `TL` (and, with `S`, the body), and `far`, the round
/// at the fact's unseal time, whose signature opens the far copy and with it
/// the body for anyone. Always `lock < far`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LockRounds {
    lock: u64,
    far: u64,
}

impl LockRounds {
    /// Two rounds as read back (a book, a served round): `far` must come
    /// after `lock`. Production derives them with [`Self::of_round`]; whoever
    /// reads them back compares with its own derivation where it has the
    /// template.
    pub fn new(lock: u64, far: u64) -> Result<Self, TimeLockError> {
        if far <= lock {
            return Err(TimeLockError::FarNotAfterLock { lock, far });
        }
        Ok(LockRounds { lock, far })
    }

    /// The rounds of a window closing at `reveal_close` whose fact unseals at
    /// `unseal_at`: refused when the unseal time is less than
    /// [`UNSEAL_MARGIN_SECS`] after `reveal_close` (`FarTooSoon`: the far copy
    /// would make the values public while posts may still land) or more than
    /// [`MAX_UNSEAL_DELAY_SECS`] after it (`FarBeyondHorizon`).
    pub fn for_window(
        chain: &BeaconChain,
        reveal_close: u64,
        unseal_at: u64,
    ) -> Result<Self, TimeLockError> {
        let lock = lock_round(chain, reveal_close).ok_or(TimeLockError::NoRound)?;
        if unseal_at < reveal_close.saturating_add(UNSEAL_MARGIN_SECS) {
            return Err(TimeLockError::FarTooSoon(unseal_at));
        }
        if unseal_at - reveal_close > MAX_UNSEAL_DELAY_SECS {
            return Err(TimeLockError::FarBeyondHorizon(unseal_at));
        }
        let far = chain
            .first_round_at_or_after(unseal_at)
            .ok_or(TimeLockError::NoRound)?;
        Self::new(lock, far)
    }

    /// The rounds of a round of `spec` closing at `reveal_close`, the far
    /// round derived from the registered `template` ([`unseal_time`]). Refused
    /// for a template whose rights are not `Redistribution::Allowed`
    /// (`NotRedistributable`, review C52-R1-2): the far copy publishes every
    /// value of every round, value rounds included, for free, which only a
    /// freely redistributable source permits.
    pub fn of_round(
        chain: &BeaconChain,
        template: &FeedTemplate,
        spec: &ObservationSpec,
        reveal_close: u64,
    ) -> Result<Self, TimeLockError> {
        if template.rights.redistribution != onyx_core::Redistribution::Allowed {
            return Err(TimeLockError::NotRedistributable);
        }
        let unseal = unseal_time(template, spec).ok_or(TimeLockError::NoUnsealTime)?;
        Self::for_window(chain, reveal_close, unseal)
    }

    /// The lock round.
    pub fn lock(&self) -> u64 {
        self.lock
    }

    /// The far round.
    pub fn far(&self) -> u64 {
        self.far
    }
}

/// The time-lock opening of a [`Tlock`]: the IBE's random `sigma` and the
/// locked secret. Given both, anyone re-encrypts and compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, borsh::BorshSerialize, borsh::BorshDeserialize)]
pub struct TlockOpening {
    /// The IBE's random value.
    pub sigma: [u8; 16],
    /// The locked secret `tau`.
    pub secret: [u8; 16],
}

/// What a tlock implementation provides (drand/tlock's scheme: Boneh-Franklin
/// IBE with the Fujisaki-Okamoto transform, identity `sha256(round)`):
/// [`QuicknetTimeLock`] (ROADMAP C50); tests also use a mock beacon.
pub trait TimeLock {
    /// The chain this implementation locks to and opens with.
    fn chain(&self) -> &BeaconChain;
    /// Lock `secret` to `round` with the IBE randomness `sigma`. Needs no
    /// signature: anyone locks to a future round. Deterministic in its inputs.
    /// `None` only where the scheme has no ciphertext for these inputs (for
    /// drand's H3, a scalar of zero or 65,534 rejections: about 2^-255),
    /// never a panic.
    fn lock(&self, round: u64, secret: &[u8; 16], sigma: &[u8; 16]) -> Option<Tlock>;
    /// Whether `signature` is the chain's signature of `round`.
    fn verify_signature(&self, round: u64, signature: &[u8]) -> bool;
    /// Open `tl` with the signature of `round`; `None` when it does not open
    /// (wrong round or chain, or a malformed ciphertext). Never panics. It must
    /// return `Some(o)` only when `lock(round, o.secret, o.sigma)` is `tl` byte
    /// for byte (review C12-R1-2: an implementation that accepts a
    /// non-canonical point encoding would let a griefer post a time-lock that
    /// opens for the coordinator but fails a client's re-encryption, so that no
    /// dismissal of it is ever accepted); [`open_canonical`] enforces it here.
    fn open(&self, round: u64, signature: &[u8], tl: &Tlock) -> Option<TlockOpening>;
}

/// [`TimeLock::open`], kept only when the opening re-encrypts to `tl` byte for
/// byte, so the coordinator and every client agree on whether a time-lock
/// opens whatever the implementation accepts (review C12-R1-2).
pub fn open_canonical<T: TimeLock>(
    tl: &T,
    round: u64,
    signature: &[u8],
    lock: &Tlock,
) -> Option<TlockOpening> {
    let o = tl.open(round, signature, lock)?;
    (tl.lock(round, &o.secret, &o.sigma)? == *lock).then_some(o)
}

/// drand quicknet's time-lock (ROADMAP C50): drand/tlock's IBE on the pinned
/// quicknet key (`onyx_tlock`, `blst`'s safe API). Opening verifies the round
/// signature against the pinned key first, then the Fujisaki-Okamoto check;
/// a malformed time-lock or signature is `None`/`false`, never a panic.
#[derive(Clone, Debug)]
pub struct QuicknetTimeLock {
    chain: BeaconChain,
    key: onyx_tlock::BeaconKey,
}

impl QuicknetTimeLock {
    /// The pinned chain (`QUICKNET_*` and `onyx_tlock::QUICKNET_PUBLIC_KEY`).
    pub fn pinned() -> Result<Self, TimeLockError> {
        let key = onyx_tlock::BeaconKey::quicknet().map_err(|_| TimeLockError::BadChainKey)?;
        Ok(QuicknetTimeLock {
            chain: BeaconChain::quicknet(),
            key,
        })
    }
}

impl TimeLock for QuicknetTimeLock {
    fn chain(&self) -> &BeaconChain {
        &self.chain
    }

    fn lock(&self, round: u64, secret: &[u8; 16], sigma: &[u8; 16]) -> Option<Tlock> {
        self.key.lock(round, secret, sigma).ok()
    }

    fn verify_signature(&self, round: u64, signature: &[u8]) -> bool {
        self.key.verify(round, signature)
    }

    fn open(&self, round: u64, signature: &[u8], tl: &Tlock) -> Option<TlockOpening> {
        let o = self.key.open(round, signature, tl).ok()?;
        Some(TlockOpening {
            sigma: o.sigma,
            secret: o.msg,
        })
    }
}

/// Where a coordinator reads drand beacon signatures (ROADMAP C51). Whatever
/// a source returns is only a candidate: [`verified_signature`] keeps it only
/// when it is the pinned chain's signature of the round, so a source that lies
/// (a forged or another round's signature) can delay a round, never open one
/// early or wrongly. `Ok(None)`: not published yet (or the source has none);
/// the round waits.
pub trait BeaconSource {
    /// The signature the source holds for `round` of the pinned chain.
    fn signature(&self, round: u64) -> Result<Option<Vec<u8>>, String>;
}

/// `source`'s signature of `round`, kept only when it verifies against `tl`'s
/// pinned key (`None` otherwise: missing, malformed, forged or another
/// round's; a source error is passed on).
pub fn verified_signature<T: TimeLock>(
    tl: &T,
    source: &dyn BeaconSource,
    round: u64,
) -> Result<Option<Vec<u8>>, String> {
    Ok(source
        .signature(round)?
        .filter(|s| tl.verify_signature(round, s)))
}

/// Published quicknet rounds from a file (tests and the anvil rehearsal; no
/// network): `{"chain": "<chain hash hex>", "rounds": [{"round": n,
/// "signature": "<hex>"}, ...]}`, each entry a verbatim drand
/// `/public/<round>` answer's `round` and `signature`. A file for another
/// chain is refused; the signatures are checked by [`verified_signature`]
/// when used, never trusted.
#[derive(Clone, Debug, Default)]
pub struct FixtureBeacon {
    rounds: std::collections::BTreeMap<u64, Vec<u8>>,
}

impl FixtureBeacon {
    /// Parse the fixture text.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let v: serde_json::Value =
            serde_json::from_str(text).map_err(|e| format!("beacon fixture: {e}"))?;
        if v["chain"].as_str() != Some(hex::encode(QUICKNET_CHAIN_HASH).as_str()) {
            return Err("beacon fixture: not drand quicknet's chain".into());
        }
        let mut rounds = std::collections::BTreeMap::new();
        for r in v["rounds"]
            .as_array()
            .ok_or("beacon fixture: rounds is not an array")?
        {
            let n = r["round"]
                .as_u64()
                .ok_or("beacon fixture: a round number")?;
            let sig = r["signature"]
                .as_str()
                .and_then(|h| hex::decode(h).ok())
                .ok_or("beacon fixture: a signature that is not hex")?;
            if rounds.insert(n, sig).is_some() {
                return Err(format!("beacon fixture: round {n} twice"));
            }
        }
        Ok(FixtureBeacon { rounds })
    }

    /// Read and parse a fixture file.
    pub fn from_file(path: &std::path::Path) -> Result<Self, String> {
        Self::from_json(
            &std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?,
        )
    }

    /// A source holding exactly `rounds` (tests).
    pub fn of(rounds: impl IntoIterator<Item = (u64, Vec<u8>)>) -> Self {
        FixtureBeacon {
            rounds: rounds.into_iter().collect(),
        }
    }

    /// The rounds held.
    pub fn rounds(&self) -> impl Iterator<Item = u64> + '_ {
        self.rounds.keys().copied()
    }
}

impl BeaconSource for FixtureBeacon {
    fn signature(&self, round: u64) -> Result<Option<Vec<u8>>, String> {
        Ok(self.rounds.get(&round).cloned())
    }
}

/// drand's public HTTP API (no account): `GET <relay>/<chain hash>/public/<round>`
/// through `curl` (HTTPS only, `.curlrc` ignored, the generic User-Agent
/// `curl`, 15 s, answers over 4 KiB refused), trying each relay in turn. A
/// round not yet published answers an HTTP error (`Ok(None)` here: the round
/// waits); a relay that is down, or answers a signature that does not verify
/// against the pinned key (review C51-R1-2: one lying relay must not stall
/// every round), is the next relay's turn.
/// Exercised only by the opt-in live test (`ONYX_DRAND_LIVE=1`), never by
/// check-all.
#[derive(Clone, Debug)]
pub struct DrandHttp {
    /// Base URLs, tried in order.
    pub relays: Vec<String>,
}

/// Public drand relays (no account): drand's own three and Cloudflare's (a
/// League of Entropy member). Each served quicknet round 33152432 on
/// 2026-10-17; a round not yet out answers HTTP 425 ("Requested future
/// beacon"). A relay can only delay a round: every signature is verified.
pub const DRAND_RELAYS: [&str; 4] = [
    "https://api.drand.sh",
    "https://api2.drand.sh",
    "https://api3.drand.sh",
    "https://drand.cloudflare.com",
];

impl Default for DrandHttp {
    fn default() -> Self {
        DrandHttp {
            relays: DRAND_RELAYS.iter().map(|s| (*s).to_string()).collect(),
        }
    }
}

/// The first of `candidates` that is the pinned quicknet chain's signature of
/// `round` (a relay's answer that does not verify is skipped, never returned).
pub fn first_verified(
    round: u64,
    candidates: impl IntoIterator<Item = Vec<u8>>,
) -> Option<Vec<u8>> {
    let key = onyx_tlock::BeaconKey::quicknet().ok()?;
    candidates.into_iter().find(|s| key.verify(round, s))
}

impl BeaconSource for DrandHttp {
    fn signature(&self, round: u64) -> Result<Option<Vec<u8>>, String> {
        let answers = self
            .relays
            .iter()
            .filter_map(|r| fetch_relay(r, round).transpose());
        let mut err = None;
        let ok = answers.filter_map(|a| a.map_err(|e| err = Some(e)).ok());
        let found = first_verified(round, ok);
        match (found, err) {
            (Some(s), _) => Ok(Some(s)),
            (None, Some(e)) => Err(e),
            (None, None) => Ok(None),
        }
    }
}

/// One relay's answer for `round`: `Ok(None)` when it has none (not published,
/// down, malformed); `Err` only for a relay that is not HTTPS.
fn fetch_relay(relay: &str, round: u64) -> Result<Option<Vec<u8>>, String> {
    if !relay.starts_with("https://") {
        return Err(format!("drand relay {relay}: https only"));
    }
    let url = format!(
        "{relay}/{}/public/{round}",
        hex::encode(QUICKNET_CHAIN_HASH)
    );
    let Ok(out) = std::process::Command::new("curl")
        .args([
            "-q",
            "-sS",
            "--fail",
            "--proto",
            "=https",
            "-A",
            "curl",
            "--max-filesize",
            "4096",
            "-m",
            "15",
        ])
        .arg(&url)
        .output()
    else {
        return Ok(None);
    };
    if !out.status.success() || out.stdout.len() > 4096 {
        return Ok(None);
    }
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return Ok(None);
    };
    if v["round"].as_u64() != Some(round) {
        return Ok(None);
    }
    Ok(v["signature"].as_str().and_then(|h| hex::decode(h).ok()))
}

/// What a time-locked round's intake used (ROADMAP C51, C52), kept with the
/// sealed snapshot and served: the lock round, its beacon signature (verified
/// against the pinned key before anything was opened), the far round every
/// post's far copy is locked to, and the time-lock opening of each dismissed
/// post whose time-lock opened, ascending by observer.
#[derive(Clone, Debug, PartialEq, Eq, borsh::BorshSerialize, borsh::BorshDeserialize)]
pub struct TimeLockRead {
    /// [`lock_round`] of the round's `reveal_close`.
    pub lock_round: u64,
    /// The far round ([`LockRounds::of_round`]): every client recomputes it
    /// from its own template and requires it equal (book version 8).
    pub far_round: u64,
    /// The pinned chain's signature of `lock_round`.
    pub signature: Vec<u8>,
    /// `(observer, opening)` of each dismissal whose time-lock opened.
    pub openings: Vec<(MinerAddress, TlockOpening)>,
}

/// A [`TimeLockRead`] as book version 7 saved it (no far round): decoded only
/// to be refused (`sealed::SealedSnapshotV7::upgrade`).
#[derive(borsh::BorshDeserialize)]
#[allow(dead_code)]
pub(crate) struct TimeLockReadV7 {
    lock_round: u64,
    signature: Vec<u8>,
    openings: Vec<(MinerAddress, TlockOpening)>,
}

/// Seconds the lock round must still lie ahead when an observer seals: an
/// observer refuses to lock to a round that is (nearly) out, whatever
/// `reveal_close` it was given (review C12-R1-4: a coordinator serving an
/// earlier `reveal_close` would otherwise read the post at once).
pub const SEAL_SLACK_SECS: u64 = 30;

/// Why a time-locked post could not be sealed, opened or checked.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TimeLockError {
    /// The sealed-reveal layer refused it.
    #[error(transparent)]
    Seal(#[from] SealError),
    /// The window has no lock round (arithmetic overflow).
    #[error("no lock round for this window")]
    NoRound,
    /// The lock round's beacon signature is not there yet (or never came).
    /// Nothing opens without it, by design.
    #[error("beacon round {0} not published yet")]
    BeaconMissing(u64),
    /// A beacon signature that is not the chain's signature of the lock round.
    #[error("not the chain's signature of round {0}")]
    BadBeacon(u64),
    /// A dismissal's opening does not re-encrypt to the post's time-lock.
    #[error("time-lock opening does not match the post")]
    BadOpening,
    /// The lock round is out, or due within [`SEAL_SLACK_SECS`], at seal time.
    #[error("lock round {0} is out or due within the slack: refusing to seal")]
    RoundTooSoon(u64),
    /// The scheme has no ciphertext for these secrets (about 2^-255; draw
    /// them again).
    #[error("no time-lock for these secrets")]
    NoLock,
    /// The pinned beacon key did not parse (a build defect, never input).
    #[error("pinned beacon key is not a valid G2 key")]
    BadChainKey,
    /// The far round is not after the lock round (ROADMAP C52).
    #[error("far round {far} is not after lock round {lock}")]
    FarNotAfterLock {
        /// The lock round.
        lock: u64,
        /// The far round.
        far: u64,
    },
    /// The fact would unseal less than [`UNSEAL_MARGIN_SECS`] after the
    /// window's reveal close (a window the template does not allow, or a far
    /// date computed wrongly): refused, never sealed.
    #[error("unseal time {0} is too close to the window's reveal close")]
    FarTooSoon(u64),
    /// The fact unseals more than [`MAX_UNSEAL_DELAY_SECS`] after the window's
    /// reveal close, past the horizon Onyx assumes for drand quicknet.
    #[error("unseal time {0} lies beyond the beacon horizon")]
    FarBeyondHorizon(u64),
    /// The spec is not the template's, so it has no unseal time.
    #[error("no unseal time: the spec is not the registered template's")]
    NoUnsealTime,
    /// The feed's rights do not allow free redistribution, which the far
    /// copy amounts to: no time-locked draw for it (review C52-R1-2).
    #[error("the feed's rights do not allow publishing its values at the far date")]
    NotRedistributable,
}

/// The far copy's IBE randomness (ROADMAP C52): `keccak256("onyx-sealed-far-
/// sigma-v1" ‖ kappa ‖ far round (8 bytes BE))[..16]`. Deterministic in the
/// body key, so whoever holds `kappa` re-encrypts the far copy exactly
/// (encrypt-with-hash; `kappa` is a fresh 128-bit secret per post, so this
/// leaks nothing a random `sigma` would hide, RESEARCH.md "Far-date unseal").
pub fn far_sigma(kappa: &[u8; 16], far_round: u64) -> [u8; 16] {
    let mut b = FAR_SIGMA_TAG.to_vec();
    b.extend_from_slice(kappa);
    b.extend_from_slice(&far_round.to_be_bytes());
    let mut o = [0u8; 16];
    o.copy_from_slice(&keccak256(&b).0[..16]);
    o
}

/// The far copy of `kappa` (ROADMAP C52): `tlock(far_round, kappa,
/// far_sigma(kappa))`. `None` only where the scheme has no ciphertext (about
/// 2^-255).
pub fn far_copy<T: TimeLock>(tl: &T, far_round: u64, kappa: &[u8; 16]) -> Option<Tlock> {
    tl.lock(far_round, kappa, &far_sigma(kappa, far_round))
}

/// What both of a post's keys bind besides their secrets: the ephemeral key,
/// both time-locks (as hashes), both rounds, the chain and the post's round,
/// attempt and observer.
struct KeyContext<'a> {
    ephemeral: &'a Point,
    lock: &'a Tlock,
    far: &'a Tlock,
    rounds: LockRounds,
    chain_hash: &'a [u8; 32],
    round_id: &'a Hash32,
    attempt: u32,
    observer: &'a MinerAddress,
}

impl KeyContext<'_> {
    fn extend(&self, b: &mut Vec<u8>) {
        b.extend_from_slice(self.ephemeral);
        b.extend_from_slice(&keccak256(self.lock).0);
        b.extend_from_slice(&keccak256(self.far).0);
        b.extend_from_slice(&self.rounds.lock.to_be_bytes());
        b.extend_from_slice(&self.rounds.far.to_be_bytes());
        b.extend_from_slice(self.chain_hash);
        b.extend_from_slice(&self.round_id.0);
        b.extend_from_slice(&self.attempt.to_be_bytes());
        b.extend_from_slice(self.observer);
    }
}

/// The key that wraps a post's body key for the coordinator: it needs BOTH the
/// ECDH point `S` and the time-locked `tau` (amendment 12's AND, review
/// C12-R1-7: it hashes both "ciphertexts", `R` and the time-locks, as KEM
/// combiners do).
#[allow(clippy::too_many_arguments)]
pub fn wrap_key(
    shared: &Point,
    tau: &[u8; 16],
    ephemeral: &Point,
    lock: &Tlock,
    far: &Tlock,
    rounds: LockRounds,
    chain_hash: &[u8; 32],
    round_id: &Hash32,
    attempt: u32,
    observer: &MinerAddress,
) -> [u8; 16] {
    let mut b = WRAP_TAG.to_vec();
    b.extend_from_slice(shared);
    b.extend_from_slice(tau);
    KeyContext {
        ephemeral,
        lock,
        far,
        rounds,
        chain_hash,
        round_id,
        attempt,
        observer,
    }
    .extend(&mut b);
    let mut o = [0u8; 16];
    o.copy_from_slice(&keccak256(&b).0[..16]);
    o
}

/// The symmetric key of a time-locked post's body: a function of the body key
/// `kappa` and the post's public context only, so whoever holds `kappa` (the
/// coordinator after the lock round, anyone after the far round) opens it.
#[allow(clippy::too_many_arguments)]
pub fn body_key(
    kappa: &[u8; 16],
    ephemeral: &Point,
    lock: &Tlock,
    far: &Tlock,
    rounds: LockRounds,
    chain_hash: &[u8; 32],
    round_id: &Hash32,
    attempt: u32,
    observer: &MinerAddress,
) -> Hash32 {
    let mut b = KEY_TAG_V3.to_vec();
    b.extend_from_slice(kappa);
    KeyContext {
        ephemeral,
        lock,
        far,
        rounds,
        chain_hash,
        round_id,
        attempt,
        observer,
    }
    .extend(&mut b);
    keccak256(&b)
}

/// The secrets an observer draws for one time-locked post.
#[derive(Clone, Copy, Debug)]
pub struct LockSecrets {
    /// The ECIES ephemeral secret `r`.
    pub ephemeral: [u8; 32],
    /// The locked secret `tau`.
    pub tau: [u8; 16],
    /// The IBE randomness `sigma`.
    pub sigma: [u8; 16],
    /// The body key `kappa` (ROADMAP C52), wrapped for the coordinator and
    /// time-locked to the far round.
    pub kappa: [u8; 16],
}

/// Seal `observation` time-locked: amendment 10's sealed post whose ciphertext
/// is `TL ‖ TF ‖ w ‖ body`, `TL` locking `tau` to `rounds.lock()`, `TF` the
/// far copy of the body key to `rounds.far()` (ROADMAP C52). The key proof and
/// the observer's signature are amendment 10's (the signature covers the whole
/// ciphertext, both time-locks included). The caller derives `rounds` from
/// the on-chain draw's config and the registered template
/// ([`LockRounds::of_round`]).
#[allow(clippy::too_many_arguments)]
pub fn seal_timelocked<T: TimeLock>(
    tl: &T,
    observer_secret: &[u8; 32],
    secrets: &LockSecrets,
    reveal_key: &Point,
    round_id: &Hash32,
    attempt: u32,
    rounds: LockRounds,
    now: u64,
    spec: &ObservationSpec,
    observation: &Observation,
    salt: &Hash32,
) -> Result<SealedPost, TimeLockError> {
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
        return Err(SealError::TooLong.into());
    }
    bytes.resize(len, 0);
    seal_bytes_timelocked(
        tl,
        observer_secret,
        &observer,
        secrets,
        reveal_key,
        round_id,
        attempt,
        rounds,
        now,
        &bytes,
    )
}

/// Seal arbitrary bytes time-locked (what a griefing observer posts; tests).
#[allow(clippy::too_many_arguments)]
pub fn seal_bytes_timelocked<T: TimeLock>(
    tl: &T,
    observer_secret: &[u8; 32],
    observer: &MinerAddress,
    secrets: &LockSecrets,
    reveal_key: &Point,
    round_id: &Hash32,
    attempt: u32,
    rounds: LockRounds,
    now: u64,
    plain: &[u8],
) -> Result<SealedPost, TimeLockError> {
    let lock = lock_bytes(tl, secrets, rounds, now)?;
    let far = far_copy(tl, rounds.far, &secrets.kappa).ok_or(TimeLockError::NoLock)?;
    let eph = public_point(&secrets.ephemeral)?;
    let shared = shared_point(&secrets.ephemeral, reveal_key)?;
    seal_parts(
        observer_secret,
        observer,
        &secrets.ephemeral,
        &eph,
        round_id,
        attempt,
        &[
            &lock,
            &far,
            &xor16(
                &secrets.kappa,
                &wrap_key(
                    &shared,
                    &secrets.tau,
                    &eph,
                    &lock,
                    &far,
                    rounds,
                    &tl.chain().chain_hash,
                    round_id,
                    attempt,
                    observer,
                ),
            ),
            &stream_xor(
                &body_key(
                    &secrets.kappa,
                    &eph,
                    &lock,
                    &far,
                    rounds,
                    &tl.chain().chain_hash,
                    round_id,
                    attempt,
                    observer,
                ),
                plain,
            ),
        ],
    )
}

/// Seal a post made of `parts` as they are (tests make posts with a broken far
/// copy or a wrong wrap with it; [`seal_bytes_timelocked`] uses it for every
/// honest post): the key proof and the observer's signature over the bytes.
#[allow(clippy::too_many_arguments)]
pub fn seal_parts(
    observer_secret: &[u8; 32],
    observer: &MinerAddress,
    ephemeral_secret: &[u8; 32],
    ephemeral: &Point,
    round_id: &Hash32,
    attempt: u32,
    parts: &[&[u8]],
) -> Result<SealedPost, TimeLockError> {
    let ciphertext: Vec<u8> = parts.concat();
    if ciphertext.len() <= TIME_LOCKED_HEAD_BYTES || ciphertext.len() > MAX_SEALED_BYTES {
        return Err(SealError::TooLong.into());
    }
    let key_proof = key_proof(ephemeral_secret, round_id, attempt, observer)?;
    let sig = eip191_sign_with(
        observer_secret,
        sealed_reveal_message(
            round_id,
            attempt,
            observer,
            ephemeral,
            &keccak256(&ciphertext),
        )
        .as_bytes(),
    )
    .ok_or(SealError::BadSecret)?;
    Ok(SealedPost {
        observer: *observer,
        ephemeral: *ephemeral,
        ciphertext,
        key_proof,
        sig,
    })
}

/// The near time-lock of a post, refusing a lock round that is out or due
/// within [`SEAL_SLACK_SECS`] by the observer's clock (review C12-R1-4).
fn lock_bytes<T: TimeLock>(
    tl: &T,
    secrets: &LockSecrets,
    rounds: LockRounds,
    now: u64,
) -> Result<Tlock, TimeLockError> {
    let due = tl
        .chain()
        .round_time(rounds.lock)
        .ok_or(TimeLockError::NoRound)?;
    if due <= now.saturating_add(SEAL_SLACK_SECS) {
        return Err(TimeLockError::RoundTooSoon(rounds.lock));
    }
    tl.lock(rounds.lock, &secrets.tau, &secrets.sigma)
        .ok_or(TimeLockError::NoLock)
}

fn xor16(a: &[u8; 16], b: &[u8; 16]) -> [u8; 16] {
    let mut o = [0u8; 16];
    for (i, x) in o.iter_mut().enumerate() {
        *x = a[i] ^ b[i];
    }
    o
}

/// [`seal_timelocked`] with fresh secrets from the OS.
#[allow(clippy::too_many_arguments)]
pub fn seal_timelocked_random<T: TimeLock>(
    tl: &T,
    observer_secret: &[u8; 32],
    reveal_key: &Point,
    round_id: &Hash32,
    attempt: u32,
    rounds: LockRounds,
    now: u64,
    spec: &ObservationSpec,
    observation: &Observation,
    salt: &Hash32,
) -> Result<SealedPost, TimeLockError> {
    let observer = secret_address(observer_secret).ok_or(SealError::BadSecret)?;
    let mut no_lock = 0u8;
    loop {
        let rand = |e: std::io::Error| SealError::Random(e.to_string());
        let ephemeral = random_salt().map_err(rand)?.0;
        let extra = random_salt().map_err(rand)?.0;
        let more = random_salt().map_err(rand)?.0;
        let mut tau = [0u8; 16];
        let mut sigma = [0u8; 16];
        let mut kappa = [0u8; 16];
        tau.copy_from_slice(&extra[..16]);
        sigma.copy_from_slice(&extra[16..]);
        kappa.copy_from_slice(&more[..16]);
        // An ephemeral key the registry's key-proof check cannot take (R.x >= n)
        // is drawn again, as in `sealed::seal_random`.
        if key_proof(&ephemeral, round_id, attempt, &observer).is_err() {
            continue;
        }
        let secrets = LockSecrets {
            ephemeral,
            tau,
            sigma,
            kappa,
        };
        match seal_timelocked(
            tl,
            observer_secret,
            &secrets,
            reveal_key,
            round_id,
            attempt,
            rounds,
            now,
            spec,
            observation,
            salt,
        ) {
            // No ciphertext for these secrets (about 2^-255): draw again, a
            // few times only, so a scheme that never locks errs, never spins.
            Err(TimeLockError::NoLock) if no_lock < 4 => no_lock += 1,
            r => return r,
        }
    }
}

/// A dismissal of a time-locked post: amendment 10's `S` and DLEQ proof, and
/// the time-lock opening when the time-lock opens (`None`: it does not open
/// under the lock round's signature, which a checker then needs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimedDismissal {
    /// `S` and its proof.
    pub dismissal: Dismissal,
    /// `(sigma, tau)`, or `None` for a time-lock that does not open.
    pub opening: Option<TlockOpening>,
}

/// What the coordinator made of a time-locked post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimedOpened {
    /// A valid reveal.
    Reveal(SignedReveal),
    /// Garbage, with the proof.
    Dismissed(TimedDismissal),
}

/// A post's near time-lock and the rest, `None` for a post too short to hold
/// one and a byte more.
fn split(ciphertext: &[u8]) -> Option<(Tlock, &[u8])> {
    if ciphertext.len() <= TLOCK_LEN {
        return None;
    }
    let mut t = [0u8; TLOCK_LEN];
    t.copy_from_slice(&ciphertext[..TLOCK_LEN]);
    Some((t, &ciphertext[TLOCK_LEN..]))
}

/// The rest after the near time-lock: the far copy, the wrapped body key and
/// the body; `None` for a rest too short to hold the far copy, the wrapped key
/// and a body byte (garbage on its face).
fn split_far(rest: &[u8]) -> Option<(Tlock, [u8; 16], &[u8])> {
    if rest.len() <= TIME_LOCKED_HEAD_BYTES - TLOCK_LEN {
        return None;
    }
    let mut far = [0u8; TLOCK_LEN];
    far.copy_from_slice(&rest[..TLOCK_LEN]);
    let mut w = [0u8; 16];
    w.copy_from_slice(&rest[TLOCK_LEN..TLOCK_LEN + 16]);
    Some((far, w, &rest[TLOCK_LEN + 16..]))
}

fn beacon_checked<T: TimeLock>(
    tl: &T,
    round: u64,
    signature: Option<&[u8]>,
) -> Result<Vec<u8>, TimeLockError> {
    let sig = signature.ok_or(TimeLockError::BeaconMissing(round))?;
    if !tl.verify_signature(round, sig) {
        return Err(TimeLockError::BadBeacon(round));
    }
    Ok(sig.to_vec())
}

/// What the coordinator path makes of a post whose near time-lock opened to
/// `tau`: the body key unwrapped with `S` and `tau`, whether the far copy is
/// exactly that key's (re-encryption), and the body's plaintext under it.
/// `None` for a post too short to hold the far copy and the wrapped key.
#[allow(clippy::too_many_arguments)]
fn inner<T: TimeLock>(
    tl: &T,
    shared: &Point,
    tau: &[u8; 16],
    post: &SealedPost,
    lock: &Tlock,
    rest: &[u8],
    rounds: LockRounds,
    ctx: &RevealContext<'_>,
) -> Option<(bool, Vec<u8>)> {
    let (far, w, body) = split_far(rest)?;
    let chain = &tl.chain().chain_hash;
    let wrap = wrap_key(
        shared,
        tau,
        &post.ephemeral,
        lock,
        &far,
        rounds,
        chain,
        &ctx.round_id,
        ctx.attempt,
        &post.observer,
    );
    let kappa = xor16(&w, &wrap);
    let far_ok = far_copy(tl, rounds.far, &kappa) == Some(far);
    let k = body_key(
        &kappa,
        &post.ephemeral,
        lock,
        &far,
        rounds,
        chain,
        &ctx.round_id,
        ctx.attempt,
        &post.observer,
    );
    Some((far_ok, stream_xor(&k, body)))
}

/// The coordinator opens a time-locked post read from the registry: only with
/// the lock round's beacon signature (`BeaconMissing` before it exists; there is
/// no other way in), after re-checking the stored key proof (C4-R3-2). A valid
/// reveal needs a near time-lock that opens canonically, a far copy that is
/// EXACTLY the unwrapped body key's (`far_copy`, ROADMAP C52: else the round
/// could not unseal without the coordinator, so the post is dismissed), and a
/// body that is the observer's valid reveal.
#[allow(clippy::too_many_arguments)]
pub fn open_timelocked<T: TimeLock>(
    tl: &T,
    reveal_secret: &[u8; 32],
    beacon_signature: Option<&[u8]>,
    observer: &MinerAddress,
    posted: &PostedSealed,
    ctx: &RevealContext<'_>,
    rounds: LockRounds,
) -> Result<TimedOpened, TimeLockError> {
    let post = SealedPost::from_posted(observer, posted);
    if !key_proof_valid(
        &post.ephemeral,
        &ctx.round_id,
        ctx.attempt,
        observer,
        &post.key_proof,
    ) {
        return Err(SealError::BadPost.into());
    }
    let sig = beacon_checked(tl, rounds.lock, beacon_signature)?;
    let dismissed = |opening| -> Result<TimedOpened, TimeLockError> {
        Ok(TimedOpened::Dismissed(TimedDismissal {
            dismissal: dismiss(reveal_secret, &post, &ctx.round_id, ctx.attempt)?,
            opening,
        }))
    };
    let Some((lock, rest)) = split(&post.ciphertext) else {
        return dismissed(None);
    };
    let Some(opening) = open_canonical(tl, rounds.lock, &sig, &lock) else {
        return dismissed(None);
    };
    let shared = shared_point(reveal_secret, &post.ephemeral)?;
    match inner(
        tl,
        &shared,
        &opening.secret,
        &post,
        &lock,
        rest,
        rounds,
        ctx,
    ) {
        Some((true, plain)) => match valid_reveal(&plain, observer, ctx) {
            Some(r) => Ok(TimedOpened::Reveal(r)),
            None => dismissed(Some(opening)),
        },
        _ => dismissed(Some(opening)),
    }
}

/// A client's check of a time-locked dismissal of a post read from the
/// registry: the key proof, the DLEQ proof, and then either
/// - an opening: it must re-encrypt to the post's time-lock (no beacon
///   signature needed; with the real scheme re-encryption computes one pairing
///   per round, review C12-R1-3); the body key unwrapped with `S` and the
///   opened `tau` must then either fail its far copy (re-encrypted to the far
///   round, ROADMAP C52) or open a body that is NOT a valid reveal; or
/// - no opening: the lock round's signature must check and the time-lock must
///   not open under it (`BeaconMissing` until a signature is given).
///
/// Returns the dismissed plaintext (empty for a post whose time-lock does not
/// open, or too short to hold the far copy).
#[allow(clippy::too_many_arguments)]
pub fn check_timelocked_dismissal<T: TimeLock>(
    tl: &T,
    reveal_key: &Point,
    observer: &MinerAddress,
    posted: &PostedSealed,
    ctx: &RevealContext<'_>,
    rounds: LockRounds,
    beacon_signature: Option<&[u8]>,
    d: &TimedDismissal,
) -> Result<Vec<u8>, TimeLockError> {
    let post = SealedPost::from_posted(observer, posted);
    if !key_proof_valid(
        &post.ephemeral,
        &ctx.round_id,
        ctx.attempt,
        observer,
        &post.key_proof,
    ) {
        return Err(SealError::BadPost.into());
    }
    if !dismissal_proof_checks(reveal_key, &post, &ctx.round_id, ctx.attempt, &d.dismissal) {
        return Err(SealError::BadProof.into());
    }
    match (&d.opening, split(&post.ciphertext)) {
        (Some(o), Some((lock, rest))) => {
            if tl.lock(rounds.lock, &o.secret, &o.sigma) != Some(lock) {
                return Err(TimeLockError::BadOpening);
            }
            match inner(
                tl,
                &d.dismissal.shared,
                &o.secret,
                &post,
                &lock,
                rest,
                rounds,
                ctx,
            ) {
                // Too short for the far copy: garbage on its face.
                None => Ok(Vec::new()),
                Some((far_ok, plain)) => {
                    if far_ok && valid_reveal(&plain, observer, ctx).is_some() {
                        return Err(SealError::FalseDismissal.into());
                    }
                    Ok(plain)
                }
            }
        }
        // An opening for a post too short to hold a time-lock is not one.
        (Some(_), None) => Err(TimeLockError::BadOpening),
        // Too short to hold a time-lock and a body: garbage on its face.
        (None, None) => Ok(Vec::new()),
        (None, Some((lock, _))) => {
            let sig = beacon_checked(tl, rounds.lock, beacon_signature)?;
            if open_canonical(tl, rounds.lock, &sig, &lock).is_some() {
                // It opens: the coordinator owed the opening.
                return Err(SealError::FalseDismissal.into());
            }
            Ok(Vec::new())
        }
    }
}

/// What anyone makes of a time-locked post once the far round is out
/// ([`unseal_far`], ROADMAP C52).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unsealed {
    /// The observer's valid reveal for this round, opening its commit.
    Reveal(SignedReveal),
    /// Anything else: no far copy that opens canonically to a body key with
    /// its derived `sigma`, or a body that is no valid reveal. The plaintext
    /// under the far copy's key, when there is one (else empty).
    Garbage(Vec<u8>),
}

/// Unseal a time-locked post WITHOUT the coordinator (ROADMAP C52, THREATS
/// REVLOG-5): with the far round's signature (verified against the pinned key:
/// `BeaconMissing`/`BadBeacon` otherwise, never a guess), open the far copy
/// (kept only when it re-encrypts byte for byte with `far_sigma` of the opened
/// key, so every implementation agrees), derive the body key and read the
/// body. No `S`, no `tau`, no reveal key. A post the coordinator took as a
/// reveal always unseals to that same reveal (it checked the far copy against
/// the same key at intake); a post it dismissed may unseal either way (a wrong
/// wrap or near time-lock is the poster's own loss, proven by the dismissal).
pub fn unseal_far<T: TimeLock>(
    tl: &T,
    far_signature: Option<&[u8]>,
    observer: &MinerAddress,
    posted: &PostedSealed,
    ctx: &RevealContext<'_>,
    rounds: LockRounds,
) -> Result<Unsealed, TimeLockError> {
    let post = SealedPost::from_posted(observer, posted);
    if !key_proof_valid(
        &post.ephemeral,
        &ctx.round_id,
        ctx.attempt,
        observer,
        &post.key_proof,
    ) {
        return Err(SealError::BadPost.into());
    }
    let sig = beacon_checked(tl, rounds.far, far_signature)?;
    let Some((lock, rest)) = split(&post.ciphertext) else {
        return Ok(Unsealed::Garbage(Vec::new()));
    };
    let Some((far, _, body)) = split_far(rest) else {
        return Ok(Unsealed::Garbage(Vec::new()));
    };
    let Some(o) = open_canonical(tl, rounds.far, &sig, &far) else {
        return Ok(Unsealed::Garbage(Vec::new()));
    };
    if o.sigma != far_sigma(&o.secret, rounds.far) {
        return Ok(Unsealed::Garbage(Vec::new()));
    }
    let k = body_key(
        &o.secret,
        &post.ephemeral,
        &lock,
        &far,
        rounds,
        &tl.chain().chain_hash,
        &ctx.round_id,
        ctx.attempt,
        observer,
    );
    let plain = stream_xor(&k, body);
    Ok(match valid_reveal(&plain, observer, ctx) {
        Some(r) => Unsealed::Reveal(r),
        None => Unsealed::Garbage(plain),
    })
}

/// [`unseal_far`] for every in-phase post of a round (ROADMAP C52, THREATS
/// REVLOG-5): `(observer, Some(reveal))` for each post that unseals to its
/// observer's valid reveal (opening `commits[observer]`), `(observer, None)`
/// for anything else, in the order given. What a checker does with it, once
/// the far round is out and without the coordinator: every observer the round
/// served (or, served sealed, listed) as revealed must be `Some` with the
/// served observation and salt; every `Some` must be served as revealed OR
/// dismissed with a proof that checks (a post with a near time-lock that does
/// not open, or a wrong wrap, is rightly dismissed and may still unseal: its
/// poster's own fault, never a reason to refuse the round, review C52-R1-1);
/// and the outcome and attempt record recomputed from the revealed reveals
/// must be the round's.
#[allow(clippy::too_many_arguments)]
pub fn unseal_posts<T: TimeLock>(
    tl: &T,
    far_signature: &[u8],
    spec: &ObservationSpec,
    round_id: &Hash32,
    attempt: u32,
    commits: &std::collections::BTreeMap<MinerAddress, Hash32>,
    posts: &[(MinerAddress, PostedSealed)],
    rounds: LockRounds,
) -> Result<Vec<(MinerAddress, Option<SignedReveal>)>, TimeLockError> {
    posts
        .iter()
        .map(|(a, p)| {
            let ctx = RevealContext {
                round_id: *round_id,
                attempt,
                spec,
                commit: commits.get(a).copied(),
            };
            Ok((
                *a,
                match unseal_far(tl, Some(far_signature), a, p, &ctx, rounds)? {
                    Unsealed::Reveal(r) => Some(r),
                    Unsealed::Garbage(_) => None,
                },
            ))
        })
        .collect()
}

#[cfg(test)]
#[path = "timelock_tests.rs"]
mod tests;
