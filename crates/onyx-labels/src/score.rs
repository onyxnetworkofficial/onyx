//! Calibration scoring for subjective labels (ADR-0016, amended loop 14).
//!
//! A contributor's label is a forecast about one address: "it belongs to
//! entity E with probability p". The rest, `1 - p`, is read as spread evenly
//! over the other `K - 1` registered entities. So a forecast is a full
//! distribution over who owns the address, and "p = 0 for an entity that
//! obviously is not the owner" says almost nothing and scores badly (L13-F1).
//!
//! When ground truth is published (a verified first-party attestation), each
//! forecast on that address committed in an earlier round is scored:
//! - Brier over the K entities, primary, in [0, 2], strictly proper in p;
//! - logarithmic `-log2(prob given to the owner)` in micro-bits, clamped at
//!   [`LOG_FLOOR_PPM`]; reported only (truncated fixed point, so not exactly
//!   proper at ppm resolution, L13-F7).
//!
//! Anti-farming rules: forecasts by a key registered to the owning entity do
//! not count (self-resolution); at most [`MAX_PER_OWNER`] forecasts per owner
//! count for one contributor; a score is eligible only with [`MIN_RESOLVED`]
//! forecasts over at least [`MIN_DISTINCT_OWNERS`] owners. These are not a
//! Sybil defence: an entity with a fresh contributor key still gets perfect
//! forecasts on its own addresses (L14-F4). Stake-bound contributors and
//! reward weighting come in M4; until then scores do not drive rewards.
//!
//! All arithmetic is integer, so every validator gets the same scores. Lower is better.

use crate::attest::{AttestContext, Attestation, Statement};
use onyx_core::ids::parse_address;
use onyx_core::{Confidence, Hash32, MinerAddress, Registry};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

/// Floor for the probability given to the owner in the log score: 0.1%.
pub const LOG_FLOOR_PPM: u32 = 1_000;
/// Resolved forecasts needed before a score may affect rewards.
pub const MIN_RESOLVED: u64 = 30;
/// Distinct owning entities needed before a score may affect rewards.
pub const MIN_DISTINCT_OWNERS: u64 = 5;
/// Most scored forecasts per (contributor, owning entity).
pub const MAX_PER_OWNER: u64 = 10;
/// Most forecasts in one scoring call (keeps all sums inside `u128`).
pub const MAX_FORECASTS: usize = 1 << 24;

const ONE: u32 = Confidence::ONE;
const ONE_SQ: u128 = 1_000_000_000_000;
/// Fractional bits of the fixed-point log2.
const LOG_FRAC_BITS: u32 = 48;

/// One label read as a forecast. One per (contributor, address).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Forecast {
    /// Contributor who committed it.
    pub contributor: MinerAddress,
    /// Namespaced canonical address.
    pub address: String,
    /// Entity named.
    pub entity: String,
    /// Probability that `address` belongs to `entity`.
    pub p: Confidence,
    /// Onyx round in which the forecast was committed (commit-reveal, M3).
    pub committed_round: u64,
}

/// Published ground truth: `address` belongs to `owner`, and to no other entity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolution {
    /// Namespaced canonical address.
    pub address: String,
    /// Entity that controls it.
    pub owner: String,
    /// Onyx round in which the evidence was first finalized. Not the
    /// statement's `valid_from`: the signer picks that and could post-date it.
    pub published_round: u64,
}

/// Ground truth from attestations, each with the round it was finalized in
/// (from the chain's record of finalized roots, M3). Each attestation is
/// verified at its own `valid_from` height, not over a range the caller picks,
/// so a renewal cannot hide an earlier one (L14-F2): the resolution's round is
/// the earliest round of any attestation that verifies for that (entity,
/// address). An address claimed by two different entities (for example
/// custodian and fund) is ambiguous and left out. Sorted by address.
pub fn resolutions_from_attestations(
    atts: &[(Attestation, u64)],
    reg: &dyn Registry,
) -> Vec<Resolution> {
    let mut by_address: BTreeMap<String, (BTreeSet<String>, u64)> = BTreeMap::new();
    for (a, round) in atts {
        let Ok(parsed) = Statement::parse(&a.statement) else {
            continue;
        };
        let Some((ns, _)) = parse_address(&parsed.address) else {
            continue;
        };
        let at_start = [(ns, parsed.valid_from, parsed.valid_from)];
        let Ok(st) = a.verify_history(&AttestContext { ranges: &at_start }, reg, *round) else {
            continue;
        };
        let e = by_address
            .entry(st.address)
            .or_insert_with(|| (BTreeSet::new(), *round));
        e.0.insert(st.entity);
        e.1 = e.1.min(*round);
    }
    by_address
        .into_iter()
        .filter(|(_, (owners, _))| owners.len() == 1)
        .filter_map(|(address, (owners, published_round))| {
            owners.into_iter().next().map(|owner| Resolution {
                address,
                owner,
                published_round,
            })
        })
        .collect()
}

/// Running score of one contributor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Score {
    /// Forecasts scored.
    pub n: u64,
    /// Sum of Brier terms in units of 1e-12.
    pub brier_sum: u128,
    /// Sum of log terms in micro-bits.
    pub log_sum: u128,
    /// Distinct owning entities among scored forecasts.
    pub owners: u64,
    /// Forecasts on resolved addresses that were committed too late to count.
    pub late: u64,
    /// Forecasts not counted: by the owner's own key, or over [`MAX_PER_OWNER`].
    pub excluded: u64,
}

/// Scoring input error.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ScoreError {
    /// Probability above one.
    #[error("probability out of range")]
    OutOfRange,
    /// Forecasts not strictly sorted by (contributor, address): one per address.
    #[error("forecasts not canonical")]
    ForecastsNotCanonical,
    /// Resolutions not strictly sorted by address (one owner per address).
    #[error("resolutions not canonical")]
    ResolutionsNotCanonical,
    /// More than [`MAX_FORECASTS`].
    #[error("too many forecasts")]
    TooMany,
    /// The registry is not the one the scoring epoch pins.
    #[error("registry root differs from the scoring epoch's")]
    WrongRegistry,
    /// Fewer than 2 registered entities.
    #[error("need at least 2 candidate entities")]
    TooFewCandidates,
}

/// Brier term over `candidates` entities, in units of 1e-12, floored.
/// `named_is_owner`: the named entity is the owner.
/// - owner named: `(1-p)^2 + (1-p)^2/m`
/// - owner not named: `p^2 + 1 - (1-p^2)/m`, with `m = candidates - 1`.
pub fn brier_term(p: Confidence, named_is_owner: bool, candidates: u64) -> u128 {
    let p = u128::from(p.0.min(ONE));
    let m = u128::from(candidates.max(2) - 1);
    let one = u128::from(ONE);
    if named_is_owner {
        let r = (one - p) * (one - p);
        r + r / m
    } else {
        let x = ONE_SQ - p * p;
        p * p + ONE_SQ - x.div_ceil(m)
    }
}

/// Log term: `-log2(prob given to the owner)` in micro-bits, truncated, with
/// that probability clamped to at least [`LOG_FLOOR_PPM`]. Reported only.
pub fn log_term(p: Confidence, named_is_owner: bool, candidates: u64) -> u64 {
    let p = u64::from(p.0.min(ONE));
    let m = candidates.max(2) - 1;
    // Probability given to the owner as num / (den * ONE).
    let (num, den) = if named_is_owner {
        (p, 1)
    } else {
        (u64::from(ONE) - p, m)
    };
    let floor = u64::from(LOG_FLOOR_PPM);
    let bits = if u128::from(num) < u128::from(floor) * u128::from(den) {
        log2_fixed(u64::from(ONE)) - log2_fixed(floor)
    } else {
        // log2(ONE) + log2(den) - log2(num) >= 0 because num/den <= ONE.
        (log2_fixed(u64::from(ONE)) + log2_fixed(den)).saturating_sub(log2_fixed(num))
    };
    u64::try_from((u128::from(bits) * 1_000_000) >> LOG_FRAC_BITS).unwrap_or(u64::MAX)
}

/// log2(x) for x >= 1 in fixed point with [`LOG_FRAC_BITS`] fractional bits,
/// truncated. Classic bit-by-bit method: square the mantissa, and each time it
/// reaches 2, emit a 1 bit and halve it. Integer only, so exact everywhere.
fn log2_fixed(x: u64) -> u64 {
    debug_assert!(x >= 1);
    let int = u64::from(63 - x.max(1).leading_zeros());
    // Mantissa in [1, 2) as Q.62.
    let mut m: u128 = if int <= 62 {
        u128::from(x) << (62 - int)
    } else {
        u128::from(x) >> (int - 62)
    };
    let two: u128 = 2 << 62;
    let mut frac: u64 = 0;
    for bit in (0..LOG_FRAC_BITS).rev() {
        m = (m * m) >> 62;
        if m >= two {
            m >>= 1;
            frac |= 1 << bit;
        }
    }
    (int << LOG_FRAC_BITS) | frac
}

impl Score {
    /// Mean Brier score in ppm (0 = perfect, 2,000,000 = worst), floored.
    pub fn mean_brier_ppm(&self) -> Option<u64> {
        (self.n > 0).then(|| {
            u64::try_from(self.brier_sum / u128::from(self.n) / 1_000_000).unwrap_or(u64::MAX)
        })
    }

    /// Mean log score in micro-bits, floored.
    pub fn mean_log_microbits(&self) -> Option<u64> {
        (self.n > 0).then(|| u64::try_from(self.log_sum / u128::from(self.n)).unwrap_or(u64::MAX))
    }

    /// True once enough forecasts over enough owners are resolved for the
    /// score to affect rewards.
    pub fn eligible(&self) -> bool {
        self.n >= MIN_RESOLVED && self.owners >= MIN_DISTINCT_OWNERS
    }

    /// Comparison of mean Brier (`Less` = better) by cross-multiplication of
    /// the integer sums, so no second rounding step can reorder two
    /// contributors. No forecasts sorts last.
    pub fn brier_cmp(&self, other: &Score) -> Ordering {
        match (self.n, other.n) {
            (0, 0) => Ordering::Equal,
            (0, _) => Ordering::Greater,
            (_, 0) => Ordering::Less,
            (a, b) => (self.brier_sum * u128::from(b)).cmp(&(other.brier_sum * u128::from(a))),
        }
    }
}

/// Score every contributor against ground truth built here, under the pinned
/// registry, from `(attestation, finalized round)` pairs (L16-F9): no caller
/// can pass resolutions made under another registry. `K` is the number of
/// entities in that registry (L14-F3).
pub fn score_all(
    forecasts: &[Forecast],
    attestations: &[(Attestation, u64)],
    reg: &dyn Registry,
    registry_root: Hash32,
) -> Result<BTreeMap<MinerAddress, Score>, ScoreError> {
    if reg.root() != registry_root {
        return Err(ScoreError::WrongRegistry);
    }
    let resolutions = resolutions_from_attestations(attestations, reg);
    score_resolved(forecasts, &resolutions, reg, registry_root)
}

/// Score against already built resolutions (tests and [`score_all`]). Forecasts must be strictly sorted by (contributor,
/// address); resolutions strictly by address. A forecast counts only if
/// committed in a round before its resolution was published, was not made by
/// a key registered to the owner, and is within [`MAX_PER_OWNER`] for that
/// contributor and owner (in address order).
pub(crate) fn score_resolved(
    forecasts: &[Forecast],
    resolutions: &[Resolution],
    reg: &dyn Registry,
    registry_root: Hash32,
) -> Result<BTreeMap<MinerAddress, Score>, ScoreError> {
    // The scoring epoch pins one registry, so K cannot move under it (L15-F6).
    if reg.root() != registry_root {
        return Err(ScoreError::WrongRegistry);
    }
    let candidates = reg.entity_count();
    if forecasts.len() > MAX_FORECASTS {
        return Err(ScoreError::TooMany);
    }
    if candidates < 2 {
        return Err(ScoreError::TooFewCandidates);
    }
    if forecasts
        .windows(2)
        .any(|w| (w[0].contributor, &w[0].address) >= (w[1].contributor, &w[1].address))
    {
        return Err(ScoreError::ForecastsNotCanonical);
    }
    if resolutions.windows(2).any(|w| w[0].address >= w[1].address) {
        return Err(ScoreError::ResolutionsNotCanonical);
    }
    if forecasts.iter().any(|f| f.p.0 > ONE) {
        return Err(ScoreError::OutOfRange);
    }
    let mut out: BTreeMap<MinerAddress, Score> = BTreeMap::new();
    let mut per_owner: BTreeMap<(MinerAddress, &str), u64> = BTreeMap::new();
    for f in forecasts {
        let Ok(i) = resolutions.binary_search_by(|r| r.address.as_str().cmp(&f.address)) else {
            continue;
        };
        let r = &resolutions[i];
        let s = out.entry(f.contributor).or_default();
        if f.committed_round >= r.published_round {
            s.late += 1;
            continue;
        }
        // Any key the owner ever registered, retired ones included (L15-F1).
        if reg.entity_key_state(&r.owner, &f.contributor).is_some() {
            s.excluded += 1;
            continue;
        }
        let used = per_owner
            .entry((f.contributor, r.owner.as_str()))
            .or_insert(0);
        if *used >= MAX_PER_OWNER {
            s.excluded += 1;
            continue;
        }
        if *used == 0 {
            s.owners += 1;
        }
        *used += 1;
        let hit = f.entity == r.owner;
        s.n += 1;
        s.brier_sum += brier_term(f.p, hit, candidates);
        s.log_sum += u128::from(log_term(f.p, hit, candidates));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use onyx_core::{EntityKind, StaticRegistry};

    /// Registry with `k` entities (K for scoring).
    fn kreg(k: u64) -> StaticRegistry {
        let mut r = StaticRegistry::new();
        for i in 0..k {
            r.add_entity(&format!("entity:k{i}"), EntityKind::Fund)
                .expect("entity");
        }
        r
    }

    const fn c(ppm: u32) -> Confidence {
        Confidence(ppm)
    }

    /// With 2 candidates the log term is the binary one. Reference values:
    /// floor(-log2(p) * 1e6) from Python `math.log2`.
    #[test]
    fn log_term_matches_reference() {
        let table: [(u32, u64); 12] = [
            (1_000, 9_965_784),
            (1_234, 9_662_441),
            (10_000, 6_643_856),
            (100_000, 3_321_928),
            (250_000, 2_000_000),
            (333_333, 1_584_963),
            (500_000, 1_000_000),
            (700_000, 514_573),
            (900_000, 152_003),
            (990_000, 14_499),
            (999_999, 1),
            (1_000_000, 0),
        ];
        for (p, want) in table {
            let got = log_term(c(p), true, 2);
            assert!(got.abs_diff(want) <= 1, "p={p}: {got} vs {want}");
            assert_eq!(log_term(c(ONE - p), false, 2), got, "complement p={p}");
        }
        // Owner not named, 5 candidates, p = 0: owner gets 1/4, i.e. 2 bits.
        assert_eq!(log_term(c(0), false, 5), 2_000_000);
        // Clamped: a certain wrong answer costs the same as 0.1%.
        assert_eq!(log_term(c(0), true, 2), log_term(c(1_000), true, 2));
        assert_eq!(log_term(c(ONE), false, 9), log_term(c(1_000), true, 2));
    }

    #[test]
    fn brier_term_exact() {
        // Two candidates: twice the binary Brier score.
        assert_eq!(brier_term(c(ONE), true, 2), 0);
        assert_eq!(brier_term(c(0), true, 2), 2 * ONE_SQ);
        assert_eq!(brier_term(c(900_000), true, 2), 2 * 10_000_000_000);
        assert_eq!(brier_term(c(900_000), false, 2), 2 * 810_000_000_000);
        // 11 candidates (m = 10), p = 0 on a non-owner: owner gets 0.1.
        // 0 + (0.9)^2 + 9 * 0.01 = 0.9.
        assert_eq!(brier_term(c(0), false, 11), 900_000_000_000);
    }

    /// L13-F1: "p = 0 for an entity that is not the owner" is no longer free.
    #[test]
    fn padding_with_p_zero_scores_badly() {
        let k = 50;
        let honest = brier_term(c(900_000), true, k);
        let padding = brier_term(c(0), false, k);
        assert!(padding > 40 * honest, "{padding} vs {honest}");
    }

    /// Strict propriety in p on a grid: if the named entity owns the address
    /// with probability q (else a uniformly random other entity), reporting q
    /// gives the strictly lowest expected Brier score.
    #[test]
    fn brier_is_strictly_proper_on_grid() {
        let grid: Vec<u32> = (0..=100).map(|i| i * 10_000).collect();
        for k in [2u64, 3, 10, 1000] {
            for &q in &grid {
                let expected = |p: u32| {
                    u128::from(q) * brier_term(c(p), true, k)
                        + u128::from(ONE - q) * brier_term(c(p), false, k)
                };
                let best = expected(q);
                for &p in &grid {
                    if p != q {
                        assert!(expected(p) > best, "k={k} q={q} p={p}");
                    }
                }
            }
        }
    }

    /// The log score is proper on a 1% grid away from the clamp only; it is
    /// reported, not used for ranking (L13-F7).
    #[test]
    fn log_is_proper_on_coarse_grid() {
        let grid: Vec<u32> = (1..100).map(|i| i * 10_000).collect();
        for &q in &grid {
            let expected = |p: u32| {
                u128::from(q) * u128::from(log_term(c(p), true, 2))
                    + u128::from(ONE - q) * u128::from(log_term(c(p), false, 2))
            };
            let best = expected(q);
            for &p in &grid {
                assert!(expected(p) >= best, "q={q} p={p}");
            }
        }
    }

    fn f(who: u8, addr: &str, entity: &str, p: u32, round: u64) -> Forecast {
        Forecast {
            contributor: [who; 20],
            address: addr.into(),
            entity: entity.into(),
            p: c(p),
            committed_round: round,
        }
    }

    fn r(addr: &str, owner: &str, round: u64) -> Resolution {
        Resolution {
            address: addr.into(),
            owner: owner.into(),
            published_round: round,
        }
    }

    fn sorted(mut fs: Vec<Forecast>) -> Vec<Forecast> {
        fs.sort_by(|a, b| (a.contributor, &a.address).cmp(&(b.contributor, &b.address)));
        fs
    }

    /// M2 acceptance: an overconfident labeler scores worse than a calibrated
    /// one with the same hit rate (7 of 10 right).
    #[test]
    fn overconfident_scores_worse_than_calibrated() {
        let truth: Vec<Resolution> = (0..10)
            .map(|i| r(&format!("t:a{i}"), "entity:x", 10))
            .collect();
        let mut fs = Vec::new();
        for who in [1u8, 2] {
            let p = if who == 1 { 990_000 } else { 700_000 };
            for i in 0..10 {
                let entity = if i < 7 { "entity:x" } else { "entity:y" };
                fs.push(f(who, &format!("t:a{i}"), entity, p, 5));
            }
        }
        let s = score_resolved(&sorted(fs), &truth, &kreg(2), kreg(2).root()).expect("score");
        let (over, calib) = (s[&[1; 20]], s[&[2; 20]]);
        assert_eq!((over.n, calib.n), (10, 10));
        assert_eq!(over.brier_cmp(&calib), Ordering::Greater);
        assert!(over.mean_log_microbits() > calib.mean_log_microbits());
        // Calibrated, 2 candidates: 2 * (0.7 * 0.09 + 0.3 * 0.49) = 0.42.
        assert_eq!(calib.mean_brier_ppm(), Some(420_000));
        assert!(!calib.eligible());
    }

    /// The reviewer's farming probe: an honest labeler (0.9, right 27 of 30)
    /// must beat one who says "p = 0, entity:zzz" on the same addresses.
    #[test]
    fn padding_labeler_does_not_rank_first() {
        let truth: Vec<Resolution> = (0..30)
            .map(|i| r(&format!("t:a{i:02}"), &format!("entity:o{}", i % 6), 10))
            .collect();
        let mut fs = Vec::new();
        for i in 0..30 {
            let a = format!("t:a{i:02}");
            let named = if i < 27 {
                format!("entity:o{}", i % 6)
            } else {
                "entity:wrong".into()
            };
            fs.push(f(1, &a, &named, 900_000, 5));
            fs.push(f(2, &a, "entity:zzz", 0, 5));
        }
        let s = score_resolved(&sorted(fs), &truth, &kreg(50), kreg(50).root()).expect("score");
        assert_eq!(s[&[1; 20]].brier_cmp(&s[&[2; 20]]), Ordering::Less);
        assert!(s[&[1; 20]].eligible(), "30 forecasts over 6 owners");
    }

    /// Self-resolution and per-owner cap (L13-F1).
    #[test]
    fn owner_keys_excluded_and_per_owner_cap() {
        let mut reg = kreg(9);
        reg.add_entity("entity:x", EntityKind::Exchange)
            .and_then(|r| r.add_entity_key("entity:x", [7; 20]))
            .expect("reg");
        let truth: Vec<Resolution> = (0..20)
            .map(|i| r(&format!("t:a{i:02}"), "entity:x", 10))
            .collect();
        let mut fs = Vec::new();
        for i in 0..20 {
            let a = format!("t:a{i:02}");
            fs.push(f(7, &a, "entity:x", ONE, 5)); // the owner's own key
            fs.push(f(1, &a, "entity:x", ONE, 5));
        }
        let s = score_resolved(&sorted(fs), &truth, &reg, reg.root()).expect("score");
        assert_eq!((s[&[7; 20]].n, s[&[7; 20]].excluded), (0, 20));
        assert_eq!((s[&[1; 20]].n, s[&[1; 20]].excluded), (MAX_PER_OWNER, 10));
        assert_eq!(s[&[1; 20]].owners, 1);
        assert!(!s[&[1; 20]].eligible());
    }

    /// Forecasts committed in or after the round the truth was published do not count.
    #[test]
    fn late_forecasts_excluded() {
        let truth = [r("t:a", "entity:x", 10)];
        let fs = [
            f(1, "t:a", "entity:x", 990_000, 9),
            f(2, "t:a", "entity:x", 990_000, 10),
            f(3, "t:b", "entity:x", 990_000, 1),
        ];
        let s = score_resolved(&fs, &truth, &kreg(2), kreg(2).root()).expect("score");
        assert_eq!(s[&[1; 20]].n, 1);
        assert_eq!((s[&[2; 20]].n, s[&[2; 20]].late), (0, 1));
        assert!(!s.contains_key(&[3; 20]), "unresolved address not scored");
    }

    #[test]
    fn inputs_must_be_canonical() {
        let reg = kreg(2);
        let truth = [r("t:a", "entity:x", 10)];
        // Two forecasts by one contributor on one address (hedging) are refused.
        let hedge = [f(1, "t:a", "entity:x", 1, 1), f(1, "t:a", "entity:y", 2, 1)];
        assert_eq!(
            score_resolved(&hedge, &truth, &reg, reg.root()),
            Err(ScoreError::ForecastsNotCanonical)
        );
        let two_owners = [r("t:a", "entity:x", 10), r("t:a", "entity:y", 10)];
        assert_eq!(
            score_resolved(&[], &two_owners, &reg, reg.root()),
            Err(ScoreError::ResolutionsNotCanonical)
        );
        assert_eq!(
            score_resolved(
                &[f(1, "t:a", "entity:x", ONE + 1, 1)],
                &truth,
                &reg,
                reg.root()
            ),
            Err(ScoreError::OutOfRange)
        );
        assert_eq!(
            score_resolved(&[], &truth, &kreg(1), kreg(1).root()),
            Err(ScoreError::TooFewCandidates)
        );
    }

    #[test]
    fn brier_cmp_uses_sums() {
        // 1/3 vs 0.333333: both floor to 333,333 ppm, but they are not equal.
        let a = Score {
            n: 3,
            brier_sum: 1_000_000_000_000,
            ..Score::default()
        };
        let b = Score {
            n: 1000,
            brier_sum: 333_333_000_000_000,
            ..Score::default()
        };
        assert_eq!(a.mean_brier_ppm(), b.mean_brier_ppm());
        assert_eq!(a.brier_cmp(&b), Ordering::Greater);
        assert_eq!(Score::default().brier_cmp(&a), Ordering::Greater);
    }

    /// L13-F6: resolutions come only from verified attestations; an address
    /// claimed by two entities is left out; one entity keeps its earliest round.
    #[test]
    fn resolutions_only_from_verified_attestations() {
        use crate::attest::test_support::*;
        use crate::attest::NONCE_LEN;
        use onyx_core::{AddressCanon, EntityKind};
        struct AnyCanon;
        impl AddressCanon for AnyCanon {
            fn namespace(&self) -> &str {
                "t"
            }
            fn canon_id(&self) -> &str {
                "onyx.canon.test.v1"
            }
            fn vectors(&self) -> &'static [(&'static str, bool)] {
                &[]
            }
            fn is_canonical(&self, _: &str) -> bool {
                true
            }
        }
        let key = key_address(&test_secret());
        let mk = || {
            let mut reg = StaticRegistry::new();
            reg.set_domain("onyx.devnet")
                .and_then(|r| r.add_entity("entity:x", EntityKind::Exchange))
                .and_then(|r| r.add_entity("entity:y", EntityKind::Custodian))
                .and_then(|r| r.add_canon(Box::new(AnyCanon)))
                .and_then(|r| r.add_proof(Box::new(StubProof)))
                .and_then(|r| r.add_entity_key("entity:x", key))
                .and_then(|r| r.add_entity_key("entity:y", key))
                .expect("reg");
            reg
        };
        let reg = mk();
        let att_window = |addr: &str, entity: &str, nonce: u8, valid_from: u64, expires: u64| {
            let text = Statement {
                domain: "onyx.devnet".into(),
                entity: entity.into(),
                address: addr.into(),
                valid_from,
                expires,
                nonce: [nonce; NONCE_LEN],
            }
            .render();
            Attestation {
                address_sig: StubProof::sign(&addr[2..], text.as_bytes()),
                entity_key: key,
                entity_sig: eip191_sign(&test_secret(), text.as_bytes()),
                statement: text,
            }
        };
        let att = |addr: &str, entity: &str, nonce: u8| att_window(addr, entity, nonce, 1, 100);
        let mut forged = att("t:c", "entity:x", 1);
        forged.address_sig = "nope".into();
        let atts = vec![
            (att("t:a", "entity:x", 1), 7),
            (att("t:a", "entity:x", 2), 5),
            (att("t:b", "entity:x", 1), 3),
            (att("t:b", "entity:y", 1), 4),
            (forged, 2),
            // L14-F2: a renewal cannot hide the first attestation's round.
            (att_window("t:d", "entity:x", 1, 1, 50), 7),
            (att_window("t:d", "entity:x", 2, 40, 140), 50),
        ];
        let want = vec![r("t:a", "entity:x", 5), r("t:d", "entity:x", 7)];
        assert_eq!(resolutions_from_attestations(&atts, &reg), want);
        // L15-F1: routine retirement of the signing key changes nothing.
        let mut rotated = mk();
        rotated
            .retire_entity_key("entity:x", &key, 100)
            .expect("retire");
        assert_eq!(resolutions_from_attestations(&atts, &rotated), want);
        // L16-F5: a retired key creates no new history: an attestation it signed
        // that was finalized at or after retirement does not count.
        let mut late = atts.clone();
        late.push((att("t:e", "entity:x", 1), 100));
        assert_eq!(resolutions_from_attestations(&late, &rotated), want);
        late.push((att("t:e", "entity:x", 2), 99));
        assert_eq!(
            resolutions_from_attestations(&late, &rotated),
            vec![
                r("t:a", "entity:x", 5),
                r("t:d", "entity:x", 7),
                r("t:e", "entity:x", 99)
            ]
        );
        // A compromised key's attestations stop counting as history: entity:x
        // claims vanish, so t:b is now claimed by entity:y alone.
        let mut stolen = mk();
        stolen
            .mark_key_compromised("entity:x", &key)
            .expect("compromised");
        assert_eq!(
            resolutions_from_attestations(&atts, &stolen),
            vec![r("t:b", "entity:y", 4)]
        );
        // Mutation L4: `score_all` scores against the resolutions it builds
        // from the attestations, under the pinned registry only.
        let fs = sorted(vec![
            f(1, "t:a", "entity:x", 900_000, 1),
            f(1, "t:d", "entity:y", 600_000, 2),
            f(2, "t:a", "entity:y", 700_000, 1),
        ]);
        let all = score_all(&fs, &atts, &reg, reg.root()).expect("scores");
        assert_eq!(all.len(), 2);
        assert_eq!(
            all,
            score_resolved(&fs, &want, &reg, reg.root()).expect("same")
        );
        assert!(all.values().all(|s| s.n > 0));
        assert_eq!(
            score_all(&fs, &atts, &reg, Hash32([9; 32])),
            Err(ScoreError::WrongRegistry)
        );
    }

    /// Mutation L4: eligibility needs both counts; the means divide by n and
    /// are absent for n = 0; the log floor's clamp for several candidates;
    /// log2 of numbers past 2^62.
    #[test]
    fn score_summaries_and_edges() {
        let s = |n, owners| Score {
            n,
            owners,
            ..Score::default()
        };
        assert!(s(MIN_RESOLVED, MIN_DISTINCT_OWNERS).eligible());
        assert!(!s(MIN_RESOLVED - 1, MIN_DISTINCT_OWNERS).eligible());
        assert!(!s(MIN_RESOLVED, MIN_DISTINCT_OWNERS - 1).eligible());
        assert_eq!(Score::default().mean_brier_ppm(), None);
        assert_eq!(Score::default().mean_log_microbits(), None);
        let m = Score {
            n: 4,
            brier_sum: 6_000_000_000_000,
            log_sum: 10,
            ..Score::default()
        };
        assert_eq!(m.mean_brier_ppm(), Some(1_500_000));
        assert_eq!(m.mean_log_microbits(), Some(2));
        // 3 candidates, owner not named: the owner gets (ONE - p) / 2. With
        // ONE - p = 1,500 that is 750 ppm, below the 1,000 ppm floor: clamped.
        assert_eq!(
            log_term(c(ONE - 1_500), false, 3),
            log_term(c(LOG_FLOOR_PPM), true, 2)
        );
        assert_eq!(log2_fixed(1 << 63), 63 << LOG_FRAC_BITS);
        assert_eq!(log2_fixed(3 << 62) >> LOG_FRAC_BITS, 63);
        assert!(log2_fixed(u64::MAX) > log2_fixed(3 << 62));
        assert!(log2_fixed(u64::MAX) < 64 << LOG_FRAC_BITS);
    }

    /// L15-F6: scoring refuses a registry other than the epoch's pinned one.
    #[test]
    fn scoring_pins_the_registry() {
        let truth = [r("t:a", "entity:x", 10)];
        let fs = [f(1, "t:a", "entity:x", 700_000, 5)];
        let epoch = kreg(2).root();
        assert!(score_resolved(&fs, &truth, &kreg(2), epoch).is_ok());
        assert_eq!(
            score_resolved(&fs, &truth, &kreg(100), epoch),
            Err(ScoreError::WrongRegistry)
        );
    }
}
