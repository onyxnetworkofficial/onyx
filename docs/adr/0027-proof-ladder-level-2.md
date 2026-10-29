# ADR-0027: Proof levels as protocol data; level 2 "re-observed" feeds

## Status
Accepted, 2026-10-10 (loop 57; the loop decides, per the owner's instruction to keep the project
moving without consulting him). Implemented in `crates/onyx-core/src/proof.rs`,
`crates/onyx-coordinator/src/observe.rs` and `adapters/source-page`; spec
docs/spec/proof-ladder.md; research RESEARCH.md "Proof ladder level 2". Off-chain only; not served
by the API yet.

## Context
Onyx can only take data in today by rerun (level 1, M1-M4) or by first-party signature (level 3,
labels, ADR-0015). Most useful data (a public page, an open API) cannot be rerun from pinned
inputs and is not signed. The proof ladder (docs/TRACKER.md N9) names five levels, but nothing in
the protocol carried a level, and no lane existed for the second rung. Prior art (SchellingCoin,
Chainlink OCR, Pyth, UMA, TLSNotary) shows two patterns: many observers with commit-reveal and a
median (levels like 2), or one proven fetch (level 3 via zkTLS). Copying (freeloading) is solved by
commit-reveal; a colluding majority is not solved by any of them.

## Decision
1. **Level is derived, not stored.** `VerificationMethod::proof_level()` gives every claim a level,
   like the lane. No existing encoding changes. A new method `ReObserved { spec }` is appended last.
2. **Markets choose a minimum level.** `MarketPolicy`; the default for money is levels 1-3.
3. **Level 2 = strict majority of ALL assigned bonded observers.** Commit-reveal with the
   observer's address inside the commit; reveals only after commits close; quorum over assigned
   observers, not over reveals, so silence never lets a minority decide.
4. **Two value kinds.** Exact: identical normalized bytes. Numeric: an integer mantissa at a fixed
   scale; the band is `lower median of all numbers ± tolerance`; the value is the lower median of
   the numbers in the band. Fixed tolerance instead of SchellingCoin's percentile band, so the
   reward does not depend on how spread honest answers are. No floats.
5. **An explicit `Unavailable` answer.** A majority of `Unavailable` settles `SourceUnavailable`
   and slashes none of them. `Unavailable` during a settled value costs the small `miss_slash`.
6. **Payouts.** Agree: reward. Disagree with a settled value: `outlier_slash`. Committed but
   unrevealed, or never committed: no pay, no slash (SIG-R1-F1 amends this: only the coordinator
   says a reveal or commit is missing, and it can withhold a signed one; a slash for silence waits
   for a public record of reveals, THREATS SEC-R2-F1-P2). A value during an outage and every reveal in a `NoResult` round: no call.
7. **Fetchers are pinned extraction steps** (`Fetcher` trait): page bytes in, answer out, network
   outside. Each reveal carries the hash of the raw page as evidence. The example fetcher reads a
   JSON pointer from synthetic fixture pages; no real source is wired in until a rights review.

## Consequences
- Any public page with open terms can become an Onyx feed at level 2, with customers and markets
  seeing that it is weaker than a rerun.
- A colluding majority of observers can settle a false value; the defence is economic (bonds,
  random assignment, slashing by later evidence), not cryptographic. Logged as L2-F2.
- Level-2 rounds are off-chain and not yet part of the ledger, the contract or the API; the
  on-chain port and the API serving path are follow-up loops.
- The example fetcher is pinned by a version tag, not by content-addressed code (L2-F5).

## Amendment (loop 58, review L2R)
Numbered 0027 because main took ADR-0026 for self-settled x402 (L2R-F9).
1. **Claims point at a result** (L2R-F3): `ReObserved { spec, result }`; `result` is the settled
   round's `result_hash`, which binds the feed, the round id and the outcome.
   `ObservationSettlement::backs` refuses a claim for another round, a round with no value, or a
   value that differs.
2. **Money default is levels 1 and 3** (L2R-F8, replaces item 2's "1-3"): `MarketPolicy` is a set.
   `OfficialSourceMatch` and `CrossSourceQuorum` move to level 4 (no observers, no bonds). The
   API's settlement endpoint enforces the catalog's policy.
3. **Drift is not a lie** (L2R-F1): numeric answers between one and two tolerances from the band
   center are no call; outliers start beyond twice the tolerance. Numeric windows are at most
   300 s.
4. **Outages pay nobody** (L2R-F2): a `SourceUnavailable` round pays no reward.
5. **Round binding** (L2R-F6): commits and results bind a round id (feed, config, observer set);
   the duplicate-hash refusal is dropped (L2R-F5).
6. **Bounds** (L2R-F7): tolerance at most 10^30; reward and slashes fit `i128`.
7. **Pinned encodings** (L2R-F10): fixtures/claims/vectors.json.

## Amendment (loop 59, review L2R2)
1. **One claim intake** (L2R2-F1): the coordinator keeps the results it settled itself
   (`ObservationResults`); `accept_claim` is the only way a claim is accepted and, for level 2,
   checks the claim against that store (and the feed's rights). `ObservationSettlement` has private
   fields and no decoder, so a submitter cannot supply one. Settling a round twice is refused.
2. **Market policy is advisory** (L2R2-F2): the API refuses settlement below the policy, but the
   level is not bound on chain and `/v1/proofs` serves the same `verifyRow` arguments. TODO
   (later contract loop): bind the proof level into the shard binding so a market contract can
   enforce it.
3. **Weakest input** (L2R2-F3): a shard's level is the weakest of the recipe rerun (1) and its
   label set (3, 4 or 5). `from_spec` fails closed to 5 when a label set is pinned; the caller
   supplies the reviewed level. Both reviewed Bitcoin test sets are level 4, so their markets do
   not settle under the money default.
4. **Stale guesses cost** (L2R2-F4): a value in a `SourceUnavailable` round pays `miss_slash`.
5. **Far minorities are judged in a clustered `NoResult`** (L2R2-F5): see R-OBS-8.
6. **`backs` reads the feed** (L2R2-F6); **one `ProofLevel` enum** (L2R2-F7: the dispute module
   re-exports `onyx_core::ProofLevel`; same names and order, so challenge messages and hashes are
   unchanged).

## Amendment (loop 60, review L2R3, q15 (a))
1. **A settled value backs one claim shape** (L2R3-F1): `ObservationSpec` gains `metric` and
   `subject`; the intake binds the claim's metric, subject, source, snapshot (`Document {
   window_end, result_hash }`), rights, label set (none) and evidence (none) to the feed and the
   result, in addition to the value and the result pointer. Table in docs/spec/proof-ladder.md.
2. **Honest site wording** (L2R3-F2): the demo market is level 4 today (unsigned company labels in
   the set); OKX labels alone are level 3; the rerun itself is level 1. The ladder shows level 4 in
   use and level 2 as built with no live feed.
3. **`Claim::decode` refuses level 2** (L2R3-F3): level-2 bytes enter only through
   `ObservationResults::decode_claim`.
4. **Corroborated values are not slashed in an outage** (L2R3-F4): in a `SourceUnavailable`
   round, a value matched by at least one other value (within tolerance) is `NoCall`; only a lone
   value pays `miss_slash`. Trade-off: two colluding guessers who agree on a stale value escape
   the slash (they are still not paid), versus a lazy majority censoring a live feed and slashing
   the honest fetchers for free; the second is worse, so corroboration wins.
5. **One result per feed per store** (L2R3-F5): `ObservationRound::settle` is crate-private; the
   store refuses a second round of a settled feed (`FeedAlreadySettled`). TODO (on-chain port):
   enforce one result per feed across coordinators.

## Amendment 3 (loop 61, review 4)
Review 4 closed the claim-backing family (L2R-F3, L2R2-F1, L2R3-F1). Follow-ups: the store keys
"one result" on the fact (`fact_key`: metric, subject, window end), not the full spec id
(L2R4-F1); `ObservationResults` is not `Clone` (L2R4-F2); `backs` is crate-private (L2R4-F3);
`accept_claim` is the level-2 intake only and refuses other methods (L2R4-F4).

## Amendment 4 (level-2 data in the query API)
1. **Endpoint.** `GET /v1/observations/{metric}?subject=[&window_end=]` serves a settled level-2
   value with its round's public transcript (configuration, observers, commits, reveals) and the
   values a client recomputes (spec id, round id, result hash, fact keys). Shape and checks:
   docs/spec/delivery.md.
2. **No trust in the server.** The client verifies with its own copy of the metric's registered
   `FeedTemplate` (`client::verify_observation`): the same rebuild a consumer makes before asking
   the chain (`observe::onchain_observation_check`, REG-R2-F3), plus a binding of the served
   answer to the result hash (`observe::result_hash_of`, now public). The server re-checks every
   round with the same function when it loads a results store, and reads nothing derived from
   the file. With a node, the client asks `OnyxRegistry.observationLevel` with its own
   arguments.
3. **Rights and markets.** A forbidden source never loads; `Restricted` gets 451. Level-2 values
   are never bound to a market (markets bind shard rows; money settles on levels 1 and 3 only).
4. **Pricing and revenue.** Priced by the metric's per-row policy, decaying from the subject's
   template-fixed decay start (`FeedTemplate::decay_start`: a dated subject's earliest allowed
   window end, a fixed subject's own window end; L2API-R3-F1). Billed to `(OBSERVATION_ROUND, registryFactKey)`, opened by the reporter with
   `OnyxRevenue.registerObservation`; with no miners, validators or label set the whole fee goes
   to the treasury for now (revenue.md R-REV-12; observer payouts are a TODO, THREATS L2API-3).
5. **Rehearsal.** `scripts/demo-local.sh` serves the recorded Treasury round next to the shard:
   402, x402 payment (local mock), the client's check, `observationLevel` = 2 on the local chain,
   and the receipt on the observation book. Fork and testnet runs skip it until the book is
   opened in the deploy (THREATS L2API-4).

## Amendment 5 (period subjects, SEC EDGAR filings)
1. **Why.** Company filings report a value per reporting period. A period may end on a weekend
   and filings are due weeks to months later, so `Dated` (business-day record, next-day
   publication, 10-day maximum) does not fit, and EDGAR serves every period of a company in one
   document, so the date belongs in the selector, not the URL.
2. **Rule.** `SubjectRule::Period { prefix, min_days, max_days }` (crates/onyx-core/src/feed.rs),
   appended after `Dated`, so no existing template id changes: subject `prefix:YYYY-MM-DD` (any
   calendar day), `{date}` once in the selector and never in the URL, observable when the
   window's UTC day is `min_days..=max_days` days after the period end, one fact per date
   (`is_dated`), price clock fixed at `period end + min_days + window` (`decay_start`).
3. **First source.** `adapters/source-edgar`: `us-gaap:CryptoAssetNumberOfUnits` per company,
   110..=730 days (the longest filing deadline is 90 days, plus 15 under Rule 12b-25, plus a
   weekend roll; SEC-R1-F4). A fact says what the company filed, never that coins exist, and it
   is the company-wide crypto-asset unit total (`companyconcept` serves only facts without
   dimensions), not a per-asset bitcoin figure (THREATS SEC-2, SEC-R1-F2).
4. **Only a value consumes a dated or period fact (SEC-R1-F1).** A round that settles
   `SourceUnavailable` or `NoResult` (the source refused the observers, a late filer had not
   filed, a disagreeing amended row) leaves the date open; a later round may settle the value,
   and the first value stands for good. A fixed subject's fact is its window, closed by any
   outcome. On chain the coordinator records value outcomes only
   (`ObservationResults::onchain_record`); `OnyxRegistry` cannot see an outcome, so a
   non-value record is a coordinator fault that blocks the date but never misleads a consumer
   (the consumer check requires a value). Templates are unchanged by this rule.

## Amendment 6 (observers drawn at random from a bonded pool, L2-F2)
1. **Why.** Until now the coordinator listed a round's observers. A coordinator that picks its
   own friends needs no bribe to settle a false value (THREATS L2-F2).
2. **Draw.** Observers are drawn, never listed. `DrawSeed { pool, beacon_height, beacon,
   attempt }` (crates/onyx-coordinator/src/observe.rs): `pool = keccak256("onyx-observer-pool-v1"
   ‖ borsh(pool))` over the pool snapshot (1..=1024 members, strictly ascending), `beacon` the
   deployment chain's block hash at `beacon_height`. Each member whose snapshot bond covers
   `outlier_slash` gets the key `keccak256("onyx-observe-draw-v1" ‖ spec_id ‖ draw_hash(seed) ‖
   address)`; the `min_observers` smallest keys are the observers, ascending by address. The
   template's observer count is the draw size. This is the M5 panel draw (ADR-0019/0022/0024:
   a hash-ranked draw over a bonded set with a beacon that did not exist when the set was fixed).
3. **Grinding (revised by review L2RND-R1).** The draw is announced on chain:
   `OnyxRegistry.announceDraw(metric, subjectKey, windowEnd, attempt, commit, beaconHeight)`
   (coordinator only) stores the draw commitment (point 11: pool hash, spec id, config hash) and a
   beacon height above the current block, once per fact and attempt, immutable; attempts are strictly sequential from 0. Once the beacon block
   exists anyone calls `fixBeacon(fact, attempt)`, which stores `blockhash(beaconHeight)` (within
   the EVM's 256-block window; past it the attempt is dead and the next one is announced).
   `recordObservation` requires the fact's latest draw to have a fixed beacon. Off chain,
   `ObservationResults::announce_draw(spec, cfg, members, beacon_height, chain)` reads the height
   from a `ChainView`, and `open_round(spec, cfg, snapshot, chain)` reads the beacon from it, never
   from a caller argument; only the latest announced attempt opens, once. Runners go through
   `observe::draw_and_open`, which announces in the store and on chain, saves the book, fixes the
   beacon and opens. A coordinator that saw the beacon cannot use another pool, spec (window) or
   config for that attempt, because the commitment fixes all three on chain and
   `recordObservation` refuses any other (point 11, L2RND-R3); nor another height, an earlier
   attempt, or another beacon.
4. **Re-draws.** Each announcement uses its attempt at once (L2RND-R1-F3): the book
   (`non_value_book`, field `draws`) counts it from the announcement, the runner saves it before the
   beacon height (temp file, fsync, rename), and a restarted store raises its count to the
   registry's `drawCount` (the chain is the authority). A dated or period fact's round without a
   value, or an abandoned announcement (`abandon_draw`), ends that attempt: the next round is
   attempt + 1, and for a dated fact it counts toward the cap of rounds without a value
   (SEC-R2-F1). An abandoned draw of a fixed-window fact (for example `ObserverExposed`, its
   observers' bonds held by another round) does not close the fact (L2RND-R1-F4): it is drawn
   again. A new attempt is refused while the previous one is announced and neither opened nor
   abandoned. Attempts never reset, not even after a reviewer's `reopen_fact`; at most
   `MAX_DRAW_ATTEMPTS` = 64 per fact (attempts 0..=63, L2RND-R1-F7), on chain and off.
   **Subject bound (review L2RND-R2).** The registry keeps, per (metric, subject) and across
   every window and attempt, the count of announced draws that did not end with a recorded value
   (`subjectStrikes[keccak256("onyx-registry-subject-draws-v1" ‖ metric ‖ subjectKey)]`):
   `announceDraw` adds one and reverts `DrawsExhausted` at the allowance, `recordObservation`
   takes one off. The allowance is `MAX_DRAWS` = 3 (the non-value cap) times (1 + owner reopens):
   only the registry owner, never the coordinator, calls `reopenDraws(metric, subjectKey)`
   (event `DrawsReopened`). So, without a recorded reopen, a coordinator that drops seen draws
   (restart, crash, lost book) or shifts a fixed feed's window to get a new fact gets at most 3
   public draws of the subject. Off chain, `draw_and_open` reads `drawBudget` before announcing
   and refuses at the bound; a dated fact's rounds without a value are raised to the chain's
   strikes since the last reopen (`sync_budget`, never local memory), so it closes at the cap
   (L2RND-R2-F3). The coordinator announces on chain first and counts locally only if that
   lands; a failed announcement is forgotten, so a saved book is never ahead of the chain
   (L2RND-R2-F5). `fixBeacon` is public: a beacon someone else already fixed is success, not an
   error (L2RND-R2-F4). A fixed feed's windows sit on its grid (`window_start % window_secs ==
   0`, `FeedTemplate::instantiate`, so also `matches` and `open_round`), so a window shifted by a
   second is refused off chain; the registry does not check the grid, since the subject bound
   already covers every window (L2RND-R2-F2). A reviewer who reopens a fact off chain
   (`reopen_fact`) needs the owner's on-chain `reopenDraws` too.
5. **Exposure.** The snapshot holds each member's free bond (bond minus `outlier_slash` already at
   stake in this store's open rounds). Opening adds `outlier_slash` per drawn observer and re-checks
   it against the full bond (`ObserverExposed`); settling releases it. One bond is never the
   collateral of two rounds at once (lesson from L31-F2, L4R-F3).
6. **Binding.** `round_id = keccak256("onyx-observe-round-v2" ‖ spec_id ‖ borsh(cfg) ‖
   borsh(observers) ‖ draw_hash)`. Commits and the result hash contain the round id, so the
   result recorded on chain already commits to the pool snapshot, beacon height, beacon and
   attempt.
7. **Clients.** A served transcript carries `pool` and `draw` (`/v1/observations`, the client,
   the site's level-2 summaries). `onchain_observation_check` (server load, the Rust client) and
   the web verifier (`level2_draw_check`) recompute the pool hash, the draw and the round id, and
   refuse observers that are not the drawn set.
8. **On chain (L2RND-R1).** `OnyxRegistry.drawMatches(metric, subjectKey, windowEnd, attempt,
   commit, beaconHeight, beacon)` is true only if the draw was announced before its beacon height,
   is the fact's latest attempt (`drawCount - 1`, so no earlier attempt is cherry-picked), names
   this commitment (the client's own `draw_commit` of pool, spec and config, L2RND-R3), and its
   beacon was fixed from `blockhash`. Clients ask it with their own
   arguments: the Rust client (`VerifiedObservation::draw_matches_calldata`,
   `client::draw_matches_from_return`) next to `observationLevel`, and `demo-local` does both.
   Rust and Solidity share a vector (`fixtures/registry/vectors.json`, `treasuryDraw`).
   Residuals: (a) the beacon is an L2 block hash, and Base's sequencer can choose which blocks
   it produces, so it can bias a beacon by withholding blocks or reordering (the same residual as
   ADR-0022 item 5); (b) the pool hash is the coordinator's attested snapshot, not read from an
   on-chain bond book; (c) the coordinator can still abandon a seen draw and announce the next
   attempt, up to 64 attempts per fact: every abandoned attempt stays public on chain
   (`drawCount`, `drawOf`); since review L2RND-R2 the subject bound caps the public draws a
   coordinator can choose from at 3 per owner reopen, and clients require it
   (`client::verify_observation_onchain`: level 2, `drawMatches`, and the round's attempt below
   the subject's allowance with strikes never above it). The bound limits the choice; it does not
   remove it: a coordinator can still pick which of up to 3 public draws reaches a value;
   (d) collusion within the pool.
9. **Rehearsals (L2RND-R1).** The Treasury round draws 5 observers and each SEC round draws 3,
   both from a simulated pool of 9 bonded observers. The draws are real on a local chain:
   `scripts/level2-rehearsal.sh` deploys `OnyxRegistry` on an anvil, and the runners announce each
   draw, save the book, fix the beacon from the anvil's block hash and open with it. The
   registry's record of each draw goes in the site summary (`draw.announcement`) and in
   `fixtures/*/draw.json`, which the recorded replays use. The site says "simulated pool of 9,
   beacon from a local test chain"; the web verifier checks the record is consistent (announced
   below the beacon height, latest attempt, the commitment it recomputes, same beacon) but cannot
   ask that chain,
   which no longer exists.
10. **Not changed.** A colluding strict majority of the drawn observers still settles a false
   value. Random drawing makes that need a colluding share of the pool, not a hand-picked set.
   With the rehearsal's numbers (pool 9, draw 5, majority 3), 3 colluders win one draw with
   probability 15/126 = 11.9% and the best of the 3 public draws a subject allows 31.6%; 4
   colluders 45/126 = 35.7% and 73.4% (THREATS L2RND-R3-F4,
   `draw_commit::collusion_odds_of_the_rehearsal_pool`).
11. **Every draw input is committed before the beacon (review L2RND-R3).** The committee depends
   on the pool, the spec id and `cfg.outlier_slash` (the draw's `min_bond`). The spec id covers
   the window, which a dated or period fact key ignores, so a coordinator used to choose the
   window, and the threshold, after seeing the beacon. Now `announceDraw` stores `commit =
   drawCommit(pool, spec, cfgHash) = keccak256("onyx-draw-commit-v1" ‖ pool_hash ‖ spec_id ‖
   cfg_hash)` with `cfg_hash = keccak256("onyx-observe-cfg-v1" ‖ borsh(cfg))` (Rust
   `observe::draw_commit`, `observe::cfg_hash`; web `level2_draw_commit`). `recordObservation(...,
   spec, result, pool, cfgHash)` reverts `DrawMismatch` unless the latest draw committed to
   exactly them, and `drawMatches` compares the commitment the client recomputes. The draw seed
   is unchanged: committing its inputs is enough, and keeps one derivation. Tests: forge
   `test_ADrawReachesOnlyItsCommittedSpecAndConfig`, `draw_commit::*` (of 20,000 windows and
   every `outlier_slash` in 0..=10,000, exactly the committed one matches the recorded draw).
   `min_bond` per template was not added as a second guard: the config hash already fixes it, and
   a template field would change every template id.


## Amendment 7 (observers sign commits and reveals, ATT-R1-F3)
1. **Why.** Commits and reveals were bare hashes and values. Whoever served a transcript could
   publish a made-up round for a fixed draw (say an all-`Unavailable` one in place of a
   dissenting committee) and every client check passed (THREATS ATT-R1-F3).
2. **Signer.** An observer's address is the address of its secp256k1 key. The pool snapshot
   already lists that address with its bond, the draw picks it, and `drawCommit` binds the pool
   hash on chain, so the signer of every commit is fixed before the beacon. No encoding of the
   pool, round id or draw changes.
3. **Messages.** EIP-191 (`v` in {27, 28}, low `s` only, as level 3), over
   `observe::observation_commit_message` = `"Onyx observe commit v1\nround: {round id}\nattempt:
   {attempt}\nobserver: {0x address}\ncommit: {commit}"` and `observation_reveal_message` =
   `"Onyx observe reveal v1\nround: …\nattempt: …\nobserver: …\nobservation: {keccak256(borsh(
   observation))}\nsalt: {salt}"`. The text prefixes separate the two domains and every other
   signed message in Onyx; the round id binds feed, config, observers and draw, and the attempt is
   spelled out as well.
4. **Rules.** `ObservationRound::commit(now, observer, hash, sig)` and `reveal(…, salt, sig)`
   refuse an unsigned one (`BadSignature`). `rebuild_round` (so store load, `check`,
   `check_attempt`, `check_sealed_attempt`, the Rust client and through it the on-chain checks)
   requires a valid signature on every commit and reveal; the web verifier mirrors the messages
   (`level2_commit_message`, `level2_reveal_message`, pinned to the coordinator by a native test)
   and checks both signatures of every round it shows. Transcripts carry `sig` on every commit and
   reveal (`commits[].sig`, `reveals[].sig`, 65 bytes hex); a sealed round keeps its signed
   commits.
5. **Salts (ATT-R2-F2).** Each observer draws its salt from the OS (`observe::random_salt`) and
   keeps it until it reveals; a salt computed from public data lets anyone open a sealed commit by
   search.
6. **Rehearsals.** Simulated observers sign with deterministic FAKE keys
   (`observe::test_keys`: `keccak256("onyx test observer key {label} {i}")`, public, holding
   nothing on any network). The recorded rehearsals were regenerated (`scripts/level2-rehearsal.sh`)
   and the site's result pins updated, since the pool addresses changed.
7. **Not covered.** A server can still leave a reveal out (the observer then shows as committed,
   not revealed). Withholding enough honest reveals of even a unanimous round turns it into
   `NoResult` and forces a re-draw, at the cost of one of the fact's 3 draws (SIG-R1-F1); so a
   missing reveal or commit is never slashed (item 6 of the base decision, amended). A sealed
   round's outcome is its word until the fact is closed (ATT-R1-F2). The rehearsal observers are
   not independent (one machine, public keys). Withholding is now caught when the observer
   posted (amendment 8).
8. **Public builds (SIG-R1-F2).** The result hash is an unsalted function of the value, so a
   low-entropy value is recovered by enumeration. An `ONYX_PUBLIC=1` build ships a sealed round
   without its result hash, value, reveals or page, and its WebAssembly verifier is built with the
   `public` feature, which holds no result pins; the page shows the feed, period and draw count
   and "Available via the paid API". On chain, `observationOf` still exposes the result hash once
   the fact posts (L2API-R1-F5).

## Amendment 8 (transcript log: posted commits and reveal receipts, SIG-R1-F1)
1. **Why.** A coordinator could leave honest observers out of a published round: drop their
   commits before it sees any answer (to re-draw a committee it dislikes) or drop revealed
   answers (to steer the value or turn a round into `NoResult` and re-draw). Each re-draw cost
   one of the fact's 3 draws, but nobody could prove it (SIG-R1-F1, SEC-R2-F1-P2).
2. **Options.** (A) Observers post their reveals on chain: provable without the coordinator, but
   a reveal holds the answer, so every observer's value would be public before or without the
   paid API, against ATT-R1-F2/SIG-R1-F2 (sealed rounds, public builds without values). A digest
   of the reveal does not help: a client cannot tell "the coordinator dropped it" from "the
   observer posted a digest of something it never sent", so any one drawn observer could veto a
   fact. (B) Coordinator receipts only: provable for reveals, but a coordinator that drops a
   COMMIT never sees an answer and owes no receipt, so blind committee grinding stays.
   (C, chosen) Commits on chain plus reveal receipts. A commit is a salted hash (no answer), so
   observers post it on chain with its block time; a reveal goes to the coordinator privately and
   the coordinator's receipt key signs a receipt naming only the commit, which the observer
   posts. Rejected: slashing the coordinator's settlement bond via `proveWithheld`. The chain
   never sees a non-value round's transcript, so it cannot judge omission itself; the client
   does, and refusing the fact removes the gain (a value from the re-drawn committee is never
   accepted).
3. **Contract (`OnyxRegistry`).** `setReceiptSigner(address)` (coordinator; the coordinator's hot
   key, not its transaction key; bound to the coordinator that set it, so after a coordinator
   change `receiptSigner()` reads zero until the new one sets its own, review REVLOG-R1-L2);
   `announceDraw` reverts `NoReceiptSigner` without one and snapshots it per draw
   (`drawReceiptSigner`, so a rotation never voids receipts). The binding is to (coordinator,
   `ledger.coordinatorEpoch()`), so a coordinator that leaves and returns (A to B to A) does not
   bring its old key back (review REVLOG-R2-L3); `OnyxLedger`'s coordinator is immutable, so its
   epoch is the constant 0, and a ledger that can change coordinators must bump it.
   `postCommit(fact, attempt, observer, round, commit, sig)` rebuilds the observer's own
   `"Onyx observe commit v1"` text (amendment 7), needs the draw's beacon fixed, keeps the first
   post per `logKey = keccak256("onyx-transcript-log-v1" ‖ fact ‖ attempt ‖ observer ‖ round)`
   with `block.timestamp` and the observer's signature in state (`commitLogOf` returns commit, time
   and signature; review REVLOG-R2-H1: never read from event logs).
   `postReceipt(…)` checks `"Onyx reveal receipt v1\nround: …\nattempt: …\nobserver: …\ncommit:
   …"` against the draw's receipt signer. Slots are keyed by round, so a post for another round
   never fills this round's slot. `transcriptComplete(fact, attempt, round, commitOpen,
   commitClose, entries)` (entries: 20-byte observer ‖ 32-byte served commit ‖ 1-byte revealed,
   for every drawn observer) is false when a commit posted with a time in [commitOpen,
   commitClose] is not the served commit, or a receipt's commit is not served with its reveal.
4. **No observer veto (FIXED after reviews REVLOG-R1-H1, REVLOG-R2 and REVLOG-R3; strike 3, see
   docs/TRACKER.md 2026-10-12: one more veto route parks the branch).** A commit posted out of
   phase or for another round is ignored. A commit posted in phase is valid (signed, timely), and
   the coordinator must use it. `DrawChain` reads the log (`log_block`, `posted_commit`,
   `transcript_complete`; required methods, no default, REVLOG-R2-L5). (a) The read is contract
   state: `commitLogOf` returns the commit, its block time and the observer's signature in one
   `eth_call` (REVLOG-R2-H1: the R1 fix read the signature with `eth_getLogs` from block 0, which
   `https://sepolia.base.org` refuses past 200 blocks and other providers cap by result count, so a
   failed read published a failed round without the posted commit and voided the fact). (b) It is
   read ONCE per round, from one block N (review REVLOG-R3-1: the R2 code computed "head minus
   confirmations" separately for the time and for each slot, and re-read the log on every
   retry, so one stale read receipted an off-chain commit that a retry then dropped). N is the
   chain's `finalized` block (`eth_getBlockByNumber("finalized")`, number and time from one
   answer; `--finality safe` for a node without it; a pinned depth only on a local anvil, chain
   id 31337) once its time is past `commit_close`; the runner polls for that (`ChainWait`,
   bounded) instead of failing. Every `commitLogOf` and `factKey` read names N, so a node that
   does not have N errors (retried), never answers an older state. Cost: on an OP-stack chain a
   block is final only once its batch is posted to L1 and that L1 block is finalized; the
   Optimism docs give "about 15 to 30 minutes" on OP Mainnet and say the wait is dominated by
   the batch interval on chains that post less often
   (https://docs.optimism.io/stack/transactions/transaction-finality). Review REVLOG-R4
   measured 1,178 s between Base Sepolia's `latest` and `finalized` blocks (one reading,
   2026-10-12). Every round therefore signs receipts only after that wait
   (`ChainWait::DEFAULT` polls up to 30 minutes; a longer wait leaves the round unfinished in
   the book, item 4e), long after `reveal_close`; the reveals themselves are checked at their
   arrival (item 4f). (c) Before any reveal, `observe::absorb_posted_commits` reads every drawn
   observer's slot at N, absorbs each in-phase post (`ObservationRound::absorb_log`, all or
   nothing) and asks `transcriptComplete` for the round with no reveal: false means the read
   was stale or wrong (receipts cannot exist yet), so it reads again. Only then is the snapshot
   {N, slots} kept in the round and saved with the round book (`finish_reveals`' checkpoint),
   BEFORE the first receipt is signed; `take_reveal` refuses a round without it, and a round
   that holds one never reads the log again (`absorb_log` refuses: `LogAlreadyRead`). Then
   `finish_reveals` takes the reveals (`take_reveal`) and asks `transcriptComplete`. (d) Nothing an observer sends stops the round (REVLOG-R2-M2): a reveal
   that does not open its commit, is badly signed, fails the feed's checks or comes late is a
   non-reveal; a second reveal is ignored and its receipt posted again. (e) Chain errors are
   retried (`ChainWait::retry`); a run that still cannot finish, or one that already took reveals,
   returns its round UNFINISHED (`RunError::unfinished`), never a failed attempt. The store keeps
   it (`ObservationResults::keep_unfinished`) and the round book (version 3; version 4 adds
   reveal arrival times, item 4f) saves it, Borsh with its snapshot, re-checked on load (the
   draw, ids, signatures, reveals, snapshot, and the round's draw against the book's saved copy
   of the registry's draw record: load does not ask the chain; review REVLOG-R3-2, wording
   REVLOG-R4); a fact with an unfinished round refuses a new
   draw (`RoundUnfinished`) and the next run finishes it on the same draw. `finish` is
   idempotent after `settle` (`settle_once` returns the recorded settlement for the same
   result, REVLOG-R3-3). Never a failed attempt: `Attempt::of_failed_round` refuses a round whose
   reveals settle a value instead of dropping them. Only a run that failed in the commit phase,
   after the log was read, is published as a failed attempt. A slot someone filled first is
   `Posted::Before`. Receipts exist only for reveals the coordinator accepted, after the absorb, so
   it always holds what a receipt names. (f) Reveal time (review REVLOG-R4-3). The receipts
   wait for the finalized block (b), so they are signed long after `reveal_close`
   (`commit_close + 120`); the runners used to hide that by taking every reveal at
   `commit_close + 1`, so a live clock would have made every reveal late. The coordinator now
   stamps each reveal with its own clock when it ARRIVES (`UnfinishedRound::receive_reveal`):
   kept only if it arrives in the reveal phase, from a drawn observer, signed by it, and is its
   first; the round is saved with the book after each kept reveal (`receive_reveals`), so every
   reveal is on disk BEFORE any receipt is signed; `finish_reveals` checks each reveal at its
   arrival time (`take_reveal`: it must open the commit the round holds after the log read).
   The book (version 4) keeps the arrival times; a version-3 book's reveals read back at
   `commit_close + 1`, the time the version-3 runners used. Recorded rehearsals replay the
   clock on the recorded pages' timeline (`ReplayClock`: each observer reveals right after the
   commit phase); a live coordinator uses `SystemClock`. The arrival time is the coordinator's
   word, as the phase check always was; seeing a reveal before the log read gives it nothing
   new (the log at the finalized block is fixed once the commit phase is over, and it already
   decides whether to receipt, item 7b). Rejected: a reveal window longer than the finality lag
   (every round ~20 minutes longer, and still void whenever the batcher lags more). (g)
   Publication (review REVLOG-R4-1). A settled round stays kept until its results are written
   (`ObservationResults::published`), and `save_book` refuses meanwhile, so the book on disk
   holds it unsettled: `run_and_save` hands it to `publish` (the binaries write the results
   store atomically), then drops it and saves the book. A crash before the write makes the next
   run settle and publish the same round on the same draw (counted once); a crash after it
   publishes again, and `ObservationStore::append` adds nothing twice. Load restores unfinished
   rounds before it closes facts, and does not close a fact with one by its count (review
   REVLOG-R4-2). Tests: forge `test_TheLogIsReadFromStateNotEvents`,
   `test_AReturningCoordinatorDoesNotResurrectItsOldKey`, `test_APostAtCommitCloseCounts`;
   `revlog_review::a_failed_log_read_is_a_wait_never_a_veto`,
   `an_error_after_receipts_never_voids_the_attempt`,
   `waiting_for_the_commit_phase_never_burns_a_draw`;
   `observe::tests::an_observer_cannot_stop_the_round_after_receipts`; REVLOG-R3:
   `revlog_review::r3_a_stale_read_is_caught_before_any_receipt`,
   `r3_a_lasting_stale_read_receipts_nothing_and_waits`, `r3_two_nodes_two_blocks_one_read_block`,
   `r3_the_snapshot_is_never_read_again`,
   `r3_an_unfinished_round_is_finished_by_the_next_run_on_the_same_draw`,
   `r3_finish_is_idempotent_after_settle`; `observe::tests::an_unfinished_round_travels_with_the_book_and_is_rechecked`,
   `settle_once_is_idempotent_only_for_the_same_result`; REVLOG-R4:
   `revlog_review::r4_a_settled_round_survives_a_crash_before_its_results_are_written`,
   `r4_a_live_clock_run_with_the_measured_finality_lag_yields_a_value`;
   `observe::tests::a_reveal_is_checked_at_its_arrival_not_at_its_receipt`,
   `a_settled_round_leaves_the_book_only_once_published`;
   `non_value_cap::lowering_the_cap_while_a_round_is_unfinished_still_loads`; on a real EVM,
   `scripts/level2-rehearsal.sh` runs `level2-inphase` on an anvil whose clock starts inside the
   round's window: a griefer's in-phase post is absorbed from the pinned block and
   `OnyxRegistry.transcriptComplete` passes (REVLOG-R3-4).
5. **Clients.** `Checked.log` / `CheckedAttempt.log` (`TranscriptLogQuery`) are built from the
   rebuilt transcript; for a sealed round the `revealed` list stands for the reveals (the
   server's word until the fact closes, item 7g). `verify_observation_onchain` takes a fourth
   return and `verify_attempts_onchain` one per published round. The `onchain` hint names
   `transcriptComplete` (REVLOG-R1-L1); the client accepts the key only with that exact value,
   tolerates its absence (older servers) and asks the call either way. The demo checks the real round on anvil and shows the same round with one
   receipted reveal dropped refused.
6. **Costs.** `postCommit` 182,480 gas (139,255 before REVLOG-R2 stored the signature),
   `postReceipt` 115,571 gas (the texts are rebuilt on chain),
   `transcriptComplete` 25,316 gas for 3 observers (`OnyxRegistryTranscriptLog.t.sol`
   `test_GasOfTheLog`). `OnyxRegistry` runtime 13,604 B after REVLOG-R2 (13,072 B after REVLOG-R1-L2; 9,158 B before
   amendment 8; limit 24,576 B).
7. **Residuals.** The transcript log closes blind commit-dropping where observers post; it does
   NOT close reveal withholding, so SEC-R2-F1-P2 and SIG-R1-F1 stay PARTIAL. (a) Protection covers
   observers that post: a commit not posted in phase, or a reveal without a posted receipt, can
   still be dropped (each drop that forces a re-draw costs one of the 3 draws, as before).
   (b) Selective receipt refusal (THREATS REVLOG-1, Medium since review REVLOG-R1-M1): the
   coordinator sees each reveal's value BEFORE it signs the receipt, so it can decline receipts
   for values it dislikes, drop those reveals and steer the value or force `NoResult` and a
   re-draw. Not provable: the observer sees no receipt at once but cannot prove it to others.
   Candidate fix (next level-2 loop): the observer posts its reveal on chain encrypted to the
   coordinator's key; a served round must include every posted encrypted reveal or the
   coordinator proves it decrypts to garbage. (c) Commit times are block times, the round's phases unix seconds; a
   sequencer that holds a post past `commit_close` makes it not count. (d) A run that fails after
   receipted reveals (`Attempt::of_failed_round` clears reveals that would settle a value) is
   refused by clients like withholding, so the coordinator must finish recording such a round,
   never publish it as a failed attempt; since REVLOG-R2 the runners keep it unfinished and
   `of_failed_round` refuses it. (e) Missing reveals are still never slashed (a missing
   receipt does not prove silence). (f) The rehearsals' observers and receipt key are public
   test keys (`test_keys::receipt_secret`); the recorded rounds' times are in the past, so their
   on-chain commit posts fall outside the commit phase and only the receipts bind them.
   (g) While a round is served sealed, its `revealed` list is the server's word: withheld
   receipted reveals listed as revealed pass `transcriptComplete` until the fact closes
   (REVLOG-5). (h) Whoever holds a draw's snapshotted receipt key can void that draw's rounds
   with a receipt for a commit nobody revealed (REVLOG-4); a coordinator change or rotation
   protects only later draws.
8. **Testnet launch: level 2 stays on anvil.** The demo driver runs the level-2 round only in
   local mode (`level2_prepare` in `adapters/source-btc/examples/demo-local.rs` returns early when
   a paid fork or testnet run is configured), so the Base Sepolia launch ships no level-2 round and
   no transcript log traffic. Before enabling it on Base Sepolia: (a) done (ROADMAP B1,
   2026-10-16): open the fact's revenue book (`OnyxRevenue.registerObservation`, R-REV-12, THREATS
   L2API-4): the deploy run opens every served fact's book from the reporter key before the API
   serves level 2 (`onyx_api::observation_book`: the book is read first, so a rerun sends
   nothing; a key that is not the reporter, a revert, an unmined send or an unreadable node stops
   the run before serving), rehearsed on a fork with `--level2`, which testnet refuses until B6; (b) done (REVLOG-R3-2,
   REVLOG-R4-1): an unfinished round is saved with the round book and finished by the next run
   on the same draw, and a settled round stays in the book until its results are written;
   (c) independent observers that post their own commits and receipts with their own keys (the
   rehearsals simulate them in the coordinator with public test keys), revealing to the
   coordinator in the reveal phase by its clock; (d) a separate hot receipt key per deployment.
   The finalized-block wait of item 4b is measured (1,178 s, review REVLOG-R4) and no longer
   voids reveals: they are checked at their arrival (item 4f, REVLOG-R4-3; the operator must run
   the coordinator on `SystemClock` with live fetches, not the recorded replay; the
   `level2-round` binaries pick it on any chain but a local anvil, `--clock replay|system`
   overrides, REVLOG-R5-1c); (e) REVLOG-1
   (selective receipt refusal) and REVLOG-5 (sealed rounds) stay open and must be stated on the
   paid API until fixed; (f) done (THREATS REVLOG-R4-5, High since review REVLOG-R5, the lead's
   decision of 2026-10-12: a draw-lifecycle bug, fixed narrowly): `fixBeacon` is idempotent
   (`fix_beacon_idempotent`: `drawOf` read before, and again with `ChainWait` retries after any
   error; a fixed beacon on the announced draw is success whoever fixed it), the book (version 5)
   keeps the announced draw before `fixBeacon` and the opened round from `draw_and_open` on, so
   the next run resumes the same draw, and a failed attempt is written before it leaves the
   book; review R6 (branch `draw-open`) closed the rest: the book never forgets a draw the chain
   may hold (a short `drawCount` or an `announceDraw` error is checked with `drawOf`; a draw is
   forgotten only when `drawOf` pinned at a final block at or past its beacon height shows it
   absent, REVLOG-R6-A/B), the give-up check reads `drawOf` and the height from one final block
   (REVLOG-R6-D); review R7 closed the rest of the lifecycle: the book is saved with the draw
   BEFORE `announceDraw` is sent, so no crash leaves a chain draw the book lacks (REVLOG-R5-2,
   REVLOG-R7-2); a chain draw the book lacks anyway (a lost or older book) is adopted when its
   commitment matches, drawn past only once a final block shows its beacon zero past its beacon
   height + 256, and otherwise the run stops; an `announceDraw` error is confirmed from the
   latest `drawOf` on every poll, so a landed draw's beacon is fixed within its 256-block window
   although Base Sepolia's finalized block lags about 500 blocks (REVLOG-R7-1); the final read
   refuses `--finality safe` and needs an RPC serving state at the finalized block (archive
   capable), else those decisions stop the run rather than void a draw (REVLOG-R7-3). Review R8
   (strike 3: R6, R7 and R8 each found a new route to a fixed beacon without a published round)
   replaced path-by-path patching with ONE invariant, the lead's decision of 2026-10-12: **once the
   book is saved with a draw (before the `announceDraw` send), the draw leaves the book only when
   (1) its round or failed attempt is durably published, or (2) one final block proves it can never
   become fatal: a block at or past its beacon height shows no such draw at its attempt (none, or
   another: `announceDraw` reverts `BeaconNotFuture`/`BadAttempt` for ever), or a block past
   `beacon_height + 256` shows its beacon zero (`fixBeacon` reverts `BeaconUnavailable` for ever);
   (a save that failed before any send also releases it). Every other outcome (an error, a stale or
   mixed read, a mismatch, a crash, a timeout) keeps the draw in the book and returns a retryable
   error.** It is enforced in one place, `ObservationResults::release_draw`, the only code that
   removes a draw, which checks each proof against the book's own draw; the saved book writes every
   draw it holds (a kept round, else announced) and refuses to save one it would drop; opening is
   one step with keeping, after the registry's record is checked (REVLOG-R8-1); multi-value reads
   of a draw come from one block. A lost book draws past an earlier chain draw with a fixed beacon
   (settled history under the invariant) and adopts the latest when its commitment matches
   (REVLOG-R8-2). Review 9 (0 merge-blocking) found a lost book adopting a draw whose round the
   results store already published, after a stale `drawCount` (REVLOG-R9-F1): a runner now reads
   the results store before a run, never adopts a draw it publishes (the latest one it publishes
   is followed by a next draw; a published value stops the run), and adopts only on ONE final
   block showing the draw as the chain's last; one runner per book (`<book>.lock`, REVLOG-R9-F3).
   The property test `revlog_r8::property_the_book_never_loses_a_draw` checks the invariant over
   20,000 seeded runs by default (crashes, stale and mixed reads, failed-but-landed sends,
   strangers' `fixBeacon`, blocks passing, other inputs and lost books) with the registry's real
   `transcriptComplete`. Correction: review R8 stated 2,400 runs; those checked the client with
   `transcriptComplete` stubbed true and missed REVLOG-R9-F1 (REVLOG-R9-F2). Not modelled: an older
   or restored book, writes that fail without a crash, two runners on one book across hosts,
   reorgs of non-final heads (REVLOG-R9-F3). Limit: a book lost after its round's receipts VOIDS
   the fact (the round cannot be finished without its receipted reveals: a fixed beacon without a
   published round, or a published round leaving out a receipted reveal), and operator action
   reaches it (a fresh `--book` path, a new host, ephemeral storage; operator warning in the
   adapters' READMEs and docs/ops/data-retention.md) until observers can resend reveals ((g),
   REVLOG-R5-1d); (g) done (ROADMAP B5, 2026-10-16): a restarted
   coordinator keeps taking reveals while the reveal window is open by arrival time
   (REVLOG-R5-1d): `observe::keep_taking_reveals` reads a `RevealInbox` until every drawn observer
   revealed or the coordinator's clock passes `reveal_close`, on the first run and on a run that
   resumes a round from the book; the window is the round's config, kept in the book and bound
   to the draw's commitment; each reveal taken is saved before the next; a round that holds its log
   snapshot (saved before the first receipt) or is settled in the store takes none (review
   B5-R1-M1). The inbox's transport is (c)'s; until then it is `NoInbox`, and a lost book
   still voids its fact as stated above.

## Amendment 9 (independent observers and a separate receipt key, ROADMAP B2 and B3, 2026-10-17)
1. **Why.** Amendment 8 item 8 (c) and (d): before level 2 runs on Base Sepolia, observers must
   be processes of their own with their own keys, revealing to a coordinator that runs on its
   own clock, and the receipt signer must be a hot key of its own per deployment (THREATS
   L2-SRC-1, REVLOG-4). Until now every rehearsal observer lived inside the coordinator and
   signed with a public test key, and every deployment signed receipts with the public test
   receipt key.
2. **Who carries what.** The draw needs no channel: an observer reads `drawOf` from the
   registry itself and checks it commits to ITS copy of the pool snapshot, the feed's canonical
   spec and config, with the beacon fixed (`fiscaldata::round::drawn_round`, i.e.
   `check_commit_allowed` of ATT-R1-F4); a coordinator that drew from another pool or config is
   refused. Commits go on chain from the observer itself (`postCommit`, amendment 8): the
   coordinator absorbs them from the transcript log, as it already did for a griefer's post.
   Only the reveal needs a private channel, since it holds the answer the paid API sells and
   must never be public before the round settles (option A of amendment 8 was rejected for that).
3. **Transport chosen: a spool folder** (`onyx_coordinator::observer_link`). Each observer
   writes its signed reveal atomically to `<spool>/0x<observer>/<round id>.json` (temporary
   file, sync, rename; file `0600`, folder `0700`); the coordinator's `DirInbox` (a
   `RevealInbox`) reads, on each poll, only the files of the round's drawn observers, at most
   16 KiB each, never through a symlink. Every reveal is authenticated by the observer's own
   signature, checked at arrival (`receive_reveal`: drawn, signed, first, inside the window by
   the coordinator's clock). Across hosts an observer delivers into its own folder over SSH
   (not built yet; item 9 says how): authenticated and encrypted by SSH, no new
   service. Why not the alternatives: (a) an HTTP endpoint on the coordinator adds a listening
   service, TLS and client authentication, and a reveal sent while the coordinator is down
   (or lost with a book, REVLOG-R5-1d) needs the observer to notice and resend; the spool's
   files outlast the downtime and a restart, so the B5 intake (`keep_taking_reveals`) reads
   them again with no resend; (b) an on-chain
   channel publishes the answer (amendment 8 option A); an encrypted on-chain reveal is the
   REVLOG-1 fix of item 7(b), still open (ROADMAP C4). The spool fits the codebase's existing
   model (the coordinator already polls an inbox, saves state with atomic renames, and reads
   the chain for everything else).
4. **The live coordinator** (`examples/level2-coordinator.rs`) runs `Observers::Independent`:
   it draws from the pool file, announces and fixes the beacon, holds no observer key and
   simulates no observer, takes reveals from the spool on the SYSTEM clock (a replayed clock is
   refused, `cast_chain::independent_clock`; the recorded `level2-round` binaries keep
   `clock_choice`: system clock on any chain but a local anvil), then absorbs, receipts and
   settles as before. A replay (the same reveal again, or a rewritten file) keeps the first; a
   late one is not taken; a forged one or one from an undrawn member is a non-reveal.
5. **The observer** (`examples/level2-observer.rs`): its own 0600 key file, its own chain
   reads, its own HTTPS fetch at the window start, a fresh random salt, commit and reveal saved
   (0600) before anything is sent, its own `postCommit`, the reveal once the chain is past the
   commit phase (item 8), then it waits for its receipt on chain (exit 3 without one: the coordinator
   withheld or did not finish, REVLOG-1; its posted commit is its evidence).
6. **Receipt key (B3).** `keyfile::ReceiptKey`: read from a 0600 key file, the public test key
   refused in a file; without a file only a replay or a local anvil (chain id 31337) runs, on
   the public test key (`ReceiptKey::for_chain`); a fork (which keeps the forked chain id) or a
   testnet needs `--receipt-key`. The runner refuses to start when the key is also the
   coordinator, the registry owner, `--reporter` (`OnyxRevenue.reporter`) or a pool member
   (`check_distinct`); the deploy run (`demo-local`, fork and testnet) keeps one key per
   `OnyxRegistry` deployment with the actor keys (`receipt-signer-<registry>.key`) and checks it
   against the deployer, the reporter, the registry owner, the customer, miners, validators and
   the observer pool. Rotation: configure a new key; the next `announceDraw` is preceded by
   `setReceiptSigner` (already in `CastChain::announce` and the demo's draw), and earlier draws
   keep their snapshotted signer (`drawReceiptSigner`, forge
   `test_ARotationKeepsTheDrawsSigner`). A round of an earlier draw is finished only with that
   draw's key: `finish_reveals` reads `drawReceiptSigner` first and stops before the log read
   and any receipt when the run's key differs (`DrawChain::draw_receipt_signer`).
7. **Not changed, stated plainly.** The rehearsal (`scripts/level2-observers.sh`) runs seven
   observer processes with distinct anvil dev keys on ONE machine: one host, one network path,
   one operator. That proves the protocol path, not independence (THREATS L2-SRC-1, L2-F2):
   separate hosts, networks and operators need real operators, the project owner's decision. EDGAR
   observers stay rehearsal-only until an operator role contact exists for the User-Agent
   (THREATS SEC-4). The coordinator still decides which reveals it receipts (REVLOG-1), and the
   pool snapshot is still its attested file (L2-F2 residual c). Money markets stay at levels 1
   and 3.
8. **Review B2-R1 (fresh adversarial reviewer, 2026-10-17; THREATS B2-R1-*).** The observer
   reveals only once the CHAIN's latest block is past `commit_close` and its own commit is on
   chain inside the phase (its clock alone let a colluding coordinator relay an honest reveal to
   a colluding observer that could still commit in time); it writes its reveal again on every
   poll until reveal close; the spool's write path never follows a planted link and refuses a
   folder open to others; a spool read error other than a missing file is reported; the public
   test receipt key also needs a loopback anvil RPC; the registry's real `owner()` is checked
   against the receipt key; runners sharing one coordinator address share its receipt key.
9. **Review B2-R2 (second fresh adversarial reviewer, 2026-10-17; THREATS B2-R2-*).** An anvil
   dev key in a receipt key file is as public as the test key: refused on any chain but a local
   anvil (`ReceiptKey::is_well_known`). The coordinator and each observer refuse to start when
   the system clock is more than 30 s from the chain's latest block time
   (`cast_chain::check_clock_skew`): reveals are stamped by the coordinator's clock and sent by
   chain time, so a fast coordinator clock would otherwise close its intake before any honest
   reveal. Stated plainly: what is built and rehearsed is the spool on ONE host in ONE OS
   account. Item 3's cross-host delivery is not built: with the modes this code enforces, a
   reveal written by another OS account (an SFTP account) is unreadable by the coordinator. The
   intended cross-host path is the observer's SSH key in the coordinator's account, restricted by
   a forced command that writes only that observer's folder (atomically, 0600), so the files
   stay the coordinator's; it is built and rehearsed before any observer runs on a host of its
   own (ROADMAP B6, THREATS B2-R2-3).

## Amendment 10 (sealed reveal posts, attempt records, level-2 disputes; ROADMAP C4, 2026-10-17)
1. **Why.** Two residuals of amendment 8 item 7: (b) REVLOG-1, the coordinator sees each
   reveal's VALUE before it decides to sign its receipt, so it can drop reveals it dislikes
   without anyone being able to prove it (bounded only by the subject's 3 draws); (g) REVLOG-5,
   while a fact is open its non-value rounds are served sealed and their `revealed` list is the
   server's word. Prior art: RESEARCH.md "Sealed reveals" (ECIES, Chaum-Pedersen DLEQ proofs,
   drand tlock, Shutter's threshold keypers, the last-revealer problem of commit-reveal).
2. **Options.** (A) Time-lock the reveal (drand tlock, IBE to a future round): nobody, the
   coordinator included, opens a reveal before the round's reveal phase ends; but at that round
   EVERYONE can open it, so every observer's value becomes public before or without the paid API
   (the reason amendment 8 rejected plain on-chain reveals, ATT-R1-F2, SIG-R1-F2). (B) A
   threshold key held by a committee (Shutter keypers): same timing, but needs a committee of
   operators Onyx does not have, and its key releases are public too. (C, chosen) The observer
   posts its reveal ON CHAIN, ENCRYPTED to a key only the coordinator holds. The post itself is
   the receipt: the chain, not the coordinator, decides that a reveal arrived, so there is no
   receipt to refuse. The coordinator must account for every post in the reveal phase: as a
   reveal of the round, or as dismissed garbage with a proof anyone can check. The values stay
   private (only the coordinator and the observer can read them; a DISMISSED post's plaintext
   becomes public with its dismissal, item 8h), as the paid API needs.
   (A) and (B) are kept as a later layer (item 9): they remove what (C) leaves, the coordinator's
   early view.
3. **Reveal key.** `OnyxRegistry.setRevealKey(x, y)` (coordinator only): an uncompressed
   secp256k1 public key, checked on the curve on chain (`y² = x³ + 7 mod p`), bound to the
   coordinator and its `coordinatorEpoch` like the receipt signer (amendment 8 item 3), and
   snapshotted by `announceDraw` into `drawRevealKey(fact, attempt)`, so a rotation never changes
   the key of a draw already announced. Optional: a draw announced without one takes no sealed
   post (`NoRevealKey`) and its observers reveal through the spool as before (REVLOG-1 open for
   them). The secret is a hot key of its own (like the receipt key, B3).
4. **Sealed post (observer).** Only once the chain is past `commit_close` and its own commit is on
   chain inside the commit phase (the B2-R1-M1 rule for the spool), the observer:
   (a) draws an ephemeral secret `r` from the OS, `R = r·G` (64 bytes `x ‖ y`);
   (b) proves it knows `r` with a Schnorr proof bound to this round, attempt and observer
   (`keyProof = addr(T) ‖ z`, `T = t·G`, `c = keccak256("onyx-sealed-key-pok-v1" ‖ R ‖ addr(T)
   ‖ round ‖ attempt ‖ observer) mod n`, `z = t + c·r`; `R.x` must be below the group order);
   (c) derives `S = r·K` (K the draw's reveal key) and the key
   `k = keccak256("onyx-sealed-reveal-key-v1" ‖ S.x ‖ S.y ‖ R ‖ round ‖ attempt (4 bytes BE) ‖
   observer)`; the plaintext is `borsh((observation, salt, reveal signature))` (its signed reveal
   of amendment 7) padded with zeros to ONE length per feed (`sealed::sealed_len`: 154 bytes for a
   numeric feed, value or `Unavailable`; 142 + `max_len` for an exact one), so a ciphertext's
   length shows nothing about the answer (review C4-R2-1: unpadded, it showed an exact answer's
   length and who answered `Unavailable`); a reveal with other padding is garbage and the ciphertext is the plaintext XOR the stream
   `keccak256("onyx-sealed-reveal-stream-v1" ‖ k ‖ i (4 bytes BE))`, i = 0, 1, … (ECIES of SEC 1
   §5.1 with Keccak as KDF and stream cipher; no MAC, because the plaintext carries the
   observer's own reveal signature and must open its commit, and the post is signed);
   (d) signs with its own key `"Onyx observe sealed reveal v1\nround: …\nattempt: …\nobserver:
   …\nkey: 0x{R.x}{R.y}\nciphertext: {keccak256(ciphertext)}"`;
   (e) calls `postSealedReveal(fact, attempt, observer, round, R.x, R.y, ciphertext, keyProof,
   sig)` (anyone may relay it). The registry needs the draw's beacon fixed and its reveal key,
   1..=1166 ciphertext bytes (an exact answer of the largest value, 1,024 bytes, sealed; review
   C4-R2-2), the observer's signature and a valid key proof
   (`keyProofValid`: `z·G − c·R = T`, checked with ONE `ecrecover`, which returns the address of
   `r_s⁻¹(s_s·P − h·G)` for the point P with x = r_s; with P = R, s_s = −c·R.x and h = −z·R.x it
   is `addr(z·G − c·R)`), and keeps the FIRST post per `logKey(fact, attempt, observer, round)`
   in state with its block time and its key proof (`sealedRevealOf`, `sealedKeyProofOf`; state,
   never event logs, review REVLOG-R2-H1).
   Why both: the observer's signature stops anyone else filling its slot (a veto by griefing,
   REVLOG-R1-H1); the key proof stops anyone posting an `R` it does not hold, whose dismissal
   (item 5) would reveal `S = k·R`. With it, a dismissal reveals only an `S` its poster could
   already compute (`r·K`): the dismissal is no decryption oracle. **Review C4-R1-1 (High,
   fixed before merge):** the first build used an ECDSA signature by `R` as the proof. That
   proves nothing: anyone makes a signature that recovers to a point `Q = r⁻¹(s·R_A − e·G)`
   related to another observer's `R_A` without knowing its secret; a garbage post under `Q` was
   dismissed honestly, and `S_A = r·S_Q + e·K` opened the other observer's reveal (a full static
   Diffie-Hellman oracle). Test `sealed_reveals::c4_r1_1_a_key_the_poster_does_not_hold_never_lands`.
5. **Coordinator intake and dismissal.** From one finalized block past `reveal_close` (the
   single-read rule of amendment 8 item 4b, now also for sealed posts), the coordinator reads every
   drawn observer's sealed post and takes those with a block time in (`commit_close`,
   `reveal_close`]: it decrypts each; a plaintext that is the observer's signed reveal, opens its
   commit and passes the feed's checks is a reveal of the round, arriving at the post's block time;
   anything else is DISMISSED with a proof: `S` and a Chaum-Pedersen proof (RFC 9497 §2.2 form,
   Fiat-Shamir over Keccak) that `log_G(K) = log_R(S)`. Anyone holding the post checks the proof,
   derives `k`, decrypts, and sees the plaintext is not a valid reveal; a false dismissal cannot
   be proven (the proof fixes `S`, so the decryption is fixed). Built in
   `onyx_coordinator::sealed` (`seal`, `open_post`, `dismiss`, `check_dismissal`).
6. **The rule, on chain.** `revealLogComplete(fact, attempt, round, commitOpen, commitClose,
   revealClose, entries)` is `transcriptComplete` (amendment 8) with a status byte per entry:
   0 not revealed, 1 revealed, 2 dismissed. Also false when a sealed post with a block time in
   (`commitClose`, `revealClose`] has status 0; when status 2 has no such post or has a receipt;
   or when a status is above 2, or observers are not strictly ascending. A post outside that
   window is ignored (not a veto).
7. **Attempt records (REVLOG-5).** `recordAttempt(fact, attempt, round, commitOpen, commitClose,
   revealClose, result, entries)` (coordinator only, once per draw, beacon fixed, only after
   `block.timestamp > revealClose`, so no later post can land inside the window) reverts
   `IncompleteTranscript` unless `revealLogComplete` holds at that moment, and stores
   `attemptDigest = keccak256("onyx-attempt-record-v1" ‖ round ‖ commitOpen ‖ commitClose ‖
   revealClose ‖ result ‖ keccak256(entries))`; `attemptMatches(…)` compares. The salted
   per-reveal binding is each entry's commit: a hash of the observation with the observer's own
   OS salt (amendment 7 item 5). For a non-value round the record publishes nothing a value can
   be enumerated from; a record of a VALUE round could be (its result hash is an unsalted function
   of the value, SIG-R1-F2, review C4-R1-6), so the coordinator records only non-value rounds.
   The record pins, before the round is served, WHICH observers the outcome is computed from and
   the outcome itself: (a) a sealed round can no longer be served one way to one client and
   another way later; (b) an observer with a sealed post that the record LISTS cannot be marked
   unrevealed (the chain refuses the record); that the record lists every drawn observer, in a
   window equal to the round's config, for the round id the client recomputes, is the CLIENT's
   check: the chain does not know the drawn set, the window or the round id, so a record that
   leaves an observer's entry out, or shifts `revealClose` before a post, lands on chain and is
   refused by clients (review C4-R1-2; the chain only requires strictly ascending entries, so no
   observer twice); likewise `recordObservation` does not require a record or
   `revealLogComplete`, clients do; (c) when the
   round is unsealed (the fact closes), the client recomputes the entries and the result from
   the reveals and must find the record: a round that listed a withheld reveal as revealed is
   then a PROOF on chain (record + posts + reveals), not a disagreement.
8. **What stays open, stated exactly.**
   (a) REVLOG-1 is CLOSED for observers that post sealed reveals in the window: the coordinator
   no longer decides receipts, and dropping a post makes `revealLogComplete` false for the true
   entries and window, so clients refuse the round (value round or attempt; the chain refuses a
   record only when the dropped observer is listed with status 0, item 7b). It stays OPEN for
   observers that
   reveal only through the spool, and for draws announced without a reveal key.
   (b) Early view: the coordinator can decrypt each post as it lands, before `reveal_close`. It
   cannot drop or change a post, but it can tell a colluding drawn observer that has not posted
   yet what the others answered, and that observer can then withhold its own reveal (the
   last-revealer problem of commit-reveal). A withheld reveal only removes the colluder's own
   vote (never slashed, SIG-R1-F1): it can turn a value into `NoResult` (one of the subject's 3
   draws), and for a numeric feed it can move the lower median within the band, so steer the
   settled value inside the tolerance and change which other observers are judged agreeing or
   outliers (review C4-R1-4). Closed by item 9 (time-lock), not built.
   (c) REVLOG-5 is REDUCED, not closed: while sealed, the outcome is still not recomputable by a
   client (no value is shown); the record makes it binding and provable at unseal (item 7). A
   coordinator that never closes the fact never unseals; item 9's time-lock to a fixed date is
   the coordinator-independent unseal, not built.
   (d) The ciphertexts are public for good: whoever later learns the reveal key reads every
   value ever posted to it (rotate it per draw to limit that; the snapshot allows it).
   (e) A drawn observer can grief by posting garbage: its slot is dismissed with a proof (costs
   the coordinator one proof per post, the observer gas); it is not slashed yet (item 10).
   (f) Sequencer: a post held past `reveal_close` by the sequencer does not count (REVLOG-2's
   class). (g) The intake needs a finalized block past `reveal_close` (about 20 more minutes on
   Base Sepolia, amendment 8 item 4b). (h) A dismissal publishes `S`, so the dismissed post's
   plaintext becomes public: when it is a signed reveal that opens its commit but fails the
   feed's checks, that observer's value is published (review C4-R1-5; its own doing, and only a
   value its feed refuses). (i) The registry keeps no observer signature of a post: the
   coordinator and clients open and check dismissals from chain state (`open_posted`,
   `check_dismissal_posted`, review C4-R1-3), trusting the registry's signature check at post
   time. The KEY PROOF is kept and re-checked before any dismissal (review C4-R3-2): the
   coordinator's dismissal publishes `S = k·R` for whatever `R` its node reports, so a node
   lying about chain state could otherwise have it open another observer's post (the C4-R1-1
   oracle without touching the chain); a post whose stored proof fails is never opened.
9. **Later layer (designed, ROADMAP C12).** Encrypt the sealed post to BOTH the coordinator's key
   and a drand quicknet round at `reveal_close` (tlock: Boneh-Franklin IBE under the League of
   Entropy's threshold BLS; `api.drand.sh` needs no account; Rust `tlock` crate, a third-party
   port, needs its own dependency review): `ct = tlock(round_at(reveal_close), ECIES(K, plain))`.
   Before `reveal_close` nobody opens anything (closes (b)); after it anyone strips the outer
   layer but only the coordinator opens the inner one (values stay private). A second time-lock
   copy of the inner key to a fixed far date (the subject's last observable day) would unseal
   every round without the coordinator (closes (c)), at the price of publishing every value then.
   Outer-layer garbage is checkable by anyone after the drand round; the inner layer keeps item 5.
10. **Level-2 disputes (designed, ROADMAP C13; not built).** Three self-contained proofs, all
   checkable from chain data plus what a served transcript carries: (i) a FALSE DISMISSAL cannot
   be made (item 5), so no dispute is needed for it; a missing dismissal is a client refusal;
   (ii) GARBAGE: a post plus its dismissal (`S`, DLEQ) proves the observer posted garbage; on
   chain, the DLEQ needs secp256k1 point arithmetic the EVM lacks (one `ecrecover` checks one
   `a·G + b·P`, the two-base check needs two general multiplications), so the verdict is either a
   Solidity secp256k1 library (several hundred thousand gas) or a panel vote (ADR-0021/0024);
   (iii) BAD AGGREGATION at unseal: an attempt record plus the reveals that open its listed
   commits, whose aggregation is not its result. Each slashes a bond: observers' bonds need the
   pool to come from an on-chain bond book (L2-F2 residual c), and the coordinator needs a
   settlement bond of its own. Until both exist, a proof leads to a client refusing the fact,
   as today.
11. **Not changed.** Money markets stay at levels 1 and 3 (`MarketPolicy`); no money path takes
   level 2. The spool, receipts and `transcriptComplete` stay for observers that do not seal.
   The contract is not deployed: the lead redeploys `OnyxRegistry` (no proxies).
12. **Built in this loop (and not).** Built: the registry (reveal key, sealed posts,
   `revealLogComplete`, attempt records), `onyx_coordinator::sealed` (encryption, the key
   proof and post signature, opening from chain state, dismissal proofs and their check, the attempt digest, the Rust mirror of
   the on-chain rules) and Rust↔Solidity vectors (`fixtures/registry/sealed-reveals.json`).
   Not built (ROADMAP C11): wiring into the round book, the live coordinator and observer
   processes (`DrawChain` reads, `CastChain` sends), the served transcript (`dismissals`, the
   record) and the Rust and web clients.
13. **Costs and tests.** `postSealedReveal` 271,992 gas (249,582 before the key proof was kept,
   review C4-R3-2) for a 154-byte ciphertext (a numeric
   reveal; CORRECTED by amendment 12 item 10: that figure was measured with an all-zero
   ciphertext, whose storage words cost no fresh-slot writes; a pseudorandom 154-byte post, as
   a real one is, costs 375,406 execution gas with the registry cold,
   `OnyxRegistrySealedReveals.t.sol::test_GasOfTimeLockedSizedPosts`), `revealLogComplete` 33,139 gas and `recordAttempt` 91,093 gas for 3 observers
   (`OnyxRegistrySealedReveals.t.sol::test_GasOfSealedPosts`); `OnyxRegistry` runtime 13,604 B to
   19,580 B (limit 24,576 B). Tests: `sealed_reveals::*` (a real round end to end against the
   simulated registry, each attack of THREATS C4-1..C4-9, C4-R1-*, C4-R2-* and C4-R3-*), `sealed::tests::*` (DLEQ and key proofs over 64 keys,
   context binding), forge `OnyxRegistrySealedReveals.t.sol` (rules, fuzzed texts, Rust vectors
   `fixtures/registry/sealed-reveals.json`) and `invariant_SealedPostsAndRecordsAreWriteOnce`.

## Amendment 11 (sealed reveals wired: processes, round book, served rounds, clients; ROADMAP C11, 2026-10-17)
1. **Why.** Amendment 10 built the protocol and the registry; nothing used it (item 12). This
   wires it through every party, with the client checks amendment 10 item 7b leaves to clients
   (review C4-R1-2). Decisions taken here, each the smallest that closes the named threat.
2. **Reveal key.** `keyfile::RevealKey`: a 0600 key file of its own, refused when it is the
   receipt key, the coordinator's transaction key, the registry's owner (read from chain), the
   reporter or a pool member, and, as a public key (the test receipt key, an anvil dev key), on
   any chain but a local anvil (THREATS C11-10). There is no public test reveal key: a coordinator
   without `--reveal-key` announces draws with none, as before: `CastChain::sync_keys` clears a key
   an earlier run set (`OnyxRegistry.clearRevealKey`, added here, review C11-R1-2; runtime
   19,580 to 19,803 B). `CastChain::sync_keys` sets the
   receipt signer and the reveal key (`setReceiptSigner`, `setRevealKey`) before the runner draws:
   each send mines a block and a draw's beacon is only `DRAW_LEAD` blocks ahead, so rotating both
   inside `announce` made the announcement land late on a 1 s anvil (C11-12, found by the
   rehearsal); `announce` calls it again, a no-op unless a key changed.
3. **Observer** (`examples/level2-observer.rs`). Once the chain is past `commit_close` and its own
   commit is on chain inside the phase (the B2-R1-M1 rule), an observer whose draw has a reveal key
   (`drawRevealKey`) seals its signed reveal with a fresh ephemeral key (`sealed::seal_random`),
   saves the post (0600) before sending, posts it itself (`postSealedReveal`), and checks its slot
   holds exactly its post with a block time in the reveal phase (exit 0; outside it, exit 3). It
   writes nothing to the spool and waits for no receipt: the post is the receipt. A draw without
   a key keeps the spool and the receipt. An observer started before a re-draw is announced reads
   the earlier draw, does not commit to it and keeps reading (a re-draw's rehearsal).
4. **Coordinator** (`observe::finish_reveals_sealed`, used by the fiscaldata runner). When
   `drawRevealKey` is set the draw is SEALED ONLY: no spool reveal is taken or receipted (review
   C11-R1-3: a reveal stamped by the coordinator's own clock, or a late post it decrypted, would
   let it choose after seeing every value whom to include); the run's reveal key must be that one (a draw announced before a rotation is finished
   with its own key; another key stops the run before any read, C11-9); then ONCE, at the chain's
   finalized block whose time is past `reveal_close` (`ChainWait::reveal_phase_over`; never
   earlier, C11-1), every drawn observer's slot is read at that block (`sealedRevealOf` and
   `sealedKeyProofOf`, through the strict ABI reader the clients use), every stored key proof is
   re-checked before anything is opened (C4-R3-2, C11-8), each post in the reveal phase of an
   observer whose reveal the round does not already hold is opened (`open_posted`): a valid reveal
   is taken as arriving at the post's block time, anything else is dismissed with a proof
   (`ObservationRound::absorb_sealed`, all or nothing). Before the read is kept the chain's own
   `revealLogComplete` must accept the round with it (a skipped slot, or a node that hides a post
   at the read block, keeps nothing: C11-2); the round is then saved with the book and the last
   check is `revealLogComplete`.
5. **Records.** The live coordinator records every round it settles before writing it to the
   results store (`observe::record_round`, when the chain records rounds: `CastChain::record_rounds`,
   set by `level2-coordinator` only): a value with `recordObservation` (the live coordinator did
   not record values before), a round without a value with `recordAttempt` once the chain's
   finalized time is past `reveal_close`, with the round's own query (every drawn observer, the
   config's window, the round id, the result hash). Both are idempotent (a retry finds the record).
6. **Round book.** Version 6: an unfinished round carries its sealed snapshot (the read block, the
   reveal key, every slot read, the dismissals), re-checked when the book is read back (block past
   `reveal_close`, drawn slots in order, key proofs, every dismissal against the key, every
   in-phase post revealed or dismissed); versions 3 to 5 still load (C11-11).
7. **Served rounds.** A round with sealed posts, dismissals or a record carries `sealed`
   (`observations::SealedLog`: `revealKey`, the in-phase `posts`, `dismissals`, `record`;
   docs/spec/delivery.md "Sealed reveals"); the key is left out otherwise, so stores and answers
   from before are unchanged. The server checks every round's sealed log at load with the client's
   function (`observations::check_sealed_log`).
8. **Clients.** Off chain (Rust `observations::check`/`check_attempt`; the web page's mirror
   `apps/web/verify/src/sealed.rs`): every post a drawn observer's, in phase, its key proof valid;
   every dismissal checks and opens no valid reveal (a replayed or false one is refused, C11-3);
   every post revealed or dismissed; a served record the round's own digest. The query is the
   client's: its drawn set, the config's window, its recomputed round id; for a keyed draw every
   revealed (or listed) observer must have an in-phase post (sealed only, C11-R1-3). On chain
   (`client::sealed_calls`, `verify_sealed_onchain`, a required argument of both on-chain
   verifiers, C11-7): `drawRevealKey` must be the served key (C11-5), `revealLogComplete` true,
   every served post the chain's (C11-4: the registry keeps no observer signature, so only the
   chain can tell a server's own garbage under an observer's name from the real post), and
   `attemptMatches` true for every round served sealed and every round without a value of a keyed
   draw (a record for every sealed round; C11-6). A round without a value of a draw WITHOUT a key,
   served with its reveals, needs no record (its reveals are public and `transcriptComplete`
   covers its receipts), so such rounds published before C11 still verify, on a registry with
   the C4 calls (a registry from before C4 answers none of them, so no level-2 answer verifies on
   chain there); a round served sealed without a record (a failed attempt, a runner that does not
   record) is refused (review C11-R1-6). The web page cannot ask the
   rehearsal's chain; it requires the summary's recorded answers to be for exactly the calls it
   built, and true (consistency only, C11-14).
9. **Rehearsed** (`scripts/level2-sealed.sh`, one machine, a local anvil, seven observer processes
   with distinct anvil dev keys, a fresh reveal key and receipt key): draw 0 with every observer
   posting garbage (five dismissed with proofs, noResult, recorded with `recordAttempt`), draw 1
   of the same fact with honest sealed posts (five opened, the value settled and recorded with
   `recordObservation`), then the Rust client checks the served answer against the anvil and
   refuses it doctored three ways.
10. **Not changed, stated plainly.** Money markets stay at levels 1 and 3. The coordinator still
   opens each post as it lands (C4-7, C12 time-lock). Spool observers and draws without a key keep
   REVLOG-1. The EDGAR runner opens no sealed post (C30). Nothing is deployed: the lead redeploys
   `OnyxRegistry` (C31). The coordinator trusts its own node for the slots it reads: a lying node
   can make it dismiss a substituted post (the key proof does not cover the ciphertext; review
   C11-R1-1, C31 binds it, C34 cross-checks a second RPC) and one lying read is kept (C11-R1-4). The buyer-facing REVLOG-1 text does not yet distinguish sealed draws
   (C33). One host, one operator (THREATS L2-SRC-1).
11. **Review C11-R1** (fresh adversarial reviewer, 2026-10-17; THREATS C11-R1-*): 3 Medium (2
   fixed: keyed draws sealed only, `clearRevealKey`; the node-trust finding to C31 and C34), Lows
   fixed or stated (an observer restarted after commit close now reaches its post; the value-round
   post check tested; the web mirror's limits, C32), Infos accepted.
12. **Tests.** `c11_sealed::*` (the live runner with independent sealed observers on the
   simulated registry, the served store and the client, each C11 attack), `keyfile::tests::c11_the_reveal_key_is_a_key_of_its_own`,
   `sealed::tests::c11_abi_returns_round_trip_and_are_strict`, the web mirrors
   (`sealed_reveals::*` in apps/web/verify) and the anvil rehearsal.
13. **Review C11-R2** (second fresh adversarial reviewer, 2026-10-17; THREATS C11-R2-*): clearing or
   rotating the reveal key mid-draw changes nothing for that draw (snapshotted at `announceDraw`),
   but a coordinator chooses per draw whether it is sealed: a keyless draw keeps REVLOG-1 (C31
   makes the registry refuse one, C33 states it per round); `CastChain::sealed_post` refuses
   `--finality safe` as `draw_of_final` does; a keyed round served with its reveals still needs its
   record (test added).

## Amendment 12 (time-lock layer for sealed reveals: dependency review and design; ROADMAP C12, 2026-10-17)
1. **Why.** Amendment 10 item 8b (THREATS C4-7): the coordinator opens each sealed post as it
   lands, before `reveal_close`, and can tell a colluding drawn observer what the others answered
   (the last-revealer problem). Item 9 designed the cure: lock each post to a drand quicknet round
   at `reveal_close` as well. ROADMAP C12 asked for a dependency review first.
2. **Dependency review (RESEARCH.md "Time-lock crates").** No crate is acceptable today. `tlock`
   0.0.10 (with `tlock_age` and the tools built on them, the only drand-tlock crates a crates.io
   search for "tlock", "drand" and "timelock" found on 2026-10-17) is unaudited by its own README, PANICS on a malformed ciphertext
   (`assert_eq!` in `ibe::decrypt`; anyone can post one, so every coordinator and client that
   opens chain posts could be crashed) and strips trailing zero bytes from what it decrypts;
   `drand_core` is an HTTP client; `drand-verify` verifies beacons only; `timelock` is another
   scheme. The pure-Rust BLS12-381 libraries say they are not reviewed (arkworks: "NOT ready for
   production use"; zkcrypto `bls12_381`: "has not been reviewed or audited", hash-to-curve
   experimental); `blst` has a published NCC Group audit (2021) and is C behind FFI. Review
   C12-R2 corrected the first reading of `blst` (item 14): its Rust binding offers every
   primitive tlock needs as safe functions and builds for `wasm32` with clang, as `secp256k1`
   already does here, so our own minimal IBE on it is an acceptable route, still to be written
   and reviewed (C50). Licences would all pass `deny.toml`; robustness and review do not. **Decision: stop at design and a tested
   composition with a mock beacon (`onyx_coordinator::timelock`); nothing ships, nothing calls
   it.** The implementation is ROADMAP C50.
3. **Composition (an "AND" of two keys, not two nested encryptions).** For a round with reveal
   close `t`: `ρ = lock_round(t)`, the first quicknet round published at or after
   `t + LOCK_MARGIN_SECS` (60 s). The observer draws `r`, `τ` (16 bytes) and the IBE randomness
   `σ` (16 bytes) and posts amendment 10's post whose ciphertext is
   `TL ‖ body`: `TL = tlock(ρ, τ)` (drand/tlock's 128 bytes: `U` in G2, 96; `V`, 16; `W`, 16) and
   `body = padded plain XOR keccak stream of k`, with
   `k = keccak256("onyx-sealed-reveal-key-v2" ‖ S ‖ R ‖ keccak256(TL) ‖ τ ‖ ρ (8 bytes BE) ‖ chain
   hash ‖ round ‖ attempt (4 bytes BE) ‖ observer)` and `S = r·K` as before (`k` hashes both
   "ciphertexts", `R` and `TL`, as KEM combiners do; review C12-R1-7). Why this order: the time-lock
   guards only a 16-byte key share, so the IBE carries drand's native 16-byte message (no
   format of our own), the ECIES part and its DLEQ dismissals stay as they are, and a client
   checks the time-lock part of a dismissal without the beacon signature in the common case
   (item 5; re-encryption still computes one pairing per round, review C12-R1-3).
   Before round `ρ` nobody holds `τ`, the coordinator included (closes C4-7 under drand's
   threshold assumption); after it anyone opens `TL`, but `k` still needs `S`, which only the
   coordinator and the observer hold, so values stay private as the paid API needs. The key
   proof and the observer's signature are amendment 10's; the signature covers the whole
   ciphertext, `TL` included.
4. **Binding the round and the chain.** `ρ` is a function of `reveal_close` alone, which the
   attempt record binds for rounds without a value (`revealClose` in `attemptDigest`, amendment 10
   item 7; value rounds have no record) and the draw's config binds at seal time, so no new field
   names it on chain; it is also in `k`. The observer must take `reveal_close` from the draw it
   reads on chain (C51), and refuses to seal when the lock round is out or due within
   `SEAL_SLACK_SECS` (30 s) by its own clock (`RoundTooSoon`; review C12-R1-4: a coordinator
   serving an earlier `reveal_close` would otherwise read the post at once). The chain (hash, public key, genesis, period) is a
   constant in code (`QUICKNET_*`), never an argument a coordinator or the registry supplies: a
   served or on-chain key could be one the coordinator holds (THREATS C12-3). A post locked to
   another round or chain does not open under `ρ`'s signature and is dismissed (C12-2, C12-3):
   only its own poster loses (an observer that lets the coordinator read its own value early
   could have told it anyway). Changing the beacon chain is a protocol version change.
5. **Opening and dismissal.** The coordinator opens a post only with `ρ`'s signature, verified
   against the pinned key (`open_timelocked`; `BeaconMissing` before it exists, `BadBeacon` for
   another round's or chain's). A valid reveal is taken; anything else is dismissed with amendment
   10's `S` and DLEQ proof plus the time-lock opening `(σ, τ)` when `TL` opens
   (`TimedDismissal`). A client re-encrypts `τ` with `σ` to `ρ` and compares with `TL` (encryption
   is deterministic given them; no beacon signature needed), derives `k` and checks the body is
   not a valid reveal (`check_timelocked_dismissal`). Both sides keep an opening only when it
   re-encrypts to `TL` byte for byte (`open_canonical`; review C12-R1-2: an implementation that
   accepts a non-canonical point encoding would otherwise let a griefer post a time-lock that opens
   for the coordinator and fails every client's re-encryption, so that no dismissal is accepted). Only a post whose `TL` does not open needs
   `ρ`'s signature to check; a post shorter than `TL` plus one byte is garbage on its face.
6. **Liveness: drand late or down, stated plainly.** There is NO path that opens a post without
   `ρ`'s signature. If drand is late, the round waits (a signature is permanent, so a late round
   loses nothing: test `timelock::tests::c12_4_without_the_beacon_nothing_opens_and_a_late_one_loses_nothing`);
   the coordinator already waits about 20 minutes past `reveal_close` for a finalized read, which
   covers a short delay. If the round never comes, the posts stay sealed for everyone and the
   round cannot settle or be recorded (`revealLogComplete` needs every in-phase post revealed or
   dismissed): sealed level-2 rounds then depend on the League of Entropy's liveness. A
   coordinator-openable fallback copy (or a coordinator switch to amendment 10 posts after the
   fact) would bring back the early view silently, so neither exists; how a draw abandoned for a
   beacon outage is re-drawn in a way clients can check is ROADMAP C53. The trade-off: privacy of
   the reveal phase against the coordinator is bought with a liveness dependency on drand.
7. **Clock margin.** Posts are timed by block time, drand by wall-clock time. If the chain's
   clock lags the wall clock by more than `LOCK_MARGIN_SECS`, `ρ` is published while posts can
   still land, and the coordinator regains an early view of those posts. The lag has no bound:
   an OP-stack sequencer back from an outage builds the missed blocks at past timestamps, so an
   outage spanning `reveal_close` lets a colluding observer post, in a catch-up block stamped
   before `reveal_close`, after `ρ` is out and every honest value is readable by the coordinator
   (review C12-R1-1; THREATS C12-6, Medium; L3C-12 is the other direction, block time AHEAD).
   C53 decides the rule (for example: a round counts only if the L1 batch holding its last
   in-phase block landed before `ρ`'s time, or rounds that overlap a sequencer gap are abandoned).
8. **Far-date unseal (designed, ROADMAP C52; not built).** REVLOG-5's residual: a coordinator that
   never closes a fact never unseals. Variant: the body key becomes a fresh 16-byte `κ`; the post
   carries `κ` wrapped by `KDF(S, τ, …)` (16 bytes) and a second `tlock(ρ_far, κ)` (128 bytes),
   with `ρ_far` the round at the subject's last observable day; the plaintext carries `σ_far` so
   the coordinator checks the far copy at intake (re-encryption) and dismisses a bad one. At
   `ρ_far` anyone reads every value of the round without the coordinator, at the price of
   publishing them all then. Cost: 144 more bytes per post. (BUILT by amendment 15, ROADMAP C52;
   corrected there: `σ_far` is derived from `κ`, not carried in the plaintext, which would have
   cost 16 bytes more than the 144 counted here.)
9. **Contract and wiring (ROADMAP C51; not built).** The registry is unchanged here. Shipping
   needs: `MAX_SEALED_BYTES` 1,166 -> 1,294 (today an exact feed fits only up to `max_len` 896
   with the time-lock; test `timelock::tests::c12_sizes_against_the_registry_cap`); a registry
   (or per-draw snapshot) in which every sealed post is time-locked, so a coordinator cannot
   announce draws whose posts it opens early (the C11-R2-1 downgrade class); the observer, the
   coordinator's wait for `ρ`, the round book, served dismissals with openings, and the Rust and
   web clients (the web page needs BLS12-381 in wasm, or recorded consistency as in C11-14).
10. **Gas and size, measured** (`OnyxRegistrySealedReveals.t.sol::test_GasOfTimeLockedSizedPosts`,
   the unchanged registry, pseudorandom ciphertexts as real ones are): a numeric post goes from
   154 to 282 bytes and `postSealedReveal` from 375,406 to 466,316 gas (+90,910, +24%), execution
   gas of the call only (no 21,000 base, no calldata cost), each post measured with the
   registry's account and storage marked cold (`vm.cool`), as a post in a transaction of its own
   finds them (review C12-R2; the first figures, 373,281 and 464,184, ran with slots the test's
   setup had left warm); the test asserts ranges. The same test posts 154 zero bytes for 274,083:
   amendment 10 item 13's 271,992 was measured with an all-zero ciphertext, whose storage words
   cost no fresh-slot writes; a real 154-byte post costs 375,406 (THREATS C12-8, C12-R2-5).
11. **Tests** (`timelock::tests::*`, mock beacon with fixed test keys, no network):
   lock-round arithmetic against drand's served round; a post opens only with `ρ`'s signature;
   the coordinator's ECDH alone and an earlier round's signature open nothing (C12-1); round
   substitution (C12-2); another chain (C12-3); a beacon that never or late arrives (C12-4);
   malleability across the layers (C12-5); garbage dismissed with an opening any client checks;
   sizes against the cap; a non-canonical time-lock (C12-R1-2) and a round already out at seal
   time (C12-R1-4). Break-the-code: 5 mutations (no canonical check, no seal freshness, no
   re-encryption, floor instead of ceiling for the round, `τ` left out of `k`), each caught (the
   last only after a test was added). Opt-in, network: `c12_live_quicknet_matches_the_pinned_chain`
   (`ONYX_DRAND_LIVE=1`, api.drand.sh, no account; never in check-all).
12. **Review C12-R1** (fresh adversarial reviewer, read-only, 2026-10-17; THREATS C12-R1-*):
   0 High; 2 Medium (clock lag unbounded under a sequencer outage: rated Medium, to C53; a
   non-canonical time-lock as a round veto: fixed, `open_canonical`); Lows fixed (seal freshness,
   `TL` in `k`, the pairing overclaim, test honesty of C12-5) or pinned (mock-only properties, to
   C50); Infos fixed (gas ranges, the crates.io search stated).
13. **Not changed.** Money markets stay at levels 1 and 3. Amendment 10 posts, the registry and
   the wiring of amendment 11 are unchanged: C4-7 stays OPEN until C50 and C51 ship.
14. **Review C12-R2** (fresh adversarial reviewer, cryptography, 2026-10-17; THREATS C12-R2-*).
   (a) Crate claims re-read in each crate's source: `tlock`'s panic is the `assert_eq!` on `U`
   (its `xor` panic is unreachable from ciphertext bytes); its trailing-zero bug is undone by
   padding a fixed 16-byte `tau`; it also exposes no `sigma`, which item 5's openings need.
   `blst`'s Rust binding needs no `unsafe` in Onyx and builds for `wasm32` with clang, the same
   pattern as `secp256k1` (libsecp256k1 behind `secp256k1-sys`) in `sealed` and the web
   verifier: the acceptable route item 2 missed, own IBE on `blst`'s safe API, is C50's. The Go
   reference drand/tlock states a Kudelski review: the source of C50's vectors. (b) Composition
   holds: an opening is unique (`U` fixes `r`, `V` then `sigma`, `W` then `tau`), so no two
   openings re-encrypt to one `TL` and the coordinator cannot choose one; with `open_canonical`,
   whether a time-lock opens is a function of its bytes for every correct implementation; what
   is left is two implementations of `lock` disagreeing (Rust and wasm) or a bug both sides
   share (an unpadded trailing-zero `tau` would dismiss 1 in 256 honest posts silently), C50's
   vectors. `k`'s
   transcript is tested part by part; it holds no registry address or EVM chain id, and
   `round_id` binds the chain only through the beacon block hash (C12-R2-3, Low, TODO C31). (c)
   Block time AHEAD of the wall clock (L3C-12, up to 1,800 s) cannot open a post early (the lock
   round lands further after the last post, only the coordinator waits longer); block time BEHIND
   is item 7's lag, which the sequencer sets (catch-up blocks at past timestamps), so C53 needs
   a checkable rule, not a measured margin. (d) Gas re-measured cold (item 10).

## Amendment 13 (the time-lock scheme: drand/tlock's IBE on blst; ROADMAP C50, 2026-10-17)
1. **Why.** Amendment 12 designed the layer against a `TimeLock` trait and a mock beacon because
   no tlock crate was acceptable; review C12-R2 named the route (own IBE on `blst`'s safe API).
2. **Decision: `crates/onyx-tlock`** implements drand/tlock's quicknet scheme exactly as Go
   v1.2.0 runs it (`ibe.EncryptCCAonG2` of kyber v1.3.1; RESEARCH.md "Time-lock interop
   vectors"): identity `sha256(round_be8)`, hash to G1 with RFC 9380's G1 tag, `U = r·G2`,
   drand's H2, H3 and H4, kilic's GT byte order, ciphertext `U ‖ V ‖ W` (128 bytes). Safe Rust
   only (`#![forbid(unsafe_code)]`) on `blst` 0.3.17, pinned, reviewed in
   docs/audit/2026-10-17-blst.md, admitted by `deny.toml` for this crate only. `lock(round, tau,
   sigma)` is deterministic and `open` returns `(sigma, tau)`, as amendment 12 item 5 needs.
3. **Robustness.** Every input is parsed and checked, never trusted: keys, signatures and `U`
   must be canonical compressed points (re-compress to the same bytes), in the prime-order
   subgroup and not infinity; `open` verifies the round signature against the key with two
   pairings, then the Fujisaki-Okamoto check compares `r·G2` with `U` byte for byte, so whatever
   opens re-encrypts to itself and nothing else opens (THREATS C50-*). Errors, never panics
   (fuzz target `tlock_open`, a seeded sweep in `tests/robust.rs`).
4. **Wiring.** `onyx_coordinator::timelock::QuicknetTimeLock` implements the trait on the pinned
   key; `TimeLock::lock` now returns `Option` (no ciphertext exists for H3's zero scalar, about
   2^-255; the observer draws again, `NoLock`). The mock beacon stays `#[cfg(test)]` for tests on
   a test clock; `c50_*` tests run the composition with the real scheme and published quicknet
   signatures. NOTHING ships yet: no process, registry path or client calls the layer, so C4-7
   stays OPEN until ROADMAP C51 wires it (registry cap and mode, observer, coordinator wait, book,
   clients) and the web verifier links `onyx-tlock` (wasm32 is built and tested, item 5).
5. **Interop and targets.** Go-made and tlock-js vectors (quicknet and quicknet-t) decrypt under
   published drand signatures, prove to be age's file key, and re-encrypt to identical bytes, on
   native (`cargo test -p onyx-tlock`) and on wasm32 built with clang and run in node
   (`scripts/check-tlock-wasm.sh`, in `scripts/check-all.sh`). `tau` values ending in zero bytes
   are covered by Onyx-made vectors only (no published Go vector has one): C70.
6. **Review.** Self-review and adversarial pass by the implementer (THREATS C50-*); a fresh
   cryptography review of the IBE is still required before C50 closes.

## Amendment 14 (time-locked sealed posts wired; ROADMAP C51, 2026-10-17)
1. **Why.** Amendments 12 and 13 designed and built the time-lock layer; nothing used it, so
   THREATS C4-7 (the coordinator opens each post as it lands) stayed open. This wires it through
   the registry, the observer, the coordinator, the round book, served rounds and both clients.
2. **Mode, no downgrade (THREATS C11-R2-1 class).** `OnyxRegistry.requireTimeLock()` (the owner or
   the coordinator) sets `timeLockRequired`; there is NO function that clears it, and it survives
   coordinator changes (it is not bound to a term). `announceDraw` snapshots it per draw into
   `drawTimeLocked(fact, attempt)` like the reveal key, and with it on refuses a draw without a
   reveal key of the coordinator's current term (`NoRevealKey`), so a coordinator cannot clear
   its key, change epoch or run keyless to get a draw whose posts it reads early or a spool draw.
   The pre-announcement path (draws announced BEFORE the mode, possibly for later windows, with
   far beacon heights) is closed too: once the mode is on, `recordObservation` and
   `recordAttempt` refuse every draw that is not time-locked (`NotTimeLocked`), so such a draw
   never yields a recorded value or a record clients accept; the fact takes a new, time-locked
   draw. A time-locked draw's posts must be longer than the 128-byte time-lock (`BadCiphertext`).
   Chosen over a constructor flag (the lead can switch an existing deployment on once) and over
   a per-draw argument (the coordinator would choose it). Clients still accept a draw that is
   not time-locked (announced before the mode, or on a registry that never set it) and say so per
   round (`SealedChecked::time_locked`); refusing them is a client cutover with the redeploy
   (C31) and the buyer text (C33).
3. **Cap.** `MAX_SEALED_BYTES` 1,166 -> 1,294 = 128 + 142 + 1,024 (`sealed::MAX_SEALED_BYTES` =
   `MAX_PLAIN_BYTES` + `TIME_LOCK_BYTES`), so an exact answer of the largest value seals
   time-locked (THREATS C12-7).
4. **Observer.** `drawTimeLocked` true: `timelock::seal_timelocked_random` with
   `QuicknetTimeLock` to the lock round of `reveal_close` taken from the config the on-chain draw
   commits to (checked by `drawn_round`; review C12-R1-4's wiring), refusing a round out or due
   within 30 s by its own clock. Otherwise amendment 10 posts, as before.
5. **Coordinator.** `finish_reveals_sealed` asks `drawTimeLocked`. For a time-locked draw, after
   the finalized block past `reveal_close` and BEFORE any slot is read, it waits for the lock
   round's signature (`ChainWait::lock_round_signature`) from its `DrawChain::beacon` source
   (`--beacon drand`: `timelock::DrandHttp`, drand's public relays over HTTPS through `curl -q`
   with a generic User-Agent and no account; `--beacon fixture:FILE`: published rounds from a
   file, local anvil only) and keeps it only when it verifies against the pinned quicknet key.
   Missing, malformed, forged or another round's: the round WAITS (an unfinished round in the
   book, nothing read or opened, the same draw finished by a later run); there is no other path
   (THREATS C12-4; abandonment is C53). Then every in-phase post is opened through
   `open_timelocked` (`absorb_sealed_locked`): a valid reveal is taken, anything else dismissed
   with `S`, the DLEQ proof and, when its time-lock opened canonically, the opening `(sigma,
   tau)`. A plain amendment 10 post or one locked to another round does not open under the lock
   round and is dismissed (its poster's own loss). A chain view that shows a draw's reveal key but
   cannot answer `drawTimeLocked` stops the run (review C51-R1-3: opening it amendment 10 style
   would dismiss every honest post and record that round for good). `DrandHttp` keeps a relay's
   answer only when it verifies, else asks the next (review C51-R1-2), sends the User-Agent
   `curl` and refuses answers over 4 KiB.
6. **Book and served rounds.** Book version 7: the sealed snapshot carries
   `timelock::TimeLockRead` (lock round, signature, openings), re-checked at load (lock round of
   the window, signature, every dismissal as a time-locked one); versions 3 to 6 still load.
   Served `sealed.timeLock` = `{lockRound, beacon, openings}` (docs/spec/delivery.md), absent for a
   draw that is not time-locked.
7. **Clients.** Rust (`observations::check_sealed_log`): `lockRound` is the client's own, the
   signature verifies, every post is longer than the time-lock, each opening is of a dismissal,
   each dismissal passes `check_timelocked_dismissal` (exact re-encryption of the opening, or no
   canonical opening under the signature); on chain `drawTimeLocked` must equal what is served
   (`client::verify_sealed_onchain`). Web (`apps/web/verify/src/sealed.rs`, wasm32 with
   `onyx-tlock`): the same checks, with the chain's `drawTimeLocked` as a recorded answer for the
   page's own call (consistency only, as C11-14).
8. **Rehearsed** (`scripts/level2-timelocked.sh`): a local anvil started in the past
   (`--timestamp`), every process on `--clock anvil` (the system clock shifted to the anvil's;
   refused off chain 31337) and a fixture of published quicknet rounds
   (`fixtures/tlock/drand/quicknet-c51-rounds.json`): `requireTimeLock`, draw 0 with garbage
   behind valid and invalid time-locks, first with the lock round missing from the beacon (the
   coordinator waits past reveal close, nothing opened or recorded), then finished on the same
   book (five dismissals, openings for the valid time-locks, recorded); draw 1 honest (five
   time-locked reveals opened with the verified signature, value recorded); the Rust client
   checks it on the anvil and refuses it doctored. The live drand relays are exercised only by
   the opt-in `timelock::tests::c51_live_drand_serves_the_published_lock_round`
   (`ONYX_DRAND_LIVE=1`), never by check-all.
9. **What stays open, stated plainly.** Under drand's threshold assumption and with block time
   not lagging the wall clock by more than 60 s, the coordinator no longer reads a time-locked
   post before the lock round (C4-7 closed for time-locked draws). Open: block time lagging under
   a sequencer outage (C12-6, rule C53), a lock round that never comes (rounds wait; abandonment
   C53), draws that are not time-locked (a registry that never sets the mode, or draws announced
   before it: clients accept them, C31/C33), nothing deployed (the lead redeploys with C31), the
   far-date unseal (C52), the Go cross-check of zero-tail time-locks (C70).
10. **Costs.** `postSealedReveal` of 282 pseudorandom bytes in a time-locked draw 466,354 gas
   cold (the length rule reads the draw's flag only for posts of 128 bytes or fewer);
   `recordAttempt` 91,093 -> 93,282 gas for 3 observers (the mode is read; packed with the reveal
   key's binder); `announceDraw` with the mode on writes one more slot (`drawTimeLocked`);
   runtime 20,466 B (limit 24,576 B; 19,803 B after amendment 11). Snapshot `.gas-snapshot`.
11. **Not changed.** Money markets stay at levels 1 and 3; no money path takes level 2. Amendment
   10 posts, the spool and receipts stay for draws that are not time-locked.
12. **Review C51-R1** (fresh adversarial reviewer, read-only, 2026-10-17; THREATS C51-R1-*): no
   early or wrong opening found; 1 Medium (the gas snapshot not yet regenerated at review time:
   regenerated before commit), 2 Lows fixed with tests (a lying relay stalled rounds; the
   `drawTimeLocked` default), 2 Infos fixed (User-Agent and answer size; a test comment).
13. **Tests.** Forge: `OnyxRegistrySealedReveals.t.sol` (cap, mode on/off, one way, keyed only,
   pre-mode records refused, post lengths fuzzed, gas) and
   `invariant_TimeLocksAreOneWayAndSnapshotted`; Rust: `c51_timelocked::*` (the live runner with
   real IBE and published signatures: settle, garbage of every kind, waits without the round,
   forged or wrong-round signatures, the book, the keyless refusal), `timelock::tests::c51_*`,
   `cast_chain::tests::c51_a_fixture_beacon_is_for_a_local_anvil_only`; web:
   `sealed_reveals::c51_*`. Break-the-code: 7 mutations (coordinator keeping an unverified
   signature, client skipping the served signature, `drawTimeLocked` or the lock round; the web
   page skipping re-encryption, the signature or `drawTimeLocked`), each caught (the second only
   after a test was added).
14. **Review C51-R2** (second fresh reviewer, cryptography and protocol, 2026-10-17; THREATS
   C51-R2-*). No early or wrong opening, no downgrade found. The registry's length rule is not a
   time-lock check (any ciphertext over 128 bytes lands; an amendment 10 numeric post is 154), and
   clients checked time-locks only on dismissals, so a coordinator could have served a plain post
   as a REVEAL unnoticed (C51-R2-1, Low: no early view of an honest post): both clients now
   require every revealed post's time-lock to open canonically under the verified lock-round
   signature (one opening per revealed post, no `S`). Downgrade paths re-checked with a test
   (same-block announce, `reopenDraws`, coordinator rotation: C51-R2-2); a version-6 book with a
   sealed round loads (test added, C51-R2-3). Liveness unchanged and stated: a lock round that
   never comes leaves the round unfinished with no bound and no other path (C53). Web verifier
   wasm (`--profile web`, before wasm-bindgen): 4,280,316 B on main, 4,395,469 B here (+115 kB,
   +37 kB gzip -9). Break-the-code: 8 mutations (no `drawTimeLocked` snapshot; pre-mode draws
   recorded; coordinator keeping an unverified signature, caught by the unit and the runner test;
   client skipping re-encryption; each client skipping the new revealed-post check; the page
   skipping re-encryption), each caught.

## Amendment 15 (far-date unseal copy; ROADMAP C52, 2026-10-18)
1. **Why.** THREATS REVLOG-5's residual (amendment 10 item 8c, amendment 12 item 8): while a fact
   is open its rounds without a value are served SEALED, their outcome pinned by the attempt
   record but recomputable only at unseal, and a coordinator that never closes the fact never
   unseals. PLAN q-C4-1 option (a), recommended there: a second time-lock of each post's body key
   to a fixed far date, so every round unseals without the coordinator, publishing its values
   then. Decided here (option (a)); the loop decides, per the owner's instruction.
2. **Post format (time-locked draws).** `post = TL ‖ TF ‖ w ‖ body`, every part fixed length but
   the body:
   - `TL = tlock(ρ, τ, σ)` as amendment 12 (128 bytes), `ρ = lock_round(reveal_close)`;
   - `κ`: a fresh 16-byte body key from the OS;
   - `TF = tlock(ρ_far, κ, far_sigma(κ))` (128 bytes), `far_sigma(κ) = keccak256("onyx-sealed-
     far-sigma-v1" ‖ κ ‖ ρ_far as 8 bytes BE)[..16]`;
   - `w = κ XOR keccak256("onyx-sealed-far-wrap-v1" ‖ S ‖ τ ‖ ctx)[..16]` (16 bytes), the body
     key wrapped for the coordinator: it needs `S = r·K` AND `τ`, amendment 12's AND;
   - `body = padded plain XOR keccak stream of k`, `k = keccak256("onyx-sealed-reveal-key-v3" ‖ κ
     ‖ ctx)`;
   - `ctx = R ‖ keccak256(TL) ‖ keccak256(TF) ‖ ρ ‖ ρ_far (8 bytes BE each) ‖ chain hash ‖ round
     ‖ attempt (4 bytes BE) ‖ observer` (both "ciphertexts" in both keys, as KEM combiners do).
   Before `ρ` nobody holds `τ`, before `ρ_far` nobody holds `κ` by the far path: the coordinator's
   early view stays closed (C4-7). After `ρ` only holders of `S` (the coordinator, the observer)
   unwrap `κ`; after `ρ_far` anyone opens `TF` and reads the body. The key proof and the
   observer's signature are amendment 10's and cover all of it. 272 bytes precede the body
   (128 in amendment 14): a numeric post is 426 bytes (282 before).
3. **Why `far_sigma` is derived (encrypt-with-hash).** Amendment 12 item 8 put `σ_far` in the
   plaintext so the coordinator could re-encrypt `TF`; that costs 16 more bytes and a new
   plaintext layout. Deriving it from `κ` gives the same check for 144 bytes: whoever holds `κ`
   recomputes `TF` exactly. It is Bellare, Boldyreva and O'Neill's encrypt-with-hash
   (deterministic encryption, secure in the random oracle model for high-entropy messages):
   `κ` is a fresh uniform 128-bit secret per post, used once, so `TF` hides it as a random `σ`
   would (RESEARCH.md "Far-date unseal"). Drand/tlock's decryption does not depend on how `σ` was
   chosen, so `TF` is an ordinary tlock ciphertext.
4. **The far round: the subject's last observable day.** `FeedTemplate::last_observable_day`
   (onyx-core): a dated subject's record day plus `max_calendar_days`, never past the calendar's
   coverage; a period subject's end plus `max_days`; none for a fixed subject, whose fact is one
   window. `latest_window_start` is the last second of that day (a fixed subject: its window
   start). `latest_reveal_close = latest_window_start + window_secs + MAX_COMMIT_GRACE_SECS
   (600) + MAX_REVEAL_SECS (86,400)` bounds every config `ObserveConfig::validate` accepts, so
   it bounds the reveal close of EVERY round (every attempt) of the fact. `unseal_time =
   latest_reveal_close + UNSEAL_MARGIN_SECS (86,400)`, and `ρ_far` is the first quicknet round at
   or after it (`LockRounds::of_round`). A pure function of the registered template (its id is on
   chain, `registerMetric`) and the spec: the observer, the coordinator and every client compute
   it themselves; a served `farRound` is only compared. Every round of a fact shares one far
   round; no round of it (value round or not) can take posts when its values become public, so publishing them helps
   no later round's observers copy. Examples: the Treasury feed (record date R, 10 days)
   unseals at R + 13 days, 00:15 UTC; EDGAR's period subjects (110..=730 days) about 622 days
   after their earliest round; a fixed subject about two days after its window.
5. **Intake (the coordinator refuses a bad far copy).** `open_timelocked`, after the lock round:
   `TL` must open canonically (amendment 12), `κ = w XOR wrap(S, τ, ctx)`, and `TF` must be
   EXACTLY `far_copy(ρ_far, κ)` (re-encryption) before the body is read; otherwise the post is
   dismissed with `S`, the DLEQ proof and the time-lock opening, even when its body is a valid
   reveal (THREATS C52-3). The check runs at intake, BEFORE `ρ_far` exists: by drand/tlock's
   decryption correctness a `TF` that re-encrypts from `κ` opens to `(far_sigma(κ), κ)` under
   `ρ_far`'s signature (tested with published signatures), so a post the coordinator takes can
   always be unsealed. What cannot be checked before `ρ_far` is a REVEALED post's far copy by a
   client (it holds no `S`, so no `κ`): that stays the coordinator's word until `ρ_far`, pinned
   on chain (the post and, for a round without a value, the attempt record), and then anyone
   checks it: a revealed post that does not unseal to its served reveal is a proof against the
   coordinator (`unseal_far`). A client's dismissal check (`check_timelocked_dismissal`, Rust and
   web) unwraps `κ` with the dismissal's `S` and the opening's `τ`: a dismissal stands when the
   far copy fails or the body is no valid reveal, and is false otherwise. A post with a valid
   `TL` but too short for `TF`, `w` and a body byte is garbage on its face.
6. **Unseal (anyone, after `ρ_far`).** `unseal_far`: the far round's signature verified against
   the pinned key (no guess: `BeaconMissing`, `BadBeacon`), `TF` opened canonically, its `σ`
   required to be `far_sigma(κ)`, the body read with `k(κ)`: the observer's valid reveal or
   garbage. No reveal key, no `S`, no `τ`. `unseal_posts` does it for every in-phase post of a
   round; the revealed set, outcome and attempt record recomputed from it must be the round's
   (THREATS C52-2). The exact rule (review C52-R1-1): every observer served or listed as revealed
   must unseal to its served reveal, and every post that unseals must be revealed OR dismissed
   with a proof that checks; a post rightly dismissed (a near time-lock that does not open, a
   wrong wrap) may still unseal, its poster's own fault and never a reason to refuse the round.
   Wiring it into the clients and the website (fetch
   or take the far signature, unseal sealed attempts, recompute) is ROADMAP C83.
7. **Horizon, beacon stop.** drand publishes no end date for quicknet; a round far in the future
   is a number like any other, but the League of Entropy may stop or retire the chain first.
   Onyx bounds the far date at `MAX_UNSEAL_DELAY_SECS` = 1,096 days after a round's reveal close
   (`FarBeyondHorizon` at seal and at intake: such a template cannot run time-locked draws; none
   today) and refuses a far date less than a day after it (`FarTooSoon`: a window the template
   does not allow). If drand stops, or quicknet is retired, before `ρ_far`, the far path never
   opens: nothing else changes (the coordinator's path needs only `ρ`), and REVLOG-5 falls back
   to amendment 10's record (binding, provable when the coordinator unseals). There is no
   fallback copy (a coordinator-openable one would reopen C4-7). A lock round that never comes is
   C53's; C53 now also states the far round's case. Changing the pinned chain is a protocol
   version change.
8. **Registry.** `MAX_SEALED_BYTES` 1,294 -> 1,438 (+144, the far copy and the wrapped key, no
   byte more: 128 + 144 + 142 + 1,024); new `FAR_COPY_BYTES` = 144; a time-locked draw's posts
   must be longer than `TIME_LOCK_BYTES + FAR_COPY_BYTES` = 272 (was 128; a length rule only,
   the chain cannot tell a far copy: the coordinator checks it, clients check its dismissals).
   No storage change; runtime 20,466 -> 20,505 B. Not deployed (C31).
9. **Book, served rounds, clients.** `TimeLockRead.far_round` (book version 8; a version-7
   time-locked round, posts without a far copy, never deployed, is refused with a clear error,
   any other version-7 round loads); the store compares a restored far round with its template's.
   Served `sealed.timeLock.farRound` (docs/spec/delivery.md); the Rust client
   (`observations::check_sealed_log`, given the template) and the web page
   (`sealed::far_round` from the pinned template) require it equal to their own, post lengths
   over 272, and check dismissals with the far rule. Posts carry no format version: the C52
   registry (C31's redeploy) needs every observer on the C52 build, since a C51 observer's
   282-byte post passes the length rule and is dismissed as garbage (review C52-R1-3). Observer (`level2-observer`) and coordinator
   (`finish_reveals_sealed` now takes the template) derive the rounds from the on-chain draw's
   config and the feed's own template.
10. **Costs, measured.** `postSealedReveal` of a numeric time-locked post with its far copy (426
   pseudorandom bytes) in a time-locked draw: 579,811 gas cold (`test_GasOfATimeLockedPost`);
   `test_GasOfTimeLockedSizedPosts` measures 154, 282 and 426 bytes side by side (377,725 /
   466,383 / 579,869: the far copy costs +113,486, about 4.5 fresh storage words). Execution gas only, as
   amendment 12 item 10. Every other registry test moves by a few hundred gas (the bytecode).
11. **Vectors.** `fixtures/tlock/onyx-far-date.json` (made by onyx-coordinator, `ONYX_BLESS=1 cargo
   test -p onyx-coordinator c52_far_date_vectors`): five far copies (body keys with zero tails,
   all zero, all 0xff) locked to published quicknet rounds; `onyx-tlock`'s known-answer checks
   derive `far_sigma` with Keccak-256, re-encrypt and open them under the real signatures on
   native and on wasm32 (`scripts/check-tlock-wasm.sh`); the coordinator's test pins its
   derivation to the same file. No Go-made far copy exists (C70).
12. **What stays open, stated plainly.** Every post of a time-locked draw carries a far copy,
   VALUE rounds included (the chain holds only their result hash), so every observer's
   observation, salt and page hash of every round becomes public on chain at the far date, for
   free: the price of REVLOG-5's closure (the paid API sells values younger than that). Only a
   feed whose rights are `Redistribution::Allowed` may run time-locked draws
   (`LockRounds::of_round` refuses others, `NotRedistributable`, review C52-R1-2; both level-2
   feeds today are `Allowed`). Clients do not yet unseal at the far date (C83). A revealed post's far copy is the
   coordinator's word until the far date (item 5). The far date has the lock round's
   assumptions: drand's threshold (a League of Entropy majority could open early) and block time
   not lagging the wall clock by more than a day (C53). Nothing is deployed (C31).
13. **Tests.** Rust: `timelock::tests::c52_*` (far round derivation, unseal without the
   coordinator, bad far copies dismissed at intake, short posts, real IBE with published
   signatures, the fixture, a round unsealed and its record recomputed), the C12-R2 transcript
   test extended to both keys, `feed::tests::c52_*`, the version-7 book migration, `c51_timelocked`
   runner tests (`c52_a_live_round_dismisses_a_bad_far_copy_and_serves_the_templates_far_round`),
   web `sealed_reveals::c52_*`; onyx-tlock `c52_far_date_vectors_check_and_a_changed_one_fails`
   and the wasm32 run; forge: cap and lengths (`test_ATimeLockedDrawsPostsCarryATimeLock`,
   `testFuzz_TimeLockedPostLengths`), gas, invariant cap; the anvil rehearsal
   `scripts/level2-timelocked.sh` asserts the far round of both draws.
14. **Review C52-R1** (fresh adversarial reviewer, cryptography and protocol, read-only,
   2026-10-18; THREATS C52-R1-*): 0 High, 0 Medium. Fixed: the unseal rule stated exactly and
   tested with a rightly dismissed post that unseals (C52-R1-1); time-locked draws only for
   freely redistributable feeds, value rounds named in the price (C52-R1-2); stale C51 observers
   stated (C52-R1-3); one gas delta from one test (C52-R1-4); test nits (C52-R1-5); multi-target
   key guessing stated (C52-R1-6). Found sound: the AND property, far-copy canonicality across
   intake, clients and unseal, the far-round bound for dated, period and fixed subjects, `kappa`
   freshness, no panic on attacker bytes, one length rule everywhere.
15. **Review C52-R2** (fresh adversarial reviewer, cryptography, protocol and mutation testing,
   2026-10-18; THREATS C52-R2-*): 0 High, 0 Medium, 3 Low, all fixed with tests. The page's
   far-round mirror now refuses a feed that is not freely redistributable, as the coordinator
   and the Rust client do (C52-R2-1); both edges of the 1,096-day horizon and of the one-day
   margin are tested (C52-R2-2); the page's far rule inside its round check and both clients'
   272-byte rule now have tests that fail when the rule is broken (C52-R2-3). Derived `sigma`
   kept (item 3): one FO ciphertext per fresh uniform key, decryption a function of the
   ciphertext, no decryption oracle before the far round; 20 mutations of the new checks, each
   caught.

## Amendment 16 (polite fetching and a per-origin budget; partner tracker T2, 2026-10-23)

A partner (partner, 2026-10-22, Q4): "16 observers hitting a rate-limited exchange endpoint at once
seems like it could get us blocked". It could: every observer fetched at `window_start`
(`examples/level2-observer.rs`).

1. Observers fetch at a slot spread over the window by a hash of the round id and their address
   (spec R-OBS-2b). The round id is fixed by the draw, so nobody picks a slot; anyone recomputes
   it. 30 s at the end stay free. In the Treasury feed's 300 s window, 16 observers land in 12 or
   more distinct seconds (`fetch::tests::sixteen_observers_do_not_share_one_second`).
2. A coordinator keeps an `OriginLoad`: a round is admitted only while the observers of the
   overlapping open rounds on its origin stay within a budget (default 16). The count is
   conservative (every round that overlaps the new one counts, even when they do not overlap
   each other). Different spellings of one host share a budget.
3. Not done: the slot cannot be enforced (fetch times are not observable), and a source may limit
   by IP range, which a budget per host does not see. The budget is a library check; the one-round
   Treasury coordinator example has a single round, so it gains nothing from it yet. A
   long-running coordinator must call `OriginLoad::admit` before announcing a draw (TODO S5).


## Amendment 17 (daily subjects and the shared runner; NS3, 2026-10-26)

Sources that publish every calendar day (earthquake counts, weather) fit neither existing rule:
`Dated` follows a business calendar and puts `{date}` in the URL once, `Period` puts it in the
selector. A query for one day names it twice (its first and last millisecond).

1. `SubjectRule::Daily { prefix, min_days, max_days }`: the subject is `prefix:YYYY-MM-DD` for any
   calendar day; the URL holds `{date}` one to four times (`MAX_DAILY_HOLES`), every one filled
   with the same date; the selector holds none. A day is observable in a window whose UTC day is
   `min_days..=max_days` after it, so a source that revises its data is read only after the
   revisions settle. A new enum variant: existing templates keep their Borsh bytes and ids.
   Tests: `feed::tests::daily_subjects_fill_every_url_date_and_bound_the_window`,
   `feed::tests::daily_templates_are_validated`.
2. The level-2 round runner moves from the Treasury adapter to `crates/onyx-level2`, unchanged
   in behaviour, behind a `Source` trait (template, fetcher, spec, registry, rehearsal key label,
   shown names). The Treasury adapter's `round` module keeps every function as a wrapper, and its
   full test suite (pinned result hashes included) passes unchanged. The EDGAR adapter keeps its
   own copy for now.
3. First daily sources: the USGS count of magnitude 5.0+ earthquakes worldwide per day
   (`adapters/source-usgs`, `min_days` 100, `max_days` 120, earthquakes only) and NOAA NCEI GHCN-Daily's maximum
   temperature at Denver International Airport (`adapters/source-ncei`, `min_days` 30,
   `max_days` 60; the NWS API keeps only about 7 days, too short to read after revisions);
   RESEARCH.md "Daily public data outside finance".
4. Limits (review NS3-R1, THREATS NS3-8 and NS3-9). The observable range is kept short (20 and
   30 days): a coordinator chooses the window, so a long range would let it read the source first
   and pick a window whose value it prefers. A source behind a CDN (USGS caches answers for 60 s)
   may hand several observers one cached copy; the observers then attest what the source's own
   CDN served, not independent reads of its database. Each USGS recording keeps the `age` and
   `x-cache` headers of every fetch.
5. On the website (data page, "Earthquakes and weather, re-observed"): both rounds are rehearsed
   on a local anvil (`ONYX_L2_ONLY=daily scripts/level2-rehearsal.sh`, sharing
   `onyx_level2::rehearse`) and checked in the browser by `check_daily_round`, the Treasury
   check made generic (`feed_round_check`) with each source's own pins. Public builds seal them.
