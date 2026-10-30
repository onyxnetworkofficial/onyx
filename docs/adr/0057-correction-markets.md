# ADR-0057: Correction markets: a Yes ("accurate") / No market per published value, settled only by a later re-observation

## Status
Accepted with amendment 1, 2026-10-30 (built in part: v0 below; not deployed). Proposed
2026-10-29 (docs/contributor-loop.md CL7). Builds on
ADR-0027 (rounds, one recorded result per fact, the subject bound), ADR-0042 (futures, payouts,
insiders; amendments 2 and 3: `OnyxObservedFacts`, `resultHash`, guards), ADR-0048 (the ladder
and contradictions) and ADR-0053 (manifest templates). Test network only; tONYX, no value.

## Context
Markets today bet on a future value: `OnyxFutures` and `OnyxParametric` settle from a fact
`OnyxObservedFacts.prove` accepts, that is, a registry result whose
`keccak256("onyx-observe-result-v1" ‖ spec ‖ round ‖ 0x00 ‖ 0x01 ‖ value as i128 LE)` matches
the recorded `result`. Nothing lets anyone put money on whether a value already published is
still right. `OnyxStakes` comes close (a claim on a fact, with re-observation first), but its
fallback is a stake vote and a drawn panel, and the contributor loop says correction markets
settle "only from observed facts, never a vote".

Constraints that shape the design:
1. `OnyxRegistry` records at most one result per fact (`AlreadyRecorded`), and a dated fact's key
   ignores the window. A second look at the same fact therefore needs a second fact key.
2. A recorded result is the coordinator's attestation (ADR-0042 amendment 2 item 5); clients
   check it off chain, contracts pay on chain without that check (acceptable for a test token).
3. "Drift is not a lie" (ADR-0027 L2R-F1): a source that revises a value does not make the
   original observers liars, and they are not slashed for it.

## Decision
1. **Which values.** Numeric level-2 values of dated, period or daily facts (one fact per date)
   whose metric has a paired recheck template (item 2). Not v1: live fixed-window feeds (a price
   that moves every second has no "correct later"), exact (byte) values, level-4 values with no
   observable source, keyed sources unless their recheck pool is keyed too (ADR-0054).
2. **The recheck template.** `recheck_of(template)` is a pure function: the same fetcher, URL
   template, selector, kind, rights and observer count; `metric` and `feed` get the suffix
   `.recheck`; the subject rule becomes `SubjectRule::Recheck { of: Box<SubjectRule>,
   after_days: u16, span_days: u16 }` (a new variant appended last, so no existing template id
   changes): observable only from `after_days` after the original rule's last observable day
   (`FeedTemplate::last_observable_day`) for `span_days`. The owner registers it like any template
   and pairs it on chain (item 3); a client recomputes `recheck_of` and compares the id with the
   registry's.
3. **New contract `OnyxCorrections`** (one per deployment, reads `OnyxRegistry`; parimutuel
   payouts through the existing `OnyxPayout` library):
   - `pair(metric, recheckMetric, tolerance)` (owner, append-only): `tolerance` is the template's
     numeric tolerance, set by review, never by a market's opener.
   - `open(metric, subjectKey, windowEnd, round, value, closeBlock, resolveBlock)`: anyone, with
     `openFee`. It checks that the original fact has a recorded result equal to
     `resultHash(spec, round, value)` (the `OnyxObservedFacts` rule, so a forged original value
     cannot open a market), that the recheck fact has no result yet, that no market on this
     original fact was ever opened (one per value: the recheck fact can record once), and that
     `closeBlock < resolveBlock <= block.number + MAX_MARKET_BLOCKS`.
   - `bet(market, side, amount)`: side 1 Yes ("accurate"), 2 No, until `closeBlock`. Insiders may
     not bet: the owner, the registry's coordinator, and the treasury (the `IOnyxFactGuard` rule).
   - `resolve(market, round, recheckValue)`: anyone. The recheck fact's recorded result must equal
     `resultHash(recheckSpec, round, recheckValue)` and be recorded at a block after `closeBlock`.
     Yes wins when `|recheckValue - value| <= tolerance`, otherwise No: whoever bet that the value
     is wrong or out of date wins the Yes pool pro rata, less `feePpm`.
   - `void(market)`: anyone after `resolveBlock` with no resolution, or when the recheck was
     recorded at or before `closeBlock`: every bet is refunded. `openFee` is refunded when
     `OnyxRegistry.drawCount(recheckFact) == 0` (nobody even tried a round) and kept otherwise
     (it pays for the round that ran).
4. **Settlement only from observed facts.** The only inputs are two registry results (the
   original and the recheck) and their opening values. There is no vote, no panel and no owner
   answer; with no recheck value, the market voids. A level-3 path (`resolveSigned`, reading an
   `OnyxSignedBook` report of the same fact) is phase 2: the book's `valueHash` covers the
   report's own payload encoding, and the mapping from a registry fact to a book fact and from a
   number to that encoding must be pinned with shared vectors first.
5. **The recheck round.** When a market opens, the operator's runner schedules a recheck round
   (ADR-0049's worker, paid from `openFee` instead of a buyer's hold) in the first window the
   recheck rule allows after `closeBlock`. Its pool snapshot excludes every address that bet in
   the market (bets are public events) and every observer of the original round (public in its
   transcript), so the recheck is an independent look; clients recompute the exclusion when they
   check the recheck round (`client::verify_observation` gains the market's bettor list as input).
6. **What a "No" changes.** The original value keeps its level and is never removed (a level only
   rises, ADR-0048). The value answer gains `recheck: {result, valueHash, recordedAt, matches}`,
   and the ladder's contradiction list (ADR-0048 amendment 2 item 4) gets a kind `Recheck` (same
   level, later, another value hash): shown as "rechecked later: the source now says otherwise",
   never a fall. Original observers are not slashed (constraint 3). The recheck value is itself a
   level-2 value of the `.recheck` metric, served like any other.

## Consequences
- Anyone who checks a published value against its source can be paid for finding a revision or
  an error, with no vote anywhere in the path.
- For re-readable public data the outcome is often visible before the close to anyone who reads
  the source: the market mostly pays whoever checks, and Yes money works as a bounty for a
  correction. That is intended; the site says so next to each market.
- One correction market per value, ever, in v1 (one recheck fact per original fact).
- The recheck costs a round (about 14 to 15 transactions on Base Sepolia); `openFee` must cover
  it, which also prices spam.
- The coordinator can still void a market by recording the recheck early or never running it;
  it cannot bet, and either way every bet is refunded.

## Alternatives
- **`OnyxStakes` claims with re-observation**: has the vote and the panel as fallbacks; a
  correction market must never settle by either.
- **`OnyxFutures` with buckets around the value**: "accurate" becomes a middle bucket and "No"
  splits into "too low" and "too high", so a challenger has to guess the direction.
- **Settle "Yes" when nobody proves "No" by a deadline**: settles on the absence of evidence,
  which an operator that never runs the recheck controls. Refused: no recheck value, no outcome.
- **A second result for the same fact in `OnyxRegistry`**: breaks "one result per fact", which
  every lane and client relies on.

## Threats (rows CL7-1..CL7-9 go into THREATS.md with their tests when built)
| ID | Threat | Mitigation | Test to write |
|---|---|---|---|
| CL7-1 | A market opened on a forged original value | `open` proves the value against the recorded result | `OnyxCorrections.t.sol::test_OpenRefusesAValueTheRegistryDidNotRecord` |
| CL7-2 | The coordinator records the recheck before the close (its value seen by insiders) | Resolution needs a recording after `closeBlock`; else void and refund | `test_ARecheckRecordedBeforeTheCloseVoids` |
| CL7-3 | A bettor sits on the recheck committee | Bettors and original observers excluded from the recheck pool; clients recompute | `the_recheck_pool_excludes_bettors_and_original_observers` |
| CL7-4 | The opener picks a tolerance that decides the market | Tolerance set by `pair` (owner, from review) | `test_TheToleranceIsThePairsNotTheOpeners` |
| CL7-5 | Spam markets on every value | `openFee` per market, one market per value ever | `test_ASecondMarketOnOneValueIsRefused` |
| CL7-6 | Draw grinding of the recheck round | The subject bound (3 public draws, ADR-0027 amendment 6) applies to the `.recheck` metric | existing `drawBudget` tests on the new metric |
| CL7-7 | The source's publisher revises to win its own bet | Outside Onyx's control: the market settles on what the source says; stated on the market | docs-only |
| CL7-8 | A recheck template that reads something else than the original | `recheck_of` is a pure function; clients compare template ids | `recheck_of_changes_only_metric_feed_and_window` |
| CL7-9 | Funds stuck when nobody resolves | `void` by anyone after `resolveBlock`; conservation invariant like `ParametricInvariants` | `invariant_CorrectionsHoldWhatTheyOwe` |

## Amendment 1 (2026-10-30): v0 as built

Built: `OnyxCorrections` (contracts/src), `onyx_core::recheck_of` and `SubjectRule::Recheck`,
`onyx_coordinator::corrections`, the `recheck` note on `/v1/observations`,
`client::verify_recheck`, `onyx-source recheck`, `DeployCorrections.s.sol` and
`scripts/corrections-anvil.sh`. Changes to the decision above, each from a threat (THREATS CL7):

1. **The recheck's draw, not only its recording, comes after the close.** `resolve` takes the
   recheck fact's result only when the draw that recorded it (the fact's latest: no draw follows
   a result) was announced after `closeBlock`. That draw commits to the pool snapshot and fixes its
   beacon, so neither the deciding committee nor its pool is known while trading is open, and the
   pool can leave out every bettor (CL7-3, CL7-10). A recorded result alone after the close would
   let a draw announced before it reveal the committee to traders. A draw that recorded nothing
   voids nothing (review CL7-R1-2: otherwise the coordinator's stray draw burnt the value).
2. **Deadline and grace.** The recheck must be recorded by `resolveBlock`. `void` is allowed at
   once when the recheck was recorded from a draw at or before the close (it can never settle the
   market),
   after `resolveBlock` when no timely recheck exists, and after `resolveBlock + graceBlocks` in
   any case (a recheck that settled no number). A timely recheck cannot be voided during the grace
   blocks, so the losing side cannot race `resolve` at the deadline (CL7-17).
3. **Several markets per value, one recheck.** Any number of markets of one value may run; the
   recheck fact records once, so they all settle from the same recheck when it is timely for them
   and void otherwise. Item 3's "one market per value ever" let a griefer's market with a hopeless
   close hold the value (review CL7-R1-1: a free squat, since the fee came back). A market opens
   only while the recheck has no result, needs minimum trading and recheck blocks and stays within
   `maxMarketBlocks`; each side holds at most half the pool cap (review CL7-R1-3).
4. **Fixed economics.** `openFee`, `minBet`, the bet and pool caps, `feePpm` (at most 10%) and
   the block windows are immutable per deployment (no setter): nobody changes them under an open
   market; the owner's only function is `pair` (CL7-14).
5. **Money paths.** The open fee goes to the treasury when the market resolves or when a recheck
   round was drawn for it (after its close), and back to the opener (pulled with
   `claimFeeRefund`) when it voids with no such draw: none at all, or one before the close, which
   is the coordinator's timing (review CL7-R1-2, CL7-R1-4). A resolution whose winning side has no bet voids (reason 1) and refunds
   every bet. Rounding dust goes to the treasury after the last winner; the contract's balance
   always equals `liabilities()`.
6. **The API note.** The answer carries `recheck` once the paired metric's round of the same
   subject settled. While the paired feed exists with no round yet, the answer is not cached as
   immutable; the ETag names the recheck's fact once its note is served. A client recomputes the
   note from the recheck's own verified answer (`client::verify_recheck`).
7. **Not built in v0:** the recheck runner (the ADR-0049 worker that runs the `.recheck` template
   in the first allowed window after the close, from `recheck_pool`) and the client's bettor-list
   check in `verify_observation` (TODO S9); the ladder's contradiction kind `Recheck` and the
   site's note and market list (TODO S6); `resolveSigned` (phase 2, item 4); the Base Sepolia
   deployment (docs/ops/runbooks/corrections-base-sepolia.md). The placeholders: 7 days after the
   original's last observable day for 7 days (`after_days`, `span_days`), and
   `DeployCorrections.defaults()`.

## Open questions
1. `openFee`, `feePpm`, `MAX_MARKET_BLOCKS`, `after_days` and `span_days`: placeholders until
   measured round costs and the partners' numbers (NS12).
2. Should the opener get a finder's share when its side wins, beyond its bet?
3. Repeated rechecks of one value (`.recheck2`, ...), or is one per value enough?
4. Should a "No" also feed dataset backing's loss event 3 (docs/spec/dataset-backing.md)?
5. Legal review before anything of value is traded (as ADR-0042).
