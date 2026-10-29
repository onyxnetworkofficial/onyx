# Contract audit L2 (2026-10-13)

ROADMAP Phase L item L2: static analysis, handler-based invariant tests, high-count fuzzing, gas
and size, and a review of admin powers, upgrade paths and keys. Branch `audit-contracts` from
main 6665104 (main c6a78a5 merged in for review L2-R1). Free, open-source tools only. Not an external audit (the mainnet gate in
[upgrades.md](../ops/upgrades.md) still needs one).

Scope: `contracts/src` — OnyxSettlement, OnyxSettlementExt, OnyxLedger (shared state),
OnyxDisputes, OnyxRevenue, OnyxRegistry, and the libraries OnyxRound (linked), OnyxCommit,
OnyxMerkle, OnyxSig.

## Result

| Area | Result |
|---|---|
| Slither 0.11.6 | 94 results: High 6, Medium 27, Low 44, Info 15, Optimization 2. 1 true positive (Low, fixed), 43 false positives, 50 accepted patterns; [triage](2026-10-13-contracts-triage.md) |
| forge lint (Foundry 1.8.5) | 122 warnings in 14 lints, all triaged (same file) |
| Invariants | 14 invariants (incl. `PayoutsMatchRules`, per payee since review L2-R2) + 2 end-of-run "no stuck funds" checks, 3 suites, 512 runs x 256 calls each (393,216 calls per seed): **0 failures on the 6 seeds listed under Invariants** (the old failing seed of review L2-R2 and 5 fresh random seeds). Until L2-R2 this row claimed 0 failures from one fixed seed (0x20261013); a random seed then failed (a handler model bug, L2-R2-1) |
| Fuzz at 10,000 runs | 17 fuzz tests (25 results: suites that inherit a test run it again): **0 failures** |
| Gas and size | Largest runtime OnyxSettlement 23,232 B (1,344 B under EIP-170, 5.5%); `.gas-snapshot` committed (461 unit tests, `gas` profile without metadata); `check-all.sh` fails on any gas change and on a storage-layout split between the settlement and its extension (fail-closed since L2-R1) |
| Contract changes | Two: OnyxRound `verdictCommitHash` parameter `seed` renamed `seed_` (Slither shadowing-local, solc warning 2519); per-shard budget reservations, extra rerun fees from lost bonds, `SpendShort` and claimable owed (L2C-7 / L2-R1-2, ADR-0022 Amendment 1); `openRound` refuses `minBond < validatorFee x members` (L2-R2-2, Amendment 1 item 7). No hash, packing (ADR-0011) or vector change; vectors not regenerated |

## Commands and tool versions

All from `contracts/` unless noted; heavy commands under
`systemd-run --user --scope -q -p MemoryMax=12G`.

| Tool | Version |
|---|---|
| Foundry (forge, anvil, cast) | 1.8.5, commit 51a52c5 (2026-10-05) |
| solc | 0.8.30, via-IR, optimizer 200 runs, evm cancun |
| Slither | 0.11.6 (`pip install slither-analyzer` into a venv; Python 3.14.4) |

| Step | Command | Exit |
|---|---|---|
| Slither | `slither . --filter-paths "lib/\|test/\|script/" --foundry-compile-all --json slither.json` | 255 (findings present; expected) |
| forge lint | `forge lint src --json` | 0 |
| Unit + fuzz (1,024) + invariants (16 x 64) | `forge test` | 0 (450 passed, 1 skipped: the Base Sepolia fork test) |
| Audit profile, whole suite | `ONYX_INVARIANT_STATS=1 FOUNDRY_PROFILE=audit forge test --threads 4 --fuzz-seed 0x20261013` | 0 (450 passed, 1 skipped; 1,062 s) |
| Audit profile, invariants on the final handlers | `ONYX_INVARIANT_STATS=1 FOUNDRY_PROFILE=audit forge test --threads 3 --fuzz-seed 0x20261013 --match-path 'test/invariant/*'` | 0 (336 s) |
| Sizes | `forge build --sizes --skip test --json` | 0 |
| Gas snapshot | `FOUNDRY_PROFILE=gas forge snapshot --no-match-test 'testFuzz_\|invariant_' --no-match-contract Fork` (and `--check`) | 0 (460 tests; passes from another path and after a comment-only edit, L2-R1-1) |
| Gas per function | `forge test --gas-report --json --no-match-test 'testFuzz_\|invariant_' --no-match-contract 'Fork\|Bound\|Harness'` | 0 |
| All checks (repo root) | `ONYX_WEB=1 scripts/check-all.sh` | 0 |
| Local rehearsal with a dispute (repo root) | `ONYX_DEMO_RPC_PORT=18645 ONYX_DEMO_API_PORT=18603 ONYX_DEMO_DRIVER=target/release/examples/demo-local scripts/demo-local.sh --dispute` | 0 (REHEARSAL PASSED, 152 txs on anvil) |

The `audit` profile (foundry.toml): fuzz `runs = 10000`; invariant `runs = 512`, `depth = 256`,
`fail_on_revert = false`, `show_metrics = true`. The default profile runs the invariants at
16 x 64 so `forge test` stays fast.

## Findings

Static-analysis findings are in the [triage](2026-10-13-contracts-triage.md) (S-1..S-94,
F-1..F-8). The table lists the true positive and every item left open or accepted on purpose.

| ID | Severity | Contract:line | Finding | Status |
|---|---|---|---|---|
| S-77 | Low | OnyxRound.sol:309 | `verdictCommitHash` parameter `seed` shadowed the library function `seed` (also solc warning 2519 on every build) | FIXED: renamed `seed_`; `test_VerdictCommitHashesMatchRust` unchanged and passing |
| S-2..6 | High (tool) / FP | OnyxLedger.sol:234-264 | "Never initialized" state: written by OnyxSettlement, read by OnyxSettlementExt through delegatecall | FP; now guarded: `check-all.sh` diffs the two storage layouts (identical, 19 slots) |
| L2C-1 | Low | OnyxSettlement (whole) | Runtime 23,232 B, 1,344 B (5.5%) under EIP-170: little room for any fix in the settlement itself | OPEN (THREATS L2C-1): new logic goes to the extension; `--sizes` in check-all fails above the limit |
| L2C-2 | Low | deploy manifest (`crates/onyx-node/src/manifest.rs`) | The client manifest pins settlement, revenue and registry code hashes; the settlement's immutables pin the dispute game's ADDRESS but nothing pins OnyxDisputes' code (it holds unlimited `dCredit`/`dSlash` power over the escrow) or the linked OnyxRound library | OPEN (THREATS L2C-2), launch checklist K-5 |
| L2C-3 | Info | OnyxRevenue.sol:194, 219 | Reporter may equal the owner (treasury); on Base Sepolia the deployer is coordinator, treasury owner and first reporter (then `setReporter` to the operator key) | ACCEPTED for testnet (THREATS L2C-3); K-1/K-3 |
| L2C-4 | Info | OnyxRegistry.sol:257 | `owner != coordinator` is checked once, at construction; the current settlement's coordinator is immutable, so it holds, but a ledger with a mutable coordinator could later make them equal | ACCEPTED (THREATS L2C-4) |
| L2C-5 | Info | OnyxLedger.sol:183-199, OnyxRegistry.sol:111, OnyxRevenue.sol:104 | Every role but the revenue reporter is immutable: coordinator, dispute game, registry owner, revenue owner/treasury. A lost or leaked key means a redeploy; docs/ops/key-management.md says role keys are replaceable without redeploying, which is not true for these | DOCS FIXED (L2-R1-5): key-management.md has a per-role rotation table; K-2/K-3 stay OPEN for L10 (THREATS L2C-5) |
| L2C-6 | Info | OnyxLedger `burned`, `rewardPool`; OnyxRevenue `unallocated()` | Burned tokens and the unspent reward pool never leave the settlement; tokens sent to OnyxRevenue without an EIP-3009 authorization can never be attributed. By design (no sweep, ADR-0025) | DOCUMENTED (THREATS L2C-6): the end-of-run invariant proves this is the ONLY money left after every lifecycle ends |
| L2C-7 | **Medium** (regraded by review L2-R1) | OnyxLedger.sol:395 (`_spend`), OnyxDisputes.sol:652 | One budget per round, shared by its shards; `_spend` paid at most what was left, silently. A rerun case pays a fee once per revealed root, so it drew more than the shard's reservation: the reviewer's PoC left the miner reward short and the honest rerun panel at 0 instead of 30; other shards' payees could be shorted; Sybil wrong roots made it a cheap grief | **FIXED** (L2-R1-2, ADR-0022 Amendment 1): per-shard reservations, extra fees from lost bonds, `SpendShort` + claimable owed; PoC ported as a regression test. L2-R2-2: `minBond >= validatorFee x members`, so owed comes only from the canary key |
| F-4 | Info | OnyxLedger.sol:270-271 | No zero-address check on `coordinator_`, `disputes_` | ACCEPTED: a zero address only bricks that deployment; K-6 checks after deploy |

## Invariants

Handlers in `contracts/test/invariant/`. Each handler action is wrapped in try/catch (a revert is
a refused action, not a failure); rules that cannot be read from state are checked inside the
handler and counted as breaches, asserted zero. Two whole-lifecycle actions (`autoRound`,
`autoCase`) drive a round or a case end to end with random choices at every step, so deep
states are reached at any depth; the other actions interleave around them.

Audit runs after review L2-R2 (final handlers, per-payee payout model): 512 runs x 256 calls per
suite, `FOUNDRY_PROFILE=audit forge test --threads 2 --fuzz-seed <seed> --match-path
'test/invariant/*'`, on the seed that failed in review L2-R2 and 5 fresh random seeds (`openssl
rand -hex 32`). Earlier runs (L2, L2-R1) used only the fixed seed 0x20261013; that is all the
"0 failures" claimed then covered.

| Seed | Exit | Registry / revenue / settlement (s) |
|---|---|---|
| 0x84aebf072a122eb24b83e2d677fea3a3f611de96901b060ed5787b29e32adaef (failed in L2-R2) | 0 | 118 / 170 / 883 |
| 0x5ce450dda8d11ee05b6fd4207aa66dbd52744798cfa613f09b4f45c4aef15eb9 | 0 | 141 / 206 / 787 |
| 0x3cd0baf9873c0e6c712e87e21bfa719b4c80db9d363442db4d2b51cb462ccdc9 | 0 | 152 / 221 / 811 |
| 0x933d9ab6717cc79de0c8c9a31d84f03f9963dde68c1cfd72dfb372e07f821fec | 0 | 147 / 212 / 937 |
| 0x3594ae71b55f776d947bf8ff76684e193e573f8ae18452dbb7c7b1d09c208b8b | 0 | 117 / 167 / 1,018 |
| 0xe653e5cca7cb395c680f297311578501716f7e96d1bf107ede494c15adf5c056 | 0 | 157 / 215 / 776 |

The settlement suite is about 2.5x slower than in L2-R1 (state-diff recording around every
payout call for the per-payee check). Six seeds are evidence, not proof: other seeds were not
run. The coverage table below is from the L2-R1 run (seed 0x20261013).

| Suite (file) | Invariant | Runs x depth | Result |
|---|---|---|---|
| Settlement + disputes (`SettlementInvariants.t.sol`) | `EscrowEqualsLiabilities`: token balance == credits + stakes + reward pool + coordinator bond + round budgets + held miner and dispute bonds + burned (exact; nothing minted, nothing leaked) | 512 x 256 | pass |
| | `FlowsMatchBalance`: only deposit paths bring tokens in, only `withdraw` takes them out, each by what it moved | 512 x 256 | pass |
| | `RewardsNeverExceedFunding`: reward pool + round budgets <= everything funded | 512 x 256 | pass |
| | `ExposureMatchesOpenPanels`: each validator's `exposure` == `panelSlash` x seats on running panels | 512 x 256 | pass |
| | `PayoutsMatchRules` (L2-R1-3, L2-R2-3): per payee and per shard, budget drained + extra fees paid from lost bonds + shortfalls recorded owed == an independent model of the rules (miner reward once, verdict fees once per validator and revealed root, a rerun's first-panel fees); recorded owed == the model's uncovered extra fees; no release short of the reward | 512 x 256 | pass |
| | `RolesLocksAndFinality`: coordinator and the bound pair fixed; no privileged call (12 kinds: `openRound`, `openCanaries`, every `d*` hook, `arbitrate`, `bind`, the extension called directly, coordinator bond) ever succeeded from another account; no stake left before its round's lock end; no coordinator bond left before `pendingClose`; a root `isFinal` reports never changes or stops being final; `everFinalized` never resets; a terminal shard status never changes | 512 x 256 | pass |
| | `afterInvariant` (no stuck funds): after every shard, case and stake is driven to its end and everyone withdraws, every shard and case has ended, credits, stakes, budgets, held bonds, coordinator bond and exposures are 0, and the balance == burned + reward pool | 512 runs | pass |
| Revenue (`RevenueInvariants.t.sol`) | `AllocatedIsPaidOrOwed`: allocated == paid out + balances + held label shares + unclaimed per-head shares (exact) | 512 x 256 | pass |
| | `NeverAllocatesMoreThanArrived`: balance + paid out >= allocated; balance == inflows - withdrawals; `paidOut` == withdrawals | 512 x 256 | pass |
| | `BooksMatchTheirFees`: a book's revenue == what was credited to it (credits modelled independently of the contract); heads x per-head <= revenue | 512 x 256 | pass |
| | `NoBreaches`: every fee split adds exactly the fee to what is owed; `credit` applies exactly the receipts an independent model applies (unused, reported used by the token, within unallocated); no registration with fields that miss the final binding or of a non-final shard; no privileged call from a non-role account; owner fixed | 512 x 256 | pass |
| | `afterInvariant` (no stuck funds): after all claims, payees for held label shares and withdrawals, allocated == paid out and the balance == only unattributed arrivals | 512 runs | pass |
| Registry (`RegistryInvariants.t.sol`) | `SubjectDrawBound`: per (metric, subject) strikes == announced - recorded and <= 3 x (owner reopens + 1) | 512 x 256 | pass |
| | `DrawsAndFactsAreImmutable`: <= 64 draws per fact, sequential, never changed, beacon height after the announcement, a fixed beacon never changes | 512 x 256 | pass |
| | `RegistrationsAreFinal`: metric kind and template, label-set level and recipe floor never change once set | 512 x 256 | pass |
| | `Roles`: owner fixed; `receiptSigner()` is zero unless the current coordinator set it in its current epoch (coordinator rotations simulated); no owner power for the coordinator and no coordinator power for the owner or anyone else; in-handler checks: a draw is sequential, its beacon in the future, within the subject bound, with a signer of this term; a beacon is the block hash and fixed only after its block; a record needs the latest draw's beacon and committed spec; an attestation respects the budget, label-set and recipe floors; commit and receipt log slots are written once, only with the right signature | 512 x 256 | pass |

Coverage of the audit run (sum over 512 runs of successful actions):

| Suite | Successful actions |
|---|---|
| Settlement + disputes | 3,995 rounds opened (1,979 by `autoRound`), 10,465 commits, 8,138 reveals, 17,169 verdicts revealed, 782 shards finalized, 1,489 pending, 1,254 voided/refunded, 455 rerun cases, 185 challenges, 574 panel draws, 267 panel votes revealed, 49 appeals, 136 arbitrations, 157 expiries, 313 cases judged, 597 releases (0 short rewards), 890 units of uncovered extra fees recorded owed (all matching the model), 3,521 `claimOwed` calls, 3,580 withdrawals |
| Revenue | 2,882 books, 8,256 `pay`, 6,599 `settle`, 5,417 receipts credited, 8,180 claims, 8,839 withdrawals |
| Registry | 1,243 facts, 1,394 draws announced, 576 beacons fixed, 209 observations recorded, 1,388 attestations, 3,074 transcript-log posts |

Limits: the handlers use one config family (short windows, panel 1 or 3, one appeal); canaries
are a single coordinator-made reveal; the token is a plain mock (no hooks). The revenue and
registry suites use mock ledgers (the settlement suite covers the real ledger's reads).

## Fuzz at 10,000 runs

`FOUNDRY_PROFILE=audit`, seed 0x20261013, every fuzz test at 10,000 runs: 0 failures.

| Test | Runs | Mean gas |
|---|---|---|
| `OnyxMerkleTest.testFuzz_RandomProofRejected` | 10,000 | 21,423,204 |
| `OnyxMerkleTest.testFuzz_RandomSiblingsAtTrueIndexRejected` | 10,000 | 22,790,468 |
| `OnyxMerkleTest.testFuzz_OneBitSiblingFlipRejected` | 10,000 | 22,766,004 |
| `OnyxMerkleTest.testFuzz_IndexOutOfRangeRejected` | 10,000 | 1,280 |
| `OnyxSettlementL35.testFuzz_AssessMatchesReference` | 10,000 | 18,793,198 |
| `OnyxSettlementReview.testFuzz_ActiveSetSortedAndRootMatches` | 10,000 | 6,952,159 |
| `OnyxSettlementReview.testFuzz_SolventAfterSettleOrVoid` | 10,000 | 2,788,754 |
| `OnyxRevenueTest.testFuzz_PartsSumToTheFee` | 10,000 | 2,132,848 |
| `OnyxRegistryTest.testFuzz_AFactKeepsItsFirstResult` | 10,000 | 914,954 |
| `OnyxRegistryTest.testFuzz_BudgetMatchesThePpmRule` | 10,000 | 418,337 |
| `OnyxRegistryTest.testFuzz_ShardIdMatchesTheLibrary` | 10,000 | 420,846 |
| `OnyxRegistryTranscriptLog.testFuzz_SignedTextsRoundTrip` | 10,000 | 1,331,722 |
| `OnyxSettlementVectors.testFuzz_TextHelpers` | 10,000 | 33,406 |
| `OnyxSettlementVectors.testFuzz_HighSRefused` | 10,000 | 10,822 |
| `OnyxCommitTest.testFuzz_ShardFieldsIgnoreRootOnly` | 10,000 | 848,159 |
| `OnyxCommitTest.testFuzz_CopiedCommitUseless` | 10,000 | 1,077,711 |
| `OnyxCommitTest.testFuzz_LabelSetBound` | 10,000 | 847,471 |

(The registry and settlement-review fuzz tests also run in suites that inherit them, so the log
shows 25 fuzz results; all passed.)

## Gas and size

EIP-170 runtime limit 24,576 B; EIP-3860 initcode limit 49,152 B. `forge build --sizes`:

| Contract | Runtime (B) | Margin (B) | Margin | Initcode (B) | Initcode margin (B) |
|---|---|---|---|---|---|
| OnyxSettlement | 23,178 | 1,398 | 5.7% | 39,804 | 9,348 |
| OnyxDisputes | 22,523 | 2,053 | 8.4% | 22,562 | 26,590 |
| OnyxSettlementExt | 14,563 | 10,013 | 40.7% | 15,554 | 33,598 |
| OnyxRegistry | 13,604 | 10,972 | 44.6% | 14,064 | 35,088 |
| OnyxRevenue | 9,282 | 15,294 | 62.2% | 9,969 | 39,183 |
| OnyxRound (linked library) | 6,126 | 18,450 | 75.1% | 6,156 | 42,996 |

Gas per call from the unit suites (`forge test --gas-report`; median / max over the calls the
tests make, max includes the bound tests' 128-validator and 32-commit cases):

| Function | Median | Max |
|---|---|---|
| `OnyxSettlement.openRound` | 926,229 | 14,232,252 |
| `OnyxSettlement.commit` | 190,899 | 228,376 |
| `OnyxSettlement.reveal` | 120,590 | 137,756 |
| `OnyxSettlement.commitVerdict` / `revealVerdict` | 73,536 / 76,505 | 107,780 / 136,326 |
| `OnyxSettlement.finalizeShard` | 401,618 | 6,622,330 |
| `OnyxSettlement.voidShard` | 123,123 | 1,627,893 |
| `OnyxSettlement.stake` / `withdraw` | 142,746 / 56,621 | 812,807 / 67,920 |
| `OnyxSettlementExt.refundVoidedShard` | 395,025 | 942,225 |
| `OnyxDisputes.challenge` / `openRerun` | 381,120 / 267,403 | 381,132 / 267,403 |
| `OnyxDisputes.draw` | 560,821 | 1,047,444 |
| `OnyxDisputes.settleCase` | 573,350 | 776,062 |
| `OnyxDisputes.release` | 217,891 | 217,891 |
| `OnyxRevenue.register` | 433,927 | 8,360,499 |
| `OnyxRevenue.credit` / `settle` / `pay` | 140,430 / 73,528 / 139,404 | 245,461 / 252,033 / 203,172 |
| `OnyxRegistry.announceDraw` / `recordObservation` | 124,737 / 42,817 | 158,974 / 204,537 |
| `OnyxRegistry.attestShard` / `postCommit` | 44,239 / 179,103 | 150,636 / 179,129 |

(Per-function figures above are from the first audit pass, before the L2-R1 budget change;
`settleCase` and `finalizeShard` now also write `Shard.spent`.) Full per-test gas:
`contracts/.gas-snapshot` (460 unit tests, `gas` profile). The gas-bound suites
(`OnyxDisputesBound`, `OnyxSettlementL35`) keep the worst cases (`settleCase` at 128 validators,
a panel of 63) inside their batch budgets.

New checks in `scripts/check-all.sh`:
1. Storage layouts of OnyxSettlement and OnyxSettlementExt must be identical (`forge inspect
   ... storageLayout`), since the extension runs on the settlement's storage. Each side is
   captured under pipefail and must be non-empty (L2-R1-4).
2. `FOUNDRY_PROFILE=gas forge snapshot --check` on the unit tests: any gas change fails; after an
   intended change, rerun without `--check` and commit `.gas-snapshot` (CLAUDE.md Commands).

## Review L2-R1 (strike 1) and its fixes

| Item | Finding | Fix | Evidence |
|---|---|---|---|
| L2-R1-1 Blocker | `forge snapshot --check` failed at HEAD and from a fresh `git archive` (12 diffs): gas depended on CBOR metadata (source paths, comments) | `[profile.gas]` in foundry.toml: `bytecode_hash = "none"`, `cbor_metadata = false`, its own `out-gas`/`cache-gas`; check-all runs `FOUNDRY_PROFILE=gas forge snapshot --check`; snapshot regenerated. The vector tests read fixtures by relative path (`"../fixtures/..."`): the absolute path's length changed 9 tests' gas | Copy of the tree at another path: `--check` exit 0, 0 diffs; plus a comment-only edit in OnyxSettlement and OnyxDisputes: exit 0, 0 diffs (control, default profile with metadata: exit 1, 41 diffs). Default-profile deployed bytecode: OnyxRound identical; OnyxRegistry and OnyxRevenue identical code (metadata only, they import OnyxLedger); settlement, extension and disputes changed only by L2-R1-2 |
| L2-R1-2 Blocker, Medium | L2C-7 is real (PoC) | ADR-0022 Amendment 1: (a) each shard draws at most its reservation `minerReward + members x validatorFee + panelSize x panelFee` (`Shard.spent`); (b) the miner reward, one fee per member and the first panel's fees fit it by construction, so they are paid in full; (c) a member's 2nd+ right root in a rerun case is paid from the case's lost miner bonds (`dExtra`), only the rest burns; (d) anything unpaid emits `SpendShort(round, shard, payee, owed, paid)` and is recorded in `owedOf`, claimable from the reward pool (`claimOwed`): claimable because the rules owe it and the payee did the work, and round leftovers return to the pool | `OnyxBudgetShortTest` (PoC: miner 1,000 + 500, panel 30, six voters 20 each, budget drew 590, 940 of the lost bond burned; Sybil third root judged in batches; canary-root fee owed then claimed), `OnyxShardReservationTest` (greedy draw on shard A capped at 560 with `SpendShort`, shard B paid its full 560). Sizes above |
| L2-R1-3 Medium | Invariants blind to budget-drained payouts; a planted double miner reward passed | Per-shard rules model in `SettlementHandler` (ghosts `ruleOwed`/`seenPaid`/`ruleShort`/`seenOwed`, measured around every `finalizeShard`, `release` and `settleCase`), `invariant_PayoutsMatchRules`, `claimOwed` action, end-of-run owed check | Planted double `dSpend` of the miner reward in a scratch copy: the settlement suite fails at the default profile (16 x 64) with "payouts differ from the rules"; copy deleted, the branch never held the plant. Unplanted: 512 x 256 pass |
| L2-R1-4 Low | Storage-layout gate failed open (`diff <(...) <(...)`) | Each side captured into a variable under pipefail, non-empty required, then compared | With a `forge` stand-in that fails, prints an empty layout, or prints differing layouts: exit 1 each; identical: passes |
| L2-R1-5 Low | key-management.md promised rotation for immutable roles | Per-role table: only the revenue reporter (`setReporter`) and the registry receipt signer (`setReceiptSigner`) rotate in place; coordinator and dispute binding need a new settlement (new ledger id); registry owner and revenue owner/treasury need that contract redeployed | docs/ops/key-management.md |

## Review L2-R2 (strike 2) and its fixes

| Item | Finding | Fix | Evidence |
|---|---|---|---|
| L2-R2-1 Blocker (test model) | `invariant_EscrowEqualsLiabilities` failed on random seed 0x84aebf072a122eb24b83e2d677fea3a3f611de96901b060ed5787b29e32adaef (ends `settleCase(id,1)`): `SettlementHandler.escrow()` counted a Decided rerun case's lost bonds in full after a partial `settleCase` had paid extra fees from them. The report's "0 failures" rested on one fixed seed | The reviewer's patch: `extraPaid[case]` sums `Credited` reason 7 per `settleCase` call and `escrow()` subtracts it while the case is Decided. No contract change | That seed and 5 fresh random seeds at 512 x 256, table under Invariants: all exit 0 |
| L2-R2-2 Blocker, Medium | A wrong root drained the reward pool through `claimOwed` when `validatorFee x voters > minBond` (PoC: fee 400, bond 1,000, six colluding voters: 1,400 owed, paid from the pool) | `openRound` (`_snapshotMembers`, +54 B) reverts `BadConfig` if `minBond < validatorFee x members`; Rust `Validation::new` returns `FeesExceedBond` (`fees_covered_by_bond`), the first point Rust knows the set (`Round::open` has only its root). ADR-0022 Amendment 1 item 7; item 3's claim corrected: owed now comes only from the canary key, at most one fee per member per shard | `test_L2R2_FeeTimesMembersAboveMinBondIsRefused` (PoC fee, cap + 1 refused; cap opens), `test_L2R2_WrongRootCannotFarmTheRewardPool` (PoC attack at the cap: 0 owed, pool untouched, six extras at fee `minBond / members` within the 1,000 bond), Rust `fees_above_the_bond_per_member_are_refused` (101 members refused, 100 allowed at bond 1,000 / fee 10), `fees_covered_by_bond_bounds`. Testnet and demo: 16 x 50,000 = 800,000 <= 4,000,000 micro-USDC. Test fixture `_cfg()` lowers the fee to 7 only for the 128-member bound tests (128 x 10 > 1,000) |
| L2-R2-3 Minor | `PayoutsMatchRules` compared per-shard totals only | Per-payee ghosts `ruleBy`/`seenBy`: the model's reward winner (lowest-sequence correct non-canary reveal), verdict fees and first-panel fees per address, against finalize's `Credited` reason 4 and the dispute game's `dSpend`/`dExtra` calldata (`vm.startStateDiffRecording`); each call's named payee must match the credits and `SpendShort` it produced | Scratch copies, default profile (16 x 64): plant A (finalize credits the next member's fee) fails `PayoutsMatchRules` ("a shard's payouts differ from the rules"); plant B (first rerun fee `dSpend` to the reveal's miner) fails it too; the L2-R1 handler on plant A passes every invariant (exit 0), so per-shard totals alone were blind. Copies deleted; the branch never held a plant |
| L2-R2-4 Optional | Cross-language vector for a rerun with 2+ revealed roots | Not done: needs a Rust generator for a full rerun, a Solidity replay with signed commits and re-blessed shared vectors (THREATS L2-R2-4, TODO L9) | Shared vectors unchanged and passing on both sides |

## Admin powers and upgrade paths

No contract has a proxy, pause, `selfdestruct`, sweep or owner-settable parameter beyond the
ones below. An upgrade is a new deployment ([upgrades.md](../ops/upgrades.md)); roots stay
readable at the old address (the ledger id binds chain id and address).

| Contract | Role | Set how | Powers | Limits on chain |
|---|---|---|---|---|
| OnyxSettlement (+Ext) | coordinator | constructor, immutable | `openRound` (config, shards, canary commitment), `openCanaries`, post/withdraw its bond; also OnyxDisputes `arbitrate` and the registry's coordinator powers | config bounded by `roundConfigValid` (windows, deterrence, round span <= 100,000 blocks, tail <= 100,000); validator set is the whole registry snapshot; bond slashed for missed canaries; bond locked to `pendingClose` |
| OnyxSettlement (+Ext) | dispute game | constructor, immutable address | `dBegin`, `dEnd`, `dPull`, `dCredit`, `dBurn`, `dSpend`, `dSlash`: unlimited money power over the escrow | only the code at that address; `openRound` refuses until the pair is bound both ways; the hooks refuse direct calls to the extension |
| OnyxDisputes | deployer | constructor, immutable | `bind` once | `bind` needs `settlement.disputes() == this` |
| OnyxDisputes | coordinator (read from the settlement) | — | `arbitrate`: posts the root of a case that needs a rerun | only in `RerunRequired` and before the deadline; the root is then judged by the same rules as a panel's |
| OnyxRevenue | owner = treasury | constructor, immutable | `setReporter` | — |
| OnyxRevenue | reporter | constructor, then owner | `credit` (attribution), `settle`, `setLabelPayee`, `registerObservation` | credits total-capped by unallocated arrivals, each `(payer, nonce)` once, only if the token reports it used; claims already credited never move (REV-1, REV-L2-F3) |
| OnyxRegistry | owner | constructor, immutable; never the coordinator at deploy | `registerMetric`, `registerLabelSet`, `registerMetricMinLevel`, `reopenDraws` | append-only; every call an event |
| OnyxRegistry | coordinator (read from the ledger) | — | `attestShard`, `announceDraw`, `recordObservation`, `setReceiptSigner` | the enforced rules in ADR-0030 (shard id recomputed, floors, budget, draw bound, beacon, committed spec) |

## Testnet key and owner map (Base Sepolia)

From `scripts/demo-testnet.sh`, `contracts/script/Deploy*.s.sol` and
`adapters/source-btc/examples/demo-local.rs` (testnet mode). Keys live in `.env` (deployer only)
and `.onyx-testnet/keys/` (gitignored, 0600); none is printed or committed.

| Key | Where | Roles on chain |
|---|---|---|
| deployer (`BASE_SEPOLIA_PRIVATE_KEY`, 0x80c9...7972) | `.env` | deploys OnyxRound, OnyxDisputes, OnyxSettlement, OnyxRevenue, OnyxRegistry; settlement **coordinator** (so also `arbitrate` and the registry's coordinator powers); disputes **binder**; OnyxRevenue **owner and treasury** (`ONYX_TREASURY` unset) and first reporter |
| `operator` | `.onyx-testnet/keys/operator.key` | OnyxRevenue **reporter** after `setReporter` (the API's hot key; gas only) |
| `registry-owner` | `.onyx-testnet/keys/registry-owner.key` | OnyxRegistry **owner** (gas only) |
| receipt signer | `onyx_coordinator::observe::test_keys::receipt_signer()` (published test key) | OnyxRegistry receipt signer, set by the coordinator (ROADMAP B3: a separate hot key per deployment) |
| `miner-0..1`, `validator-0..15`, `customer` | `.onyx-testnet/keys/` | miners, bonded validators, x402 payer |

## What a mainnet key ceremony needs (for docs/ops/launch-checklist.md)

| ID | Item |
|---|---|
| K-1 | One key per role, made for mainnet only: deployer (used once, then retired), coordinator (hot, in a KMS/HSM), revenue owner/treasury (multisig), revenue reporter (hot API key), registry owner (multisig or timelock), receipt signer (hot, separate from the coordinator's transaction key). No key holds two of these; the treasury is never the reporter (L2C-3). |
| K-2 | The multisig and KMS addresses exist before deploy: coordinator, registry owner and revenue owner are immutable constructor arguments (L2C-5). |
| K-3 | A written plan for a lost or leaked immutable key: redeploy, announce the new ledger id, and stop serving the old one; update key-management.md, which now promises rotation without redeploying (L2C-5). |
| K-4 | Parameters reviewed and recorded before deploy: token address (Circle's list for the chain), domain, `unbondDelay`, `minValidatorStake`, `minCoordinatorBond`, `canarySlash`, revenue split. |
| K-5 | After deploy, read back and publish in a signed manifest: `coordinator()`, `disputes()`, `settlement()`, `ext()`, `owner()`, `reporter()`, `treasury()`, the split, and the runtime code hashes of the settlement, its extension, the dispute game, the OnyxRound library, revenue and registry (L2C-2, REV-12). |
| K-6 | Zero-address and pairing checks on the read-back (no zero `coordinator`/`disputes`, `disputes.settlement() == settlement`, registry owner != coordinator). |
| K-7 | Fund the reward pool in tranches: funded tokens never leave except as round rewards (L2C-6). |
| K-8 | An external audit before any mainnet deploy (upgrades.md mainnet gate). |

## Checks

| Check | Exit |
|---|---|
| `forge test` | 0 |
| `FOUNDRY_PROFILE=audit forge test` (whole suite) | 0 |
| `FOUNDRY_PROFILE=audit forge test --match-path 'test/invariant/*'` (final handlers) | 0 |
| After review L2-R1: `forge test` | 0 (488 passed, 1 skipped: the fork test) |
| After review L2-R1: `FOUNDRY_PROFILE=audit forge test --match-path 'test/invariant/*'` | 0 |
| After review L2-R1: `FOUNDRY_PROFILE=gas forge snapshot --check` | 0 (460 tests, 0 diffs) |
| After review L2-R1: `ONYX_WEB=1 scripts/check-all.sh` | 0 (ALL CHECKS PASSED, web gate and E2E included; 17 min 53 s) |
| After review L2-R1: `scripts/demo-local.sh --dispute` | 0 (REHEARSAL PASSED; dispute round: panel decided, payouts checked against the rules by the driver; ports 18645/18603) |
| `ONYX_WEB=1 scripts/check-all.sh` | 0 (ALL CHECKS PASSED, web gate and E2E included; 1,441 s) |
| After review L2-R2: `forge test` | 0 (490 passed, 1 skipped: the fork test) |
| After review L2-R2: `FOUNDRY_PROFILE=audit forge test --match-path 'test/invariant/*'`, 6 seeds (table under Invariants) | 0 on each |
| After review L2-R2: `FOUNDRY_PROFILE=gas forge snapshot --check` | 0 after regenerating (319 entries changed: `openRound`'s new check runs in every test that opens a round; 2 new tests; 461 entries) |
| After review L2-R2: `cargo test -p onyx-coordinator -p onyx-source-btc` | 0 (297 passed) |
| After review L2-R2: `ONYX_WEB=1 scripts/check-all.sh` | 0 (ALL CHECKS PASSED, web gate and E2E included; 919 s) |
| After review L2-R2: `scripts/demo-local.sh --dispute` | 0 (REHEARSAL PASSED; ports 18645/18603; 165 s) |
| `scripts/demo-local.sh --dispute` | 0 |

Rust↔Solidity vectors: no hashed packing changed, so nothing was re-blessed. The Solidity side
(`OnyxSettlementVectors`, `OnyxDisputesVectors`, `OnyxApiVector`, `OnyxMerkle`
`test_RustVectorsVerify`, `OnyxRegistry` vectors) passes in `forge test`; the Rust side passes
in `cargo test --workspace` inside check-all.
