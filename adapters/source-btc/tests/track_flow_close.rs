//! The Bitcoin flow close rule (ADR-0036 amendment 4 item 1, THREATS TR-13,
//! TR-C9-R2-2, TR-C9-R2-3), checked on a model of block acceptance: a block's
//! median time past (BIP 113) is the median of its eleven predecessors'
//! timestamps; a node accepts a block whose timestamp is above that median and
//! at most `allowance` past its own clock (`MAX_FUTURE_BLOCK_TIME`, 7,200 s, in
//! Bitcoin Core 27 and later on system time; before 27 the network-adjusted
//! time could add up to 70 minutes). A day's blocks are those whose median
//! time past is in `[start, end)`. No production code computes the day yet
//! (ROADMAP C40, TR-C9-R1-7); these tests pin the arithmetic the rule rests on.

use onyx_source_btc::{TRACK_FLOW_LEAD_SECS, TRACK_FLOW_PERIOD_SECS};

/// 2026-09-22 00:00 UTC.
const START: u64 = 20_718 * 86_400;
const END: u64 = START + TRACK_FLOW_PERIOD_SECS;
const CLOSE: u64 = START - TRACK_FLOW_LEAD_SECS;

/// Bitcoin Core 27+: a block at most 2 h ahead of the node's clock.
const MAX_FUTURE: u64 = 7_200;
/// Before Bitcoin Core 27: the clock could be shifted by peers up to 70 min.
const MAX_ADJUST: u64 = 70 * 60;

/// A chain: (arrival time, timestamp) per block, built by `stamp`, which
/// picks a timestamp from (arrival, median time past); every block is checked
/// against the acceptance rule with `allowance`.
fn chain(
    first: u64,
    gap: u64,
    blocks: usize,
    allowance: u64,
    stamp: impl Fn(u64, u64) -> u64,
) -> Vec<(u64, u64, u64)> {
    // Eleven old blocks so every block has a median time past.
    let mut ts: Vec<u64> = (0..11).map(|i| first - 11 * gap + i * gap).collect();
    let mut out = Vec::new();
    for i in 0..blocks as u64 {
        let arrival = first + i * gap;
        let mut last: Vec<u64> = ts[ts.len() - 11..].to_vec();
        last.sort_unstable();
        let mtp = last[5];
        let t = stamp(arrival, mtp);
        assert!(t > mtp && t <= arrival + allowance, "invalid block");
        ts.push(t);
        out.push((arrival, t, mtp));
    }
    out
}

fn first_of_day(c: &[(u64, u64, u64)]) -> u64 {
    c.iter()
        .find(|b| b.2 >= START)
        .map(|b| b.0)
        .expect("a block of the day")
}

fn last_of_day(c: &[(u64, u64, u64)]) -> u64 {
    c.iter()
        .rev()
        .find(|b| b.2 < END)
        .map(|b| b.0)
        .expect("a block of the day")
}

#[test]
fn the_constants_are_a_utc_day_and_the_future_block_limit() {
    assert_eq!(TRACK_FLOW_PERIOD_SECS, 86_400);
    assert_eq!(TRACK_FLOW_LEAD_SECS, MAX_FUTURE);
}

/// Honest miners (timestamp = arrival, a block every 600 s): the day's first
/// block comes about six blocks after the day starts, hours after the close.
#[test]
fn honest_blocks_of_the_day_come_after_the_day_starts() {
    let c = chain(START - 20_000, 600, 400, MAX_FUTURE, |a, _| a);
    let first = first_of_day(&c);
    assert!(
        first >= START + 5 * 600,
        "first day block at start{:+}",
        first as i64 - START as i64
    );
    assert!(first - CLOSE >= 3 * 3_600);
}

/// Every miner stamps as far ahead as nodes accept, a block every second:
/// the day's first block is accepted no earlier than the close (at the same
/// second at the earliest), on Bitcoin Core 27 or later.
#[test]
fn no_block_of_the_day_is_accepted_before_the_close() {
    for gap in [1, 30, 600] {
        let c = chain(
            START - 40_000,
            gap,
            80_000 / gap as usize,
            MAX_FUTURE,
            |a, _| a + MAX_FUTURE,
        );
        let first = first_of_day(&c);
        assert!(
            first >= CLOSE,
            "gap {gap}: first day block at close{:+}",
            first as i64 - CLOSE as i64
        );
    }
}

/// TR-C9-R2-3 (accepted): a node before Bitcoin Core 27 whose peers shift
/// its clock 70 minutes ahead accepts a day's block up to 70 minutes before
/// the close; this needs miners stamping six blocks in a row far ahead.
#[test]
fn an_old_node_with_a_shifted_clock_could_see_a_day_block_before_the_close() {
    let c = chain(
        START - 40_000,
        1,
        80_000,
        MAX_FUTURE + MAX_ADJUST,
        |a, _| a + MAX_FUTURE + MAX_ADJUST,
    );
    let first = first_of_day(&c);
    assert!((CLOSE - MAX_ADJUST..CLOSE).contains(&first));
}

/// TR-C9-R2-2: the day's last blocks (median time past still before the end)
/// come after the day ends: about an hour with honest miners, without a
/// bound when miners keep timestamps just above the median. A forecast
/// revealed at the day's end (`as_of`) is public while they are mined.
#[test]
fn the_days_last_blocks_come_after_the_day_ends() {
    let c = chain(END - 20_000, 600, 400, MAX_FUTURE, |a, _| a);
    let last = last_of_day(&c);
    assert!(
        last > END + 3 * 600 && last < END + 2 * 3_600,
        "last day block at end{:+}",
        last as i64 - END as i64
    );
    // Miners stamping the least they may hold the median back.
    let c = chain(END - 20_000, 600, 400, MAX_FUTURE, |_, mtp| mtp + 1);
    assert!(last_of_day(&c) > END + 24 * 3_600);
}
