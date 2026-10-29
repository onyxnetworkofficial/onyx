# ADR-0021: Disputes by a drawn panel of bonded validators that rerun the shard

## Status
Accepted, 2026-10-09 (M5 loop 1, project loop 29). The owner chose q7 (a) ("Go"). Implemented in
`crates/onyx-coordinator/src/dispute.rs`; tests on real blocks in `adapters/source-btc/tests/dispute.rs`.
Amended loop 49 (items 29-32) after the M5 merge-gate review (M5-L46-F1..F4). On-chain port:
ADR-0024 (M6 loop 2).

## Context
M4 settles a shard by a unanimous undisputed root or by a rerun (ADR-0019). Nothing lets anyone
correct a settled root afterwards: a lazy majority that went unchallenged at validation (blind
Agrees on unanimous lazy reveals) finalizes a wrong root. Research (RESEARCH.md "M5") compared
interactive fault proofs (Truebit, Arbitrum BoLD, Optimism Cannon), optimistic oracles (UMA) and
juries (Kleros). A one-step on-chain WASM proof must match wasmi exactly and is costly to build;
a token vote decides by stake, not by recompute. The project's north star: disputes settle by
deterministic recompute, never by vote.

## Decision
1. A bonded challenge against a settled shard, within a window after settlement, naming the
   claimed root; signed over the round tag (binds the ledger id, L27-F1).
2. A panel of `panel_size` bonded validators drawn by a keyed hash, excluding everyone with a
   stake in the outcome (miners, challenger, appellants, verdict givers, earlier panels). Members
   rerun the shard with the code settlement uses and commit-reveal the root. The panel is a
   recompute, not an opinion: every honest member gets the same root.
3. Strict majority of the last panel decides. Overturns are recorded as compensating entries
   (append-only ledger), the finalized root is corrected, and miners and validators are judged
   again against the new root.
4. Not unanimous: appeal with a doubled bond to a `2k + 1` panel, at most `max_appeals` times. No
   majority at the end: the settlement stands and bonds are returned (testnet backstop).
5. Dispute config is in the round config and tag. Only `ProofLevel::Recomputed` claims are
   disputable now; other levels return `Unsupported`.
6. Handed to M6 (docs/spec/disputes.md): beacon from chain, challenge commit-reveal against
   mempool copying, escrowed bonds, a final arbiter, block-height clock (every window here uses a
   caller-supplied `now`; a coordinator could apply a settlement late and close the challenge
   window before anyone sees it), panel canaries, hiding earlier panels' reveals from appeal
   panels, and data availability for panel reruns (a panel where nobody can fetch the inputs ends
   in Stands; L30-F1).

Loop 30 amendment (after the loop-29 review, L29-F1..F10):
7. A signed challenge opens once per ledger (consumed key). A shard is closed to disputes after
   Upheld, Overturned or Corrected; after Stands a new challenge may open.
8. Panels are exactly `panel_size` or `2k + 1` members. Too few candidates: no first panel
   (`NoPanel`) or no appeal (`NoAppeal`, the current panel decides; superseded by item 29).
9. Every panel is funded by the bond of whoever asked for it: its majority members' fees come out
   of that bond whatever the outcome; a funder whose root is not the final majority also loses
   the rest. Bonds must cover the panel's fees; `min_challenge_bond >= panel_fee x (panel_size +
   1)`, so doubling appeal bonds fund every larger panel. Panel slashes are burned. Nothing is
   minted: each dispute records `paid_out <= forfeited`.
10. Settlement takes a shard on unanimity only if more than `panel_size` bonded validators outside
    its miners and verdict-givers exist; otherwise it reruns the shard. A shard that could not be
    disputed is never trusted on agreement alone.
11. Validator slash caps are against the ledger's net history per address, across validations,
    rejudges and panels.
12. `panel_slash >= 9 x panel_fee` (a lazy member who always reveals the reference loses money if
    at most 90% of disputes are frivolous). The challenger's reward is half of the net new miner
    slashes; the no-valid-root marker never earns the upheld bonus. Panel reruns refuse registries
    whose rules are not built in.

Loop 31 amendment (after the loop-30 review, L30-F1..F6):
13. Stands is never free: every funder pays its panels' full fees, burned (no member earns them),
    and keeps the rest of its bond. A panel where nobody revealed (the shard could not be rerun:
    inputs gone, rerun too slow, foreign rules) slashes nobody and its funder's fees are burned.
14. Validator slashes are capped when `Ledger::apply` records them, against the live ledger-wide
    total; the shard records' reversible effects are rebuilt from the capped entries. The
    `Validation::new` snapshot is only an early estimate.
15. A validator whose remaining bond cannot cover `panel_slash` is never drawn and does not count
    toward the rule-10 pool; the pool also subtracts the missed-task slash the same settlement is
    about to take from an assigned, silent member.
16. Work settled with no valid root stays taken until its challenge window closes and no dispute
    on it is open; a dispute never rewrites a finalization owned by another round.
17. Rule 10 cannot be abused: shrinking the pool only forces the settler to rerun the shard,
    never a trusted wrong root. At `rate_ppm` = 100% with diligent validators every shard is rerun
    and none is disputable, by design.

Loop 33 amendment (after the loop-31 review, L31-F1..F5):
18. Stands judges nobody: without a final majority no panel member is slashed, for silence or for
    a reveal, so one sybil reveal cannot turn honest silence (members who cannot rerun) into
    slashes. Funders still pay their panels' fees, burned (item 13).
19. A member on an open panel stakes `panel_slash` until the dispute resolves (`panel_exposure`
    in the ledger). Eligibility for a panel, and for the settlement-time pool of rule 10, is
    `bond - ledger slashes - open exposure >= panel_slash`.
20. Rule 10's pool is counted against everything the settlement can still slash: one
    `validator_slash` per verdict committed on an unsettled shard and one `missed_slash` per
    unsettled shard where the validator is assigned and silent. A shortfall that appears later
    (other rounds, other disputes) is caught at challenge time: a challenge on a shard settled by
    unanimity that finds no panel marks the shard "rerun required" (`DisputeError::RerunRequired`)
    and the settler reruns it with `Ledger::rerun_shard`, which corrects the reference like a
    dispute (compensating entries, rejudge) and pays nobody. A machine that cannot rerun returns
    `Undecided` and the mark stays.
21. A correction to the no-valid-root marker holds the work until the shard's challenge window
    ends (like settling with no valid root, item 16); a correction to a valid root clears any
    hold or burn on the work.

Loop 46 amendment (PLAN q13 decided (a) 2026-10-10; fixes L33-N1..N7):
22. Stands is removed as a final answer. A last panel with no strict majority, or a challenge
    that finds no panel (on any shard, not only unanimous ones), marks the shard "rerun
    required". Funders' panel fees burn as before; nobody is slashed for a split. The dispute
    waits (`DisputeOutcome::RerunRequired`, the only non-final outcome) and the settler's rerun
    (`Ledger::rerun_shard`) decides it as a majority would: Upheld, Overturned or Corrected, with
    the same payouts and losses (appellants backing another root lose the rest of their bond).
    The panel is the fast path; the rerun is the final arbiter. This closes L33-N1: a bribed
    minority (plus honest members who cannot rerun) can force a split but not keep a wrong root.
23. A rerun closes the shard to disputes; a rerun that confirms a unanimous root records it as
    `SettlePath::Recomputed`. A sham challenge can force at most one rerun per shard (L33-N3).
24. Silence on a panel is slashed only if someone on that panel revealed the final majority root
    (L33-N4); one sybil's junk reveal no longer makes honest silence slashable.
25. Rule 10's pool counts only slashes the settlement will certainly take: one `missed_slash` per
    unsettled shard where the validator is assigned and committed nothing, and on the first call
    one `validator_slash` per verdict committed and never revealed. Wrong-verdict slashes depend
    on the reference and are not counted (L33-N2; the worst case dropped every active validator
    and sent every shard to a rerun). Commits are read once into per-shard verdict-givers
    (L33-N5). A later shortfall is still caught at challenge time (item 22).
26. `DisputeConfig::is_valid` requires `challenge_window > panel_commit_window +
    panel_reveal_window + appeal_window`, so a challenge opened in the window can finish one full
    panel before the window ends (L33-N6).
27. Sham disputes tie up small validators' exposure (item 19), which skews later draws toward
    large bonds (L33-N7). Accepted for the testnet; TODO (M6): cap exposure per validator or
    weight draws by remaining bond.
28. The Solidity round tag (`OnyxRound.roundTag`) now carries the dispute fields
    (`onyx-round-v4`), matching Rust; shared vectors regenerated (ADR-0022 handoff item).
29. Loop 49 (M5-L46-F2): an appeal on a split panel that finds no full larger panel is not taken
    (`DisputeError::RerunRequired`); the dispute is marked `rerun_forced`, can be resolved at
    once, and resolves `RerunRequired` like a split final panel. A non-unanimous majority never
    decides because the pool was too small (this replaces item 8's "the current panel decides").
    Residual: such an appeal costs its signer nothing (THREATS loop 49).
30. Loop 49 (M5-L46-F4): a rerun has a deadline, `challenge_window` heights after the shard is
    marked (no new config field, so the round tag and shared vectors are unchanged). After it,
    `Ledger::expire_rerun` ends the waiting dispute `Unresolved`: funders lose only the panel
    fees already burned, nobody else is paid or slashed, and the shard is closed and never final.
31. Loop 49 (M5-L46-F4): `Ledger::finality(now, round, shard)` -> `Unknown`, `Disputable`
    (window open or dispute open), `RerunPending { deadline }`, `Unresolved` or `Final`;
    `Ledger::final_root(now, work)` returns a root only when `Final`. `finalized()` still lists
    roots a dispute or rerun may change; anything that pays or settles on a root reads `Final`.
32. Loop 49 (M5-L46-F1): held no-valid-root work (item 21's L30-F4 hold) stays held while its
    shard waits for a rerun, so another round cannot finalize it before the rerun does.

## Consequences
- Wrong settled roots can be corrected after settlement; a frivolous challenger loses its bond,
  half to the miner it attacked.
- The round tag changes (`onyx-round-v4`) to carry the dispute config.
- Residual: a bribed majority of the last panel decides the outcome: it can force a rerun, or
  force Upheld, Overturned or Corrected to any root it names (honest miners then lose their bonds
  and the finalized root is rewritten). Bounded only by random draws from the bonded set and
  doubling bonds. The mainnet path is an on-chain proof of the rerun (q7 (b)).
- Trust left after loop 46: the settler's rerun decides every split. In M6 the contract cannot
  run WASM, so the settler posts the rerun result, and that post must itself be challengeable.
- Settlement now keeps a per-shard record (reveals, verdicts, reference-dependent entries) in the
  ledger; ledger memory grows with settled shards (fine for the prototype).
