# Spec: proof ladder slice 2 (ROADMAP C64, north-star loop NL13; ADR-0048)

Status: **Built 2026-10-28** (ADR-0048 amendment 2), except a node route serving the cross-lane
form: no testnet fact has two lanes yet, so `compose`, `to_json_v2` and `check_served_v2` are built
and tested on fixtures and no route serves `v: 2`. Open question 1 is decided: warn on the site,
and `onyx-check` does not refuse on a contradiction. Drafted 2026-10-27 (lane B); extends slice 1
(commit `a6d7adf`).

## Scope

Slice 1 serves a one-step ladder per value: each node serves its own lane's step. C64 asks for
four things:

1. **Cross-lane rises.** A node that holds a level-3 and a level-2 answer of one fact (same
   `fact_key(metric, subject, as_of)`, no lane in it) serves both steps in one ladder, with `at`
   lined up across lanes; the client checks each step through its own lane's check (THREATS NS5-1).
2. **Contradictions.** A weaker value names a stronger answer of the same fact with another value
   (THREATS NS5-5): not a rise of this value, a warning on it.
3. **A ladder on level-3 answers** (`/v1/signed`), once the browser's level-3 client takes it.
4. **`onyx-check` row levels:** the agent tool binds a row's `proofLevel` to the caller's own
   `verifyRowLeveled` (THREATS NS5-6).

## Interfaces

- **`crates/onyx-core/src/ladder.rs`**: `Ladder::of` already takes proofs of any lane; add
  `Contradiction { level, evidence, at, value: Hash32 }` (the stronger answer's value hash, never
  the value) and `Ladder::contradicted_by(&[Contradiction])`. `at` for every lane is the timestamp
  of the finalized block of the step's chain record (`ShardAttested`, `ObservationRecorded`,
  `ReportRecorded`), never a server clock (fixes NS5-7's server hint for rows).
- **`crates/onyx-api/src/ladder.rs`**: `to_json` emits all steps plus `contradictions`;
  `check_served` takes a per-lane checker map: each step stronger than the client's own lane must
  be checked by that lane's client check (`client::verify_observation` for level 2, the signed
  check of `signed_serve` for level 3, `verify_response` plus `verifyRowLeveled` for rows), or the
  ladder is refused (a rise no check ran on, as in slice 1).
- **`crates/onyx-api/src/observations.rs` and `signed_serve.rs`**: a node with both stores looks
  up the other lane by `fact_key` and adds that step; `signed_serve.rs` adds `ladder` to level-3
  answers (`ladder::signed_json` exists).
- **`apps/web/verify`** (the browser's Rust checks) and `apps/web/src/lib/ladder.ts`: the level-3
  client takes the ladder; the page shows a contradiction as "a stronger answer says otherwise".
- **`crates/onyx-check/src/lib.rs`**: the row kind calls `verifyRowLeveled` on the caller's RPC
  and refuses a row whose stated level is above it.

## Rules

1. **R-LAD2-1.** A step from another lane is trusted only after that lane's own check passed on
   the client; otherwise the ladder is refused, never trimmed silently.
2. **R-LAD2-2.** `at` of every step is a chain time; steps stay ordered by it; a step whose chain
   record is not final is not served.
3. **R-LAD2-3.** A contradiction never lowers or raises the value's level; it is shown next to it
   and a client can choose to refuse values with one.
4. **R-LAD2-4.** A contradiction carries the stronger answer's value hash only, so a sealed public
   build seals it like the ladder (SIG-R1-F2).

## Review of slice 1 to fix in this slice

The lane B hand review of commit `a6d7adf` (THREATS NS5-8..14) adds work to C64: clients keep the
last accepted ladder (NS5-9); a node's ladder is append-only so a late, earlier, stronger proof is
not a false fall (NS5-10); cross-lane steps go in a versioned field so v1 clients keep working
(NS5-11); one canonical value form before `value_hash` (NS5-12); a badge says "stated" unless a
check backs it (NS5-13); export and settlement levels bound by `onyx-check` (NS5-14). NS5-8 (the
on-chain result hash undoes the public seal) waits for the owner's policy choice.

## Tests that prove the Done-when (C64)

- `onyx-core` `ladder::tests::a_level_3_step_joins_a_level_2_ladder_of_the_same_fact`,
  `::steps_order_by_chain_time`, `::a_contradiction_never_changes_the_level`.
- `onyx-api` `tests::a_node_with_both_lanes_serves_both_steps`,
  `tests::a_cross_lane_step_without_its_lane_check_is_refused`,
  `tests::a_weaker_value_names_the_stronger_contradiction`, `tests::signed_answers_carry_a_ladder`.
- `onyx-check` `a_row_level_above_verify_row_leveled_is_refused`.
- e2e "a contradiction is shown next to the value".

## Assumptions

- A fact can have answers in two lanes on testnet: a level-3 signed fact and a level-2 observation
  of the same metric and day need a source that both signs and is observable; none exists today,
  so the cross-lane test uses fixtures, and the live case waits for such a source.
- The browser's level-3 client (C61) can carry one more field without a format change.

## Open questions

1. Should a client refuse a value with a contradiction by default, or only warn? Leaning: warn on
   the site, refuse in `onyx-check` unless `--allow-contradicted`.
2. Two contradictions at different levels: show the strongest only, or all?
