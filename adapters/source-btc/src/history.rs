//! A history: consecutive block-range shards of the series and range recipes
//! over one stretch of chain, and the one hash that pins them (the website's
//! one-week history, `examples/export-history.rs`).
//!
//! Shards never cross a UTC day (by block header time), so the website can
//! load and check one day of leaves at a time against the one pinned hash.
//!
//! Shards are sized by inputs plus outputs, not by block count (docs/spec/
//! recipes.md, "Sizing BTC shards"): [`plan_shards`] packs consecutive blocks
//! while the estimated fuel stays under a target, and the preflight then
//! confirms each shard on the canonical engine.

use onyx_core::{keccak256, Hash32};

/// Fuel to decode label set v2 (9,850 labels), measured on wasmi
/// (`tests/series_props.rs`, `tests/golden_series.rs`).
pub const LABEL_DECODE_FUEL: u64 = 560_000_000;
/// Fuel per input or output, measured on wasmi (same tests). 3,800 read up
/// to 3.2% low on the one-day history (HIST-R1-F6), so the planner uses 3,950.
pub const FUEL_PER_IO: u64 = 3_950;
/// Planning target: 45% of the 4 B limit, so the preflight's 50% rule has
/// slack for blocks denser than the estimate.
pub const PLAN_FUEL_TARGET: u64 = 1_800_000_000;

/// Estimated fuel of one shard with `io` inputs plus outputs.
pub fn estimated_fuel(io: u64) -> u64 {
    LABEL_DECODE_FUEL.saturating_add(FUEL_PER_IO.saturating_mul(io))
}

/// Split consecutive blocks `(height, inputs + outputs)` into shards whose
/// estimated fuel stays at or under `target`. A block that alone exceeds the
/// target is a shard of its own (the preflight then decides). Heights must be
/// consecutive and ascending; returns inclusive `(from, to)` ranges.
pub fn plan_shards(blocks: &[(u64, u64)], target: u64) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let mut start: Option<(u64, u64)> = None; // (from, io so far)
    let mut last = 0;
    for &(h, io) in blocks {
        match start {
            Some((from, acc)) if estimated_fuel(acc.saturating_add(io)) <= target => {
                start = Some((from, acc + io));
            }
            Some((from, _)) => {
                out.push((from, last));
                start = Some((h, io));
            }
            None => start = Some((h, io)),
        }
        last = h;
    }
    if let Some((from, _)) = start {
        out.push((from, last));
    }
    out
}

/// The UTC calendar date (`YYYY-MM-DD`) of a unix time: days since the epoch
/// to a proleptic Gregorian date (H. Hinnant's `civil_from_days`).
pub fn utc_date(unix: u64) -> String {
    let z = (unix / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// One planned UTC day: `(date, first height, last height, its shards)`.
pub type DayPlan = (String, u64, u64, Vec<(u64, u64)>);

/// Split consecutive blocks `(height, inputs + outputs, header time)` into
/// UTC days (by header time; a block whose time falls on an earlier date than
/// the block before it stays with that block's day, so days stay contiguous),
/// then plan each day's shards with [`plan_shards`]. Returns
/// `(date, day from, day to, shards)` per day, in order.
pub fn plan_days(blocks: &[(u64, u64, u64)], target: u64) -> Vec<DayPlan> {
    let mut days: Vec<(String, Vec<(u64, u64)>)> = Vec::new();
    for &(h, io, time) in blocks {
        let date = utc_date(time);
        match days.last_mut() {
            Some((d, list)) if *d >= date => list.push((h, io)),
            _ => days.push((date, vec![(h, io)])),
        }
    }
    days.into_iter()
        .filter_map(|(date, list)| {
            let from = list.first()?.0;
            let to = list.last()?.0;
            Some((date, from, to, plan_shards(&list, target)))
        })
        .collect()
}

/// Rows per chart bucket on the website's 7-day view: six blocks, about an
/// hour. Buckets start at each day's first block, so a day's buckets can be
/// checked with that day's leaves alone.
pub const BUCKET_BLOCKS: u64 = 6;

/// One shard of a history: its range and the roots of both recipes on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryShard {
    /// First height.
    pub from: u64,
    /// Last height (inclusive).
    pub to: u64,
    /// Root of `btc-entity-flow-series` on the shard.
    pub series_root: Hash32,
    /// Root of `btc-entity-balance` on the shard.
    pub balance_root: Hash32,
}

/// The hash a website pins for a history: domain, both recipe ids, the label
/// set's content hash, the tip block hash (internal byte order), then each
/// shard in order as `from (u64 BE) || to (u64 BE) || series root || balance
/// root`. Any edited row changes a root; any moved, dropped or added shard
/// changes the list.
pub fn history_commitment(
    series_recipe: &Hash32,
    balance_recipe: &Hash32,
    label_set: &Hash32,
    tip: &[u8; 32],
    shards: &[HistoryShard],
) -> Hash32 {
    let mut b = Vec::with_capacity(16 + 32 * 4 + shards.len() * 80);
    b.extend_from_slice(b"onyx-history-v1");
    b.extend_from_slice(&series_recipe.0);
    b.extend_from_slice(&balance_recipe.0);
    b.extend_from_slice(&label_set.0);
    b.extend_from_slice(tip);
    for s in shards {
        b.extend_from_slice(&s.from.to_be_bytes());
        b.extend_from_slice(&s.to.to_be_bytes());
        b.extend_from_slice(&s.series_root.0);
        b.extend_from_slice(&s.balance_root.0);
    }
    keccak256(&b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shards_stay_under_the_target_and_cover_every_block() {
        let blocks: Vec<(u64, u64)> = (100..244).map(|h| (h, 4_000 + (h % 7) * 9_000)).collect();
        let shards = plan_shards(&blocks, PLAN_FUEL_TARGET);
        assert_eq!(shards.first().map(|s| s.0), Some(100));
        assert_eq!(shards.last().map(|s| s.1), Some(243));
        for w in shards.windows(2) {
            assert_eq!(w[0].1 + 1, w[1].0, "contiguous");
        }
        for (from, to) in &shards {
            let io: u64 = blocks
                .iter()
                .filter(|(h, _)| (from..=to).contains(&h))
                .map(|b| b.1)
                .sum();
            assert!(estimated_fuel(io) <= PLAN_FUEL_TARGET || from == to);
        }
    }

    #[test]
    fn a_block_over_the_target_is_its_own_shard() {
        let shards = plan_shards(&[(1, 10), (2, 1_000_000), (3, 10)], PLAN_FUEL_TARGET);
        assert_eq!(shards, vec![(1, 1), (2, 2), (3, 3)]);
    }

    #[test]
    fn utc_dates_are_right() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(951_782_400), "2000-02-29");
        assert_eq!(utc_date(1_791_590_399), "2026-10-09");
        assert_eq!(utc_date(1_791_590_400), "2026-10-10");
    }

    #[test]
    fn shards_never_cross_a_day_and_days_are_contiguous() {
        let day = 86_400;
        // 300 blocks, ten minutes apart, starting at 23:00 on day 0; one
        // block's time steps back across midnight (allowed by the header rules).
        let mut blocks: Vec<(u64, u64, u64)> = (0..300u64)
            .map(|i| {
                (
                    1_000 + i,
                    9_000 + (i % 5) * 20_000,
                    day * 20_000 + 23 * 3_600 + i * 600,
                )
            })
            .collect();
        blocks[10].2 = blocks[5].2;
        let days = plan_days(&blocks, PLAN_FUEL_TARGET);
        assert_eq!(days.first().map(|d| d.1), Some(1_000));
        assert_eq!(days.last().map(|d| d.2), Some(1_299));
        for w in days.windows(2) {
            assert_eq!(w[0].2 + 1, w[1].1, "days contiguous");
            assert!(w[0].0 < w[1].0, "dates increase");
        }
        for (_, from, to, shards) in &days {
            assert_eq!(shards.first().map(|s| s.0), Some(*from));
            assert_eq!(shards.last().map(|s| s.1), Some(*to));
        }
        assert!(days.len() >= 3);
    }

    #[test]
    fn the_commitment_moves_with_every_field() {
        let s = |from, to, a: u8| HistoryShard {
            from,
            to,
            series_root: Hash32([a; 32]),
            balance_root: Hash32([a + 1; 32]),
        };
        let base = [s(1, 5, 1), s(6, 9, 3)];
        let c = |shards: &[HistoryShard], tip: [u8; 32]| {
            history_commitment(
                &Hash32([7; 32]),
                &Hash32([8; 32]),
                &Hash32([9; 32]),
                &tip,
                shards,
            )
        };
        let reference = c(&base, [0; 32]);
        assert_ne!(reference, c(&base, [1; 32]));
        assert_ne!(reference, c(&[s(1, 5, 1)], [0; 32]));
        assert_ne!(reference, c(&[s(1, 4, 1), s(5, 9, 3)], [0; 32]));
        assert_ne!(reference, c(&[s(1, 5, 2), s(6, 9, 3)], [0; 32]));
    }
}
