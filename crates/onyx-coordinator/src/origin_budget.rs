//! Per-origin fetch budget for level-2 rounds (partner tracker M2, a partner's Q4).
//!
//! Each drawn observer of a round fetches the source once inside the window.
//! A coordinator that opens many rounds on one origin at once would still send
//! a burst of fetches to it, and a rate-limited source could block every
//! observer. [`OriginLoad`] admits a round only while the observers of all
//! open rounds whose windows overlap it, on the same origin
//! ([`onyx_core::fetch::url_origin`]), stay within the budget. Observers spread
//! their own fetches over the window ([`onyx_core::fetch::fetch_time`]).
//!
//! The count is conservative: rounds that overlap the new one are summed even
//! when they do not overlap each other.

use onyx_core::fetch::url_origin;
use onyx_core::proof::ObservationSpec;
use std::collections::BTreeMap;

/// Default most observer fetches one origin gets from overlapping open rounds.
pub const DEFAULT_MAX_FETCHES_PER_ORIGIN: u32 = 16;

/// Why a round is not admitted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OriginBudgetError {
    /// The spec's URL has no origin a budget can be kept for.
    #[error("no budget origin in the spec url")]
    NoOrigin,
    /// A round on this origin and window is already open.
    #[error("a round on {origin} for this window is already open")]
    AlreadyOpen {
        /// The origin.
        origin: String,
    },
    /// Admitting the round would pass the origin's budget.
    #[error("{origin}: {load} fetches already overlap this window, budget {max}")]
    OverBudget {
        /// The origin.
        origin: String,
        /// Fetches already planned by overlapping open rounds.
        load: u32,
        /// The budget.
        max: u32,
    },
}

/// One open round as the budget sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Open {
    feed: String,
    window_start: u64,
    window_end: u64,
    observers: u32,
}

/// Open rounds per origin, and the budget they share.
#[derive(Debug, Clone)]
pub struct OriginLoad {
    max: u32,
    open: BTreeMap<String, Vec<Open>>,
}

impl OriginLoad {
    /// A budget of `max` fetches per origin over overlapping windows. A budget
    /// below one round's observers admits no round of that size.
    pub fn new(max: u32) -> Self {
        Self {
            max,
            open: BTreeMap::new(),
        }
    }

    /// Fetches the open rounds on `spec`'s origin already plan inside its
    /// window.
    pub fn load(&self, spec: &ObservationSpec) -> Result<u32, OriginBudgetError> {
        let origin = url_origin(&spec.url).ok_or(OriginBudgetError::NoOrigin)?;
        Ok(self.overlapping(&origin, spec))
    }

    fn overlapping(&self, origin: &str, spec: &ObservationSpec) -> u32 {
        self.open
            .get(origin)
            .map(|rounds| {
                rounds
                    .iter()
                    .filter(|r| {
                        r.window_start <= spec.window_end && spec.window_start <= r.window_end
                    })
                    .fold(0u32, |n, r| n.saturating_add(r.observers))
            })
            .unwrap_or(0)
    }

    /// Admit a round of `spec`, or say why not. Nothing changes on refusal.
    pub fn admit(&mut self, spec: &ObservationSpec) -> Result<(), OriginBudgetError> {
        let origin = url_origin(&spec.url).ok_or(OriginBudgetError::NoOrigin)?;
        let rounds = self.open.get(&origin);
        if rounds.is_some_and(|rs| {
            rs.iter().any(|r| {
                r.feed == spec.feed
                    && r.window_start == spec.window_start
                    && r.window_end == spec.window_end
            })
        }) {
            return Err(OriginBudgetError::AlreadyOpen { origin });
        }
        let load = self.overlapping(&origin, spec);
        let want = u32::from(spec.min_observers);
        if load.saturating_add(want) > self.max {
            return Err(OriginBudgetError::OverBudget {
                origin,
                load,
                max: self.max,
            });
        }
        self.open.entry(origin).or_default().push(Open {
            feed: spec.feed.clone(),
            window_start: spec.window_start,
            window_end: spec.window_end,
            observers: want,
        });
        Ok(())
    }

    /// Forget every open round whose window ended before `now`: its fetches
    /// are over.
    pub fn prune(&mut self, now: u64) {
        for rounds in self.open.values_mut() {
            rounds.retain(|r| r.window_end >= now);
        }
        self.open.retain(|_, rounds| !rounds.is_empty());
    }

    /// Open rounds counted, over all origins.
    pub fn open_rounds(&self) -> usize {
        self.open.values().map(Vec::len).sum()
    }
}

impl Default for OriginLoad {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_FETCHES_PER_ORIGIN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onyx_core::hash::keccak256;
    use onyx_core::proof::ObservationKind;
    use onyx_core::rights::{DataRights, Redistribution};
    use onyx_core::source::SourceId;

    fn spec(url: &str, feed: &str, start: u64, observers: u16) -> ObservationSpec {
        ObservationSpec {
            feed: feed.into(),
            metric: onyx_core::MetricId(feed.into()),
            subject: onyx_core::Subject::Entity("entity:example-exchange".into()),
            fetcher: keccak256(b"fetcher"),
            url: url.into(),
            selector: "/data/value".into(),
            kind: ObservationKind::Exact { max_len: 16 },
            window_start: start,
            window_end: start + 300,
            min_observers: observers,
            rights: DataRights {
                license: "CC0-1.0".into(),
                redistribution: Redistribution::Allowed,
                source: SourceId("example.page".into()),
                attribution: None,
                terms: String::new(),
            },
        }
    }

    const A: &str = "https://api.example.com/x";

    #[test]
    fn one_origin_is_capped_over_overlapping_windows() {
        let mut b = OriginLoad::new(16);
        b.admit(&spec(A, "a.one", 1_000, 5)).expect("admitted");
        b.admit(&spec(A, "a.two", 1_100, 5)).expect("admitted");
        b.admit(&spec(A, "a.three", 1_200, 5)).expect("admitted");
        // 15 planned; a fourth round of 5 would make 20.
        assert_eq!(
            b.admit(&spec(A, "a.four", 1_250, 5)),
            Err(OriginBudgetError::OverBudget {
                origin: "api.example.com".into(),
                load: 15,
                max: 16
            })
        );
        // A round of 1 still fits.
        b.admit(&spec(A, "a.five", 1_250, 1)).expect("admitted");
        assert_eq!(b.open_rounds(), 4);
    }

    #[test]
    fn spellings_of_one_origin_share_its_budget() {
        let mut b = OriginLoad::new(8);
        b.admit(&spec("https://API.example.com/x", "a.one", 1_000, 5))
            .expect("admitted");
        assert!(matches!(
            b.admit(&spec("https://api.example.com.:443/y?z", "a.two", 1_000, 5)),
            Err(OriginBudgetError::OverBudget { .. })
        ));
    }

    #[test]
    fn other_origins_and_later_windows_do_not_count() {
        let mut b = OriginLoad::new(10);
        b.admit(&spec(A, "a.one", 1_000, 10)).expect("admitted");
        b.admit(&spec("https://other.example.org/", "b.one", 1_000, 10))
            .expect("admitted");
        // Starts after the first window ends (1_300).
        b.admit(&spec(A, "a.two", 1_301, 10)).expect("admitted");
    }

    #[test]
    fn one_round_larger_than_the_budget_is_refused() {
        let mut b = OriginLoad::new(16);
        assert!(matches!(
            b.admit(&spec(A, "a.one", 1_000, 17)),
            Err(OriginBudgetError::OverBudget {
                load: 0,
                max: 16,
                ..
            })
        ));
        assert_eq!(b.open_rounds(), 0);
    }

    #[test]
    fn the_same_round_is_not_counted_twice() {
        let mut b = OriginLoad::new(100);
        b.admit(&spec(A, "a.one", 1_000, 5)).expect("admitted");
        assert!(matches!(
            b.admit(&spec(A, "a.one", 1_000, 5)),
            Err(OriginBudgetError::AlreadyOpen { .. })
        ));
    }

    #[test]
    fn prune_frees_the_budget_after_the_window() {
        let mut b = OriginLoad::new(5);
        b.admit(&spec(A, "a.one", 1_000, 5)).expect("admitted");
        assert!(b.admit(&spec(A, "a.two", 1_200, 5)).is_err());
        b.prune(1_301);
        assert_eq!(b.open_rounds(), 0);
        b.admit(&spec(A, "a.two", 1_400, 5)).expect("admitted");
    }

    #[test]
    fn a_url_without_an_origin_is_refused() {
        let mut b = OriginLoad::default();
        assert_eq!(
            b.admit(&spec("https://127.0.0.1/x", "a.one", 1_000, 3)),
            Err(OriginBudgetError::NoOrigin)
        );
    }
}
