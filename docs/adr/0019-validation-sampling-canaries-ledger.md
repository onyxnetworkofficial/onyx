# ADR-0019: Validation by sampled recompute, a bonded validator set, canaries, and a ledger

## Status
Accepted, 2026-10-08 (M4 loop 22). Rewritten loop 23 after the L22 review (2 Critical, 4 High);
amended loop 24 after the L23 review (items 1, 2, 5, 6, 7, 8); amended loop 25 after the L24
review and the owner's q11 choice (a) (items 9-12 below).
Implemented in `crates/onyx-coordinator/src/{validate.rs, recompute.rs, round.rs}` and
`crates/onyx-validator`. End to end on real blocks: `adapters/source-btc/tests/validate.rs`.

## Context
M3 accepts any revealed root whose rows are stored; a lazy miner with an invented root passes it.
Validators must recompute, but rerunning every shard is costly. The loop-22 design let any key act
as a validator (a lazy miner ground keys until one was assigned to its own reveal), let the
coordinator label canaries "correct" or "wrong" (framing honest validators, stealing rewards),
judged each reveal on its own (two lazy identities finalized an invented root), and let a free
Disagree divert the reward. North star: disputes settle by deterministic recompute, never by vote.

## Decision
1. Validators are a bonded registry (`ValidatorSet`). Its root, the canary commitment and the beacon
   height are fixed in the round config (and signed round tag) when the round opens, and
   `Validation::new(&round, ..)` refuses another set (L23-F2). Only members get tasks; nobody is
   assigned their own reveal; total slashes per validator are capped at its bond in a validation
   (L23-F8; bond locking across rounds is M6).
2. Seed (computed inside `Validation::new`): `keccak256("onyx-seed-v2" ‖ round ‖ round_tag ‖
   validator_set_root ‖ (shard, root, miner) triples sorted ascending ‖ beacon)`. Superseded by
   ADR-0022 item 37 (M6-G4): `keccak256("onyx-seed-evm-v1" ‖ round_tag ‖ beacon)`, the
   settlement contract's form.
3. Assignment: member `v` checks reveal `(shard, miner)` iff the first 8 bytes of
   `keccak256(seed ‖ v ‖ shard ‖ miner)` fall below `rate_ppm / 1e6` (exact integers). Public; a
   private VRF (RFC 9381) is later work.
4. Verdicts (Agree / Disagree with a different root / Abstain) are committed then revealed, signed
   over a message bound to the seed. A Disagree naming the revealed root is refused.
5. Truth is the shard's reference root. A shard whose reveals all share one root, that no verdict
   disputes and that holds no canary takes that root, and nothing else is consulted. Every other
   shard is settled against `recompute(shard)`, a deterministic rerun of its pinned inputs
   (`onyx_miner::compute`); no caller can name a reference (L23-F1). Without recompute the shard
   stays pending and a later `settle` call settles it (each shard once). The settler's recompute is
   itself checkable by anyone; M5 adds a bonded challenge against a wrong settlement.
6. Canaries: committed before the round as a strictly sorted list of `(shard, miner, root)`; each
   must match an actual reveal and come from an address with no ledger history (checked once, on the
   first settlement, against the ledger itself). Canaries earn
   nothing and never finalize. Validators are judged on canaries like on any reveal: against the
   reference, never against the coordinator's word.
7. Settlement with a reference: the first correct non-canary reveal (commit order) earns the miner
   reward and finalizes the work; every wrong reveal loses its bond; a verdict that matches the
   reference earns the fee once per (validator, shard, root); a wrong verdict is slashed; an assigned
   validator who never answers a shard (no verdict, or only Abstain) is slashed `missed_slash` once per
   shard; canary fees are paid at most once per (validator, shard); forfeits and unrevealed verdicts
   are slashed once.
8. Ledger: entries, finalized work (`work_id -> (round, root)`), open work, applied (round, shard)
   pairs (applying twice changes nothing), and known miner addresses (canaries must be fresh). Work
   ids and miners come from the round, through the settlement. Every settled shard leaves `open`, so
   work without a correct reveal can be offered again (L23-F4).

## Loop 25 amendment (L24, q11 option a)
9. Settlement reruns, nobody supplies a reference (L24-F1). `settle` takes a `RecomputeEnv`
   (concrete `StaticRegistry`, CAS, canonical `Host`, a source of snapshot bytes) and reruns every
   shard that is not unanimous, undisputed and canary-free from the spec the round pinned
   (`onyx_coordinator::recompute`, the same code miners and validators run). Inputs are checked
   against their pinned hashes (CAS on read, `content_hash` for source bytes, registry root), so a
   caller can only withhold them: the shard stays pending. Only deterministic outcomes decide:
   rows, a recipe failure (root of the canonical failure leaf), or no valid root (the canonical
   engine cannot load the recipe, or rows too large; every reveal is wrong and validators are not
   judged). Validators classify the same way (`Validator::judge`).
10. The validation rules are part of the round (L24-F3). `ValidationConfig` (windows, rate, reward,
    fee, slashes, canary deadline) is in `RoundConfig` and the round tag ("onyx-round-v2").
    `Round::open` refuses a rate of 0, a rate that does not deter (`deterrence_holds(min_bond,
    miner_reward, rate)`) and windows out of order. One validation per round.
11. One ledger per round (L24-F2, F5, F7). `Round::open(.., &mut Ledger)` refuses a used round id,
    marks the work open, records the round tag and copies the known miners into the round (canary
    freshness uses that copy). `Validation::new(.., &mut Ledger)` records the seed once.
    `Settlement` is sealed (private fields, built only by `settle`) and `Ledger::apply` accepts it
    only when its round, tag and seed are the ones recorded in that ledger. Ledger fields are
    private.
12. Canary deadline (L24-F4): `settle` must get the opened list before `canary_open_close`; from
    then on `None` settles without canaries, and the coordinator's wrong reveals are slashed like
    anyone's.
Loop 26 amendment (L25 review):
13. Recipes are checked by the canonical engine when a round opens (`Host::prepare`,
    `RoundError::RecipeRejected`), so a shard nobody can compute never opens. `Verdict::NoValidRoot`
    (a validator found that no root can be correct) is a dispute; on the NoValidRoot path it is the
    only right verdict and validators are judged (L25-F1).
14. Address rules are a closed set fixed at compile time (L25-F2). The registry root binds a rule
    only by id and vector results, so the same root can hide other code. `onyx_core::RuleSet` names,
    per rule id, the Rust type the binary trusts; `Validation<R: RuleSet>` reruns a shard only if
    `StaticRegistry::rules_built_in::<R>()` holds, else the shard stays pending. Built-in rule
    types are stateless. `onyx_source_btc::BtcRules` is the Bitcoin set. The choice is the
    binary's, at compile time; no runtime value can bring rule code into settlement.
15. The canary freshness snapshot lives in the ledger's record of the round (taken at
    `Round::open`), so a copy of the round opened elsewhere cannot bring an empty one (L25-F4).
    `Validation::new` also records the round's committers and validators as known, and canary
    freshness uses the snapshot, everything known when validation starts, and the validator set
    (L25-F6).
16. `pending_close` (in the config and tag, at or after `canary_open_close`): a shard still
    undecided then is voided: no rewards, no slashes, work released, `SettlePath::Voided` (L25-F5).
17. Validator deterrence is checked at open (`ValidationConfig::validators_deterred`, L25-F8), and a
    committed but unrevealed verdict is slashed once, not also as a missed task (L25-F9).
Loop 27 amendment (L26 review):
18. A miner is recorded in the ledger as active in a round the moment its commit is accepted
    (`Round::commit(.., &mut Ledger<R>)`, which also refuses a ledger that did not open the round).
    `Validation::new` treats every address active in another round as known, and `Ledger::apply`
    re-checks the settlement's canaries: one active in another round, or a validator, makes the
    settlement unappliable. The validation order of rounds no longer matters (L26-F1).
19. Voided work whose reveals are public is burned: it stays in `Ledger::taken` and is never offered
    again, so a copier cannot take its honest root. `pending_close >= canary_open_close +
    MIN_PENDING_GAP` (100 heights) at `Round::open` (L26-F2).
20. The ledger is bound to one rule set: `Ledger<R: RuleSet>`, `Validation<R>` takes only a
    `Ledger<R>` (a compile error otherwise), and every settlement records `TypeId::of::<R>()`, which
    `apply` checks (L26-F3). `Ledger` is not `Clone`. The trust that remains is the published
    settlement binary; in M6 independent settlers run it and their results must agree.
21. `ValidationConfig::min_canary_ppm` (in the tag): validator deterrence is computed at the declared
    canary density, since a blind Agree is caught only on a wrong reveal (L26-F4). It is declared,
    not enforced: a coordinator that plants fewer canaries, or opens none, is caught only once it has
    a bond (L25-F7, M6).
Remaining after loop 27: the beacon is a parameter until M6 reads it from the chain (L25-F3, L24-F3
PARTIAL); the coordinator can choose after the verdicts whether to open its canaries and how many to
plant (L25-F7, L26-F4 PARTIAL) and can void a disputed shard by withholding inputs (L25-F5 residual;
the work is burned, so this now costs it the work too), all until a coordinator bond in M6; the
coordinator orders calls, so "before" between rounds is its order until the chain orders them (M6);
a free Disagree costs the settler a rerun (L24-F6, M5 deposit); blind Agree on unanimous shards of
known miners is not caught (L24-F8, M5).

Loop 28 amendment (L27 review; q12 option a, 2026-10-09: M4 off-chain is frozen here):
22. Every ledger has a random id, drawn at construction and never taken from a caller, and it is in
    every round tag (`onyx-round-v3`). The same round opened in another ledger has another tag, so
    miners' signed commits do not replay into it and neither copy validates or settles against the
    other ledger (L27-F1).
23. Canary freshness for `apply` is the snapshot stored when the round's validation starts (known at
    open, committers of other rounds, validators seen, known miners), not live ledger state, so later
    activity by a canary key is no veto (L27-F2). Ledger call order, not caller-supplied heights,
    decides "before".
24. A voided shard burns its work only if a non-canary reveal made a root public; a canary-only
    shard is released (L27-F3).
25. A rerun reference is finalized even when no reveal matched it (no reward): the root is public, so
    the work is never offered again (L27-F4).

Handed to M6 (the settlement contract is the single canonical ledger and orders all calls; these
cannot be closed by an off-chain library whose caller controls every call):
- cross-round call ordering (which round's commit or validation happened "first");
- the sampling beacon, read from the chain at `beacon_height` (L25-F3, L24-F3 PARTIAL);
- a coordinator bond, slashed for an unopened canary list (L25-F7) and for under-planting canaries
  (L26-F4 PARTIAL, `min_canary_ppm`) and deliberate voiding (L25-F5 residual);
- ledger identity: the contract address replaces the in-process random id;
- the clock: every phase check uses block height, never a caller-supplied `now`, so
  `MIN_PENDING_GAP` and all windows mean something (L28-F2);
- validator selection: `validator_set_root` is a snapshot of the on-chain bonded registry, with
  bonds locked across rounds, not a set the coordinator names (L28-F3);
- who settles and with which inputs: a withheld source or a registry that fails the built-in rule
  check counts as deliberate voiding and is covered by the coordinator bond, or is routed to an M5
  challenge (L28-F4);
- funds: miner and validator bonds and rewards are escrowed and funded on-chain; the off-chain
  ledger is bookkeeping and balances may go negative (L28-F5);
- honest outages: a source outage at `pending_close` burns work with nobody at fault; remedy: pay
  the earliest reveal matching a later finalized root (L28-F1); canary-only voids where the canary
  revealed the correct root are covered by the coordinator bond (L28-F6).
Items 8, 16 and 18 are superseded where they say work is "released" or canaries are re-checked live
at apply: see items 23-25.
Root selection (a unanimous undisputed root or a deterministic rerun of pinned inputs decides) holds
off-chain and is unchanged by M6.

## Consequences
- A sybil key cannot vote; a lazy miner loses its bond whenever its root is judged wrong; a
  griefing Disagree cannot take the first correct miner's reward and costs the griefer its slash.
- A validator that copies the plurality root is slashed when the coordinator makes a wrong root the
  plurality with canaries. Silent assigned validators pay `missed_slash`.
- Pending (disputed) shards hold rewards and slashes until M5 supplies the reference.
- Remaining: assignments are public once the seed is known (bribery targeting); the beacon can be
  biased by the Base sequencer; a brand-new honest miner's address could be pre-listed as a canary
  if the coordinator predicts it (canary lists are committed before the round, so the address must
  be known in advance).
