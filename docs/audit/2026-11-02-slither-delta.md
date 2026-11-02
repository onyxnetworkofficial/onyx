# Contract delta audit: Slither and invariants since LC-16 (2026-11-02)

ROADMAP L17, second pass. The first pass ([2026-10-27-lc16-slither.md](2026-10-27-lc16-slither.md))
covered every contract changed since the L2 audit up to main `037bc1a`. This pass covers what
changed in `contracts/src` since then (`git diff --stat 037bc1a HEAD -- contracts/src`, at commit
`054cca71`): the new `OnyxStorage` and `OnyxStorageLib` (S15, S16; deployed on Base Sepolia),
`OnyxCorrections` (CL7), `OnyxParametric` (insurance), `OnyxPrepaidV2` and `OnyxRevenueV2` (CL6),
and edits to `OnyxFutures`, `OnyxObservedFacts`, `OnyxPanelOracle`, `OnyxReobserve` and
`IOnyxMarkets` (2,669 lines added, 18 removed). Free, open-source tools only; no contract was
changed. Not an external audit (that is ROADMAP H5).

## Result

| Gate | Command (from a copy of `contracts/`) | Result |
|---|---|---|
| Format, sizes, tests, storage layout, gas snapshot | `scripts/check-all.sh` (as every push) | passed on `054cca71`; `OnyxStorage` 22,057 B runtime (2,519 B under EIP-170) |
| Invariants at audit depth, new suites | `FOUNDRY_PROFILE=audit forge test --threads 4 --fuzz-seed 0x20261102 --match-path 'test/invariant/*' --match-contract 'Storage\|Corrections\|Credits\|Parametric'` (512 runs x 256 calls) | exit 0 in 755 s: 16 invariants in 4 suites (storage 5, corrections 4, credits 5, parametric 2) and the storage handler's reach test, 0 failures |
| Slither 0.11.6 | `slither . --filter-paths "lib/\|test/\|script/" --json slither.json` | 202 results over 43 contracts (High 13, Medium 60, Low 90, Informational 36, Optimization 3); 63 touch a changed contract, triaged below: **0 security findings**, 1 cosmetic true positive |

Tools: Foundry 1.8.5, solc 0.8.30 (via-IR), Slither 0.11.6 from PyPI in the venv at
`~/.cache/onyx-slither-venv` (Python 3.14). Heavy steps under `systemd-run --user --scope -p
MemoryMax=12G`. The 139 results that touch no changed contract lie in contracts the first pass or
the L2 triage already cleared (their High and Medium counts match: the High count rose from 8 to
13 only by the five results below).

## Triage of the 63 results on changed contracts

| Detector (impact) | Count | Where | Verdict and reason |
|---|---|---|---|
| reentrancy-balance (High) | 1 | `OnyxStorage._pull`: balance before `transferFrom`, the difference checked after | FP: the check is the guard against a token that moves less than asked. The token is immutable (set at deploy; tONYX, `OnyxTestToken`, has no transfer hooks), and every function that calls it (`register`, `cancelRegistration`, `withdrawBond`, `createDeal`, `extendDeal`, `closeDeal`, `withdrawEarnings`, `sweepTreasury`) holds the contract's `nonReentrant` lock; `answer`, `settleMissed`, `decline`, `activate`, `requestExit` and `recordSeed` move no token |
| reentrancy-balance (High) | 1 | `OnyxPrepaidV2.depositWithAuthorization`: balance before `receiveWithAuthorization`, `before + amount` checked after | FP: same pattern (exactly the authorized amount arrived); `nonReentrant`; the token is immutable test USDC with EIP-3009 |
| reentrancy-balance (High) | 2 | `OnyxRevenueV2.withdrawToCredits`: `approve` and the caller-chosen `credits.depositFor` between the balance reads | FP: the caller's balance is zeroed and counted paid before the calls, the lock is held, the allowance is exactly the amount and reset to zero after, and the balance check proves exactly that amount left; every token-moving `OnyxRevenue` function (`register`, `credit`, `settle`, `pay`, `withdraw`) takes the same lock, so a hostile `credits` can only move the caller's own share (THREATS CL6-7) |
| uninitialized-state (High) | 1 | `OnyxCorrections._bets` "never initialized" | FP: a mapping written through a storage pointer (`uint128[2] storage b = _bets[marketId][msg.sender]` in `bet`), which the detector does not follow; payouts from it are tested (`OnyxCorrections.t.sol::test_AnEmptyWinningSideRefundsEveryBet`, the corrections invariants) |
| reentrancy-no-eth (Medium) | 1 | `OnyxStorage.extendDeal` writes `deposit` and `endEpoch` after `_pull` | FP for the deployed token (no hooks) and `nonReentrant`; a cross-function entry would need a token callback. Effects-before-pull is the tidier order for the next deployment (no behaviour change; noted, not changed here, since the source must keep matching the deployed `0x9244…ee43`) |
| incorrect-equality (Medium) | 7 | `OnyxStorage.recordSeed` (`bh == 0`), `_checkpoint` (`_cpFrom[k-1] == from`), `_ask` and `challengedHolders` (`s == 0`); `OnyxReobserve.prune`, `isAssignable` (`at == 0`); `OnyxPrepaidV2.payRelayer` (`day == relayDay`) | FP: zero sentinels of hashes and timestamps and an epoch or day index, never a balance |
| uninitialized-local (Medium) | 7 | `OnyxCorrections._le128.r` and `OnyxObservedFacts._le128.r`, `OnyxPrepaidV2.allocate.total`, `OnyxPanelOracle.finalize.tally` and `draw.chosen`, `OnyxReobserve.outcome.unavailable` and `same` | FP: accumulators and flags that start at zero on purpose (the same five as the first pass, plus the two copies of `_le128` and `allocate`) |
| unused-return (Medium) | 6 | `OnyxStorage._checkAnchor` (`book.factView`, `book.keyState`), `OnyxCorrections.pair` (`registry.metrics` twice), `OnyxPanelOracle._choose`, `isValidator` | FP: tuple destructuring that takes the fields it needs (state and accepted hash; life and suspension; kind and template) |
| calls-loop (Low) | 3 | `OnyxPanelOracle.isValidator`, `_choose`; `OnyxPrepaidV2.allocate` | Accepted: calls to the deployment's own contracts, bounded (`MAX_SEATS`, `MAX_BATCH`), as in the first pass |
| missing-zero-check (Low) | 2 | `OnyxReobserve.bind`, `OnyxFutures` constructor `treasury_` | Accepted, as in the first pass |
| reentrancy-benign (Low) | 4 | `OnyxStorage.createDeal`, `register`, `extendDeal`; `OnyxPrepaidV2.depositWithAuthorization` | Accepted: state or events after a guarded call to the immutable token |
| timestamp (Low) | 18 | windows in `OnyxFutures`, `OnyxParametric`, `OnyxPanelOracle`, `OnyxReobserve`, `OnyxObservedFacts`, `OnyxPrepaidV2` | Accepted: windows of an hour or more, never a random source (`OnyxStorage` uses block numbers, not time) |
| assembly (Informational) | 2 | `OnyxPrepaidV2.signedBy`, `OnyxPanelOracle._choose` | Accepted: the same signature split and length trim as the first pass |
| costly-loop (Informational) | 3 | `OnyxPrepaidV2.redeem`, `OnyxPanelOracle.finalize`, `_drop` | Accepted: bounded loops |
| missing-inheritance (Informational) | 2 | `OnyxPrepaidV2` could inherit `IOnyxCredits`; `OnyxTestToken` could inherit `IApproveToken` | Accepted: the functions exist; cosmetic |
| unindexed-event-address (Informational) | 1 | `OnyxPrepaidV2.ReporterSet` | Accepted: a rare admin event |
| unused-state (Informational) | 1 | `OnyxStorage.GIB` is never used | **True positive, cosmetic**: a dead internal constant (GiB rounding uses the manifest's byte count through `OnyxStorageLib`); remove at the next storage deployment, no effect on behaviour |
| cache-array-length (Optimization) | 1 | `OnyxPanelOracle._choose` | Accepted, as in the first pass |

## Role and access table of the new external functions

`OnyxStorage` has no owner and no admin path; every parameter is immutable (ADR-0060 decision 9).

| Function | Who may call | What it moves |
|---|---|---|
| `register` | anyone (becomes the registration's operator) | pulls the bond from the caller; 2% to the treasury account at once |
| `activate`, `cancelRegistration` | that registration's operator | cancel pushes the bond less the fee back to the operator |
| `requestExit`, `withdrawBond` | the node's operator | the bond back to the operator, two epochs after leaving |
| `createDeal`, `extendDeal` | anyone (the deal's payer; extend: anyone pays, the remainder still returns to the payer) | pulls the deposit from the caller |
| `closeDeal` | anyone, after the deal's last epoch | pushes the unpaid deposit to the deal's payer |
| `decline`, `answer` | the node's operator | `answer` credits earnings in the contract; no token moves |
| `settleMissed` | anyone (10% of the slash to the caller unless it is the node's own operator) | credits only; slashes stay in the contract |
| `withdrawEarnings` | anyone, for its own earnings | pushes the caller's earnings |
| `sweepTreasury` | anyone | pushes the slash account to the immutable treasury |
| `recordSeed` | anyone, in the 256 blocks after an epoch starts | nothing |

The other new contracts keep the roles their reviews set (THREATS CL6, CL7 and the insurance
section): `OnyxCorrections.pair`, `OnyxParametric.offer`, `OnyxPrepaidV2.setReporter` and
`setSource` are the owner's (a multisig before mainnet, ROADMAP H1); `OnyxPrepaidV2.redeem`,
`allocate` and `payRelayer` are the reporter's; betting, underwriting, buying, depositing,
withdrawing and claiming are anyone's for their own funds and every one of those holds the
contract's `nonReentrant` lock; `resolve`, `void` and `settle` are permissionless state moves
after their windows.

## What this does not cover

Logic bugs a static analyzer cannot see (the reviews in THREATS cover those), economic attacks
(STORAGE-3 and the CL6, CL7 sections), the off-chain node, and an independent review (H5).
