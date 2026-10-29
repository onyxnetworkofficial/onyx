//! Daily level-2 facts as track-record facts and resolutions (ADR-0036 amendment 7,
//! docs/spec/track-records-served.md, NORTH-STAR NS16, ROADMAP C66).
//!
//! A daily level-2 metric (one fact per UTC calendar day, subjects `<prefix>:YYYY-MM-DD`) gets
//! a close-time rule: statements about day `D` close [`DAILY_LEAD_SECS`] before `D` starts in
//! UTC, so no part of the day is public when they close (a local day west of UTC starts later
//! still). The fact is the period `[D, D + 1)`, as of the next UTC midnight.
//!
//! A round becomes a resolution only once the client's own node shows it recorded at level 2
//! ([`Recorded`], from `verify_observation_onchain`): an open, failed or unrecorded round gives
//! none (R-TRS-1), so a score never rests on a value that is not settled. Nothing here decides
//! or returns a level: the recorded level is the chain's, checked by the caller's node.

use crate::{Fact, MetricRule, Resolution, TrackParams};
use onyx_core::{day_number, Hash32, MetricId, ProofLevel, SourceId, Subject, Value};

/// One UTC day.
pub const DAY: u64 = 86_400;
/// Statements about a day close this long before the UTC day starts.
pub const DAILY_LEAD_SECS: u64 = 3_600;

/// Why a round gives no resolution, or a subject no fact.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum DailyError {
    /// The subject is not `<prefix>:YYYY-MM-DD`.
    #[error("subject {0:?} is not a day of this metric")]
    BadSubject(String),
    /// The client's node does not show the round recorded: it is open, failed or unknown.
    #[error("the round is not recorded on chain (open or failed): no resolution")]
    OpenRound,
    /// The chain records the round at another level than 2.
    #[error("the round is recorded at level {0}, not 2")]
    WrongLevel(u8),
    /// The round's result hash is zero.
    #[error("zero result hash")]
    ZeroEvidence,
    /// The round was recorded before its day ended, which no daily rule allows.
    #[error("recorded at {recorded} before the day ended at {day_end}")]
    Early {
        /// When the round was recorded.
        recorded: u64,
        /// When the day ended.
        day_end: u64,
    },
}

/// What the client's own node showed about a round: the level the registry records it at and
/// the block time it became public (both from `verify_observation_onchain`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recorded {
    /// Level the registry records.
    pub level: ProofLevel,
    /// Block time of the record (unix seconds).
    pub public_from: u64,
}

impl Recorded {
    /// A round the registry records at level 2, public from `public_from`.
    pub fn level2(public_from: u64) -> Self {
        Recorded {
            level: ProofLevel::ReObserved,
            public_from,
        }
    }
}

impl TrackParams {
    /// The same params with level 2 as the only resolver level: for daily level-2 metrics,
    /// whose values are checked at level 2 (ADR-0036 amendment 7 item 2).
    pub fn with_level2_resolvers_only(mut self) -> Self {
        self.resolver_levels = vec![ProofLevel::ReObserved];
        self
    }
}

/// The close-time rule of a daily metric: a UTC day, closing [`DAILY_LEAD_SECS`] before it
/// starts; `tolerance` in the value's own units (mantissa units for `Fixed`).
pub fn daily_rule(metric: MetricId, tolerance: u128) -> MetricRule {
    MetricRule::period_start(metric, DAY, DAILY_LEAD_SECS, tolerance)
}

/// The fact of `subject` (`<prefix>:YYYY-MM-DD`): the UTC day, as of the next midnight.
pub fn daily_fact(metric: MetricId, prefix: &str, subject: &str) -> Result<Fact, DailyError> {
    let bad = || DailyError::BadSubject(subject.into());
    let date = subject
        .strip_prefix(prefix)
        .and_then(|s| s.strip_prefix(':'))
        .ok_or_else(bad)?;
    let day = u64::try_from(day_number(date).ok_or_else(bad)?).map_err(|_| bad())?;
    let as_of = day
        .checked_add(1)
        .and_then(|d| d.checked_mul(DAY))
        .ok_or_else(bad)?;
    Ok(Fact {
        metric,
        subject: Subject::Aggregate(subject.into()),
        as_of,
    })
}

/// The resolution a recorded level-2 round gives: `fact`, the settled `value`, the source the
/// value was read from as resolver, the round's result hash as evidence, public from the
/// record's block time. `recorded` is `None` while the client's node does not show the round
/// recorded (open or failed): then there is no resolution (R-TRS-1).
pub fn resolution_of(
    fact: Fact,
    value: Value,
    result: Hash32,
    recorded: Option<Recorded>,
    resolver: SourceId,
) -> Result<Resolution, DailyError> {
    let rec = recorded.ok_or(DailyError::OpenRound)?;
    if rec.level != ProofLevel::ReObserved {
        return Err(DailyError::WrongLevel(rec.level.number()));
    }
    if result == Hash32::ZERO {
        return Err(DailyError::ZeroEvidence);
    }
    if rec.public_from < fact.as_of {
        return Err(DailyError::Early {
            recorded: rec.public_from,
            day_end: fact.as_of,
        });
    }
    Ok(Resolution {
        fact,
        value,
        level: rec.level,
        resolver,
        evidence: result,
        public_from: rec.public_from,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use onyx_core::keccak256;

    const PREFIX: &str = "test.station";
    const SUBJECT: &str = "test.station:2026-09-27";
    /// 2026-09-28 00:00 UTC.
    const DAY_END: u64 = 1_790_553_600;

    fn metric() -> MetricId {
        MetricId("test.daily.tmax".into())
    }

    fn fact() -> Fact {
        daily_fact(metric(), PREFIX, SUBJECT).expect("fact")
    }

    fn tmax(m: i128) -> Value {
        Value::Fixed {
            mantissa: m,
            scale: 1,
        }
    }

    fn archive() -> SourceId {
        SourceId("test.archive".into())
    }

    #[test]
    fn a_final_round_gives_one_level_2_resolution() {
        let public = DAY_END + 30 * DAY;
        let r = resolution_of(
            fact(),
            tmax(283),
            keccak256(b"result"),
            Some(Recorded::level2(public)),
            archive(),
        )
        .expect("resolution");
        assert_eq!(r.fact.as_of, DAY_END);
        assert_eq!(r.fact.subject, Subject::Aggregate(SUBJECT.into()));
        assert_eq!(r.value, tmax(283));
        assert_eq!(r.level, ProofLevel::ReObserved);
        assert_eq!(r.resolver, archive());
        assert_eq!(r.evidence, keccak256(b"result"));
        assert_eq!(r.public_from, public);
    }

    /// The Done-when test of NL4: no score from an open round.
    #[test]
    fn no_resolution_from_an_open_round() {
        let h = keccak256(b"result");
        let ask = |rec: Option<Recorded>, result: Hash32| {
            resolution_of(fact(), tmax(283), result, rec, archive()).err()
        };
        assert_eq!(ask(None, h), Some(DailyError::OpenRound));
        let other = Recorded {
            level: ProofLevel::BondedClaim,
            public_from: DAY_END + DAY,
        };
        assert_eq!(ask(Some(other), h), Some(DailyError::WrongLevel(4)));
        assert_eq!(
            ask(Some(Recorded::level2(DAY_END + DAY)), Hash32::ZERO),
            Some(DailyError::ZeroEvidence)
        );
        assert_eq!(
            ask(Some(Recorded::level2(DAY_END - 1)), h),
            Some(DailyError::Early {
                recorded: DAY_END - 1,
                day_end: DAY_END
            })
        );
    }

    #[test]
    fn the_daily_rule_and_fact_name_one_utc_day() {
        assert_eq!(
            daily_rule(metric(), 5),
            MetricRule::period_start(metric(), DAY, DAILY_LEAD_SECS, 5)
        );
        assert_eq!(fact().as_of, DAY_END);
        assert_eq!(fact().as_of % DAY, 0);
        for bad in [
            "test.station:2026-9-27",
            "other.station:2026-09-27",
            "test.station2026-09-27",
            "test.station:1960-01-01",
        ] {
            assert!(daily_fact(metric(), PREFIX, bad).is_err(), "{bad}");
        }
        let p = TrackParams::new(vec![]).with_level2_resolvers_only();
        assert_eq!(p.resolver_levels, vec![ProofLevel::ReObserved]);
    }
}
