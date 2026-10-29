# ADR-0024: On-chain disputes in a second contract, `OnyxDisputes`

## Status
Accepted, 2026-10-10 (M6 loop 2, project loop 51). Implemented in
`contracts/src/{OnyxDisputes,OnyxSettlement,OnyxRound}.sol`; tests in
`contracts/test/OnyxDisputes*.t.sol`; shared vectors in `fixtures/settlement/vectors.json`
(`disputes` section, written by `adapters/source-btc/tests/evm_vectors.rs`). Not deployed.

## Context
ADR-0021 (M5) models disputes off-chain; ADR-0022 left the dispute game for M6 loop 2. Open
threats: a single Disagree, NoValidRoot or canary sends a shard to Pending and then void, a free
veto (M6-F10); nobody is paid to settle a round whose shards are all Pending, so round-level
slashes can be skipped (M6-L44-F1); a split panel can be "appealed" for free when no larger panel
exists, cancelling the silent member's slash (M5-L49-F1); the Rust model marks a rerun and then
returns an error, which a contract would revert (M5-L49-F3). `OnyxSettlement` is 587 bytes under
the EIP-170 limit (RESEARCH.md "M6 loop 2"), so the game cannot be added to it.

## Decision
1. Contract split. `OnyxSettlement` stays the ledger (escrow, registry, rounds, verdicts,
   unanimous settlement). `OnyxDisputes` runs the panel game and judges shards against a decided
   root. To fit EIP-170 the ledger is split once more: all state, types and shared rules live in
   the abstract `OnyxLedger`; `OnyxSettlement` and `OnyxSettlementExt` both inherit it and declare
   no state of their own, so their layouts are identical. `OnyxSettlement`'s constructor creates
   the extension with the same parameters, and its fallback forwards every unknown call to it by
   `delegatecall` (same storage, same `msg.sender`). The extension serves the dispute readers and
   hooks plus rarely called entry points (`getFinalizedRoot`, `verifyRow`, unbonding, coordinator
   bond, refunds, canary deadline, budget readers); its hooks refuse direct calls. `OnyxRound`
   functions used only at open (round tag, set root, canary commit, commit text, round checks)
   are a linked library. Wiring: `OnyxDisputes` is deployed first; the settlement takes its
   address (immutable); the deployer then calls `OnyxDisputes.bind(settlement)` once, which
   checks `settlement.disputes() == address(this)`; `openRound` refuses until the pair is bound
   both ways. Hooks callable only by that address: `dBegin`, `dEnd`, `dPull`, `dCredit`, `dBurn`,
   `dSpend`, `dSlash`. All tokens stay in the settlement's escrow and are paid by its pull
   withdrawals. Measured (loop 51): settlement 23,362 B, extension 11,740 B, disputes 21,231 B.
   Loop 52 moved `openCanaries` (once per round) to the extension: settlement 22,910 B,
   extension 13,193 B, disputes 21,706 B.
2. Payouts that a dispute could reverse wait for finality. A unanimous shard still finalizes at
   `finalizeShard` (status `Finalized`, unrevealed bonds burned, Agree fees paid, missed tasks
   slashed), but the winning miner's reward and the revealed bonds stay in escrow until the root
   is final; then anyone calls `OnyxDisputes.release(round, shard)`. Final means status
   `Finalized` and either the challenge window (`challengeWindow` blocks from `finalizedAt`) is
   over or a case decided the shard (`closed`). `getFinalizedRoot` and `verifyRow` answer only
   for final roots, so the API (ADR-0023) never serves a disputable root on chain either.
   A shard is taken on unanimity only when a panel could still be drawn (Rust L29-F4, review
   M6-L2-F1): more than `panelSize` round members who are not a revealing miner, committed no
   verdict on the shard, and whose stake less open exposure and this shard's missed-task slash
   covers `panelSlash` (`OnyxRound.canSit`, vector `canSit`). Otherwise it goes `Pending` and a
   rerun case decides it. Rust also subtracts missed tasks on the other shards of the same
   settle call; on chain each shard settles alone, so that sum is not known. A validator later
   slashed below the line is caught at `draw`, which finds no panel and sends the case to the
   rerun (as Rust does at challenge time). The count also depends on the order shards of one
   round are finalized: a validator's exposure and round-level slashes change between calls.
   That can only make a shard go `Pending` or reach a draw that finds no panel, never make a
   wrong root final (review M6-L3).
   A shard id stays `live` from the round that offers it until it ends with no public reveal
   (`Empty`, or voided before any reveal); while live, no other round can offer it, so a
   Pending or InCase shard is never finalized and paid twice (Rust `taken`: open, held and
   burned work; M6-L3-F1). A shard a case ended is never offered again (`everFinalized`).
3. Stakes stay locked through the dispute tail. Opening a round locks every member until
   `pendingClose + disputeTail(cfg)`, where `disputeTail = 2 x challengeWindow + (maxAppeals +
   1) x (256 + panelCommitWindow + panelRevealWindow + appealWindow) + SETTLE_MARGIN`: the
   latest challenge start plus the longest case (every level drawn at the end of its 256-block
   beacon window, then the coordinator's rerun deadline), plus 64 blocks to run `settleCase`
   batches. A rerun deadline never passes `lockEnd - SETTLE_MARGIN`, so a coordinator's late
   `arbitrate` still leaves time to judge while stakes are locked (review M6-L2-F4). `_checkConfig` refuses a tail above
   `MAX_DISPUTE_TAIL` = 100,000 blocks (stricter than Rust; the contract still never opens a
   round Rust refuses). Round-level slashes (unrevealed verdicts) now apply until the lock ends,
   and every money-moving path applies them first: the first `finalizeShard`, `settleRound`,
   `voidShard` and any case (`dBegin`). This restores Rust parity (ADR-0022 item 32 dropped them
   after `pendingClose`) and fixes M6-L44-F1.
4. Two kinds of case, one game. A **rerun case** (`openRerun`, anyone, no bond) takes a
   `Pending` shard before `pendingClose`: the panel attests the rerun the contract cannot run, so
   the shard gets a reference instead of a free void (M6-F10). Its first panel is paid from the
   round budget, which `openRound` now funds with `panelSize x panelFee` per shard (each shard
   draws only on its own share, and extra rerun fees come from lost bonds: ADR-0022
   Amendment 1). A
   **challenge** (`challenge`, bond >= `minChallengeBond`) takes a `Finalized`, non-closed shard
   within its challenge window. The claimed root is committed, not posted:
   `keccak256("onyx-claim-v1" || ledger id || round || shard || challenger || root || salt)`,
   revealed with `revealClaim` before the case is decided, so a mempool copier gains nothing
   (old R-CHAL-2). Case ids use Rust's `dispute_id` layout (claim commitment in the root slot;
   zero challenger and claim for a rerun case). One case per shard at a time; a decided shard is
   closed (no correction chains, ADR-0021 Handed to M6).
5. Panels as Rust (ADR-0021 items 2, 8, 15, 19): `panelSize` members, then `2 x previous + 1`
   per appeal, the last at most `MAX_PANEL` = 63 (Rust and Solidity config checks, review
   M6-L2-F2: a draw and its judging stay inside the 15M budget at 128 validators), drawn from
   the round's members by `keccak256("onyx-panel-v1" || seed || case id || level || beacon ||
   address)` ascending, excluding the shard's committers, anyone who
   committed a verdict on it, the challenger and appellants, earlier panels of the case, and
   members whose stake minus open exposure is below `panelSlash`. The beacon is
   `blockhash(anchor)`, the block the case or appeal opened; `draw` is callable from `anchor + 1`
   for 256 blocks. Members commit `panel_commit_hash(case id, level, member, root, salt)` (Rust)
   and reveal it. A drawn member stakes `panelSlash` of exposure until the case ends.
6. Appeals: after a level's reveals close and before its appeal window ends, if that panel is not
   unanimous and appeals remain, anyone may appeal with a bond of at least
   `minChallengeBond * 2^level` and at least the next panel's fees (`appealFloor`; the floor is
   set by the config, never by the bond the challenger chose, M6-L2-F3), naming the root it backs. An appeal never reverts for lack of a panel: if the next draw
   finds too few candidates, the case goes to the rerun (item 7) and the appellant's bond is
   judged there like any funder's (M5-L49-F1, M5-L49-F3). A challenge that finds no first panel
   is also accepted and goes to the rerun (no mark-then-revert anywhere).
7. Decision after the last level's appeal window: a strict majority of that panel's members
   (missing reveals count against every root) decides root R. Reveals are tallied per root as
   they arrive, so `resolve` reads the majority in O(1) (review M6-L2-F2). No majority, no panel, or a draw
   that lapsed: **rerun required**. The coordinator, which already runs the off-chain rerun
   (`Ledger::rerun_shard`), posts R with `arbitrate` within `challengeWindow` blocks; R decides
   exactly as a majority would. After the deadline anyone calls `expire`: the case ends
   `Unresolved` (funders pay the full fees of panels that were drawn, burned, and get the rest
   back; a panel never drawn costs nothing, review M6-L2-F5; revealed
   miner bonds return; nobody is paid or slashed; the shard is never final), as Rust
   `expire_rerun`.
8. Judging against R (`settleCase`, batched over round members so the bound holds):
   - Miners as Rust `settle`: a reveal equal to R gets its bond back and the earliest such commit
     gets `minerReward`; any other non-canary reveal loses its bond; canary bonds return;
     unrevealed bonds burn (already burned on a unanimous shard).
   - Validators as Rust `settle` (`verdictRight`, pinned by the `verdictRight` vector): a right
     verdict earns `validatorFee` once per (validator, shard, revealed root); each wrong verdict
     costs `validatorSlash`; assigned and unanswered costs `missedSlash`. On a challenged
     unanimous shard, fees and missed tasks were already settled, so only wrong verdicts are
     slashed (each `validatorSlash`, more than the fee kept).
   - Panels (every level): members who revealed R earn `panelFee` from that level's funder (the
     budget for a rerun case's level 0); members who revealed another root lose `panelSlash`;
     silent members lose it only if someone on their panel revealed R (ADR-0021 item 24). This
     also applies when the rerun decided, which differs from Rust item 22 (no slashes on a
     split): R is the final arbiter, a wrong reveal is never honest, and slashing silence where
     a peer found R closes the escape in M5-L49-F1.
   - Funders: an appellant backing R keeps the rest of its bond, otherwise loses it (burned).
     Challenger: R = the old root (Upheld): loses the rest, half to the earliest correct miner,
     half burned. R different and its revealed claim = R (Overturned): keeps the rest and earns
     half of the miner bonds lost; R different otherwise (Corrected): loses the rest. The
     challenger commits its claim at `challenge` and may reveal it until the case is judged;
     `settleCase` waits `CLAIM_GRACE` (16) blocks after a decision against the old root for an
     unrevealed claim, so a `resolve` front-run cannot turn Overturned into Corrected
     (M6-L3-F2; Rust names the claimed root in the open challenge, so it has no such gap).
   - A drawn panel where nobody revealed costs its funder the full panel's fees, burned
     (Rust L30-F1, M6-L3-F3).
   - Shard: R = `no_valid_root_marker(shard)` -> status `NoValidRoot`; else `Finalized` with R
     and the binding of a reveal equal to R (zero if none: the root is public but no miner
     published its rows, so the API cannot serve it). Either way it is closed and never offered
     again.
9. Exposure caps or bond-weighted draws (L33-N7), panel canaries, and hiding earlier panels'
   reveals from appeal panels stay open (THREATS).

## Consequences
- A lone Disagree, NoValidRoot or canary no longer voids a shard for free: honest miners open a
  rerun case and the wrong verdict is slashed. A wrong unanimous root can be overturned on chain
  and the lazy miners lose their bonds, because their reward and bonds were never released.
- Honest miners wait `challengeWindow` blocks for their reward and must call `release`.
- Trust left: the coordinator's `arbitrate` decides every split or empty panel (as the settler's
  rerun does off-chain). It cannot touch a shard a panel decided by majority. Mainnet path:
  q7 (b) on-chain proofs. The Base sequencer can still bias `blockhash` beacons (ADR-0022 item 5).
- Rust and Solidity differ in four documented places: the rerun case (Rust's settler reruns a
  Pending shard directly; on chain a panel attests it first), slashing on a rerun-decided
  split (item 8), the panel pool at settlement, which on chain does not subtract missed
  tasks on other shards and depends on finalize order (item 2), and the challenger's
  committed claim with its `CLAIM_GRACE` (item 8).
