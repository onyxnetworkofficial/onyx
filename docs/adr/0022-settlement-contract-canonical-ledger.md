# ADR-0022: The settlement contract is the canonical ledger

## Status
Accepted, 2026-10-09 (M6 loop 1, project loop 32). Implemented in
`contracts/src/{OnyxSettlement,OnyxRound,OnyxSig}.sol`; tests in
`contracts/test/OnyxSettlement*.t.sol`; shared vectors in `fixtures/settlement/vectors.json`
(written by `adapters/source-btc/tests/evm_vectors.rs`). Not deployed: no testnet key yet.
Amended loop 34 after the M6 loop 32 review (items 10-17), loop 35 after the M6 loop 34
review (items 18-23), loop 44 (items 32-33: M6-L34-F7, M6-L37-F2), and M5 loop 49 (item 36).
Amendment 1 (review L2-R1, 2026-10-13): per-shard reservations and recorded shortfalls; item 7
(review L2-R2): `minBond >= validatorFee x members`.

## Context
M4 and M5 run as an off-chain model. Their reviews (ADR-0019 "Handed to M6", ADR-0021) showed
that a library cannot stop a caller who controls every call, every ledger copy, the clock and the
call order. The chain can. Research notes: RESEARCH.md "M6: settlement contract".

## Decision
1. One deployed `OnyxSettlement` contract is the ledger of one network. Its address is the
   ledger id: `bytes32(uint256(uint160(address)))`, used in the round tag (Rust `round_tag`
   `ledger_id`). A copy of the ledger is a different contract with a different id, so signed
   commits cannot be replayed into it (L27-F1).
2. Block numbers are the only clock. Every window (`commitClose` ... `pendingClose`) is checked
   against `block.number`; no function takes a caller-supplied `now` (L28-F2).
3. Transaction order is the call order across rounds (L26-F1 residual).
4. Validators are a bonded on-chain registry (`stake`, `requestUnbond`, `withdrawStake`).
   A round's validator set is the whole active registry at the opening block, and its root must
   equal `cfg.validatorSetRoot`; the coordinator cannot pick members (L28-F3). Opening a round
   locks every member's stake until `pendingClose`; unbonding waits for the later of
   `unbondDelay` blocks and every lock.
5. Beacon: `seed = keccak256("onyx-seed-evm-v1" || round tag || blockhash(beaconHeight))`, drawn
   by anyone after `beaconHeight`. `blockhash` covers only the last 256 blocks; if nobody draws it
   in time the round is voided and every bond and the budget return. The Base sequencer can bias
   `blockhash` (and `prevrandao`); accepted for the testnet and listed as a residual. Rust uses
   this same form since M6-G4 (item 37).
6. Escrow: all bonds, stakes and rewards are held in one ERC-20 (testnet USDC; a mock in tests)
   and paid by pull withdrawals with checks-effects-interactions and a reentrancy guard. Opening a
   round moves `shards x (minerReward + members x validatorFee)` from the reward pool into the
   round's budget, so payouts never depend on later funding (L28-F5).
7. The coordinator posts a bond. Not opening the canary list by `canaryOpenClose` slashes
   `canarySlash` from it and settlement proceeds without canaries (L25-F7). A canary must match a
   reveal and come from an address whose first commit is in that round.
8. Commits are relayable: the miner signs the Rust `commit_message` (EIP-191, low-s, v in
   {27, 28}); the contract rebuilds the text byte for byte and pulls the signed bond from the
   miner. Reveals recompute the binding and commit hashes (ADR-0011) and must match the shard's
   registered fields hash.
9. Loop-1 settlement: a shard whose reveals all share one root, with no Disagree or NoValidRoot
   verdict and no canary, finalizes on-chain: first reveal paid, bonds returned, Agree verdicts
   paid once per shard, assigned silent validators slashed `missedSlash`, committed-but-unrevealed
   verdicts slashed `validatorSlash` once (L25-F9). Any other shard is `Pending`; at
   `pendingClose` anyone may void it (bonds of revealers return). Finalized roots are readable
   (`getFinalizedRoot`) and rows verifiable (`verifyRow`, ADR-0003).

10. Loop 34 (M6-F1): settling a shard needs a drawn seed, so validators are never judged against
    seed 0 and a late `drawSeed` cannot void a settled shard.
11. Loop 34 (M6-F7): voiding is lazy. A missed beacon marks the round voided and returns its
    budget in one bounded call; `refundVoidedShard(id, shard)` returns one shard's bonds (at most
    `MAX_COMMITS_PER_SHARD`), only for shards still Open or Pending, each once. A full round
    (64 shards x 32 commits) voids and refunds with every call under 1M gas.
12. Loop 34 (M6-F2): a commit hash of zero is refused (`BadCommit`) and duplicates are tracked by
    an explicit `exists` flag, independent of the hash value.
13. Loop 34 (M6-F3): a shard finalizes only with at least one assigned, revealed Agree verdict;
    otherwise it is Pending. `openRound` needs at least `MIN_ROUND_MEMBERS` (3) validators and
    checks, like Rust `Round::open`: every phase at least `MIN_WINDOW` (20) blocks, a verdict
    window that starts after the seed block, `pendingClose >= canaryOpenClose + MIN_PENDING_GAP`
    (100), miner deterrence `minBond x ratePpm >= minerReward x 1e6`, and validator deterrence
    at the canary density (`minCanaryPpm > 0`, `validatorSlash x minCanaryPpm >= validatorFee x
    1e6`, `validatorSlash >= missedSlash > 0`).
14. Loop 34 (M6-F4): the reward goes to the earliest commit among the reveals
    (`OnyxRound.earliest`, matching Rust's commit-order `ShardOutcome::first`), never to the
    first reveal transaction. Pinned by the `rewardOrder` vector.
15. Loop 34 (M6-F5): the contract snapshots the validator set itself at `openRound` and writes
    its root into the stored config and the tag; `cfg.validatorSetRoot` from the caller is
    ignored (read it back with `roundSetRoot`). A registry change just before opening cannot
    make the call fail. Item 4's "must equal `cfg.validatorSetRoot`" is superseded.
16. Loop 34 (M6-F8): the active set is bounded at `MAX_VALIDATORS` with O(1) insert and remove
    (order does not matter; `activeSet` sorts a copy). When full, a newcomer must bring more
    stake than the lowest member, who is evicted into normal unbonding (`Evicted`). Sybils at
    the minimum stake can no longer lock honest validators out.
17. Loop 34 (M6-F9): the ledger id is `keccak256("onyx-ledger-evm-v1" || chain id || address)`
    (Rust `onyx_core::evm_ledger_id`), replacing item 1's padded address. The round-tag format is
    unchanged ("onyx-round-v3"); binding the chain id through the ledger id means the same
    address on another chain is another ledger and commits never replay across chains.
18. Loop 35 (M6-L34-F1): settling is O(members + reveals). Verdicts are tallied per (round,
    shard, validator) as they are committed and revealed; `finalizeShard` reads one tally per
    member and runs the assignment check only for members with no verdict at all, on miner
    addresses held in memory. Measured at the bound (128 validators x 32 reveals): 5.25M gas
    with everyone agreeing at rate 100% (was 38.1M), 3.47M with one Agree, 4.11M at rate 10%.
    No extra config bound is needed: the worst case is fixed by `MAX_VALIDATORS` and
    `MAX_COMMITS_PER_SHARD`, and a regression test holds it under 15M.
19. Loop 35 (M6-L34-F1, F3): settling closes at `pendingClose`, the block stakes and the
    coordinator bond unlock. From then on `voidShard` takes any shard still Open or Pending:
    revealed bonds return, unrevealed ones are forfeit, nobody is paid or slashed, and the budget
    becomes reclaimable. A shard nobody settled can never lock its funds, and no slash can be
    escaped by withdrawing a stake before a late settle. Voiding 32 reveals: under 1M gas.
20. Loop 35 (M6-L34-F3): the first settle after `canaryOpenClose` applies a missed canary
    deadline itself; `missCanaryDeadline` is callable only before `pendingClose`.
21. Loop 35 (M6-L34-F4): `missCanaryDeadline` needs a drawn seed; if the beacon has lapsed it
    voids the round instead of slashing the coordinator on a dead round.
22. Loop 35 (M6-L34-F2): a validator slashed below `minValidatorStake` leaves the set and starts
    normal unbonding (`UnbondRequested`), so the remainder is withdrawable after the delay.
23. Loop 35 (M6-L34-F5): the validator-set root is hashed from one buffer written in place and
    `activeSet` sorts with an O(n log n) quicksort. `openRound` at the bound (128 validators x 64
    shards): 11.9M gas (was 14.1M). The reward-order vector is now produced by a real Rust round
    (`Round::close` / `ShardOutcome::first`, M6-L34-F6).
24. Loop 36 (M6-L35-F1): a round runs at most `MAX_ROUND_SPAN` = 100,000 blocks (about 2.3 days
    at 2 s) from opening to `pendingClose`, so no stake or bond can be locked longer than that.
25. Loop 36 (M6-L35-F2): a round whose seed was never drawn ends one way whatever the call order:
    `voidShard` on it voids the whole round and refunds every bond (as `refundVoidedShard`), and
    `drawSeed` at or after `pendingClose` voids instead of drawing a seed nothing could use.
26. Loop 36 (M6-L35-F4): `activeSet` sorts with a heapsort, O(n log n) on every staking order.
27. Loop 36 (M6-L35-F3): finalize worst case is a slash cascade (every silent member slashed
    below the minimum and deactivated): 5.64M to 6.33M gas at 128 x 32, under the 15M budget. A
    collapsed registry blocks new rounds (`TooFewMembers`) until validators stake again; that is
    the intended outcome of slashing everyone who stayed silent.
28. Loop 36: `contracts/script/Deploy.s.sol` deploys MockUSDC and OnyxSettlement, only on Base
    Sepolia (84532) or anvil (31337), reading the key from `BASE_SEPOLIA_PRIVATE_KEY` at run time.
    Runbook: docs/ops/deploy-base-sepolia.md.

29. Loop 37 (M6-L36-F1): the contracts compile with `via_ir = true` and `optimizer_runs = 200`.
    At the old setting (no IR, 20,000 runs) `OnyxSettlement` was 34,136 bytes of runtime code,
    over the EIP-170 limit of 24,576, so a real deploy would have failed; it is now 21,716 bytes.
    `scripts/check-all.sh` runs `forge build --sizes`, which fails above the limit, and
    `test_ContractsFitEip170` asserts it on the deployed code (the test EVM does not enforce it).
30. Loop 37 (M6-L36-F2, F5): `Deploy.run()` asks the node for its chain id (`eth_chainId`)
    before signing and refuses when it differs from `block.chainid` (a `--chain` flag) or is not
    84532 / 31337. Environment overrides are range-checked before their casts.
31. Loop 37 (M6-L36-F6): `drawSeed` voids the round from `verdictCommitClose` on, not only from
    `pendingClose`: a seed drawn after verdict commits close would leave no verdict window and let
    the caller choose between a verdictless settlement and a void.

32. Loop 44 (M6-L34-F7): unrevealed verdicts are slashed as Rust does. Each verdict commit a
    validator never reveals costs `validatorSlash` once, at round level, whatever its shard's
    outcome (Finalized, Pending, canary, later voided). The contract keeps a per-validator
    counter `unrevealedVerdicts[round][validator]` (+1 at `commitVerdict`, -1 at
    `revealVerdict`); the first `finalizeShard` of the round, or the new permissionless
    `settleRound`, applies `n x validatorSlash` (capped at the stake) to each member once, in
    O(members). On a shard, an unrevealed commit counts as answered (no missed slash, L29-F9
    rule) and a revealed Agree still earns the fee, as Rust judges each revealed verdict. The
    lock rule wins over parity in one case: round-level slashes apply only before
    `pendingClose` (M6-L34-F3), so if nobody settles in time they are dropped, where Rust's
    first `settle` would still apply them. A voided or unseeded round has no verdicts, so
    nothing is slashed in either language. Cross-language vector: `unrevealedSlashing` in
    `fixtures/settlement/vectors.json`, written by Rust's `Validation::settle` on a real round.
33. Loop 44 (M6-L37-F2): `drawSeed` voids unless `MIN_VERDICT_WINDOW` (= `MIN_WINDOW`, 20)
    blocks remain before `verdictCommitClose`; a seed drawn at `verdictCommitClose - 1` left a
    1-block verdict window. The config check (`beaconHeight + 1 + MIN_WINDOW <=
    verdictCommitClose`) guarantees the first drawable block always leaves the full window.
34. Loop 45 (M6-L44-F3): a round must let the seed be drawn in at least `MIN_DRAW_SLACK` (10)
    blocks: `_checkConfig` requires `beaconHeight + MIN_VERDICT_WINDOW + MIN_DRAW_SLACK <=
    verdictCommitClose`, so every block `beaconHeight + 1 .. verdictCommitClose -
    MIN_VERDICT_WINDOW` draws with a full verdict window. Rust mirrors both rules at the same
    thresholds: `Round::open` refuses the same configs, and `Validation::new` returns
    `SeedTooLate` when `drawSeed` would void (the off-chain model only refuses to validate; it records no void or refund, which the contract does; Rust also accepts a draw at `now == beaconHeight`, where the contract needs `block.number > beaconHeight`, so Rust has one more draw height), through the shared rule
    `onyx_coordinator::validate::seed_draw_voids`. The `seedDraw` vector pins the decision
    (tests `test_L45_F3_*`, `validation_rules_are_fixed_at_open`, `self_assignment_is_refused`).
35. Loop 45 (M6-L44-F4): the `unrevealedSlashing` vector also covers Abstain + unrevealed and
    silent + unrevealed, pinning that an unrevealed commit counts as an answer (no missed
    slash) and is slashed once at round level.
36. M5 loop 49 (M5-L46-F3): `_checkConfig` also requires `OnyxRound.disputeConfigValid`, a
    mirror of Rust `DisputeConfig::is_valid` (odd panel, non-zero panel windows, bond and
    slash, challenge window longer than one panel, `panelSlash >= 9 x panelFee`,
    `minChallengeBond >= panelFee x (panelSize + 1)`, `maxAppeals <= 4`), so the contract never
    opens a round Rust refuses. Runtime 23,989 B (margin 587 under EIP-170). The size check in
    `scripts/check-all.sh` skips test-only contracts (the L35 `Harness` extends this contract
    and is never deployed). The rerun deadline and finality states (ADR-0021 items 30-31) add no
    config field, so the round tag and vectors are unchanged; they reach the contract with the
    on-chain dispute game (M6 loop 2).
37. M6-G4: Rust adopts the contract's seed and ledger id. `validate::sample_seed(tag, beacon)`
    is `keccak256("onyx-seed-evm-v1" || tag || beacon)` (the old `onyx-seed-v2` form also bound
    round id, validator set root and sorted reveal triples; the tag already binds the first two,
    `Validation::new` checks the members against that root, and the reveals are fixed when the
    round closes, before the beacon block). `Ledger::mirror_of_contract(chain_id, address)` gives a
    ledger whose id is the contract's `ledgerId()`, so its round tags, seeds, verdict commits
    and dispute ids are the contract's; off-chain use keeps the random id of `Ledger::new`
    (L27-F1). The `evmSeed` vector is a real Rust round on such a ledger: tag, beacon, seed and
    each validator's tasks at a 50% rate; `test_EvmSeedMatchesRust` checks the contract assigns
    the same tasks, and `test_DrawnSeedIsTagAndBlockhash` checks `drawSeed` stores this form.

## Consequences
- M6 loop 2 (ADR-0024): the dispute game is on chain in `OnyxDisputes`; the ledger is split
  for EIP-170 (`OnyxLedger` base, `OnyxSettlementExt` by delegatecall); items 2 and 32 change:
  payouts a dispute can reverse wait for finality, and stakes stay locked through the dispute
  tail so round-level slashes no longer drop at `pendingClose`.
- The ADR-0019 "Handed to M6" items for the clock, ledger identity, call order, validator
  selection, escrow and the canary bond are enforced by the chain. Still open for M6 loop 2:
  rerun attestation and the M5 dispute game for Pending shards, judging validators on canaries,
  the Rust seed and ledger-id alignment (done: item 37), the round-tag v4 fields from M5, a
  deploy script.
- Gas (forge gas report, 3 validators, 2 miners): commit about 237k, reveal about 120k,
  finalize about 350k, open about 575k. Bounds: 128 validators, 64 shards per round, 32 commits
  per shard. Loop 34: each void refund under 1M gas; eviction when the set is full under 2M.
- Loop 35: measured at the bounds (regression tests assert < 15M against the 16.7M cap):
  finalize 5.25M (128 x 32, all agree), voidShard 0.99M (32 reveals), openRound 11.9M (128 x 64).
- Loop 36 (heapsort, slash cascade): openRound 12.16M at 128 x 64 in any staking order (8.81M
  with 1 shard); finalize worst case 6.33M (slash cascade at a 3% rate).
- Loop 37, re-measured under `via_ir` (the settings that are deployed): finalize at 128 x 32
  5.25M all agree, 3.50M one Agree, 4.30M at rate 10%; slash cascade 5.68M / 6.52M / 6.83M at
  rate 100% / 10% / 3%; voidShard 0.99M (32 reveals); openRound 12.44M at 128 x 64 (12.42M in
  the heapsort killer order), 9.28M with 1 shard. All under the 15M test budget (cap 16.7M).
- A shard nobody settles before `pendingClose` is voided: lazy validators on it are not slashed
  and nobody is paid. Validators earn fees only by settling in time, so they have the incentive on shards that can finalize (not on Pending shards: see THREATS M6-L44-F1).
- Loop 44, re-measured (via_ir): finalize at 128 x 32 5.61M all agree, 3.86M one Agree,
  4.66M at rate 10% (+0.36M for the round-level loop); slash cascade 6.04M / 6.88M / 7.19M at
  rate 100% / 10% / 3%; every member unrevealed on all 32 reveals 5.93M; `settleRound` with all
  128 members slashed below the minimum 4.22M, so a first finalize that also runs the round-level
  cascade stays under 4.22M + 7.19M = 11.4M; voidShard 0.99M; openRound 12.44M at 128 x 64
  (12.42M killer order), 9.28M with 1 shard. Runtime code 22,386 of 24,576 bytes.
- Still open for M6 loop 2: the free veto (M6-F10), through the on-chain rerun attestation and
  dispute game. Unrevealed-verdict parity (M6-L34-F7) was fixed in loop 44.
- The coordinator still opens rounds and holds canary keys; its bond is the only penalty.
- Rows availability (the CAS) is not visible on-chain; a shard whose rows are withheld cannot
  be checked by validators and ends Pending or voided.

## Amendment 1 (review L2-R1, 2026-10-13): each shard pays only from its own reservation

Context. Item 6 moves `shards x (minerReward + members x validatorFee + panelSize x panelFee)`
into one round budget, and `_spend` paid "at most what is left" of it, silently. A rerun case
pays a validator's fee once per revealed root it judged right (Rust `settle`, ADR-0024 item 8),
so with two roots six voters drew 12 fees from a reservation of 6, the miner reward was cut and
the honest rerun panel got 0 instead of 30 (PoC ported as
`OnyxBudgetShortTest.test_L2R1_RerunFeesDoNotShortTheMinerOrThePanel`). The budget being
round-wide, a later shard's payees could be shorted too, and Sybil miners revealing distinct
wrong roots made this a cheap grief (THREATS L2C-7).

Decision.
1. Per-shard reservation. Each shard may draw at most `_shardAllot` = `minerReward + members x
   validatorFee + panelSize x panelFee` from the round budget, tracked in `Shard.spent`
   (`shardSpent(id, shard)` reads it with the cap). `openRound` funds exactly `shards x
   _shardAllot`, so no shard can draw on another's share.
2. Rule-mandated items fit by construction: the miner reward once, at most one fee per member
   (the Agree fees at `finalizeShard`, or each member's first right root in a rerun case), and
   the first rerun panel's fees. They are paid from the reservation in full whatever the order.
3. Extra fees (a member's second and later right roots in a rerun case) never touch the budget:
   they are paid from the case's lost miner bonds (non-canary reveals that differ from the
   decided root), which would otherwise burn (`dExtra`, credit reason 7); `_finish` burns only
   what is left. Each distinct wrong non-canary root has at least one bond of at least
   `minBond` behind it, and rule 7 makes `minBond >= validatorFee x members`, so that bond
   covers one extra fee per member: a Sybil or colluding miner adding wrong roots pays the
   extra fees it causes. (Before review L2-R2 this held only when `validatorFee x members <=
   minBond`, which no rule required: at fee 400, bond 1,000 and six colluding voters, a wrong
   root drew 2,400 of extras against a 1,000 bond and `claimOwed` paid the 1,400 rest from the
   reward pool.)
4. Never silent. Whatever cannot be paid (extra fees beyond the lost bonds, which with rule 7
   come only from the canary key, whose bonds return: at most `members x validatorFee` owed per
   shard in total (one member may be owed two fees, since `lostLeft` is spent in member order;
   review L2-R3-2); or
   any draw above a reservation, which the rules never make) emits
   `SpendShort(round, shard, payee, owed, paid)` and is recorded in `owedOf(payee)` /
   `owedTotal`.
5. Owed is claimable, not forfeited: `claimOwed(who)` (anyone) credits it from the reward pool,
   as far as the pool reaches. Justification: the rules owe it (Rust pays it), the payee did the
   work, and the reward pool is the fees' funding source; a round's unspent reservation returns
   to the pool at `reclaimBudget`, so in practice the pool covers it. Forfeiting would let a
   canary or a cheap wrong root cut honest validators' pay with no trace on their balance.
6. Size and layout: the settlement's finalize path writes `spent` once (no cap logic there:
   nothing was spent on an Open shard); the cap, `dExtra`, `claimOwed` and the readers live in
   `OnyxSettlementExt`, the lost-bond pool in `OnyxDisputes` (`Case.lostLeft`). Runtime after:
   settlement 23,178 B (1,398 B under EIP-170), extension 14,563 B, disputes 22,523 B. New state
   is appended to `OnyxLedger` (`_owed`, `_owedTotal`) and `Shard.spent` packs with
   `finalizedAt`; the two layouts stay identical (checked in `scripts/check-all.sh`). No hashed
   packing (ADR-0011) or shared vector changes; Rust pays the same payees the same amounts
   (`Ledger::mirror_of_contract` tests unchanged), the contract only adds where the money comes
   from.
7. Bond covers the fees (review L2-R2). `openRound` refuses (`BadConfig`) a round whose
   snapshotted set has `validatorFee x members > minBond`; Rust refuses it in
   `Validation::new` (`ValidationError::FeesExceedBond`, `fees_covered_by_bond`), the first
   point the round's set is known there (`Round::open` has only its root). So owed, and
   with it what `claimOwed` can take from the reward pool, is limited to the canary key: at
   most `members x validatorFee` per shard in total (not one fee per member: review L2-R3-2).
   Testnet and demo configs pass at 16 members: 16 x 0.05 USDC = 0.8 USDC <= 4 USDC; at the
   128-validator cap they would not (6.4 USDC), so a stake set grown past 80 makes `openRound`
   revert until the coordinator lowers the fee to `minBond / 128` (THREATS L2-R3-1, a liveness
   item, no funds at risk). Runtime cost 54 B (settlement 23,232 B, 1,344 B under EIP-170).

Consequences. Tests: `OnyxBudgetShortTest` (PoC, Sybil roots in batches, an uncovered canary
fee owed and claimed) and `OnyxShardReservationTest` (a greedy draw on one shard is capped and
recorded, the other shard's payees are untouched). The settlement invariant suite models the
rules per shard (`invariant_PayoutsMatchRules`: budget drained + extra fees + recorded owed ==
the rules' amounts; recorded owed == the model's uncovered extras); a planted double miner
reward now fails it (docs/audit/2026-10-13-contracts.md). Review L2-R2:
`test_L2R2_FeeTimesMembersAboveMinBondIsRefused` (the ported PoC config is refused),
`test_L2R2_WrongRootCannotFarmTheRewardPool` (the PoC's attack at the largest allowed fee: no
owed, reward pool untouched), Rust `fees_above_the_bond_per_member_are_refused`; the payout
invariant now also compares per payee (a planted fee to the wrong member fails it).
