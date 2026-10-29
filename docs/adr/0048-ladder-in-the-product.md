# ADR-0048: The ladder in the product: every answer states its level, and a level only rises

## Status
Accepted, 2026-10-27 (docs/NORTH-STAR.md NS5, pillar 4 "Values climb the ladder";
docs/north-star-loop.md NL2). First slice implemented by `onyx_core::ladder`, `onyx_api::ladder`,
the `ladder` and `proofLevel` fields of the row, metric, export, settlement and observation
answers, the client checks `client::verify_observation` and `client::check_row_ladder`, the
`level2-check` examples, and the website's `LevelBadge` and `src/lib/ladder.ts`.

## Context
The proof ladder exists as protocol data (ADR-0027, docs/spec/proof-ladder.md): a claim's level is
derived from its method, a shard's level is attested once in `OnyxRegistry` (ADR-0030), a level-2
fact is recorded once, a level-3 report is settled by `OnyxSignedBook`. What the product lacked:
1. Row, metric, export and settlement answers carried no level at all; the observation and signed
   answers carried a constant.
2. Nothing said how a value's level changes when stronger proof of it arrives, nor how a client
   tells a real rise from an operator who lifts a number.
3. The track record (ADR-0036) compares a source's values with later stronger answers, but by
   design never changes a level (R-TR-9) and is fenced off from level code
   (`scripts/check-boundaries.sh`), so it cannot be the ladder.

## Decision
1. **A value's level is the strongest public proof of exactly that value.** Proofs are records of
   their own lanes (a finalized shard and its attestation, a recorded level-2 observation, a
   settled level-3 report). They only accumulate, so the level only rises. Nothing stores a level
   that a proof does not back.
2. **The ladder is the history of rises.** `onyx_core::ladder::Ladder { value, steps }`: `value`
   is `keccak256("onyx-ladder-value-v1" ‖ key ‖ value hash)`, so the ladder never carries the value
   itself; `key` is a lane-free fact key (`keccak256("onyx-ladder-fact-v1" ‖ borsh(metric,
   subject, as_of))`, no source and no lane, so a level-2 and a level-3 answer of one fact meet)
   or a shard row's key (`onyx-ladder-row-v1`). Each `Step { level, evidence, at }` is strictly
   stronger and no older than the one before; at most one step per level. `Ladder::of` builds it
   from any set of proofs (time order, each stronger proof a rise, the rest change nothing).
   A stronger answer with another value is not a rise of this value (TODO NS5-5).
3. **Every value answer carries `proofLevel` and `ladder`.** The ladder's top is the answer's
   level. Each step names its evidence (the lane check's input: a shard root, a level-2 result
   hash, a level-3 report hash) and the chain record that made it public
   (`OnyxRegistry.ShardAttested`, `OnyxRegistry.ObservationRecorded`,
   `OnyxSignedBook.ReportRecorded`). The OpenAPI file describes both (ADR-0045).
4. **A client trusts a step only through its lane's own check.** `onyx_api::ladder::check_served`
   refuses a ladder of another value, one whose top is not the stated level (a forged level), one
   without the client's own verified step, one with a step stronger than that (a rise no check
   ran on), and one that drops or rewrites a step of the ladder the client accepted before (a
   fall). A node in this slice serves only its own lane's step; a client run without a ladder
   field (an older node) uses its own step.
5. **Rows are bound to the registry's level by the client.** A row's `proofLevel` is the
   operator's catalog level (floored by the registry when pinned); `client::check_row_ladder`
   compares it with the level the client's own `verifyRowLeveled` call returned.
6. **The site shows the level as a badge everywhere a dataset is listed, and the history where a
   checked ladder exists** (the two Base Sepolia level-2 values, as `level2-check` accepted them
   from the hosted API). The page refuses a ladder that falls or states a level its steps do not
   reach. A public build seals the ladder like the round (no value key, no evidence), because both
   are unsalted functions of a value the paid API sells (SIG-R1-F2).

## Consequences
- The level a client acts on is never the operator's word alone: a lifted level, a forged step or
  a fall is refused by the client, in Rust and on the page.
- Rises across lanes are defined but not yet served: no node holds two lanes of one fact, and a
  client has no cross-lane check yet (slice 2, TODO NS5-1). Today every served ladder has one step.
- Lanes keep their own time units for `at` (a round's settle time, a report's acceptance, a row's
  finalization hint); a cross-lane rise must line them up first (TODO NS5-1).
- The track record stays outside: it never reads or writes a ladder (R-TR-9 unchanged); only its
  value rule `level2_value` now calls the shared `onyx_core::ladder::observed_value`.
- Level-3 answers keep their constant `level: 3` until the browser's level-3 client takes the
  field (TODO NS5-3); level-5 track answers are scores of a source, not values, and get no ladder.

## Amendment 1 (2026-10-27): values of free public sources are free to read (THREATS NS5-8)
The owner's choice on THREATS NS5-8 (relayed by the coordinator, 2026-10-27): of the two options
(free public-source values are free, or the registry records a salted commitment), **free**.
`OnyxRegistry`'s `ObservationRecorded.result` is an unsalted function of the value, and the round
id and spec are public, so for a small value (a count, a temperature in tenths) a seal on the site
does not hold: one RPC read and an enumeration give it back.
1. The public site ships the level-2 rounds and the level history of Treasury Fiscal Data, USGS
   and NCEI unsealed (`scripts/build-web.sh`), and the data page says these values are free to
   read. The SEC holdings round stays sealed (it never ran on chain; the seal holds there).
2. The query API serves them at no charge: `scripts/deploy-api.sh` refuses an export whose catalog
   prices a shipped level-2 metric or sets a default price. A paid observation (ADR-0049) still
   charges for running a round; reading its value afterwards is free.
3. A salted on-chain commitment stays possible for a future paid source whose value must not be
   guessable; it is not built (the TODO test named in NS5-8 moves with it, ROADMAP C64).
Decision 7's seal now applies only to rounds the paid API sells (today none on chain).

## Amendment 2 (2026-10-28): ladder slice 2 (ROADMAP C64, north-star loop NL13)
Spec: docs/spec/ladder-slice-2.md. What slice 2 decides and builds:
1. **One value form (NS5-12).** A ladder's value key hashes `canonical_value` (trailing zero
   digits of a `Fixed` dropped, scale 0 an `Int`, a `UInt` that fits an `i128` an `Int`):
   `ladder_value_hash` in every lane. The book's own `valueHash` stays the hash it recorded. Only
   a value with trailing zeros gets a new key (on testnet: the Denver precipitation `0.0`, now
   `Int(0)`), so a node and a client must run the same release.
2. **Cross-lane ladders are opt-in (NS5-11, R-LAD2-1).** `ladder` keeps its slice-1 form (one
   lane) in every default answer, so a slice-1 client never meets a step it would refuse. A
   cross-lane ladder is a separate form, `v: 2`, with each step's own record and
   `contradictions` (`onyx_api::ladder::{compose, to_json_v2, parse_v2, check_served_v2}`); a
   client takes a step stronger than its own only after that step's own lane check passed, else
   the whole ladder is refused, never trimmed. No node serves `v: 2` yet: no testnet fact has
   two lanes (a source that both signs and is observed), so no route was added for it.
3. **A late, earlier, stronger proof is no fall (NS5-10).** `check_extends` accepts a missing
   step when a stronger step no later than it is present; a step changed in place, a stronger
   step that came later, or a weaker top is still a fall. Nodes keep rebuilding ladders with
   `Ladder::of`; clients accept the rebuild.
4. **Contradictions (NS5-5, R-LAD2-3, R-LAD2-4).** `onyx_core::ladder::Contradiction`: a
   strictly stronger answer of the same fact with another value hash, shown next to the value,
   never changing its level; it carries the other value's hash only. A client checks its shape
   (stronger, evidence named, another value), not the other answer's lane: it warns, it does not
   prove (open question 1 of the spec: warn on the site; `onyx-check` does not refuse on it).
5. **Level-3 answers carry `ladder` (NS5-3).** `/v1/signed` adds the one-step ladder (level 3 on
   the report hash, at the book's `acceptedAt`, a chain time). The browser's level-3 check takes
   it only when it is exactly that step (`signed_view::expected_ladder`) and still takes a body
   from an older node without it.
6. **Clients remember (NS5-9).** `onyx-check --ladder-state DIR` and `level2-check
   --ladder-state DIR` keep the ladder accepted per value key and refuse a later answer that does
   not extend it; the site keeps it in the reader's browser (`localStorage`, per value key) and
   refuses a fallen ladder on the next visit.
7. **Levels bound to the caller's registry (NS5-6, NS5-14).** `onyx-check --registry ADDR` reads
   `levelOf(round, shard)` on the caller's node: a `proof`, `metric`, `export` or `settlement`
   answer may not state a stronger level, and a row answer's ladder must be the row's one step at
   that level. A shard with no attestation counts as the weakest level.
8. **Badges say "stated" (NS5-13).** A level badge that no check on the page backs (the
   catalog's) reads "Level N · Name · stated"; a badge with an accepted ladder does not.
Not built: a node route that serves `v: 2` (item 2), and the row step's `at` is still the
server's `finalizedUnix` hint (R-LAD2-2 for rows waits for the attestation block's time).
