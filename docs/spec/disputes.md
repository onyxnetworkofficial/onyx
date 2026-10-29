# Spec: disputes by recompute

## Scope
Bonded challenges against a shard that settlement decided with a reference (unanimous root,
rerun root, or "no valid root"), decided by a randomly drawn panel of bonded validators who rerun
the shard from its pinned inputs, with appeals to larger panels. Off-chain model in M5
(`crates/onyx-coordinator/src/dispute.rs`, ADR-0021); the M6 settlement contract enforces the
parts listed under "Handed to M6". Proof-ladder levels other than "recomputed" are later work.

## Rules
1. Challenge: within `challenge_window` heights of the shard's settlement, anyone posts a signed
   challenge with bond `>= min_challenge_bond`, naming the root it claims is right (not the
   reference). It is signed over the round tag, which carries the ledger id, so it opens only in
   the ledger that settled the shard. One open dispute per (round, shard). A signed challenge
   opens once per ledger; a shard is closed after Upheld, Overturned or Corrected, or after the
   settler reran it (L33-N3). A challenge that finds no panel marks the shard "rerun required"
   (`DisputeError::RerunRequired`) and the settler's rerun decides (q13 (a)).
2. Panel: `panel_size` (odd) members of the round's bonded validator set, drawn by sorting
   candidates on `keccak256("onyx-panel-v1" ‖ seed ‖ dispute id ‖ level ‖ beacon ‖ address)`.
   Never drawn: the shard's miners, the challenger and appellants, validators who committed a
   verdict on the shard, earlier panels of the dispute. Exactly `panel_size` or no panel. A shard
   with too few such candidates is never settled on unanimity: settlement reruns it.
3. Panel members rerun the shard (`honest_panel_root`, the same code settlement uses) and commit,
   then reveal, the root, signed EIP-191 and bound to the dispute id and level.
4. Appeal: a panel that is not unanimous (a missing reveal or two roots) may be appealed after its
   reveals close and before `appeal_window` ends, with a bond `>= min_challenge_bond x 2^level`
   and `>=` the next panel's fees (`DisputeConfig::appeal_floor`; never set by the bond a
   challenger chose, M6-L2-F3), naming the root the appellant backs. The next panel has exactly
   `2 x previous + 1` members, drawn from those not yet drawn; at most `max_appeals` rounds. If
   no full panel can be drawn, the appeal is not taken (`DisputeError::RerunRequired`) and the
   dispute resolves as "no majority" (rule 5): a split panel never decides by its bare majority
   (M5-L46-F2).
5. Resolution, by strict majority of the last panel's members (missing reveals count against
   every root):
   - Upheld (majority = reference): challenger loses its bond; half to the first correct miner,
     half burned.
   - Overturned (majority = challenger's root): every entry the old reference caused (miner
     reward and slashes, verdict fees and slashes) is reversed by a compensating entry; miners and
     validators are judged again against the new root; the challenger keeps its bond and earns
     half of the new miner slashes; the rest is burned. The finalized root is corrected.
   - Corrected (majority = a third root): as Overturned, but the challenger loses its bond.
   - No majority: not final (q13 (a), loop 46). Each funder pays its panels' fees, burned;
     nobody on any panel is slashed, for silence or for a reveal (L30-F1, L31-F1). The shard is
     marked "rerun required" and the dispute waits (`DisputeOutcome::RerunRequired`). The
     settler's rerun (`Ledger::rerun_shard`) then decides it exactly as a majority would (Upheld,
     Overturned or Corrected, same payouts; appellants backing another root lose the rest).
     The rerun is due `challenge_window` heights after the mark. After that,
     `Ledger::expire_rerun` ends the dispute `Unresolved`: funders keep their bonds minus the
     burned fees, nobody else moves, and the shard is closed and never final (M5-L46-F4).
   Funding: each panel's fees (`panel_fee` to each member on the final majority) are paid out of
   the bond of whoever asked for that panel, whatever the outcome; a funder whose root is not the
   final majority also loses the rest of its bond. Upheld: half of the challenger's remaining
   bond goes to the first correct miner. Overturned: the challenger earns half of the net new
   miner slashes. Panel members who revealed another root than the final majority are slashed
   `panel_slash` (burned), capped with every validator slash at their bond across the ledger;
   silent members are slashed only on a panel where someone revealed the majority root (L33-N4).
   Nothing is minted (`paid_out <= forfeited`). Ledger entries are only ever appended.
6. Config (`DisputeConfig`) is part of the round config and its tag: fixed when the round opens.
   `panel_slash >= 9 x panel_fee`, `min_challenge_bond >= panel_fee x (panel_size + 1)`, windows
   and bonds non-zero, `max_appeals <= 4`, the last appeal's panel `(panel_size + 1) x
   2^max_appeals - 1 <= 63` (`MAX_PANEL`, M6-L2-F2), and `challenge_window > panel_commit_window
   + panel_reveal_window + appeal_window` (L33-N6). The Solidity round tag carries the same fields
   (`onyx-round-v4`, shared vectors in fixtures/settlement/vectors.json).
7. Settlement-time pool (rule 2): a validator counts if its bond, minus ledger slashes, open panel
   exposure and the slashes this settlement will certainly take (missed tasks; unrevealed
   verdicts on the first call), still covers `panel_slash`. Wrong-verdict slashes are not
   counted (L33-N2).
8. Finality (M5-L46-F4): a root is final once no dispute can change it: its challenge window is
   over with no dispute open and no rerun pending, or a dispute or rerun decided it. A pending
   rerun or an expired one is never final. No-valid-root work stays held while a rerun is
   pending (M5-L46-F1).

## Interfaces (M5)
- `DisputeConfig { challenge_window, min_challenge_bond, panel_size, panel_commit_window,
  panel_reveal_window, appeal_window, max_appeals, panel_fee, panel_slash }` in `RoundConfig`.
- `ProofLevel { Recomputed, ReObserved, SourceProven, BondedClaim, TrackRecord }`.
- `Ledger::open_dispute(now, &SignedChallenge, beacon) -> dispute id`
- `Ledger::commit_panel_vote(now, &SignedPanelVote)`, `reveal_panel_vote(now, id, member, root, salt)`
- `Ledger::appeal(now, &SignedAppeal, beacon)`, `Ledger::resolve_dispute(now, id) -> DisputeOutcome`
- `Ledger::rerun_shard(round, shard, &RecomputeEnv, &AttestCache) -> DisputeOutcome` for a shard
  marked "rerun required" (`Ledger::rerun_required()`): a challenge found no panel (L31-F3), or a
  dispute ended with no majority (q13 (a)); a waiting dispute is decided by the rerun root.
  `ShardRecord::path()` (a confirmed unanimous root becomes `Recomputed`, L33-N3).
  `Ledger::expire_rerun(now, round, shard) -> DisputeOutcome::Unresolved` after the deadline.
  `Ledger::finality(now, round, shard) -> Finality` and `Ledger::final_root(now, work)`: what
  readers settle on (`finalized()` also lists roots still disputable). `Ledger::panel_exposure(v)`: `panel_slash`
  staked on open panels (L31-F2). `ShardRecord::dispute_config()`.
- `honest_panel_root::<R>(&RecomputeEnv, &ShardRecord, &AttestCache)` (None unless the registry's
  rules are built in for `R`); `Dispute::paid_out()` / `forfeited()`; client helpers on
  `onyx_validator::Validator`: `challenge`, `panel_vote`, `panel_salt`, `appeal`.

## On chain (M6 loop 2, ADR-0024)
`contracts/src/OnyxDisputes.sol` runs these rules on Base Sepolia, with these differences:
- Done on chain: beacon from `blockhash` after the case or appeal opens; challenger's claim
  committed and revealed after the panel (no mempool copying); bonds escrowed in the settlement;
  block-height clock; the coordinator posts the rerun that decides a split (`arbitrate`), or the
  case expires `Unresolved`.
- A `Pending` shard (no unanimous reference) gets a **rerun case**: a panel attests the rerun the
  contract cannot run (Rust's settler reruns it directly). Its first panel is paid by the round.
- An appeal or challenge that finds no panel is taken and goes to the rerun (never mark then
  revert, M5-L49-F3). Every panel is judged against the decided root, also after a rerun: wrong
  reveals and silence where a peer found the root are slashed (Rust item 22 slashes nobody on a
  split; on chain this closes M5-L49-F1).
- A challenged unanimous shard keeps the verdict fees already paid; each wrong Agree is slashed
  `validatorSlash` (Rust reverses the fee and slashes).
- Payouts a dispute can reverse (miner reward, revealed bonds) wait until the root is final.

Still open (THREATS "M6 loop 2"): exposure caps or bond-weighted draws (L33-N7); panel canaries;
hiding earlier panels' reveals from appeal panels (lazy copying); data availability for panel
reruns; correction chains (a decided shard is closed).

## Assumptions
- At most 90% of disputes are frivolous (sets `LAZY_PANEL_RATIO`).
- Voided shards have no reference and pay nobody, so they are not disputable.
- A whole-shard rerun fits in a panel commit window. If not, add bisection over trace checkpoints
  (Arbitrum BoLD, Optimism Cannon; RESEARCH.md "M5").
- Honest members are a majority of any drawn panel with high probability. A bribed majority of
  the last panel decides any outcome (a rerun, or a correction to any root; L30-F5), bounded only
  by drawing and bond escalation.
- Validators with less bond left than `panel_slash`, after ledger slashes and what open panels
  stake, are never drawn (L30-F3, L31-F2); validator slashes are capped at apply time against
  the ledger total (L30-F2).
- The settler can rerun a shard marked "rerun required"; until it does, the shard is closed to
  new challenges and its root is not final (`Finality::RerunPending`); past the deadline it is
  `Unresolved` (L31-F3, M5-L46-F4).
- Subjective claims (labels) do not use panels; they feed calibration scoring (ADR-0016).

## Open questions
- Panel sizes and windows for real value (testnet: 3, then 7; 50-height windows).
- Whether members whose root differs from the rerun that decided a split should be slashed
  (loop 46: no; a split slashes nobody, so honest members who cannot rerun stay safe).
- Proof levels 2-5 (re-observed, source-proven, bonded claim, track record): evidence formats.
