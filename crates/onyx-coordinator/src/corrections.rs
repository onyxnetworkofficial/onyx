//! Correction markets (docs/contributor-loop.md CL7, ADR-0057): the off-chain
//! rules a recheck runner follows and a client recomputes. `OnyxCorrections`
//! settles a market only from the paired `.recheck` metric's recorded result;
//! what the chain cannot see is checked here:
//!
//! - [`outcome`]: Yes ("accurate") when the recheck value is within the pair's
//!   tolerance of the original, else No. The contract's rule, to the unit.
//! - [`recheck_pool`] / [`check_recheck_pool`]: the recheck round's observer
//!   pool leaves out every address that bet in the market and every observer
//!   of the original round (ADR-0057 item 5, THREATS CL7-3), so the recheck is
//!   an independent look. Bets are public events and the original round's
//!   observers are in its public transcript, so a client recomputes both.
//! - [`recheck_window_start`]: the first window the recheck rule allows at or
//!   after the market's close; `None` when the recheck's days are over (a
//!   stale value: the market can only void, THREATS CL7-13).

use crate::observe::{check_pool, Observer};
use onyx_core::{FeedTemplate, Hash32, MinerAddress, Subject, SubjectRule};

/// Seconds per day.
const DAY: u64 = 86_400;

/// The side that wins a correction market (`OnyxCorrections.YES` / `NO`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// The value was accurate: the recheck is within the tolerance.
    Yes = 1,
    /// The value was wrong or is out of date.
    No = 2,
}

/// Why a recheck pool or schedule is refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CorrectionsError {
    /// The pool is no valid snapshot (empty, too large or not ascending).
    #[error("the recheck pool is not a valid pool snapshot")]
    BadPool,
    /// A bettor or an original observer is in the recheck pool.
    #[error("the recheck pool holds {0}, a bettor or an observer of the original round")]
    Excluded(String),
    /// The template is no recheck template.
    #[error("not a recheck template")]
    NotRecheck,
}

/// Yes when `|recheck - value| <= tolerance` (no overflow: the distance is
/// taken in `u128` from `i128`s), as `OnyxCorrections.resolve` decides.
pub fn outcome(value: i128, recheck: i128, tolerance: u128) -> Side {
    if value.abs_diff(recheck) <= tolerance {
        Side::Yes
    } else {
        Side::No
    }
}

fn hex(a: &MinerAddress) -> String {
    let mut s = String::from("0x");
    for b in a {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// `pool` without any bettor of the market and any observer of the original
/// round, in the same (ascending) order: the snapshot the recheck round draws
/// from. `bettors` and `originals` may be in any order and hold duplicates.
pub fn recheck_pool(
    pool: &[Observer],
    bettors: &[MinerAddress],
    originals: &[MinerAddress],
) -> Vec<Observer> {
    let mut out: Vec<MinerAddress> = bettors.iter().chain(originals).copied().collect();
    out.sort_unstable();
    out.dedup();
    pool.iter()
        .filter(|o| out.binary_search(&o.address).is_err())
        .copied()
        .collect()
}

/// A client's check of a recheck round's pool snapshot: a valid pool that
/// holds no bettor and no original observer (THREATS CL7-3).
pub fn check_recheck_pool(
    pool: &[Observer],
    bettors: &[MinerAddress],
    originals: &[MinerAddress],
) -> Result<(), CorrectionsError> {
    check_pool(pool).map_err(|_| CorrectionsError::BadPool)?;
    if let Some(o) = pool
        .iter()
        .find(|o| bettors.contains(&o.address) || originals.contains(&o.address))
    {
        return Err(CorrectionsError::Excluded(hex(&o.address)));
    }
    Ok(())
}

/// The first window start the recheck `template` allows for `subject` at or
/// after `close_unix` (the market's close, in chain time): on the feed's
/// window grid within the recheck days. `Ok(None)` when no recheck day is
/// left (the market can only void); refused for a template that is no
/// recheck.
pub fn recheck_window_start(
    template: &FeedTemplate,
    subject: &Subject,
    close_unix: u64,
) -> Result<Option<u64>, CorrectionsError> {
    if !matches!(template.subject, SubjectRule::Recheck { .. }) {
        return Err(CorrectionsError::NotRecheck);
    }
    let Some((first, last)) = template.recheck_days(subject) else {
        return Err(CorrectionsError::NotRecheck);
    };
    let (Ok(first), Ok(last)) = (u64::try_from(first), u64::try_from(last)) else {
        return Ok(None);
    };
    let step = template.window_secs.max(1);
    let after = close_unix.saturating_add(1);
    let start = first
        .saturating_mul(DAY)
        .max(after.div_ceil(step).saturating_mul(step));
    let end = last.saturating_add(1).saturating_mul(DAY);
    Ok((start < end).then_some(start))
}

/// The recheck note a value answer carries (ADR-0057 item 6): the recheck's
/// recorded result, its value hash, when it settled, and whether it matches
/// the value within the tolerance. A "No" is a note next to the value, never
/// a fall: the value keeps its level.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecheckNote {
    /// The recheck round's result hash (as recorded in `OnyxRegistry`).
    pub result: Hash32,
    /// `ladder_value_hash` of the recheck value.
    pub value_hash: Hash32,
    /// The recheck round's settle time (unix seconds).
    pub recorded_at: u64,
    /// Within the tolerance of the value.
    pub matches: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use onyx_core::{recheck_of, DataRights, MetricId, ObservationKind, Redistribution, SourceId};

    fn obs(n: u8) -> Observer {
        Observer {
            address: [n; 20],
            bond: 10,
        }
    }

    /// THREATS CL7-4 mirror: the contract's rule, at the edges and at the
    /// extremes of i128 (no overflow).
    #[test]
    fn the_tolerance_decides_like_the_contract() {
        assert_eq!(outcome(18_463, 18_465, 2), Side::Yes);
        assert_eq!(outcome(18_463, 18_461, 2), Side::Yes);
        assert_eq!(outcome(18_463, 18_466, 2), Side::No);
        assert_eq!(outcome(18_463, 18_460, 2), Side::No);
        assert_eq!(outcome(5, 5, 0), Side::Yes);
        assert_eq!(outcome(i128::MIN, i128::MAX, u128::MAX), Side::Yes);
        assert_eq!(outcome(i128::MIN, i128::MAX, u128::MAX - 1), Side::No);
        assert_eq!(Side::Yes as u8, 1);
        assert_eq!(Side::No as u8, 2);
    }

    /// THREATS CL7-3: the recheck pool excludes bettors and original
    /// observers; a client refuses a pool that holds any of them.
    #[test]
    fn the_recheck_pool_excludes_bettors_and_original_observers() {
        let pool: Vec<Observer> = (1..=8).map(obs).collect();
        let bettors = [[3; 20], [9; 20], [3; 20]];
        let originals = [[6; 20], [1; 20]];
        let p = recheck_pool(&pool, &bettors, &originals);
        assert_eq!(
            p.iter().map(|o| o.address[0]).collect::<Vec<_>>(),
            vec![2, 4, 5, 7, 8]
        );
        check_recheck_pool(&p, &bettors, &originals).expect("clean");
        assert_eq!(
            check_recheck_pool(&pool, &bettors, &originals),
            Err(CorrectionsError::Excluded(hex(&[1; 20])))
        );
        assert_eq!(
            check_recheck_pool(&pool[1..], &bettors, &[]),
            Err(CorrectionsError::Excluded(hex(&[3; 20])))
        );
        assert_eq!(
            check_recheck_pool(&[], &[], &[]),
            Err(CorrectionsError::BadPool)
        );
        let mut unsorted = p.clone();
        unsorted.reverse();
        assert_eq!(
            check_recheck_pool(&unsorted, &bettors, &originals),
            Err(CorrectionsError::BadPool)
        );
    }

    fn daily() -> FeedTemplate {
        FeedTemplate {
            feed: "quakes.count".into(),
            metric: MetricId("quakes.count".into()),
            fetcher: onyx_core::keccak256(b"fetcher"),
            url_template: "https://example.org/c?d={date}".into(),
            selector: "count".into(),
            kind: ObservationKind::Numeric {
                scale: 0,
                tolerance: 1,
            },
            window_secs: 600,
            min_observers: 5,
            rights: DataRights {
                license: "public domain".into(),
                redistribution: Redistribution::Allowed,
                source: SourceId("example".into()),
                attribution: None,
                terms: String::new(),
            },
            subject: SubjectRule::Daily {
                prefix: "quakes".into(),
                min_days: 1,
                max_days: 30,
            },
        }
    }

    /// The runner's schedule: the first grid window at or after the close and
    /// within the recheck days; none once they are over (a stale value).
    #[test]
    fn the_recheck_runs_in_the_first_allowed_window_after_the_close() {
        let t = daily();
        let r = recheck_of(&t, 5, 2).expect("recheck");
        let s = Subject::Aggregate("quakes:2026-10-01".into());
        let first = u64::try_from(onyx_core::day_number("2026-10-01").expect("day") + 35)
            .expect("u64")
            * DAY;
        // Closed long before the recheck days: the first recheck day's start.
        assert_eq!(recheck_window_start(&r, &s, 0), Ok(Some(first)));
        // Closed inside them: the next grid window strictly after the close.
        assert_eq!(
            recheck_window_start(&r, &s, first + 601),
            Ok(Some(first + 1_200))
        );
        assert_eq!(
            recheck_window_start(&r, &s, first + 600),
            Ok(Some(first + 1_200))
        );
        let start = recheck_window_start(&r, &s, first + 5_000)
            .expect("ok")
            .expect("some");
        r.instantiate(&s, start).expect("an allowed window");
        // Over: no window, the market can only void.
        assert_eq!(recheck_window_start(&r, &s, first + 2 * DAY), Ok(None));
        assert_eq!(
            recheck_window_start(&t, &s, 0),
            Err(CorrectionsError::NotRecheck)
        );
    }
}
