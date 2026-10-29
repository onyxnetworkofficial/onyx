# Spec: settlement on Base

## Scope
What lives onchain on Base Sepolia (chain id 84532): the canonical ledger of one Onyx network
(ADR-0022). Validator registry, coordinator bond, reward pool, round lifecycle, verdicts,
canaries, finalized roots, and escrow with pull withdrawals. Contract:
`contracts/src/OnyxSettlement.sol`; hashing and texts: `OnyxRound.sol`, `OnyxSig.sol`,
`OnyxCommit.sol`; Merkle verifier: `OnyxMerkle.sol`. M6 loop 2 (ADR-0024) adds the dispute game
in `OnyxDisputes.sol` and splits the ledger for EIP-170: shared state and rules in the abstract
`OnyxLedger.sol`, rarely called entry points and the dispute hooks in `OnyxSettlementExt.sol`,
reached by `delegatecall` from `OnyxSettlement`'s fallback at the same address. The old sketch
`IOnyxSettlement.sol` is removed. Dispute rules: docs/spec/disputes.md.

## Interfaces
- Escrow: `withdraw()` (pull), `fundRewards(amount)`, `balanceOf(addr)`, `burned`, `rewardPool`.
- Registry: `stake(amount)` (when the set is full, more stake than the lowest member evicts
  it), `requestUnbond()`, `withdrawStake()`, `activeSet()` (sorted by address),
  `validators(addr) -> (stake, lockedUntil, unbondAt, active)`.
- Coordinator: `postCoordinatorBond(amount)`, `withdrawCoordinatorBond()`.
- Rounds: `openRound(id, RoundConfig, shardIds ascending, ShardInit[])` (coordinator only; the
  contract snapshots the whole active registry and writes its root; `cfg.validatorSetRoot` is
  ignored), `roundTag(id)`, `roundSetRoot(id)`, `ledgerId()`,
  `commit(id, shard, miner, commitHash, bond, sig)` (relayable; EIP-191 over the Rust
  `commit_message`), `reveal(id, shard, miner, CommitmentFields, salt)`, `drawSeed(id)`,
  `seedOf(id)`.
- Verdicts: `commitVerdict(id, shard, miner, verdictCommit)` (sender = validator, must be a
  snapshot member assigned by the seed), `revealVerdict(id, shard, miner, kind, root, salt)`
  (kind = Rust Borsh tag: 0 Agree, 1 Disagree, 2 Abstain, 3 NoValidRoot).
- Canaries: `openCanaries(id, Canary[] sorted, salt)`, `missCanaryDeadline(id)`.
- Settlement: `finalizeShard(id, shard)`, `voidShard(id, shard)`,
  `refundVoidedShard(id, shard)` (after a missed beacon; one shard per call), `reclaimBudget(id)`.
- Extension (same address, ADR-0024): `isFinal(id, shard)`, `roundConfig`, `shardState`,
  `shardMiners`, `commitOf`, `verdictOf`, `tallyOf`, and the hooks only `OnyxDisputes` may call:
  `dBegin`, `dEnd`, `dPull`, `dCredit`, `dBurn`, `dSpend(id, shard, to, amount)`, `dExtra`,
  `dSlash`. Also served there:
  `getFinalizedRoot`, `verifyRow`, `requestUnbond`, `withdrawStake`, `postCoordinatorBond`,
  `withdrawCoordinatorBond`, `missCanaryDeadline`, `refundVoidedShard`, `reclaimBudget`,
  `roundSetRoot`, `roundBudget`, `shardSpent(id, shard) -> (spent, allot)`, `owedOf(who)`,
  `owedTotal`, `claimOwed(who)` (ADR-0022 Amendment 1).
- Disputes (`OnyxDisputes`, its own address): `openRerun`, `challenge`, `revealClaim`, `draw`,
  `commitVote`, `revealVote`, `appeal`, `resolve`, `arbitrate` (coordinator), `expire`,
  `settleCase(caseId, maxMembers)`, `release(id, shard)`; readers `caseInfo`, `panel`, `votes`,
  `exposure`, `caseOf`, `caseCount`, `released`; pure rules `disputeId`, `claimCommit`,
  `panelCommitHash`, `panelKey`, `drawPanel`, `noValidRootMarker`, `verdictRight`.
- Readers: `getFinalizedRoot(id, shard) -> (root, binding, finalizedAt)` (final roots only),
  `verifyRow(id, shard, row, index, leafCount, siblings)`, `shardStatus`, `roundMembers`,
  `roundBudget`, `everFinalized(shardId)`.
- Events: `Staked`, `UnbondRequested`, `Evicted`, `RoundOpened`, `Committed`, `Revealed`, `SeedDrawn`,
  `RoundVoided`, `VerdictRevealed`, `CanariesOpened`, `CanaryDeadlineMissed`, `Finalized`,
  `ShardPending`, `ShardVoided`, `Credited`, `Slashed` (reason codes `CR_*` / `SL_*`),
  `SpendShort(round, shard, payee, owed, paid)` (a rule-mandated payment found too little).
- Cross-language vectors: `fixtures/settlement/vectors.json` (round tag, commit message and
  digest, signature, validator set root, canary commitment, verdict hashes, assignments, chain-
  bound ledger id, reward order).

## Rules
- The ledger id is keccak256("onyx-ledger-evm-v1" || chain id || contract address); block
  numbers are the only clock; transaction order is the call order (ADR-0022 items 1-3, 17).
- Only roots and small metadata go onchain. Rows stay offchain in the CAS.
- A root is final once it is `Finalized` and its challenge window is over, or a dispute case
  decided it. Only final roots are served (`getFinalizedRoot`, `verifyRow`). A unanimous shard's
  reward and revealed bonds stay in escrow until then; `OnyxDisputes.release` pays them
  (ADR-0024 item 2). A `Pending` shard can get a rerun case before `pendingClose`; a case ends
  `Finalized`, `NoValidRoot` or `Unresolved` and closes the shard.
- Stakes stay locked until `pendingClose + disputeTail(cfg)` (at most `MAX_DISPUTE_TAIL` =
  100,000 blocks), so every dispute of the round can still slash; round-level slashes apply on
  the first finalize, `settleRound`, `voidShard` or case, until then (ADR-0024 item 3).
- `openRound` funds one rerun panel per shard (`panelSize x panelFee`) and refuses to open until
  `OnyxDisputes` is bound to this ledger.
- A finalized commitment binds root, leaf count, recipe, runtime profile, label set, source
  snapshot, schema and rights (R-SETTLE-1); `getFinalizedRoot` returns root and binding hash.
- A shard id that finalized once is never opened again.
- Settling needs a drawn seed and happens in [verdictRevealClose, pendingClose), while every
  stake and the coordinator bond are locked. Unanimous, undisputed, canary-free shards with at
  least one assigned Agree verdict finalize; the earliest commit among the reveals is paid. All
  others stay Pending until a rerun case decides them (ADR-0024). At `pendingClose` any shard still
  Open or Pending is voided: revealed bonds return, unrevealed are forfeit, and the round-level
  slashes apply if nobody applied them yet (stakes are still locked).
- Each verdict commit a validator never reveals costs `validatorSlash` once, at round level,
  whatever its shard's outcome (as Rust). The first `finalizeShard` of a round, or
  `settleRound`, applies it from the `unrevealedVerdicts` counter, before `pendingClose` only.
  On its shard an unrevealed commit counts as answered (no missed slash); a revealed Agree
  still earns the fee. Vector: `unrevealedSlashing` in `fixtures/settlement/vectors.json`.
- Verdicts are tallied as they arrive; settling reads O(members + reveals) storage slots and
  stays under 15M gas at the bound (128 validators x 32 reveals; worst case 7.19M with a slash
  cascade, plus at most 4.22M for a round-level cascade on the first settle; via_ir build). The first settle after `canaryOpenClose` applies a
  missed canary deadline; `missCanaryDeadline` needs a drawn seed (a lapsed beacon voids the
  round) and is callable only before `pendingClose`.
- A validator slashed below the minimum stake leaves the set and starts normal unbonding.
- A missed beacon voids the round; bonds return per shard with `refundVoidedShard`, once. A
  round still unseeded at `pendingClose` is voided whichever of `drawSeed` and `voidShard` runs
  first, and every bond comes back.
- A round runs at most `MAX_ROUND_SPAN` = 100,000 blocks from opening to `pendingClose`.
- A round needs at least 3 validators. Window order, each phase at least 20 blocks: open + 20 <=
  commitClose; commitClose + 20 <= revealClose <= beaconHeight; beaconHeight + 30 <=
  verdictCommitClose (`MIN_VERDICT_WINDOW` 20 + `MIN_DRAW_SLACK` 10, AUD-1-F7); verdictCommitClose + 20 <= verdictRevealClose <= canaryOpenClose;
  canaryOpenClose + 100 <= pendingClose. Miner and validator deterrence as Rust (ADR-0022 item 13).
- Commit hashes are non-zero; one commit per (round, shard, miner).
- A seed is drawn only while at least `MIN_VERDICT_WINDOW` (20) blocks remain before
  `verdictCommitClose`; drawing it later voids the round.

## Assumptions
- Base Sepolia gas is cheap enough: commit about 237k, reveal about 120k, finalize about 350k
  gas with 3 validators (forge gas report); at the bounds (via_ir build, as deployed) finalize
  5.61M (worst case 7.19M, 11.4M with a round-level cascade), void 0.99M, open 12.44M.
- Runtime bytecode fits EIP-170 (via_ir, 200 runs; CI fails on a contract over the limit):
  `OnyxSettlement` 23,137 B, `OnyxSettlementExt` 13,407 B, `OnyxDisputes` 22,146 B,
  `OnyxRound` (linked) 6,126 B, `OnyxRevenue` 9,282 B, `OnyxRegistry` 6,047 B (measured
  2026-10-11 with `forge build --sizes --skip test`, AUD-1-F7).
- Dispute gas at the bounds (forge, via_ir): `settleCase` worst batch 7.18M (8 members x 32 wrong
  verdicts each, plus closing the case); `draw` of a 63-member panel from 125 eligible 10.7M;
  closing that case 6.0M (`contracts/test/OnyxDisputesBound.t.sol`).
- The Base sequencer does not bias `blockhash(beaconHeight)` (testnet trust; ADR-0022 item 5).
- Testnet only; deploy key from `.env` (not present yet; nothing is deployed). Deploy with
  `contracts/script/Deploy.s.sol` per docs/ops/deploy-base-sepolia.md.
- Bonds and rewards use one ERC-20 (testnet USDC on Base Sepolia; `MockUSDC` in tests).

## Open questions
- Answered in M6 loop 2 (ADR-0024): a drawn panel attests the rerun of a Pending shard, and the
  coordinator's posted rerun decides a split. Mainnet: on-chain proofs (q7 (b)).
- Batch many shard roots into one root per round to save gas?
- Which testnet USDC contract on Base Sepolia to point at (address to be confirmed from Circle's
  or Base's documentation before deploying)?
