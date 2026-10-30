//! Time alignment of market data from several venues (ADR-0058, contributor loop CL8).
//!
//! Each venue is its own level-2 feed of closed buckets on the UTC grid
//! ([`crate::SubjectRule::Bucketed`]). A [`Composite`] lists the venue feeds that make one
//! unified feed (2..=[`MAX_VENUES`], sorted, one unit and one scale, one bucket length), and
//! [`aligned`] is a pure function of the venue facts recorded for one bucket:
//! - a venue counts only when its fact is recorded at level 2 with a value and its four clocks
//!   are consistent ([`VenueFact::clock_check`]); every other listed venue is shown with a
//!   closed [`Reason`];
//! - the value is the lower median of the included values (the ADR-0027 rule), the band their
//!   minimum and maximum (ADR-0017), `disagreement` is set when
//!   `(max - min) × 10^6 > disagree_ppm × |median|`;
//! - while any listed venue has no round yet and its bucket can still be observed:
//!   [`Status::Pending`], no value (review CL8-R1-1: whoever pays for rounds never picks the
//!   subset of venues that counts);
//! - a listed venue never observed in its bucket's range (`lapsed`), fewer than `min_venues`
//!   included, or not more than half of the listed venues: [`Status::Insufficient`], no value,
//!   no band;
//! - the answer's rights are the strictest of the listed venues' ([`Composite::rights`]).
//!
//! The four clocks (ADR-0058 item 4) are stated in every answer: the bucket (`[start, end)` on
//! the UTC grid, `as_of = end`), each venue's `venue_time` (the bucket start the venue returned),
//! the observers' `observed_from..=observed_to`, and the chain's `recorded_block`/`recorded_at`.
//! No field is "now".
//!
//! Integers only (ADR-0009): values are mantissas at the composite's scale. Every answer has an
//! id, `keccak256("onyx-aligned-v1" ‖ borsh(answer))` (ADR-0002, ADR-0003), so a client that
//! recomputes the answer from the venue facts compares one hash ([`check_answer`]).

use crate::feed::{Bucket, BUCKET_SIZES};
use crate::{keccak256, Hash32, Redistribution};
use borsh::{BorshDeserialize, BorshSerialize};

/// Fewest venues a composite lists (it needs at least [`MIN_VENUES`] for a value).
pub const MIN_LISTED: usize = MIN_VENUES as usize;
/// Most venues a composite lists.
pub const MAX_VENUES: usize = 16;
/// Fewest included venues for a value (ADR-0058 item 5, proposed 3).
pub const MIN_VENUES: u16 = 3;
/// The default disagreement threshold, parts per million of the median (0.5%).
pub const DISAGREE_PPM: u32 = 5_000;
/// The tag an answer's id is derived from.
pub const ANSWER_TAG: &[u8] = b"onyx-aligned-v1";

/// Why a composite or an alignment is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AlignError {
    /// The composite breaks a rule.
    #[error("composite invalid: {0}")]
    Composite(&'static str),
    /// A venue is not in the composite's unit or scale (THREATS CL8-3).
    #[error("venue {0} is not in the composite's unit and scale")]
    Unit(String),
    /// A venue's terms forbid passing its data on (THREATS CL8-12).
    #[error("venue {0}: its terms forbid redistribution")]
    Forbidden(String),
    /// The bucket is not on the composite's grid.
    #[error("bucket start {0} is not on the composite's grid")]
    OffGrid(u64),
    /// A listed venue has no input (THREATS CL8-4).
    #[error("listed venue {0} is missing from the inputs")]
    MissingVenue(String),
    /// An input names a venue the composite does not list, or repeats one.
    #[error("input {0} is not a listed venue, or comes twice")]
    UnknownVenue(String),
    /// The answer is not the one recomputed from the venue facts.
    #[error("the answer differs from the one recomputed from the venue facts: {0}")]
    Mismatch(&'static str),
}

/// One venue feed of a composite.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct VenueRef {
    /// The venue feed's level-2 metric id (a bucketed manifest's metric).
    pub metric: String,
    /// The venue (its publisher id, the manifest's `rights.source`): one venue counts once
    /// (THREATS CL8-10).
    pub venue: String,
    /// The host the venue feed is fetched from: one host counts once.
    pub host: String,
    /// The terms row the venue is cleared under (docs/ops/data-sources.md section 1): one
    /// reviewed party counts once, whatever its hosts or source names (review CL8-R1-3).
    pub terms_row: String,
    /// The venue feed's unit.
    pub unit: String,
    /// The venue feed's scale.
    pub scale: u8,
    /// The venue feed's bucket length.
    pub bucket_secs: u32,
    /// The venue's reviewed redistribution terms.
    pub redistribution: Redistribution,
}

/// A unified feed over venue feeds (ADR-0058 item 5).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Composite {
    /// The composite's metric id.
    pub metric: String,
    /// One unit for every venue (no conversion is ever done).
    pub unit: String,
    /// One scale for every venue.
    pub scale: u8,
    /// One bucket length for every venue, one of [`BUCKET_SIZES`].
    pub bucket_secs: u32,
    /// The venue feeds, sorted by metric, unique.
    pub venues: Vec<VenueRef>,
    /// Fewest included venues for a value, at least [`MIN_VENUES`].
    pub min_venues: u16,
    /// The disagreement threshold, 1..=1,000,000 ppm.
    pub disagree_ppm: u32,
}

impl Composite {
    /// Refuse a composite that breaks a rule: 2..=16 venues sorted by metric and unique, no
    /// venue or host twice, one unit, scale and bucket length, no venue whose terms forbid
    /// redistribution, `MIN_VENUES <= min_venues <= venues`, a threshold in 1..=10^6 ppm.
    pub fn validate(&self) -> Result<(), AlignError> {
        let bad = |why| Err(AlignError::Composite(why));
        if self.metric.is_empty() || self.unit.is_empty() {
            return bad("metric and unit");
        }
        if !BUCKET_SIZES.contains(&self.bucket_secs) {
            return bad("a bucket is an hour or a day");
        }
        if !(MIN_LISTED..=MAX_VENUES).contains(&self.venues.len()) {
            return bad("a composite lists 3 to 16 venues");
        }
        if !self
            .venues
            .windows(2)
            .all(|w| w[0].metric.as_bytes() < w[1].metric.as_bytes())
        {
            return bad("venues must be sorted by metric, without duplicates");
        }
        let twice = |key: fn(&VenueRef) -> &str| {
            let mut k: Vec<&str> = self.venues.iter().map(key).collect();
            k.sort_unstable();
            k.windows(2).any(|w| w[0] == w[1])
        };
        if twice(|v| v.venue.as_str())
            || twice(|v| v.host.as_str())
            || twice(|v| v.terms_row.as_str())
        {
            return bad(
                "one venue (source, host or terms row) is listed twice: each venue counts once",
            );
        }
        for v in &self.venues {
            if v.unit != self.unit || v.scale != self.scale || v.bucket_secs != self.bucket_secs {
                return Err(AlignError::Unit(v.metric.clone()));
            }
            if v.redistribution == Redistribution::Forbidden {
                return Err(AlignError::Forbidden(v.metric.clone()));
            }
        }
        if self.min_venues < MIN_VENUES || usize::from(self.min_venues) > self.venues.len() {
            return bad("min_venues is at least 3 and at most the venues listed");
        }
        if self.disagree_ppm == 0 || self.disagree_ppm > 1_000_000 {
            return bad("disagree_ppm is 1..=1000000");
        }
        Ok(())
    }

    /// The composite's rights: the strictest of its venues' (THREATS CL8-7). `Forbidden` never
    /// loads ([`Self::validate`]).
    pub fn rights(&self) -> Redistribution {
        let rank = |r: Redistribution| match r {
            Redistribution::Allowed => 0,
            Redistribution::Restricted => 1,
            Redistribution::Forbidden => 2,
        };
        self.venues
            .iter()
            .map(|v| v.redistribution)
            .max_by_key(|r| rank(*r))
            .unwrap_or(Redistribution::Forbidden)
    }

    /// `keccak256("onyx-composite-v1" ‖ borsh(self))`: the id a client pins.
    pub fn id(&self) -> Hash32 {
        let mut b = b"onyx-composite-v1".to_vec();
        b.extend_from_slice(&borsh::to_vec(self).unwrap_or_default());
        keccak256(&b)
    }
}

/// One venue's level-2 fact for a bucket, with its clocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct VenueFact {
    /// The settled value, a mantissa at the composite's scale.
    pub value: i128,
    /// The bucket start the venue returned (its own clock; the manifest asserts it equals the
    /// subject's).
    pub venue_time: u64,
    /// The round's window start.
    pub window_start: u64,
    /// The round's window end.
    pub window_end: u64,
    /// The earliest `observed_at` of the agreeing reveals (the observers' clocks).
    pub observed_from: u64,
    /// The latest `observed_at` of the agreeing reveals.
    pub observed_to: u64,
    /// The block of `ObservationRecorded` (the chain's clock).
    pub recorded_block: u64,
    /// That block's time.
    pub recorded_at: u64,
}

impl VenueFact {
    /// The four clocks agree with each other and with the bucket (THREATS CL8-8): the venue
    /// returned exactly this bucket; the window opens after the bucket closed; every agreeing
    /// reveal lies in the window; the chain recorded the fact after the last reveal and after the
    /// window. Anything else means some clock lies, and the venue is not counted.
    pub fn clock_check(&self, bucket: &Bucket) -> bool {
        self.venue_time == bucket.start
            && self.window_start >= bucket.end
            && self.window_start <= self.window_end
            && self.window_start <= self.observed_from
            && self.observed_from <= self.observed_to
            && self.observed_to <= self.window_end
            && self.recorded_at >= self.window_end
            && self.recorded_block > 0
    }
}

/// What is known about one listed venue for a bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum VenueInput {
    /// Recorded at level 2 with a value.
    Recorded(VenueFact),
    /// No recorded round yet, and the bucket can still be observed: the answer is pending.
    NotObserved,
    /// The round settled `SourceUnavailable` (no bucket, another bucket, a bucket with no
    /// trades, an outage).
    Unavailable,
    /// The round settled without a value (no quorum).
    NoResult,
    /// The venue's data is withheld from this answer (its terms).
    RightsWithheld,
    /// No recorded round, and the bucket's observable range is over: it never will have one.
    Lapsed,
}

/// Why a listed venue is not counted (ADR-0058 item 5; `ClockMismatch` is amendment 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Reason {
    /// `not_observed`: no round yet.
    NotObserved,
    /// `unavailable`: the round settled `SourceUnavailable`.
    Unavailable,
    /// `no_result`: the round settled no value.
    NoResult,
    /// `rights_withheld`: withheld under the venue's terms.
    RightsWithheld,
    /// `clock_mismatch`: the fact's clocks disagree with the bucket or each other (CL8-8).
    ClockMismatch,
    /// `lapsed`: no round was recorded while the bucket was observable.
    Lapsed,
}

impl Reason {
    /// The closed code an answer shows.
    pub fn code(self) -> &'static str {
        match self {
            Reason::NotObserved => "not_observed",
            Reason::Unavailable => "unavailable",
            Reason::NoResult => "no_result",
            Reason::RightsWithheld => "rights_withheld",
            Reason::ClockMismatch => "clock_mismatch",
            Reason::Lapsed => "lapsed",
        }
    }

    /// The reason of a code.
    pub fn from_code(s: &str) -> Option<Reason> {
        [
            Reason::NotObserved,
            Reason::Unavailable,
            Reason::NoResult,
            Reason::RightsWithheld,
            Reason::ClockMismatch,
            Reason::Lapsed,
        ]
        .into_iter()
        .find(|r| r.code() == s)
    }
}

/// One listed venue in an answer.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct VenueLine {
    /// The venue feed's metric.
    pub metric: String,
    /// The venue.
    pub venue: String,
    /// Counted with this fact, or left out for this reason.
    pub outcome: Result<VenueFact, Reason>,
}

/// Whether the answer has a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Status {
    /// At least `min_venues` included: a value and a band.
    Aligned,
    /// Fewer than `min_venues` included, or not more than half of the listed venues: no value.
    Insufficient,
    /// A listed venue's bucket can still be observed and has no recorded round: no value yet.
    Pending,
}

/// The aligned answer for one bucket (ADR-0058 items 4 to 6).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct AlignedAnswer {
    /// The composite's id ([`Composite::id`]).
    pub composite: Hash32,
    /// The composite's metric.
    pub metric: String,
    /// The bucket (UTC grid, end exclusive); `as_of` is `bucket.end`.
    pub bucket: Bucket,
    /// Value or not.
    pub status: Status,
    /// The lower median of the included values.
    pub value: Option<i128>,
    /// The minimum and maximum of the included values.
    pub band: Option<(i128, i128)>,
    /// `floor((max - min) × 10^6 / |median|)`, `u64::MAX` when it does not fit or the median is
    /// zero with a spread.
    pub spread_ppm: Option<u64>,
    /// `(max - min) × 10^6 > disagree_ppm × |median|`.
    pub disagreement: bool,
    /// The composite's unit.
    pub unit: String,
    /// The composite's scale.
    pub scale: u8,
    /// The strictest rights of the listed venues.
    pub rights: Redistribution,
    /// Every listed venue, in the composite's order.
    pub venues: Vec<VenueLine>,
}

impl AlignedAnswer {
    /// `keccak256(ANSWER_TAG ‖ borsh(self))`.
    pub fn id(&self) -> Hash32 {
        let mut b = ANSWER_TAG.to_vec();
        b.extend_from_slice(&borsh::to_vec(self).unwrap_or_default());
        keccak256(&b)
    }

    /// `as_of`: the bucket's end. No field of an answer is "now".
    pub fn as_of(&self) -> u64 {
        self.bucket.end
    }

    /// The included venues' facts, in the composite's order.
    pub fn included(&self) -> impl Iterator<Item = (&VenueLine, &VenueFact)> {
        self.venues
            .iter()
            .filter_map(|l| l.outcome.as_ref().ok().map(|f| (l, f)))
    }
}

/// Lower median of a non-empty sorted slice (ADR-0027).
pub fn lower_median(sorted: &[i128]) -> Option<i128> {
    sorted.get(sorted.len().checked_sub(1)? / 2).copied()
}

/// `(spread_ppm, disagreement)` of a band around `median` (ADR-0058 item 6), in integers only.
/// Exact for every `i128` input: the products are 256-bit, never rounded or saturated before the
/// comparison; `spread_ppm` saturates at `u64::MAX` (display only).
pub fn spread(min: i128, max: i128, median: i128, disagree_ppm: u32) -> (u64, bool) {
    let diff = max.abs_diff(min);
    let base = median.unsigned_abs();
    let scaled = mul_wide(diff, 1_000_000);
    let limit = mul_wide(base, u128::from(disagree_ppm));
    let flag = scaled > limit;
    let ppm = match base {
        0 if diff == 0 => 0,
        0 => u64::MAX,
        _ => div_wide(scaled, base)
            .and_then(|q| u64::try_from(q).ok())
            .unwrap_or(u64::MAX),
    };
    (ppm, flag)
}

/// `a × b` as a 256-bit `(high, low)` pair.
fn mul_wide(a: u128, b: u128) -> (u128, u128) {
    const M: u128 = u64::MAX as u128;
    let (a1, a0) = (a >> 64, a & M);
    let (b1, b0) = (b >> 64, b & M);
    let (p00, p01, p10, p11) = (a0 * b0, a0 * b1, a1 * b0, a1 * b1);
    let mid = (p00 >> 64) + (p01 & M) + (p10 & M);
    let lo = (p00 & M) | (mid << 64);
    let hi = p11 + (p01 >> 64) + (p10 >> 64) + (mid >> 64);
    (hi, lo)
}

/// `(high, low) / d` when the quotient fits 128 bits (`high < d`), by shift and subtract.
fn div_wide((hi, lo): (u128, u128), d: u128) -> Option<u128> {
    if d == 0 || hi >= d {
        return None;
    }
    let (mut r, mut q) = (hi, 0u128);
    for i in (0..128).rev() {
        let carry = r >> 127;
        r = (r << 1) | ((lo >> i) & 1);
        q <<= 1;
        if carry == 1 || r >= d {
            r = r.wrapping_sub(d);
            q |= 1;
        }
    }
    Some(q)
}

/// The aligned answer for the bucket starting at `bucket_start` from `inputs`, one per listed
/// venue in the composite's order, `(venue metric, input)` (see the module docs).
pub fn aligned(
    c: &Composite,
    bucket_start: u64,
    inputs: &[(String, VenueInput)],
) -> Result<AlignedAnswer, AlignError> {
    c.validate()?;
    let bucket =
        Bucket::on_grid(bucket_start, c.bucket_secs).ok_or(AlignError::OffGrid(bucket_start))?;
    // THREATS CL8-4: exactly the listed venues, each once, in order.
    for (i, v) in c.venues.iter().enumerate() {
        match inputs.get(i) {
            Some((m, _)) if *m == v.metric => {}
            Some((m, _)) if !c.venues.iter().any(|x| x.metric == *m) => {
                return Err(AlignError::UnknownVenue(m.clone()))
            }
            _ => return Err(AlignError::MissingVenue(v.metric.clone())),
        }
    }
    if let Some((m, _)) = inputs.get(c.venues.len()) {
        return Err(AlignError::UnknownVenue(m.clone()));
    }
    let venues: Vec<VenueLine> = c
        .venues
        .iter()
        .zip(inputs)
        .map(|(v, (_, input))| VenueLine {
            metric: v.metric.clone(),
            venue: v.venue.clone(),
            outcome: match input {
                VenueInput::Recorded(f) if f.clock_check(&bucket) => Ok(*f),
                VenueInput::Recorded(_) => Err(Reason::ClockMismatch),
                VenueInput::NotObserved => Err(Reason::NotObserved),
                VenueInput::Unavailable => Err(Reason::Unavailable),
                VenueInput::NoResult => Err(Reason::NoResult),
                VenueInput::RightsWithheld => Err(Reason::RightsWithheld),
                VenueInput::Lapsed => Err(Reason::Lapsed),
            },
        })
        .collect();
    let mut values: Vec<i128> = venues
        .iter()
        .filter_map(|l| l.outcome.as_ref().ok().map(|f| f.value))
        .collect();
    values.sort_unstable();
    // Review CL8-R1-1: the payer or the coordinator never picks which venues count. No value
    // while a listed venue can still be observed (pending), none when a listed venue was never
    // observed in its range (lapsed), and none from half of the listed venues or fewer.
    let pending = inputs.iter().any(|(_, i)| *i == VenueInput::NotObserved);
    let lapsed = inputs.iter().any(|(_, i)| *i == VenueInput::Lapsed);
    let enough = !pending
        && !lapsed
        && values.len() >= usize::from(c.min_venues)
        && values.len() * 2 > c.venues.len();
    let (value, band, spread_ppm, disagreement) =
        match (enough, lower_median(&values), values.first(), values.last()) {
            (true, Some(m), Some(lo), Some(hi)) => {
                let (ppm, flag) = spread(*lo, *hi, m, c.disagree_ppm);
                (Some(m), Some((*lo, *hi)), Some(ppm), flag)
            }
            _ => (None, None, None, false),
        };
    Ok(AlignedAnswer {
        composite: c.id(),
        metric: c.metric.clone(),
        bucket,
        status: match (pending, value.is_some()) {
            (true, _) => Status::Pending,
            (false, true) => Status::Aligned,
            (false, false) => Status::Insufficient,
        },
        value,
        band,
        spread_ppm,
        disagreement,
        unit: c.unit.clone(),
        scale: c.scale,
        rights: c.rights(),
        venues,
    })
}

/// The client check (ADR-0058 item 7, THREATS CL8-4): `answer` is exactly what [`aligned`] gives
/// for its own bucket from `inputs` (which the client verified, venue by venue, as level-2
/// answers). An answer that leaves out a listed venue, adds one, moves a value, a clock, the band,
/// the flag or the rights is refused.
pub fn check_answer(
    c: &Composite,
    inputs: &[(String, VenueInput)],
    answer: &AlignedAnswer,
) -> Result<(), AlignError> {
    if answer.composite != c.id() {
        return Err(AlignError::Mismatch("another composite"));
    }
    let listed: Vec<&str> = c.venues.iter().map(|v| v.metric.as_str()).collect();
    let shown: Vec<&str> = answer.venues.iter().map(|l| l.metric.as_str()).collect();
    if listed != shown {
        return Err(AlignError::Mismatch(
            "the answer does not show exactly the composite's venues",
        ));
    }
    let want = aligned(c, answer.bucket.start, inputs)?;
    if want.id() != answer.id() {
        return Err(AlignError::Mismatch("value, band, flag, clocks or rights"));
    }
    Ok(())
}
