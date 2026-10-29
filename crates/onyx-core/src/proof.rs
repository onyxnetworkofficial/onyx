//! The proof ladder (ADR-0027, docs/spec/proof-ladder.md).
//!
//! Every claim carries a [`ProofLevel`], derived from its verification method,
//! so a customer or a market can ask for the lowest level it accepts. This
//! module also holds the data types of level 2, "re-observed": several bonded
//! observers read the same public source in the same window with a pinned
//! fetcher and commit-reveal what they saw. The round and settlement rules live
//! in `onyx_coordinator::observe`.

use crate::claim::{MetricId, Subject, VerificationMethod};
use crate::commitment::MinerAddress;
use crate::hash::{keccak256, Hash32};
use crate::ids::is_dotted_id;
use crate::rights::{DataRights, RightsError};
use crate::value::{MAX_FIXED_SCALE, MAX_VALUE_BYTES};
use borsh::{BorshDeserialize, BorshSerialize};

/// How strongly a value is checked. Level 1 is the strongest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, BorshSerialize, BorshDeserialize)]
pub enum ProofLevel {
    /// 1. Anyone reruns a pinned recipe on pinned public inputs.
    Recomputed,
    /// 2. Several bonded observers read the same public source in the same
    ///    window and commit-reveal; a strict majority of all observers agrees.
    ReObserved,
    /// 3. The source signs it (first-party key), or a proof of the exact HTTPS
    ///    response (zkTLS), or a device signature.
    SourceProven,
    /// 4. A claim: a submitter's value that anyone can check, but that no
    ///    bonded observers re-observed and no proof backs (bond and challenge
    ///    window planned). Includes `OfficialSourceMatch` and
    ///    `CrossSourceQuorum` (L2R-F8).
    BondedClaim,
    /// 5. Judged now, scored later when the truth is known (proper scoring rule).
    TrackRecord,
}

impl ProofLevel {
    /// All levels, strongest first.
    pub const ALL: [ProofLevel; 5] = [
        ProofLevel::Recomputed,
        ProofLevel::ReObserved,
        ProofLevel::SourceProven,
        ProofLevel::BondedClaim,
        ProofLevel::TrackRecord,
    ];

    /// The level's number, 1 (strongest) to 5.
    pub fn number(self) -> u8 {
        match self {
            ProofLevel::Recomputed => 1,
            ProofLevel::ReObserved => 2,
            ProofLevel::SourceProven => 3,
            ProofLevel::BondedClaim => 4,
            ProofLevel::TrackRecord => 5,
        }
    }

    /// The level with this number, if any.
    pub fn from_number(n: u8) -> Option<ProofLevel> {
        ProofLevel::ALL.into_iter().find(|l| l.number() == n)
    }

    /// True if this level is at least as strong as `minimum`.
    pub fn meets(self, minimum: ProofLevel) -> bool {
        self.number() <= minimum.number()
    }

    /// The weaker of two levels: a value made from several inputs is only as
    /// strong as its weakest input (L2R2-F3).
    pub fn weakest(self, other: ProofLevel) -> ProofLevel {
        if self.number() >= other.number() {
            self
        } else {
            other
        }
    }
}

impl VerificationMethod {
    /// The proof level a claim checked this way has (docs/spec/proof-ladder.md).
    pub fn proof_level(&self) -> ProofLevel {
        match self {
            VerificationMethod::Recompute { .. } => ProofLevel::Recomputed,
            // Only a settled round of bonded observers is level 2 (L2R-F8).
            VerificationMethod::ReObserved { .. } => ProofLevel::ReObserved,
            // The submitter matched the value against a fetched document or
            // several fetched sources: anyone can redo the match, but no bonded
            // observers did and nothing proves the bytes came from the
            // publisher, so it is a claim (level 4), not level 2 or 3.
            VerificationMethod::OfficialSourceMatch
            | VerificationMethod::CrossSourceQuorum { .. } => ProofLevel::BondedClaim,
            // The source signed it with a registered key (ADR-0028), or the
            // entity signed for itself (labels, ADR-0015).
            VerificationMethod::FirstPartyAttestation { .. }
            | VerificationMethod::SignedSource { .. } => ProofLevel::SourceProven,
            VerificationMethod::CalibratedJudgment => ProofLevel::TrackRecord,
        }
    }
}

/// The proof levels a market or a customer accepts (a set, not a threshold:
/// the money default skips level 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct MarketPolicy {
    /// Bit `n - 1` set: level `n` accepted.
    accepted: u8,
}

impl MarketPolicy {
    /// A policy accepting exactly `levels`.
    pub fn of(levels: &[ProofLevel]) -> Self {
        MarketPolicy {
            accepted: levels
                .iter()
                .fold(0u8, |m, l| m | (1u8 << (l.number() - 1))),
        }
    }

    /// Default for anything that moves money: levels 1 and 3 only (ADR-0027
    /// item 2, L2R-F8). Level 2 joins once level-2 results can be disputed
    /// (a drawn panel that re-fetches); until then a colluding majority of
    /// observers has no check, so level 2 never settles money by default.
    pub fn money_default() -> Self {
        Self::of(&[ProofLevel::Recomputed, ProofLevel::SourceProven])
    }

    /// True if a value at `level` may settle this market.
    pub fn accepts(&self, level: ProofLevel) -> bool {
        self.accepted & (1u8 << (level.number() - 1)) != 0
    }
}

impl Default for MarketPolicy {
    /// The money default: what an unconfigured market accepts.
    fn default() -> Self {
        Self::money_default()
    }
}

/// Fewest observers a level-2 feed may name: with 3, one liar can never form a
/// strict majority.
pub const MIN_OBSERVERS: u16 = 3;
/// Most observers per round (bounds settlement work).
pub const MAX_OBSERVERS: u16 = 255;
/// Longest observation window, in seconds (one day), for exact feeds.
pub const MAX_WINDOW_SECS: u64 = 86_400;
/// Longest observation window for a numeric feed, in seconds (L2R-F1): a
/// moving number drifts during the window, and honest answers must stay within
/// the feed's `tolerance` of each other. Five minutes keeps the drift a feed
/// author has to cover small; a slower source can use a shorter window, never
/// a longer one.
pub const MAX_NUMERIC_WINDOW_SECS: u64 = 300;
/// Largest numeric tolerance, in mantissa units (L2R-F7): `2 * tolerance`
/// stays far inside `u128`, and a tolerance this wide is already meaningless
/// for any real feed. Feed review still has to judge the tolerance per source.
pub const MAX_TOLERANCE: u128 = 1_000_000_000_000_000_000_000_000_000_000;
/// Longest source URL.
pub const MAX_URL_LEN: usize = 512;
/// Longest selector.
pub const MAX_SELECTOR_LEN: usize = 256;

/// The shape of a level-2 value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum ObservationKind {
    /// Normalized bytes; observers agree only on identical bytes.
    Exact {
        /// Longest value, 1..=`MAX_VALUE_BYTES`.
        max_len: u16,
    },
    /// A decimal number as an integer `mantissa * 10^-scale`; observers agree
    /// when within `tolerance` of the median (in mantissa units, no floats).
    Numeric {
        /// Decimal places, 0..=`MAX_FIXED_SCALE`.
        scale: u8,
        /// Allowed absolute distance from the median, in mantissa units
        /// (at most `MAX_TOLERANCE`). Answers within it agree; answers beyond
        /// twice it are outliers; between the two is no call (L2R-F1).
        tolerance: u128,
    },
}

/// A level-2 feed: what to read, where, how, and when.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ObservationSpec {
    /// Feed id (dotted id), e.g. `example.page.usd_per_unit`.
    pub feed: String,
    /// The metric a settled value of this feed is a claim about (L2R3-F1): a
    /// level-2 claim must name exactly this metric.
    pub metric: MetricId,
    /// The subject a settled value is about (L2R3-F1): a level-2 claim must
    /// name exactly this subject. The registry check runs on the claim.
    pub subject: Subject,
    /// Content hash of the pinned fetcher (the extraction code every observer runs).
    pub fetcher: Hash32,
    /// The public source, `https://` only (visible ASCII).
    pub url: String,
    /// Fetcher-specific selector (visible ASCII), e.g. a JSON pointer.
    pub selector: String,
    /// Value shape and agreement rule.
    pub kind: ObservationKind,
    /// First second (unix) at which a fetch counts.
    pub window_start: u64,
    /// Last second (unix) at which a fetch counts; the value is "as of" this time.
    pub window_end: u64,
    /// Fewest assigned observers for a round of this feed.
    pub min_observers: u16,
    /// Rights of the source. Forbidden sources are refused (hard limit).
    pub rights: DataRights,
}

/// Why an [`ObservationSpec`] or an [`Observation`] is refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ObservationError {
    /// Malformed field.
    #[error("observation spec invalid: {0}")]
    BadSpec(&'static str),
    /// The source's rights forbid ingest or are malformed.
    #[error(transparent)]
    Rights(#[from] RightsError),
    /// The answer's shape does not match the feed's kind.
    #[error("answer does not match the feed's kind")]
    WrongKind,
    /// Exact value empty or too long.
    #[error("exact value empty or longer than the feed allows")]
    BadLength,
    /// Fetch time outside the feed's window.
    #[error("observed_at outside the observation window")]
    OutsideWindow,
    /// A value without the hash of the page it came from, or an `Unavailable`
    /// answer that names a page.
    #[error("page hash missing for a value, or present for Unavailable")]
    BadPageHash,
}

fn visible_ascii(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

impl ObservationSpec {
    /// Structural and rights check.
    pub fn validate(&self) -> Result<(), ObservationError> {
        if !is_dotted_id(&self.feed) {
            return Err(ObservationError::BadSpec("feed id"));
        }
        if !is_dotted_id(&self.metric.0) {
            return Err(ObservationError::BadSpec("metric id"));
        }
        let subject = match &self.subject {
            Subject::Entity(s)
            | Subject::Address(s)
            | Subject::Instrument(s)
            | Subject::Aggregate(s) => s,
        };
        if subject.is_empty() || subject.len() > MAX_SELECTOR_LEN || !visible_ascii(subject) {
            return Err(ObservationError::BadSpec("subject"));
        }
        if self.fetcher == Hash32::ZERO {
            return Err(ObservationError::BadSpec("fetcher hash"));
        }
        if self.url.len() > MAX_URL_LEN
            || !visible_ascii(&self.url)
            || !self.url.starts_with("https://")
            || self.url.len() == "https://".len()
        {
            return Err(ObservationError::BadSpec(
                "url must be https:// and visible ASCII",
            ));
        }
        if self.selector.len() > MAX_SELECTOR_LEN || !visible_ascii(&self.selector) {
            return Err(ObservationError::BadSpec("selector"));
        }
        match self.kind {
            ObservationKind::Exact { max_len } => {
                if max_len == 0 || usize::from(max_len) > MAX_VALUE_BYTES {
                    return Err(ObservationError::BadSpec("exact max_len"));
                }
            }
            ObservationKind::Numeric { scale, tolerance } => {
                if scale > MAX_FIXED_SCALE {
                    return Err(ObservationError::BadSpec("numeric scale"));
                }
                if tolerance > MAX_TOLERANCE {
                    return Err(ObservationError::BadSpec("numeric tolerance"));
                }
            }
        }
        let max_window = match self.kind {
            ObservationKind::Exact { .. } => MAX_WINDOW_SECS,
            ObservationKind::Numeric { .. } => MAX_NUMERIC_WINDOW_SECS,
        };
        if self.window_start >= self.window_end || self.window_end - self.window_start > max_window
        {
            return Err(ObservationError::BadSpec("observation window"));
        }
        if self.min_observers < MIN_OBSERVERS || self.min_observers > MAX_OBSERVERS {
            return Err(ObservationError::BadSpec("min_observers"));
        }
        self.rights.check_ingest()?;
        Ok(())
    }

    /// Keccak-256 of `"onyx-observe-spec-v1" ‖ borsh(spec)`.
    pub fn id(&self) -> Hash32 {
        let mut b = OBSERVE_SPEC_TAG.to_vec();
        b.extend_from_slice(&borsh::to_vec(self).unwrap_or_default());
        keccak256(&b)
    }
}

const OBSERVE_SPEC_TAG: &[u8] = b"onyx-observe-spec-v1";
const OBSERVE_COMMIT_TAG: &[u8] = b"onyx-observe-v1";

/// What one observer saw.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, BorshSerialize, BorshDeserialize)]
pub enum Answer {
    /// Normalized bytes (an `Exact` feed).
    Bytes(Vec<u8>),
    /// A mantissa at the feed's scale (a `Numeric` feed).
    Number(i128),
    /// The source did not answer, or the selector found nothing usable.
    Unavailable,
}

/// One observer's reveal.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Observation {
    /// The extracted answer.
    pub answer: Answer,
    /// When the observer fetched (unix seconds, self-reported; must be in the window).
    pub observed_at: u64,
    /// Keccak-256 of the raw bytes fetched (evidence for disputes); zero only for `Unavailable`.
    pub page_hash: Hash32,
}

impl Observation {
    /// Check the reveal's shape against the feed.
    pub fn check(&self, spec: &ObservationSpec) -> Result<(), ObservationError> {
        match (&self.answer, spec.kind) {
            (Answer::Bytes(v), ObservationKind::Exact { max_len }) => {
                if v.is_empty() || v.len() > usize::from(max_len) {
                    return Err(ObservationError::BadLength);
                }
            }
            (Answer::Number(_), ObservationKind::Numeric { .. }) | (Answer::Unavailable, _) => {}
            _ => return Err(ObservationError::WrongKind),
        }
        if self.observed_at < spec.window_start || self.observed_at > spec.window_end {
            return Err(ObservationError::OutsideWindow);
        }
        let unavailable = self.answer == Answer::Unavailable;
        if unavailable != (self.page_hash == Hash32::ZERO) {
            return Err(ObservationError::BadPageHash);
        }
        Ok(())
    }
}

/// The commit an observer posts before any reveal:
/// `keccak256("onyx-observe-v1" ‖ round_id ‖ observer ‖ borsh(observation) ‖ salt)`.
/// The observer's address is inside, so a copied commit can never be revealed
/// by the copier. `round_id` (`ObservationRound::round_id`) binds the feed, the
/// round's timing and economics, and its observer set, so reveals of one round
/// can never be replayed into another round of the same feed (L2R-F6).
pub fn observation_commit(
    round_id: &Hash32,
    observer: &MinerAddress,
    observation: &Observation,
    salt: &Hash32,
) -> Hash32 {
    let mut b = OBSERVE_COMMIT_TAG.to_vec();
    b.extend_from_slice(&round_id.0);
    b.extend_from_slice(observer);
    b.extend_from_slice(&borsh::to_vec(observation).unwrap_or_default());
    b.extend_from_slice(&salt.0);
    keccak256(&b)
}

/// A pinned extraction step. Fetching (the network) stays outside; a fetcher
/// only turns the bytes of a page into an [`Answer`]. Same bytes in, same
/// answer out, on every machine.
pub trait Fetcher {
    /// The content hash the feed pins in [`ObservationSpec::fetcher`].
    fn id(&self) -> Hash32;

    /// Extract the answer from the page bytes. A selector that finds nothing
    /// usable answers [`Answer::Unavailable`], never a guess.
    fn extract(&self, spec: &ObservationSpec, page: &[u8]) -> Answer;
}

/// Build an observation from a fetch result (`None`: the source did not answer).
/// Refuses a fetcher the feed does not pin.
pub fn observe<F: Fetcher + ?Sized>(
    fetcher: &F,
    spec: &ObservationSpec,
    page: Option<&[u8]>,
    observed_at: u64,
) -> Result<Observation, ObservationError> {
    if fetcher.id() != spec.fetcher {
        return Err(ObservationError::BadSpec(
            "fetcher is not the one the feed pins",
        ));
    }
    let obs = match page {
        None => Observation {
            answer: Answer::Unavailable,
            observed_at,
            page_hash: Hash32::ZERO,
        },
        Some(bytes) => match fetcher.extract(spec, bytes) {
            Answer::Unavailable => Observation {
                answer: Answer::Unavailable,
                observed_at,
                page_hash: Hash32::ZERO,
            },
            answer => Observation {
                answer,
                observed_at,
                page_hash: keccak256(bytes),
            },
        },
    };
    obs.check(spec)?;
    Ok(obs)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::claim::AttestationScheme;
    use crate::rights::Redistribution;
    use crate::source::SourceId;

    pub(crate) fn spec(kind: ObservationKind) -> ObservationSpec {
        ObservationSpec {
            feed: "example.page.value".into(),
            metric: MetricId("example.page.value".into()),
            subject: Subject::Entity("entity:example".into()),
            fetcher: keccak256(b"test fetcher"),
            url: "https://example.org/page.json".into(),
            selector: "/data/value".into(),
            kind,
            window_start: 1_000,
            window_end: 1_060,
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

    #[test]
    fn levels_order_and_numbers() {
        for (i, l) in ProofLevel::ALL.iter().enumerate() {
            assert_eq!(usize::from(l.number()), i + 1);
            assert_eq!(ProofLevel::from_number(l.number()), Some(*l));
        }
        assert_eq!(ProofLevel::from_number(0), None);
        assert_eq!(ProofLevel::from_number(6), None);
        assert!(ProofLevel::Recomputed.meets(ProofLevel::SourceProven));
        assert!(ProofLevel::SourceProven.meets(ProofLevel::SourceProven));
        assert!(!ProofLevel::BondedClaim.meets(ProofLevel::SourceProven));
    }

    /// L2R2-F3: a value is as strong as its weakest input.
    #[test]
    fn weakest_input_decides() {
        use ProofLevel::*;
        assert_eq!(Recomputed.weakest(SourceProven), SourceProven);
        assert_eq!(BondedClaim.weakest(Recomputed), BondedClaim);
        assert_eq!(TrackRecord.weakest(ReObserved), TrackRecord);
        for l in ProofLevel::ALL {
            assert_eq!(l.weakest(l), l);
        }
    }

    #[test]
    fn money_markets_take_levels_one_and_three_only() {
        // L2R-F8: level 2 waits for level-2 disputes.
        let m = MarketPolicy::money_default();
        let ok: Vec<u8> = ProofLevel::ALL
            .iter()
            .filter(|l| m.accepts(**l))
            .map(|l| l.number())
            .collect();
        assert_eq!(ok, vec![1, 3]);
        assert_eq!(MarketPolicy::default(), m);
        let all = MarketPolicy::of(&ProofLevel::ALL);
        assert!(ProofLevel::ALL.iter().all(|l| all.accepts(*l)));
        assert!(!MarketPolicy::of(&[]).accepts(ProofLevel::Recomputed));
    }

    #[test]
    fn every_method_has_a_level() {
        let cases = [
            (
                VerificationMethod::Recompute {
                    recipe: keccak256(b"r"),
                },
                1,
            ),
            // L2R-F8: no observers, no bonds: a claim, below a bonded quorum.
            (VerificationMethod::OfficialSourceMatch, 4),
            (
                VerificationMethod::CrossSourceQuorum {
                    sources: vec![],
                    threshold: 2,
                    tolerance: 0,
                },
                4,
            ),
            (
                VerificationMethod::ReObserved {
                    spec: keccak256(b"s"),
                    result: keccak256(b"r"),
                },
                2,
            ),
            (
                VerificationMethod::FirstPartyAttestation {
                    scheme: AttestationScheme::Eip191,
                },
                3,
            ),
            // ADR-0028: a report signed by a registered key of the source.
            (
                VerificationMethod::SignedSource {
                    key: keccak256(b"k"),
                    report: keccak256(b"rep"),
                },
                3,
            ),
            (VerificationMethod::CalibratedJudgment, 5),
        ];
        for (m, n) in cases {
            assert_eq!(m.proof_level().number(), n, "{m:?}");
        }
    }

    #[test]
    fn spec_rules() {
        let ok = spec(ObservationKind::Numeric {
            scale: 2,
            tolerance: 50,
        });
        assert_eq!(ok.validate(), Ok(()));
        let bad = |f: &dyn Fn(&mut ObservationSpec)| {
            let mut s = ok.clone();
            f(&mut s);
            s.validate().is_err()
        };
        assert!(bad(&|s| s.url = "http://example.org/x".into()));
        assert!(bad(&|s| s.url = "https://".into()));
        assert!(bad(&|s| s.url = "https://example.org/a b".into()));
        assert!(bad(&|s| s.selector = String::new()));
        assert!(bad(&|s| s.fetcher = Hash32::ZERO));
        assert!(bad(&|s| s.feed = "Not A Dotted Id".into()));
        assert!(bad(&|s| s.window_end = s.window_start));
        assert!(bad(&|s| s.window_end = s.window_start + MAX_WINDOW_SECS + 1));
        // L2R-F1: a numeric feed's window is short, so drift stays small.
        assert!(bad(
            &|s| s.window_end = s.window_start + MAX_NUMERIC_WINDOW_SECS + 1
        ));
        assert!(!bad(
            &|s| s.window_end = s.window_start + MAX_NUMERIC_WINDOW_SECS
        ));
        // An exact feed keeps the one-day window.
        assert!(!bad(&|s| {
            s.kind = ObservationKind::Exact { max_len: 8 };
            s.window_end = s.window_start + MAX_WINDOW_SECS;
        }));
        // L2R-F7: a tolerance wide enough to make every answer agree is refused.
        assert!(bad(&|s| s.kind = ObservationKind::Numeric {
            scale: 2,
            tolerance: u128::MAX
        }));
        assert!(!bad(&|s| s.kind = ObservationKind::Numeric {
            scale: 2,
            tolerance: MAX_TOLERANCE
        }));
        assert!(bad(&|s| s.min_observers = 2));
        // MAX_OBSERVERS (255) is allowed, one more is not (mutation L4).
        assert!(!bad(&|s| s.min_observers = MAX_OBSERVERS));
        assert!(bad(&|s| s.min_observers = MAX_OBSERVERS + 1));
        assert!(bad(&|s| s.kind = ObservationKind::Exact { max_len: 0 }));
        assert!(bad(&|s| s.kind = ObservationKind::Numeric {
            scale: MAX_FIXED_SCALE + 1,
            tolerance: 0
        }));
        // Hard limit: a source that forbids redistribution is never a feed.
        assert!(bad(&|s| s.rights.redistribution = Redistribution::Forbidden));
        // Every length and range limit is inclusive (mutation L4).
        let url_at = |n: usize| format!("https://{}", "a".repeat(n - "https://".len()));
        assert!(!bad(&|s| s.url = url_at(MAX_URL_LEN)));
        assert!(bad(&|s| s.url = url_at(MAX_URL_LEN + 1)));
        assert!(!bad(&|s| s.selector = "s".repeat(MAX_SELECTOR_LEN)));
        assert!(bad(&|s| s.selector = "s".repeat(MAX_SELECTOR_LEN + 1)));
        assert!(!bad(
            &|s| s.subject = Subject::Aggregate("x".repeat(MAX_SELECTOR_LEN))
        ));
        assert!(bad(
            &|s| s.subject = Subject::Aggregate("x".repeat(MAX_SELECTOR_LEN + 1))
        ));
        assert!(bad(&|s| s.subject = Subject::Aggregate(String::new())));
        assert!(bad(&|s| s.subject = Subject::Aggregate("a b".into())));
        let max_len = u16::try_from(MAX_VALUE_BYTES).expect("fits");
        assert!(!bad(&|s| s.kind = ObservationKind::Exact { max_len }));
        assert!(bad(&|s| s.kind = ObservationKind::Exact {
            max_len: max_len + 1
        }));
        assert!(!bad(&|s| s.kind = ObservationKind::Numeric {
            scale: MAX_FIXED_SCALE,
            tolerance: 0
        }));
    }

    /// Listing a level twice accepts it once (mutation L4: `|`, not `^`).
    #[test]
    fn market_policy_of_is_a_set() {
        assert_eq!(
            MarketPolicy::of(&[ProofLevel::Recomputed, ProofLevel::Recomputed]),
            MarketPolicy::of(&[ProofLevel::Recomputed])
        );
    }

    #[test]
    fn observation_shape_window_and_page_hash() {
        let s = spec(ObservationKind::Exact { max_len: 8 });
        let page = keccak256(b"page");
        let ok = Observation {
            answer: Answer::Bytes(b"open".to_vec()),
            observed_at: 1_030,
            page_hash: page,
        };
        assert_eq!(ok.check(&s), Ok(()));
        let with = |f: &dyn Fn(&mut Observation)| {
            let mut o = ok.clone();
            f(&mut o);
            o.check(&s)
        };
        assert_eq!(
            with(&|o| o.answer = Answer::Number(1)),
            Err(ObservationError::WrongKind)
        );
        assert_eq!(
            with(&|o| o.answer = Answer::Bytes(vec![])),
            Err(ObservationError::BadLength)
        );
        assert_eq!(
            with(&|o| o.answer = Answer::Bytes(vec![b'x'; 9])),
            Err(ObservationError::BadLength)
        );
        assert_eq!(
            with(&|o| o.observed_at = 999),
            Err(ObservationError::OutsideWindow)
        );
        assert_eq!(
            with(&|o| o.observed_at = 1_061),
            Err(ObservationError::OutsideWindow)
        );
        // Both window ends and an answer of exactly max_len are inside (mutation L4).
        assert_eq!(with(&|o| o.observed_at = 1_000), Ok(()));
        assert_eq!(with(&|o| o.observed_at = 1_060), Ok(()));
        assert_eq!(with(&|o| o.answer = Answer::Bytes(vec![b'x'; 8])), Ok(()));
        assert_eq!(
            with(&|o| o.page_hash = Hash32::ZERO),
            Err(ObservationError::BadPageHash)
        );
        assert_eq!(
            with(&|o| o.answer = Answer::Unavailable),
            Err(ObservationError::BadPageHash)
        );
        assert_eq!(
            with(&|o| {
                o.answer = Answer::Unavailable;
                o.page_hash = Hash32::ZERO;
            }),
            Ok(())
        );
    }

    #[test]
    fn commit_binds_round_observer_answer_and_salt() {
        let s = spec(ObservationKind::Exact { max_len: 8 }).id();
        let o = Observation {
            answer: Answer::Bytes(b"open".to_vec()),
            observed_at: 1_030,
            page_hash: keccak256(b"page"),
        };
        let a = [1u8; 20];
        let salt = keccak256(b"salt");
        let base = observation_commit(&s, &a, &o, &salt);
        assert_ne!(
            base,
            observation_commit(&keccak256(b"other"), &a, &o, &salt)
        );
        assert_ne!(base, observation_commit(&s, &[2u8; 20], &o, &salt));
        assert_ne!(base, observation_commit(&s, &a, &o, &keccak256(b"salt2")));
        let mut o2 = o.clone();
        o2.answer = Answer::Bytes(b"shut".to_vec());
        assert_ne!(base, observation_commit(&s, &a, &o2, &salt));
    }

    struct Upper;
    impl Fetcher for Upper {
        fn id(&self) -> Hash32 {
            keccak256(b"test fetcher")
        }
        fn extract(&self, _: &ObservationSpec, page: &[u8]) -> Answer {
            if page.is_empty() {
                Answer::Unavailable
            } else {
                Answer::Bytes(page.to_ascii_uppercase())
            }
        }
    }

    #[test]
    fn observe_pins_the_fetcher_and_hashes_the_page() {
        let s = spec(ObservationKind::Exact { max_len: 8 });
        let o = observe(&Upper, &s, Some(b"open"), 1_010).expect("observe");
        assert_eq!(o.answer, Answer::Bytes(b"OPEN".to_vec()));
        assert_eq!(o.page_hash, keccak256(b"open"));
        let down = observe(&Upper, &s, None, 1_010).expect("down");
        assert_eq!(
            (down.answer, down.page_hash),
            (Answer::Unavailable, Hash32::ZERO)
        );
        let empty = observe(&Upper, &s, Some(b""), 1_010).expect("empty");
        assert_eq!(empty.answer, Answer::Unavailable);
        let mut other = s.clone();
        other.fetcher = keccak256(b"another fetcher");
        assert!(observe(&Upper, &other, Some(b"open"), 1_010).is_err());
        assert_eq!(
            observe(&Upper, &s, Some(b"open"), 2_000),
            Err(ObservationError::OutsideWindow)
        );
    }
}
