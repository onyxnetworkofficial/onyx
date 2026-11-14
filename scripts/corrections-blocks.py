#!/usr/bin/env python3
"""The close and deadline blocks of a new correction market (CL7, ADR-0057, runbook step 6).

The close is the earliest the contract allows (`minTradingBlocks` after the head) plus MARGIN
blocks for the transactions to land, or the block expected at CLOSE_AT (ISO 8601 UTC, e.g.
2026-11-17T00:00:00Z). CLOSE_AT is refused before that earliest close, later than one day before
the last recheck day starts (the recheck keeps two days or more), or so late that fewer than
`minRecheckBlocks` are left before the deadline. The deadline is the block expected at 12:00 UTC
on the day after the last recheck day (`onyx-source recheck` prints `recheck_days`), so a recheck
round started on the last day can still be recorded by it; without CLOSE_AT it is moved later
when the close leaves less than `minRecheckBlocks`. Blocks are counted at the chain's measured
BLOCK_MS; `DeployCorrections.checkOpening` checks the result again on chain before anything is
sent. Used by scripts/corrections-sepolia.sh `open` and scripts/corrections-anvil.sh. Prints
`key value [UTC date]` lines: close_block, close_time, resolve_block, resolve_time, recheck_end
(times in unix seconds; recheck_end is when the last recheck day ends).
Usage: python3 -I scripts/corrections-blocks.py HEAD HEAD_TIME BLOCK_MS MIN_TRADING MIN_RECHECK MARGIN LAST_RECHECK_DAY [CLOSE_AT]
"""
import datetime, sys

DAY = 86_400


def utc(t):
    return datetime.datetime.fromtimestamp(t, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def unix_of(iso):
    """Unix seconds of an ISO 8601 time that names UTC (a `Z` or `+00:00` suffix)."""
    t = datetime.datetime.fromisoformat(iso)
    if t.utcoffset() != datetime.timedelta(0):
        raise ValueError(f"CLOSE_AT {iso!r} is not a UTC time (end it in Z)")
    return int(t.timestamp())


def blocks(head, head_time, block_ms, min_trading, min_recheck, margin, last_day, close_at=None):
    """(close block, resolve block, recheck end) for a market opened at `head`."""
    if block_ms <= 0 or min(head, head_time, min_trading, min_recheck, margin) < 0:
        raise ValueError("every number is non-negative and BLOCK_MS positive")
    last = datetime.date.fromisoformat(last_day)
    recheck_end = (last.toordinal() + 1 - datetime.date(1970, 1, 1).toordinal()) * DAY
    at = lambda t: head + -(-(t - head_time) * 1000 // block_ms)  # the block expected at t
    resolve = at(recheck_end + DAY // 2)
    earliest = head + min_trading + margin
    if close_at is None:
        return earliest, max(earliest + min_recheck, resolve), recheck_end
    t = unix_of(close_at)
    close = at(t)
    if close < earliest:
        raise ValueError(f"CLOSE_AT {close_at} is block {close}, before the earliest allowed close, block {earliest}")
    if t > recheck_end - 2 * DAY:
        raise ValueError(f"CLOSE_AT {close_at} is later than {utc(recheck_end - 2 * DAY)}, a day before the last recheck day starts")
    if close + min_recheck > resolve:
        raise ValueError(f"CLOSE_AT {close_at} leaves fewer than {min_recheck} blocks before the deadline, block {resolve}")
    return close, resolve, recheck_end


def main(argv):
    if len(argv) not in (8, 9):
        sys.exit("usage: " + __doc__.rsplit("Usage: ", 1)[1].strip())
    try:
        nums = [int(a) for a in argv[1:7]]
        close, resolve, end = blocks(*nums, *argv[7:])
    except ValueError as e:
        sys.exit(f"corrections-blocks: {e}")
    head, head_time, block_ms = nums[:3]
    at = lambda b: head_time + (b - head) * block_ms // 1000
    for k, v, t in [("close_block", close, None), ("close_time", at(close), at(close)),
                    ("resolve_block", resolve, None), ("resolve_time", at(resolve), at(resolve)),
                    ("recheck_end", end, end)]:
        print(f"{k:14} {v}" + (f" {utc(t)}" if t is not None else ""))


if __name__ == "__main__":
    main(sys.argv)
