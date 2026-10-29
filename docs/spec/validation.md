# Spec: rounds, commit-reveal, validation

## Scope
How miners submit and validators check. Commit-reveal in M3, sampling, canaries and the
ledger in M4. Phase order fixed in M0: `crates/onyx-coordinator` (`RoundPhase`).

## Round
Commit -> Reveal -> Validate -> Challenge -> Finalized. Phases only move forward.

## Commit-reveal (M3)
- Commit: `onyx_core::commit_hash` = `keccak256("onyx-commit-v1" || round:u64 BE || shard ||
  binding_hash || miner:20 bytes || salt)` (ADR-0011). The binding hash covers root, leaf count,
  recipe, runtime profile, label set, snapshot, schema and rights. Binding the miner address means
  a copied commit is useless to another miner. Shared Rust/Solidity vectors: `fixtures/commit/`.
- Every commit posts a bond. No reveal by the deadline forfeits the bond, so commits are not a
  free option (R-COMMIT-1). For low-entropy outputs (booleans, small integers) the first-commit
  reward only applies when the bond exceeds the reward; otherwise all correct revealers share it.
- Reveal opens only after commits close. Reveal must match the commit and the miner.
- Reward goes to the first valid commit per shard by block order onchain (never by a coordinator
  sequence number). Later identical roots earn nothing.

## Validation (M4)
- Shards are fixed by the coordinator before the round as `ShardSpec`s and registered onchain;
  a reveal whose fields do not hash to the registered shard fields is rejected (R-SHARD-1, ADR-0011).
- One commit per (miner, round, shard).
- Every claim, label set and shard in a round is judged against the registry root pinned in its
  `ShardSpec` (R-REG-4). A registry change applies from the next round; registries are
  retrievable by root (tooling M3).
- Sampling seed (R-SEED-1, ADR-0022 item 37): `keccak256("onyx-seed-evm-v1" || round_tag ||
  beacon)`, where `beacon = blockhash(beacon_height)` and `beacon_height >= reveal_close` is fixed
  when the round opens. The round id and the validator set root are bound through the round tag
  (with the domain, the ledger id, the config and the shard ids); the reveals are fixed before the
  beacon block. If the seed is not derived within 256 blocks the round rolls over; the seed never
  falls back to zero.
- Deterrence condition, stated so parameters can be checked: `slash >= reward / p_sample`.
- Assignment is private per validator (VRF keyed by the validator) so a validator cannot compute
  which tasks are real and which are canaries (R-CANARY-1).
- Canaries (inspired by Truebit forced errors, RESEARCH.md): the coordinator mixes in tasks with
  known answers, both known-wrong and known-correct, so neither "approve all" nor "reject all"
  survives. Canaries are real bonded reveals, indistinguishable onchain. The coordinator commits to
  the hash of its canary list before the round, so it cannot frame a validator. Rules (R-CANARY-2):
  canary reveals earn no reward and never finalize (the list is opened at the end of Validate,
  before Challenge, and canary shards are dropped); each canary uses a fresh unlinked address;
  ordering for "first valid commit" comes from block order, never from the coordinator.
- Data availability (R-DA-1, R-DA-2): the full row batch must be in the CAS before reveal closes.
  An availability challenge costs a fee of at least the paid price of the rows requested plus a bond;
  the rows checked are chosen by the round seed, not by the challenger; the response goes to the
  verifiers, not to the public. So availability checks cannot be used as a free scraper or to grief.
- Validators commit-reveal their verdicts too, so they cannot copy each other.

## Interfaces
- `RoundPhase::next()`.
- M3 (ADR-0018): `onyx_coordinator::cas::{Cas, MemCas, FsCas, Kind}` (content-addressed, integrity
  on read); `onyx_coordinator::round::Round::{open, phase, commit, reveal, close}` with
  `RoundConfig { commit_close, reveal_close, min_bond, validator_set_root, canary_commit,
  beacon_height, validation }` (loop 25: `open` takes the `Ledger`); `onyx_miner::Miner::{work, salt}`.
- Availability: a shard opens only if its recipe, params, label set and cited attestations are in the
  CAS; a reveal needs its row batch in the CAS.
- M4 (ADR-0019): `onyx_coordinator::validate::{sample_seed, assigned, deterrence_holds, Verdict,
  verdict_commit_hash, verdict_message, SignedVerdict, Canary, canary_commit, Validation, Settlement,
  SettlePath, RevealStatus, Ledger, ValidationConfig}`; `onyx_coordinator::recompute::{compute,
  classify, Reference, RecomputeEnv}` (settle reruns disputed shards itself, loop 25);
  loop 26: `Validation<R: onyx_core::RuleSet>` (built-in address rules only,
  `StaticRegistry::rules_built_in`, `onyx_source_btc::BtcRules`), `Verdict::NoValidRoot`,
  `ValidationConfig { pending_close, .. }` and `validators_deterred`, `SettlePath::Voided`,
  `RevealStatus::Voided`, `RoundError::RecipeRejected`;
  loop 27: `Ledger<R: RuleSet>` (one rule set per ledger; not `Clone`), `Round::commit(now, c,
  &mut Ledger<R>)` (records the committer, `RoundError::NotOpenedHere`), `Ledger::round_tag`,
  `MIN_PENDING_GAP`, `ValidationConfig::min_canary_ppm`;
  loop 28: `Ledger::{new, id}` (random id per ledger), `round_tag(domain, ledger_id, id, cfg,
  shard_ids)` (`onyx-round-v3`), canary snapshot fixed at `Validation::new`;
  `onyx_validator::Validator::{judge, commit, salt}`; `onyx_miner::compute` (wrapper).
- M6-G4 (ADR-0022 item 37): `sample_seed(round_tag, beacon)` = `keccak256("onyx-seed-evm-v1" ‖
  round_tag ‖ beacon)`, the contract's seed; `Ledger::mirror_of_contract(chain_id, address)` uses the
  contract's ledger id, so a Rust validator gets exactly the tasks the contract judges.
- Ledger entries (M4): `Reward { round, actor, amount, reason }`, `Slash { round, actor, amount, evidence_hash }`.

## Assumptions
- One coordinator in the prototype (trusted for ordering only, not for correctness).
- Validators are paid enough per sample to rerun; cost model in docs/ops/cost-model.md.

## Open questions
- Canary rate: high enough to catch laziness, low enough to stay cheap. Set by simulation in M4.
- Beacon source on Base: future block hash vs. a VRF. Block hash is biasable by the sequencer.
- How to make canaries indistinguishable from real tasks when shards are public chain ranges.
