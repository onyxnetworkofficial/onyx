# Contract static-analysis triage (L2, 2026-10-13)

Every Slither and `forge lint` result on `contracts/src` at branch `audit-contracts` (from main
6665104), with a decision. Report: [2026-10-13-contracts.md](2026-10-13-contracts.md).

Decisions: **FIXED** (true positive, changed, with the test that covers it), **FP** (false
positive, why), **ACCEPTED** (real pattern, deliberate, why). Line numbers are of main 6665104.

## Tools and commands

| Tool | Version | Command (from `contracts/`) |
|---|---|---|
| Slither | 0.11.6 (`pip install slither-analyzer` in a venv, Python 3.14.4) | `slither . --filter-paths "lib/\|test/\|script/" --foundry-compile-all --json slither.json` |
| forge lint | Foundry 1.8.5 (51a52c5) | `forge lint src --json` |
| solc | 0.8.30, via-IR, optimizer 200 runs, evm cancun | (through Foundry) |

Slither exits 255 when it reports findings (expected). Raw outputs are not committed (generated
exports); rerun the commands above to reproduce.

## Slither: 94 results

| Impact | Results | FIXED | FP | ACCEPTED |
|---|---|---|---|---|
| High | 6 | 0 | 6 | 0 |
| Medium | 27 | 0 | 27 | 0 |
| Low | 44 | 1 | 6 | 37 |
| Informational | 15 | 0 | 3 | 12 |
| Optimization | 2 | 0 | 1 | 1 |
| **Total** | **94** | **1** | **43** | **50** |

| ID | Detector | Impact | Where (contract:line) | Decision | Why |
|---|---|---|---|---|---|
| S-1 | reentrancy-balance | High | OnyxRevenue.sol:340 (`settle`) | FP | The balance is read before `transferWithAuthorization` and compared after on purpose: `settle` proves exactly `a.value` arrived. The token is the immutable USDC (FiatToken: no transfer hooks). Every token-moving entry point (`settle`, `pay`, `credit`, `register`, `withdraw`) is `nonReentrant`, and any extra arrival fails the strict `!= a.value` check (revert), so a reentrant deposit cannot be counted. Tests: `test_SettleMovesTheFundsAndCreditsTheShardInOneCall`, invariant `RevenueInvariantTest` (allocated never above arrivals). |
| S-2..6 | uninitialized-state | High | OnyxLedger.sol:234 `coordinatorLockedUntil`, :244 `firstCommitRound`, :258 `verdicts`, :260 `tallies`, :264 `unrevealedVerdicts` | FP | Slither reads `OnyxSettlementExt` alone. These are written by `OnyxSettlement` (`openRound`, `commit`, `commitVerdict`) and read by the extension through `delegatecall` on the same storage (ADR-0024 item 1). Both inherit `OnyxLedger` and declare no other state; `scripts/check-all.sh` now fails if the two storage layouts ever differ (`forge inspect ... storageLayout`). Exercised by every dispute test and `SettlementInvariantTest`. |
| S-7 | divide-before-multiply | Medium | OnyxRevenue.sol:418 (`split`) | FP | Per-head shares are floored on purpose; the treasury takes the remainder, so the parts sum to the fee exactly (Rust `revenue::split`). Tests: `testFuzz_PartsSumToTheFee` (10,000 runs), invariant `invariant_AllocatedIsPaidOrOwed`. |
| S-8..14 | reentrancy-no-eth | Medium | OnyxDisputes.sol:554 (`settleCase`, x2), :508 (`expire`), :697 (`_finish`), :734 (`_settlePanel`); OnyxRevenue.sol:362 (`pay`), :340 (`settle`) | FP | OnyxDisputes calls only the settlement it is bound to once (immutable code; its `d*` hooks never call back into OnyxDisputes; the one token call, `dPull`, is the last call of `challenge`/`appeal` and `nonReentrant` in the settlement). OnyxRevenue: `nonReentrant` on every token-moving function and an immutable trusted token. Invariants `invariant_ExposureMatchesOpenPanels`, `invariant_EscrowEqualsLiabilities`. |
| S-15..26 | uninitialized-local | Medium | OnyxDisputes.sol:212, 627, 629, 646, 679, 680, 743, 744; OnyxRevenue.sol:257, 265; OnyxSettlement.sol:456; OnyxSig.sol:41 | FP | Counters, flags and accumulators that rely on Solidity's zero default. |
| S-27..33 | unused-return | Medium | OnyxDisputes.sol:225, 243, 349, 598, 811; OnyxRegistry.sol:428; OnyxRevenue.sol:251 | FP | Tuple destructuring of view getters (`shardState`, `validators`, `shardMiners`) that keeps only the fields needed. |
| S-34..70 | calls-loop | Low | OnyxDisputes `_choose`, `_ctx`, `_judgeValidator`, `_settleMiners`, `_settlePanel`, `_settleChallenger`, `_finish`, `expire`; OnyxSettlement `_assess`; OnyxSettlementExt `reclaimBudget`; OnyxRevenue `register`, `credit` (37 results) | ACCEPTED | Every loop calls a trusted contract (the bound settlement or dispute game, the immutable token's `authorizationState`), so no callee can selectively revert to grief. Bounds: 128 validators, 32 commits per shard, 64 shards, panels of at most 63, 64 receipts. Gas at the bound: `OnyxDisputesBound` (`test_SettleCaseAtTheBound`, batched by `maxMembers`). |
| S-71..72 | reentrancy-benign | Low | OnyxRevenue.sol:362 (`pay`), :340 (`settle`) | FP | As S-8: `nonReentrant`, trusted token. |
| S-73..76 | reentrancy-events | Low | OnyxDisputes.sol:805 (`release`), :697 (`_finish`), :554 (`settleCase`), :508 (`expire`) | FP | Events after calls to the bound settlement, which cannot reenter or reorder them. |
| S-77 | shadowing-local | Low | OnyxRound.sol:309 (`verdictCommitHash` parameter `seed` shadows function `seed`) | **FIXED** | Renamed to `seed_`; also removes solc warning 2519 from every build. No behaviour change: `OnyxSettlementVectorsTest.test_VerdictCommitHashesMatchRust` (shared Rust vectors) passes unchanged. |
| S-78..81 | assembly | Info | OnyxRound.sol:242 (`validatorSetRoot`), OnyxSig.sol:63 (`recover`), OnyxSettlement.sol:636 (`fallback`), OnyxDisputes.sol:335 (`_choose`) | ACCEPTED | Reviewed: in-place buffer checked by `test_ValidatorSetRootMatchesRust` and `testFuzz_ActiveSetSortedAndRootMatches`; 65-byte `r‖s‖v` load with canonical v/s checks (`testFuzz_HighSRefused`); standard delegatecall forwarder; memory-safe length shrink of a fresh array. |
| S-82..83 | costly-loop | Info | OnyxLedger.sol:401 (`_burn` in `voidShard`), OnyxRevenue.sol:421 (`_credit` in `credit`) | ACCEPTED | Bounded loops (32 commits, 64 receipts). |
| S-84..85 | cyclomatic-complexity | Info | OnyxDisputes.sol:614 (`_judgeValidator`, 13), :734 (`_settlePanel`, 12) | ACCEPTED | Mirrors Rust `Validation::settle` branch for branch; covered by `OnyxDisputesTest` and the invariants. |
| S-86 | missing-inheritance | Info | OnyxSettlementExt.sol:18 (could inherit `IOnyxLedgerView`) | ACCEPTED | It implements every function of the interface; declaring it changes nothing. |
| S-87 | too-many-digits | Info | OnyxRound.sol:254 | ACCEPTED | The `"onyx-validators-v1"` domain tag as one word (vector-checked). |
| S-88..89 | unindexed-event-address | Info | OnyxRegistry.sol:200 `ReceiptSignerSet`, OnyxRevenue.sol:150 `ReporterSet` | ACCEPTED | One event per rotation; changing the signature would break indexers for nothing. |
| S-90..92 | unused-state | Info | OnyxLedger.sol:174, 175, 178 (`CR_SHARD_VOIDED`, `CR_VERDICT`, `SL_MISSED` unused in the extension) | FP | Constants used by `OnyxSettlement`; constants take no storage. |
| S-93 | cache-array-length | Opt | OnyxSettlement.sol:107 (`_evictLowest`) | ACCEPTED | Rare path, at most 128 iterations; the settlement has 1,397 B of EIP-170 headroom, so code churn for gas is not worth it. |
| S-94 | constable-states | Opt | OnyxLedger.sol:234 `coordinatorLockedUntil` | FP | Written by `OnyxSettlement._snapshotMembers` (same reason as S-2). |

## forge lint: 122 warnings (14 lints)

Overlaps with Slither (same decision as above): `calls-loop` 30, `reentrancy-events` 13,
`reentrancy-no-eth` 5, `uninitialized-local` 12, `uninitialized-state` 1, `unused-return` 7.

| ID | Lint | Count | Where | Decision | Why |
|---|---|---|---|---|---|
| F-1 | encode-packed-collision | 2 | OnyxSig.sol:56 (`eip191Digest`), OnyxRound.sol:227 (`commitMessage`) | FP | EIP-191 prefixes the message with its decimal length, so the packing is unambiguous; the commit text is fixed-width hex fields between fixed labels with only the last field (decimal bond) variable. Both are Rust-vector checked (`test_CommitMessageDigestAndSignatureMatchRust`). |
| F-2 | arbitrary-send-erc20 | 1 | OnyxLedger.sol:306 (`_pull`) | FP | `from` is `msg.sender` (stake, fund, bond, through `dPull` the challenger or appellant) or the miner whose EIP-191 signature over this exact bond was just verified (`commit`; each signed commit usable once). |
| F-3 | incorrect-strict-equality | 1 | OnyxRevenue.sol:353 | FP | Intended: exactly the authorized value must arrive (ADR-0026). |
| F-4 | missing-zero-check | 4 | OnyxLedger.sol:270-271, OnyxSettlement.sol:37-38 (`coordinator_`, `disputes_`) | ACCEPTED (Info) | A zero coordinator or dispute address only bricks that deployment (no round can open: `openRound` requires the bound pair). The deploy scripts set both and the deploy doc checks `coordinator()`; immutables need a check at deploy time (launch checklist item K-6). |
| F-5 | missing-events-access-control | 2 | OnyxDisputes.sol:143 (`bind`), OnyxSettlement.sol:223 (`isRoundMember`) | ACCEPTED (Info) / FP | `bind` runs once at deploy, readable as `settlement()`; round membership is committed by the `RoundOpened` set root. |
| F-6 | non-reentrant-not-first | 1 | OnyxSettlementExt.sol:313 (`dPull`) | FP | The modifiers before it (`onlyDisputes`, `delegated`) make no external call. |
| F-7 | require-revert-in-loop | 11 | OnyxSettlement `_initShards`, OnyxSettlementExt `openCanaries`/`reclaimBudget`, OnyxLedger `_shard`, OnyxRevenue `_credit` | FP | All-or-nothing validation is the intent. |
| F-8 | unsafe-typecast | 32 | `uint64(block.number)`/`uint64(block.timestamp)` (safe for ~10^11 years), lengths bounded by the constants above, `uint128` casts after explicit range checks (`budget`, `a.value`, slash cap), `uint64` lock end (`pendingClose <= open + 100,000` and tail `<= 100,000`) | FP | Each cast is bounded where it is made. |
