# Spec: dataset backing (draft, input for NORTH-STAR NS12, ROADMAP S12)

Status: **Proposed 2026-10-27** (lane B, design only; nothing is built). Input for the token
economics the partners decide (NS12). Every economic number is a named parameter in
`UPPER_CASE`; none has a value here. Prior art: RESEARCH.md "Dataset backing (S12, 2026-10-27)".
Review: THREATS DB-1..DB-8 (a hand review of this draft; rules R-DB-9..13 came from it).

## Scope

**Backers** stake behind a dataset. They earn a share of the fees buyers pay for that dataset's
answers, and they lose part of the pool when the dataset is proven wrong. Backing is a public,
priced statement that a dataset is right, made by people who lose money if it is not. It does not
change any proof level: a backed level-4 value is still level 4 (a backing is not a check).

One rule above all others:

> **R-DB-0. Backers are paid only from real paid demand.** Their reward is a share of fees that
> buyers actually paid for that dataset (`OnyxRevenue` receipts and direct payments). No token
> emission, subsidy or treasury top-up pays backers on its own. If NS12 adds an emission, it may
> only match paid fees, below the fee minus the shares already paid out (the NS12 line), never
> replace them.

Out of scope: the token itself, prices (pricing.md), and who decides a dispute (disputes.md,
stakes-and-markets.md: backing reuses their outcomes, it never decides anything).

## Interfaces

- **Dataset.** The unit backers stand behind: one metric and its subjects as the catalog lists it
  (`catalog::DatasetPricing`), so every shard or fact of that metric belongs to it. A dataset id is
  `keccak256("onyx-dataset-v1" ‖ metric key)`.
- **`OnyxBacking` (new contract, one per deployment).**
  - `back(dataset, amount)`: the stake enters a queue and starts earning after `ENTRY_DELAY`.
  - `requestExit(dataset, amount)`: starts `EXIT_DELAY`; the stake earns nothing from then on and
    stays at risk until `withdraw` (R-DB-3).
  - `withdraw(dataset)`: after `EXIT_DELAY`, if no loss event of the dataset is open.
  - `onPaid(dataset, amount)`: called by `OnyxRevenue` when it splits a fee of that dataset; the
    backers' cut is booked per share (the usual accumulator, pull payments as R-REV-4).
  - `slash(dataset, event)`: anyone may call it with a decided loss event (R-DB-4); it moves
    `LOSS_PPM` of the pool (capped by `LOSS_CAP`) to the event's beneficiaries.
- **`OnyxRevenue` split.** A new role `backersBps` per fee. Proposed: it comes out of the
  treasury's share (`backersBps <= treasuryBps - MIN_TREASURY_BPS`), so miners, validators and
  label payees keep exactly what R-REV-2 gives them today. A dataset with no backers sends the cut
  back to the treasury.
- **Loss events** (decided elsewhere, read here):
  1. `OnyxDisputes` overturns a shard of the dataset (a challenger proved the root wrong);
  2. an `OnyxStakes` claim on a fact of the dataset ends `Wrong`;
  3. a stronger lane contradicts a served value of the dataset (C64's contradiction: a level-1 or
     level-2 value of the same fact differs from the dataset's level-4 value).
  Each event names the dataset, the evidence and the decided time.

## Rules

1. **R-DB-1 Fees, not emissions.** R-DB-0.
2. **R-DB-2 Entry delay.** New backing earns only after `ENTRY_DELAY`, longer than the time from a
   buyer's payment to its credit (the reporter's batch), so nobody backs just before a known large
   purchase and leaves after it.
3. **R-DB-3 Exit delay and freeze.** `EXIT_DELAY` is longer than the longest window in which a loss
   event can still be opened for anything the stake earned on (the dispute `challenge_window` and
   appeals, the stakes' commit, reveal and fallback windows, and `CONTRADICTION_WINDOW` for C64).
   While any loss event of the dataset is open, no withdrawal completes. A stake that asked to exit
   is slashed like any other until it is withdrawn.
4. **R-DB-4 Loss sizing.** A decided loss event takes `LOSS_PPM` of the dataset's backed pool at
   the event's open time (exits requested after it do not shrink it), capped at `LOSS_CAP` per
   event and `LOSS_CAP_PER_PERIOD` per `PERIOD`. It is split between the party that proved the error
   (`FINDER_SHARE_PPM`), harmed buyers of the wrong values in the window (`BUYER_REFUND_PPM`, pro
   rata to what they paid for those values), and a burn or the treasury (the rest).
5. **R-DB-5 No profit from a self-made loss.** A finder's share never exceeds what that finder's
   own backing of the dataset loses in the same event plus `FINDER_SHARE_PPM` of the others' loss,
   and a finder must have held no backing exit request open at the event (so a backer cannot exit,
   then report the error to collect from the remaining backers).
6. **R-DB-6 Wash buying never pays.** For any single address, the sum of every share it can
   receive from one fee (as miner, validator, label payee and backer) is below the fee:
   `minersBps + validatorsBps + labelsBps + backersBps < 10000 - MIN_TREASURY_BPS`, with
   `MIN_TREASURY_BPS > 0`. A buyer who pays itself loses at least `MIN_TREASURY_BPS` of every fee.
   Any emission tied to paid queries (NS12) counts in this sum.
7. **R-DB-7 Self-backing is allowed and labelled.** A supplier (the dataset's submitter, miner or
   observer operator, by declared operator) may back its own dataset; the site shows backing by the
   dataset's own operators apart from outside backing, and outside backing is what "backed by"
   counts show. Per-address caps are not used (addresses are free to make); amounts are shown,
   never a count of backers.
8. **R-DB-8 Backing never changes a level or a vote.** Backed amounts are not weights in disputes,
   panels, re-observation or the ladder.
9. **R-DB-9 Supplier first loss (review DB-3).** A dataset can be backed by outsiders only once its
   operators (declared) hold at least `SUPPLIER_FIRST_LOSS` of backing in a first-loss tranche. A loss
   takes that tranche first. The finder's share is paid only out of the first-loss tranche; outside
   backers' losses go to buyer refunds and the burn or treasury, never to the finder. Poisoning one's
   own dataset and reporting it from another address then returns at most one's own money.
10. **R-DB-10 Only checked decisions slash (review DB-4).** Loss events 1 and 3 (recompute,
   observation) slash in full; a stakes claim slashes only when it ended `Wrong` by observation, never
   by a stake vote or the fallback panel (which a funded actor can capture: THREATS NL6-13, LC16-2).
11. **R-DB-11 Bound fees only (review DB-5).** The backers' cut is paid only from receipts whose
   dataset the payment itself names (the x402 resource, bound into the receipt), not from the
   reporter's routing alone (R-REV-3, REV-1).
12. **R-DB-12 Refunds never exceed what was paid (review DB-7).** A buyer's refund is at most what
   it paid for the wrong values, and only for values bought before the loss event opened.
13. **R-DB-13 Plain words (review DB-8).** The site says "backers lose up to `LOSS_CAP` if this
   dataset is shown wrong; this is not insurance", next to every backed amount.

## Assumptions

- `OnyxRevenue` knows a fee's dataset (each receipt names a round and shard, each shard its metric;
  a level-2 fact names its metric).
- Loss events 1 and 2 already exist on chain; event 3 needs C64's contradictions on chain.
- The token for backing is the one NS12 picks; on testnet tONYX (no value).

## Open questions (for the partners)

1. Every `UPPER_CASE` parameter above, in particular `backersBps`, `MIN_TREASURY_BPS`, `LOSS_PPM`,
   `LOSS_CAP`, `ENTRY_DELAY` and `EXIT_DELAY`.
2. Does a loss burn or go to the treasury?
3. Should buyers of a wrong value be refunded from the pool (R-DB-4), or is the pool only a signal?
4. Is the unit a metric, or should a supplier be able to back one subject (a station, a country)?
