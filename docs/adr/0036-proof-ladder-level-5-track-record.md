# ADR-0036: Proof level 5, a source's track record against later stronger answers

## Status
Accepted, 2026-10-17 (ROADMAP C1, branch `track-record`; the loop decides, per the owner's
instruction). Core computation and commitment implemented in `crates/onyx-track` (sector-agnostic,
depends only on `onyx-core` and `onyx-merkle`). Spec: docs/spec/proof-ladder.md "Level 5".
Research: RESEARCH.md "Proof ladder level 5: track records". Not on chain yet (ROADMAP C7).
Amendment 1 (2026-10-17, ROADMAP C5, branch `track-intake`): the statement intake
(`onyx_track::intake`). Amendment 2 (2026-10-17, ROADMAP C6, branch `track-serving`): served at
`/v1/track` and shown on the website (no real record yet; the labelled example). Amendment 3
(2026-10-17, ROADMAP C7, branch `track-onchain`): resolutions read from on-chain facts only,
commitments in an on-chain append-only log, declared operators (affiliates), and the answers
usable first judge an entry. Amendment 4 (2026-10-17, ROADMAP C9, branch `track-followups`):
period facts closing before their period starts (the Bitcoin flow rule), skill against persistence,
level-3 key statuses read per post, and intake receipts. Amendment 5 (2026-10-18, ROADMAP C20,
branch `track-serve`): the logs and intake served for a client rerun (not when they hold priced
level-1/2 answers), the coordinator's publication file read by the node, paged histories, and
`since` remembered in the browser. Amendment 6 (2026-10-18, ROADMAP C21 and C23, branch
`track-pages`): log format 2 (chained row hashes in publication order, versioned by hash domain
and publication version), logs served in pages and checked in one linear pass, and the registry
served as data for client reruns.

## Context
Level 5 is "judged now, scored later": a value nobody can check today (an early report, an
estimate, a forecast) that becomes checkable once a stronger answer for the same fact exists. The
only level-5 scoring built so far is the Brier score of label forecasts (ADR-0016), keyed by
contributor and address. There was no per-source history for ordinary data: a customer looking at
a level-5 value from a source had no way to see how that source's earlier values turned out.

Three facts shape the design:
- Agreement with other sources rewards copying and herding (ADR-0016 context; peer prediction
  exists because plain agreement is not incentive-compatible, RESEARCH.md). A record must compare
  a source with the truth when it arrives, never with its peers.
- Names are cheap. A new source with no history must start with nothing, never a default good
  score, or a source with a bad record simply re-registers (whitewashing; RESEARCH.md).
- Level 5 is the weakest level. A record must not become a back door into money settlement: the
  money default stays levels 1 and 3 (`MarketPolicy::money_default`, L2R-F8, lead's L16-R4 rule).

## Decision
1. **What a record is.** For each (source, metric), the list of the source's statements about
   facts of that metric, each with its outcome once a stronger answer (a "resolution") exists for
   the same fact. A fact is `(metric, subject, as_of)`, keyed
   `keccak256("onyx-track-fact-v1" ‖ borsh(fact))`. No source is in the fact key, so every
   source's statement about one fact meets the same resolution.
2. **Compared with the truth, never with peers.** A statement is judged only against resolutions
   at accepted resolver levels, and only those at the strongest level present for the fact (a
   weaker answer never contests a stronger one), never one whose `resolver` is the stating source
   itself (`SelfResolved`, not counted). Accepted levels: a subset of {1, 2, 3}, default {1, 3}
   (the money default's levels, for the same reason: a colluding level-2 majority has no dispute
   path yet, so it could frame a source). Levels 4 and 5 never resolve anything. Other sources' statements are not an input to
   any outcome; the record only shows how many sources stated on the metric ("best of N") and
   whether this source was the first to state an agreeing value (`agreed_first`).
3. **Outcomes, in order.**
   - `Late`: the source's earliest statement on the fact was committed after the fact's close
     (`as_of + close_after_secs` of the metric's rule). Not counted: a value stated once the truth
     may be out shows nothing.
   - `Unresolved`: no resolution yet. `SelfResolved`: the only resolutions at the strongest
     level come from the stating source. `Contested`: the other resolvers' resolutions at the
     strongest level disagree (nobody scored, like ADR-0016's address claimed by two entities); a
     source's own answer never contests the one that judges it (review TR-R2-1). `Void`: the earliest public time of the usable
     resolutions is at or before the fact's close (the truth was public before statements closed;
     nobody scored).
   - `Equivocated`: the fact resolved cleanly and the source stated it more than once with a
     different value or confidence. Counted as a confident miss (Brier term 1). Hedging never
     helps; an identical restatement is one statement. An equivocation on a fact that never
     resolves counts nothing, as every other statement on it (review TR-R1-10).
   - Otherwise `Agreed` (within the metric's tolerance; exact for `Bool` and `Bytes`) or
     `Disagreed`.
   Resolved = agreed + disagreed + equivocated.
4. **Confidence is scored, not just agreement.** Each statement carries a confidence in ppm that
   its value will agree. Per counted entry, binary Brier in units of 1e-12: agreed
   `(1e6 - p)^2`, disagreed `p^2`, equivocated `1e12`. Strictly proper for the event "this value
   agrees" (Gneiting and Raftery 2007, RESEARCH.md), so for the value it states a source gains
   nothing by misreporting its certainty. The source chooses the event, though: stating junk at
   confidence 0 would score a perfect 0 (review TR-R1-3). So a statement's confidence is at least
   500,000 ppm (it claims its value is more likely right than not), and a ranking puts the
   agreement count first and Brier second. Means and comparisons are cross-multiplied integer sums, never a rounded float
   (ADR-0009, as ADR-0016 item 3).
5. **Rules per metric, pinned in the params.** `TrackParams { resolver_levels, rules }`, one
   `MetricRule { metric, close_after_secs, tolerance }` per metric, strictly ascending. A metric
   without a rule has no track record (fail closed). The tolerance must be 0 for `Bool` and
   `Bytes` and at most `proof::MAX_TOLERANCE`. `params_hash = keccak256("onyx-track-params-v1" ‖
   borsh(params))`. Rules are a reviewer decision, like feed templates.
6. **Thresholds before a rate is shown as a rate.** A record is "established" with at least 30
   resolved statements over at least 5 distinct subjects: ADR-0016 item 6's numbers, reused, not
   separately derived (PLAN q-TR-1). Below that, a client shows the counts and says "not
   established", never a percentage.
7. **Deterministic computation.** Inputs: params, the registry (sources registered with
   ingestible rights, metrics registered, subjects valid, value kinds match), the statement log
   (strictly ascending by (source, metric, fact key, statement hash)) and the resolution list
   (strictly ascending by (fact key, resolution hash), each public at or before the computation's
   `as_of`, review TR-R2-2), each at most 2^24 items. Integers only,
   Borsh, Keccak-256 (ADR-0002, 0003, 0009). Same inputs, same bytes, on every machine.
8. **Commitment.** `compute` returns per-fact entries (sorted by source, metric, fact key) and
   per-(source, metric) records (sorted by source, metric) and per-metric summaries (sources,
   resolved facts). Each record names its contiguous range of entries. All three are Borsh rows in
   `onyx-merkle` trees (ADR-0003). `TrackCommitment { prev, as_of, registry_root, params_hash,
   statements_hash, resolutions_hash, entries_root, entry_count, records_root, record_count,
   metrics_root, metric_count }`, hashed with domain `onyx-track-commitment-v1`. `prev` chains
   each commitment to the one before (zero for the first).
9. **History cannot be rewritten.** Every statement's `committed_at` is at most the commitment's
   `as_of`. `check_extends(prev, prev_log, next, next_log)`, where a log is (params, statements,
   resolutions): chained, later, both logs match their hashes, the later params keep every rule
   and the resolver levels (rules may only be added for new metrics; review TR-R1-2), every
   earlier statement and resolution is kept unchanged, every statement new to the later log has
   `committed_at` after the earlier `as_of`, and every resolution new to it is for a fact the
   earlier log had not resolved and was public after the earlier `as_of` (review TR-R1-1). So an
   outcome already committed changes only by the source's own later statement (an equivocation);
   a later contradiction of a resolved fact waits for a dispute path (C40; amendment 3 item 6 lets a
   later answer judge only entries not yet judged). The registry is not
   compared: it only grows, and outcomes never read it.
10. **How a client verifies.** (a) One record: a Merkle proof against `records_root`, counts in
    bounds. (b) Its history: the metric's summary against `metrics_root` (equal to the record's
    "one of N" and coverage), every entry in the record's range with proofs and strictly
    ascending fact keys, plus the entries just outside the range (or the tree ends), which must
    belong to another (source, metric); the recount must equal the record (`verify_history`).
    This proves the record adds up over its committed entries, not that each outcome is right.
    (c) Everything: rerun `compute` on the published logs and compare the commitment.
11. **How it is shown.** Always labelled level 5. Per (source, metric), never one blended score
    across metrics: "agreed A of R resolved", agreed first, mean Brier, and the uncounted
    outcomes (late, void, contested, unresolved), "best of N sources" and coverage (statements
    against resolved facts of the metric). Below the threshold of item 6: counts only.
12. **What a record must never be used for.**
    - Raising a proof level. A level-5 value stays level 5 whatever its source's record; nothing
      in `onyx-track` returns a `ProofLevel` or a `MarketPolicy`, a file that uses
      `onyx_track` may not touch level or market-policy code, and only manifests listed in
      `TRACK_DEPENDENTS` may depend on the crate, never renamed, checked on the manifests and on
      `cargo metadata` (`scripts/check-boundaries.sh`, review TR-R2-3).
    - Settling money. The money default stays levels 1 and 3 (L2R-F8, L16-R4).
    - Choosing a value among sources (a record-weighted vote is an agreement oracle with extra
      steps).
    - Rewards or slashing, until a later ADR binds sources to stake (as ADR-0016 item 7).
    - Scoring people. Sources are registered `SourceId`s with reviewed rights: organisations and
      publishers, never individuals.
13. **No shared vectors yet.** Only Rust computes records today. Vectors in `fixtures/` come with
    the first second implementation (browser verifier or contract), as for the Merkle tree.

## Consequences
- A customer can see, per source and metric, how earlier values turned out against recomputed or
  source-signed answers, and check it without trusting the server.
- Copying another source earns the same agreement count but not `agreed_first`, and the "best of
  N" line shows when a good record is one lucky name among many (Sybil analysis: THREATS TR-*).
- The record trusts its inputs' authenticity: which statements a source made (the intake must
  authenticate them) and which resolutions exist (until they are bound to on-chain facts). A
  coordinator that omits a resolution leaves the fact `Unresolved`, which is visible but not
  refutable yet (THREATS TR-5).
- Close times are per-metric constants. A truth published elsewhere before the close (the
  R-SCORE-1 problem) still lets a fast copier score; `Void` covers only resolutions whose own
  public time is before the close.
- Review TR-R1 (fresh adversarial reviewer, 2026-10-17) found three merge blockers, fixed in
  this branch before merge: resolutions appended to erase a committed miss (TR-R1-1), params
  changed to rewrite old outcomes (TR-R1-2), junk at confidence 0 scoring a perfect Brier
  (TR-R1-3); and seven more (THREATS TR-R1-4..10). Review TR-R2 (second fresh reviewer, same
  day) fixed three more: a source's own answer contesting its miss (TR-R2-1), a resolution dated
  after its commitment (TR-R2-2) and a renamed workspace dependency past the fence (TR-R2-3).
- Alternatives rejected: agreement with other sources or with the majority (rewards copying,
  manipulable by Sybils); one global score per source (hides which metrics it is good at;
  invites selection on easy metrics); a rolling window chosen at display time (lets a source pick
  its best period); a Beta-prior smoothed rate (needs a chosen prior; counts and a minimum sample
  are simpler to state honestly).

## Amendment 1 (2026-10-17, ROADMAP C5, branch `track-intake`): the statement intake
The record trusted its statement log (Consequences above; THREATS TR-6, TR-7, TR-13): who added a
statement, and when, were caller input. This amendment decides PLAN q-TR-2 and makes both
checkable. Code: `crates/onyx-track/src/intake.rs` (pure, no I/O) and, for a chain over Foundry's
`cast`, `crates/onyx-coordinator/src/track_chain.rs`. `onyx-track` now also depends on
`onyx-labels`, for EIP-191 recovery only. Spec: proof-ladder.md "Level 5", "Intake".
Research: RESEARCH.md "Track-record intake".

1. **Only the source's registered key (q-TR-2, decided).** A statement enters only as a
   `SignedStatement`: the payload `{ intake, key, source, fact, value, confidence }` signed with
   EIP-191 (`onyx-track-signed-statement-v1:0x<digest>`, the level-3 form of ADR-0028) by a key of
   `source` in the batch's `KeyBook`. A key is the source's reviewed level-3 key (`SourceKey`, same
   id), never a contributor's or the coordinator's: a record is about the publisher, so only the
   publisher's key may speak for it. Every key of every book must be one of the level-3
   registry's reviewed `SourceKey`s (with its provenance: where the source published it), checked
   by the intake and by the client, so the coordinator cannot slip in a key of its own (review
   TR-R3-2). `intake` is the id of one deployment (chain id, sink, poster,
   the poster's first nonce), so a statement signed for one intake never replays into another. A
   payload already taken is refused (`Replay`), whoever relays it and whatever its signature.
2. **`committed_at` from a finalized record, never a clock.** The intake seals pending statements
   into a batch, a Merkle tree of the signed statements under a `BatchHeader { intake, seq, prev,
   keys, statements_root, statement_count, reveal_by }`, and posts only the header's hash. Once the
   post is final, every statement of the batch gets `committed_at` = the timestamp of the block
   that carries the post. That proves the statement existed by then (the RFC 3161 and
   OpenTimestamps sense), which is the direction that matters: it can only make a statement look
   later, never earlier. Nobody, the coordinator included, picks the time.
3. **Every post counts.** Headers chain (`seq`, `prev`), and the record proves how many posts are
   final: today the poster's nonce at the finalized head (a dedicated poster account, its first
   nonce bound in the intake id), on C2 the book's own count. A post outside the chain (a hedge
   committed and never shown) breaks the count and the client's check fails (`HiddenPost`).
4. **Sealed until close (TR-7).** A statement stays private until `reveal = max(committed_at,
   close)`; only its batch's root is public before. The Merkle leaf hashes the whole signed
   statement including its 65-byte signature, which nobody without the key can predict, so even a
   `Bool` value cannot be guessed from the root. A commitment at `as_of` holds exactly the
   statements whose reveal is at or before `as_of`. A batch holds statements of one close only
   (`reveal_by`, checked for every revealed statement), so by `max(post time, reveal_by)` every
   leaf of it must be revealed and no statement can stay sealed past its own close behind a far
   one: a hedge sealed with both values is due whole at its close and scores `Equivocated`
   (review TR-R3-1). Copying a value before the close needs the source's own leak.
   Because a statement now enters the first commitment after its reveal, item 9's rule becomes:
   every statement new to a later log has `max(committed_at, close) > prev.as_of` (it was sealed
   or not yet made then); the anchor check of item 7 bounds `committed_at` itself. The relaxed
   rule holds only when the params pin an intake: with `intake` zero `committed_at` is caller
   input, nothing bounds it, and the rule stays `committed_at > prev.as_of` (review TR-R4-1).
5. **Keys change only forward, never back in time.** The header pins the key book's hash, so a
   rotation or revocation takes effect at the next batch's block. A key the level-3 registry
   retires or marks compromised is revoked in the next book and its pending statements are
   dropped; statements already in a final batch stand. Undoing them would let a source "lose" its
   key to erase its misses. A revoked key never returns and a signer belongs to one source for
   good (as L3R4-F1). A thief's statements until the revocation are visible and count against the
   source, like any other use of its key (THREATS TR-I-4).
6. **The commitment's time is chain time, and its intake is pinned.** A track commitment fed by the
   intake has `as_of` = the timestamp of a finalized head block, and its log holds every post at or
   below that block. `TrackParams.intake` names the intake (zero: none) and may never change along a
   commitment chain, so a chain cannot switch to a second poster's posts, and a client that sees it
   set must run `verify_log`, not only `check_extends` (review TR-R3-3).
   Block timestamps strictly increase (assumed in the spec), so no later post carries an earlier
   time.
7. **What a client checks (`verify_log`).** Posts `0..n` read from the record at the head, each a
   header with the right `intake`, `seq` and `prev`; every key book valid and extending the one
   before; every revealed statement proven under its batch root, signed by an active key of its
   source in that batch's book, admissible under the params and revealed no earlier than its
   reveal time; no payload twice; complete batches past their reveal; and the commitment's log
   equal to the revealed statements with `committed_at` = their post's block time. With
   `check_extends` and a rerun of `compute`, the record now rests on chain data and signatures,
   not on the coordinator's word (resolutions remain C7).
8. **Floods.** The intake caps pending statements per source per batch and per batch (a size
   bound, not a scoring rule). Dilution cannot come from volume alone: one entry per (source,
   fact), only resolved facts count, a second value on a fact is a confident miss, a rate needs
   5 distinct subjects, coverage and "one of N" are committed. Diluting with easy facts stays
   visible only as coverage; a skill score against a naive baseline is a follow-up (C9, PLAN
   q-TR-5).
9. **Close-time rules reviewed per metric.** Each registered metric is reviewed against the
   earliest time its truth is public anywhere (spec table "Close-time rules"). The intake refuses
   a statement whose close is at or before the latest finalized time (`PastClose`), and `Late` stays
   for one sealed before its close and posted after it. Outcome of the review: no metric served
   today gets a rule yet. Bitcoin level-1 metrics are public as their blocks are mined, so a
   close must fall at the period's start, which the fact does not carry (C9; amendment 4 item 1
   gives them a period rule); level-2 feeds do not
   resolve under the default levels (q-TR-4); level-4 metrics never resolve. The example level-3
   metric is the only ruled one.
10. **Pluggable record; C2 later.** `FinalizedRecord { head, posts }` is the only chain interface.
    Today `onyx_coordinator::track_chain::CastRecord` posts the header hash as the calldata of a
    transaction from a dedicated poster to a sink address (no contract) and reads it back at the
    `finalized` block (a pinned depth only on a loopback anvil), on the intake's chain id only
    (review TR-R3-4). C2's signed-source book on chain can carry
    the same headers with its own count and block time and implement the same trait; no rule here
    changes, and this branch edits no contract.
11. **Still never a level or money.** `onyx-coordinator` may depend on `onyx-track` only through
    files that name no level or market-policy code (`track_chain.rs`); the fence now covers every
    `onyx-track` source file and refuses `track_chain` beside level code too
    (`scripts/check-boundaries.sh`, TR-11).

Review TR-R3 (fresh adversarial reviewer, 2026-10-17): two merge blockers fixed before commit (a
far `reveal_by` kept a hedge sealed forever, TR-R3-1; the key book was not bound to the level-3
reviewed keys, TR-R3-2), plus the intake pinned in the params (TR-R3-3), the chain id and finality
checked by the record (TR-R3-4) and a per-source pending counter (TR-R3-5).

Consequences: TR-6 (impersonation, backdating) and TR-7 (copying before the close) are closed by
signatures, anchors and sealing; TR-13 has its per-metric review. What is still trusted: the
chain's block time (on an OP stack the sequencer sets it within `max_sequencer_drift`, RESEARCH),
the completeness of resolutions (C7), the coordinator's liveness (it can delay a post, never
backdate one; a delayed statement may turn `Late`, visible to the source that holds its signed
payload; THREATS TR-I-6), and the leaves' privacy on the coordinator until reveal. Rejected: the
coordinator's clock (the problem itself); each source posting its own transactions (gas per
statement and a chain account per source, with no gain over a signed payload in a counted batch);
values in clear on chain (copyable at once); a new contract now (C2 owns the signed-source book).

## Amendment 2 (2026-10-17, ROADMAP C6, branch `track-serving`): served and shown
Code: `onyx_track::wire` (feature `wire`), `onyx_track::example`, `onyx_api::track_serve`,
`apps/web/verify/src/track_view.rs`, the Data page's "Track records" section. Spec:
docs/spec/delivery.md "Track records", proof-ladder.md "Display".

1. **One served form, one check.** A record is served as Borsh rows with their proofs, never as
   decoded fields, and checked by `wire::check_record_body`, the same code in the Rust client and
   the browser (ADR-0020). It proves item 10 (a) and (b): the record under the commitment and the
   record adding up over its whole committed history. Item 10 (c), a rerun of `compute`, needs the
   logs, which are not served yet (ROADMAP C20).
2. **Only what exists.** A server publishes only computed books whose commitment extends the one
   it published before (`check_extends`), so it never moves back or forks its own chain. No
   publication, no rule or no statement is a 404 `no_track_record` with its reason. Today no
   served metric has a close-time rule (amendment 1 item 9), so a node serves only 404s.
3. **The example is never real.** The website shows the example (`example.*` ids, invented
   values) labelled "example" on the section and every card, beside a plain empty state that
   says why there is no real record. The body's `example` flag must equal whether an id is an
   example id, a node's store refuses example ids, and the browser pins the example's latest
   commitment.
4. **Freshness, partly.** Until commitments are on chain (C7) a client cannot know a commitment
   is the latest (THREATS TR-R2-5). It refuses one older than its own floor (`min_as_of`), and,
   when it saw one before, asks with `since` and refuses any commitment that does not chain back
   to it by hash. Chaining is not extending: a commitment over a rewritten log can be chained to
   the known one; refusing it needs the logs and `check_extends` (TR-S-14, C20). The browser's
   example check uses the pinned commitment as both.
5. **Rights.** In `ONYX_RIGHTS=fallback` a label-derived metric's records are 403
   `label_rights_withheld` before any lookup (the L14 pattern): the registry says the metric needs
   a label set, its name has an `entity` segment, or it is not registered (fail closed).
6. **Still never a level or money.** Serving is free and bound to no market. onyx-api and
   onyx-web-verify may depend on onyx-track only through one module each (`track_serve`,
   `track_view`) that names no level or market-policy code; onyx-api's router takes the track
   routes as an opaque `Router` (`router_with`), and the node's `app.rs` names track code on one
   line only, `track_serve::empty_routes(rights)`, which takes no data and returns only a
   `Router` (`scripts/check-boundaries.sh`, TR-11, TR-S-9).

Consequences: records are checkable by anyone against a commitment, and the site's level-5
section cannot show a made-up or stale record without failing its check. Still trusted: that
the served commitment is the latest (C7), that the logs behind it were computed honestly (a rerun
needs C20), and resolutions' completeness (TR-5, C7). Rejected: decoded JSON fields beside the
rows (two sources of truth); showing the example without a label or mixed with real rows; a
paid endpoint (a record is context for a value, not a value). Review TR-S-R1 (fresh reviewer,
same day): the fence let app.rs read a record beside its payment code (now one mount line), the
docs claimed `known` caught forks (reworded, TR-S-14), plus four low findings (THREATS TR-S-R1-*).

## Amendment 3 (2026-10-17, ROADMAP C7, branch `track-onchain`): resolutions and commitments on chain
Decides PLAN q-TR-3. Closes TR-5 (a resolution omitted or forged by the coordinator), TR-R2-4
(affiliates), TR-R2-5 and TR-R1-11 (a stale or forked commitment shown as the latest), and
narrows TR-S-14. Code: `onyx_track::chain` (pure), `onyx_coordinator::track_chain::{CastFacts,
CommitmentLogCast}` (reads and posts through `cast`), `OnyxSignedBook` (`postTrackCommitment`,
`trackHead`, `trackCommitmentAt`, `declareOperator`, `operatorOf`). Spec: proof-ladder.md
"Level 5", "On chain". Research: RESEARCH.md "Track records on chain".

1. **Chain mode, pinned.** `TrackParams.chain` = `FactChain { chain_id, book, registry }.id()`
   (domain `onyx-track-chain-v1`; zero: resolutions stay caller input, as before, for tests and
   the example). Like `intake` it never changes along a commitment chain (`extends`). With it
   set, a commitment's resolution list is never an input: `chain::derive_resolutions(params,
   reg, view, statements, prev, preimages)` builds it from one chain at one finalized head
   (`ChainView`), and every client reruns it (`chain::verify_resolutions`) and compares.
2. **What is read, per fact the statement log names** (nothing else, so on-chain spam on facts
   nobody stated adds nothing):
   - Level 3 (if accepted): for every source with a key ever registered in `OnyxSignedBook`
     (`KeyRegistered`), the book fact `(source, metric, subject, as_of)`; when `settleable`
     returns it at the head, its report (from the direct `postReport` calldata, else a preimage)
     must hash to the settled report hash and value hash and be about this fact. Resolution:
     the report's value, level 3, resolver = the source, evidence = the report hash,
     `public_from` = `acceptedAt + DISPUTE_WINDOW_SECS` (when the chain made it usable).
   - Level 2 (if accepted): the metric's registered template and kind (`metrics`), the
     observation `observationOf(metric, subject, as_of)` with that window end; its preimage
     (template, spec, round id, outcome) must be the registered template's (`matches`), of this
     fact, and hash to the recorded spec id and result hash. A `Value` outcome resolves (bytes;
     `Int` at scale 0; else `Fixed` at the feed's scale), resolver = the feed's source,
     evidence = the result hash, `public_from` = the recording block's time.
   - Level 1: params accepting it are refused in chain mode (`Level1Unsupported`, fail closed,
     before the first commitment). Its rows are off chain (only roots are), no per-fact index
     exists, and `verifyRowLeveled` is level code the track fence keeps out of track files
     (ROADMAP C40). A refusal per stated fact of a metric registered as a recipe halted the whole
     record once the registry registered a recipe for a metric already stated on (review
     TR-C7-R2-1), so chain-mode params name levels 3 or 2 and 3 until a reader exists.
   - An answer the chain holds without its preimage is `Withheld` (refused, never skipped). An
     answer `compute` would refuse (a resolver Onyx does not register, a value of another kind)
     is skipped by every client alike, so a malformed on-chain fact cannot halt the record.
   - The previous commitment's resolutions are kept unchanged. For a fact the previous
     commitment's statements named, a new answer with `public_from` at or before its `as_of` was
     usable then: it is never taken later (a coordinator that left it out is caught by the walk
     of item 3, and a registry that has grown since cannot halt the chain; review TR-C7-R1-2).
     For a fact first named now, any answer is taken (old ones can only give `Late` or `Void`;
     review TR-C7-R1-1). For a fact the previous commitment resolved, only answers no stronger
     than its weakest committed answer (item 6).
   - Accepting level 2 on a chain with no registry is refused (`NoRegistry`; review
     TR-C7-R1-8). Level 2's result hash is the coordinator's attested settlement
     (`recordObservation`, ADR-0030): chain mode proves it is the recorded one, not that the round
     settled it (level 2 is not a default resolver level, q-TR-4; review TR-C7-R1-7).
3. **Pinned reads.** A commitment's `as_of` is its head block's timestamp (block times strictly
   increase, so the block is unique). `CastFacts` pins a `finalized` head (a fixed depth only on a
   loopback anvil) on the configured chain id, names it by hash in every `eth_call` (EIP-1898) so
   a replaced block fails, and re-checks the head's hash after every log scan. A client reads at
   the commitment's head: the `head` of its `TrackCommitted` event, final and timed at `as_of`
   (`CastFacts::at_commitment`; an archive node for an old one). `verify_resolutions` takes the
   previous log on trust, so the full check is `chain::verify_chain`: every log entry from the
   first, each at its own head, with the registry it names, its resolutions rederived, extending
   the one before, equal to a rerun of `compute`, the last being the log's latest (review
   TR-C7-R1-3: a forged early commitment carried by honest later ones).
4. **Commitments on chain.** `OnyxSignedBook.postTrackCommitment(track, head, borsh)` takes the
   288-byte Borsh commitment, hashes it with the commitment domain, reads `prev` and `as_of`, and
   appends it to the caller's log `(poster, track)` only if `prev` is the log's latest (zero for
   the first), `as_of` rises and is not past the block time, and `head` is a past block. Nothing
   is ever overwritten; `TrackCommitted` carries the bytes. A client pins `(chain, book, poster,
   track)` (`chain::CommitmentLog`), reads `trackHead` at a finalized block and accepts only that
   commitment (`chain::check_latest`, `wire::Expect.latest`), and finds a commitment it knew at
   its index (`check_in_log`): the log is one chain, so no fork after it can be the latest. A
   rewrite chained to a known commitment can still be posted; it is then public and attributable,
   and refusing it needs the logs (C20). Another poster's log is not this one. Shared vectors:
   fixtures/track/commitment-vectors.json (Rust writes, the book hashes alike).
5. **Operators (affiliates, TR-R2-4).** `OnyxSignedBook.declareOperator(source, operator)`: the
   reviewer, once per source, before the source's first key, never changed. `TrackParams.operators`
   lists them (strictly ascending, non-zero; a source not listed is its own operator); an answer
   never judges a statement of a source with the resolver's operator (`SelfResolved`), so an
   affiliate can neither make its operator's statement agree nor contest its miss. `extends`
   keeps every declared operator; `check_extends` refuses one declared for a source the earlier
   logs hold (`OperatorRegrouped`): a grouping never reaches back into committed outcomes. In
   chain mode every stating source must have a key in the book (so its operator was fixed before
   it could state) and the params' operators must equal the book's for every source of the log
   with a book key (`check_operators`). A source without a book key (a level-2 feed's source)
   could be declared after it resolved, so its declaration is not used and the params list none
   for it (review TR-C7-R1-4); level-2 feeds are owner-registered templates, reviewed as such.
6. **The answers usable first judge an entry** (closes TR-C7-4, the self-shield, within what
   item 2 reads). Before, a resolved fact took no new resolution, so a source whose own or
   affiliate's answer came first stayed `SelfResolved` for good, its miss uncounted, even after an
   independent answer. Now, per (source, fact), `compute` leaves out the source's own and its
   operator's answers first, takes the strongest level among the rest (so its own stronger answer
   cannot outrank them; review TR-C7-R1-5), and of that level the answers public within
   `CONTEST_WINDOW_SECS` (3,600 s, the book's dispute window) of the earliest: `Contested` when
   they disagree, `Void` when that earliest time is at or before the close. The entry is judged
   only once that window has passed at `as_of` (else `Unresolved`), so a fast wrong answer never
   judges alone (review TR-C7-R1-6). R-TR-8 changes to match: a resolution new to a later log must
   be public after the earlier `as_of` (except on a fact no earlier statement named, item 2) and,
   for a fact the earlier log resolved, no stronger than its weakest committed answer. Every entry
   judged before was judged by answers at least that strong whose window closed by the earlier
   `as_of`, so a later answer changes none of them; it judges only entries left self-resolved or
   pending. `first` (R-TR-5) becomes: no other agreed entry on the fact committed earlier and
   judged no later; the metric summary's resolved facts use the same window. Left open: a source's
   own answer at a weak level still fixes the fact's floor, so an independent answer at a stronger
   level arriving in a later commitment is not taken (C40).
7. **Still never a level or money.** `onyx_track::chain` returns no level or market policy;
   `track_chain` reads `settleable`, `factView`, `operatorOf`, `metrics`, `observationOf` and the
   commitment log, none of them level code; the new example is fenced like the intake's
   (`scripts/check-boundaries.sh`, TR-11). Nothing in the book's track functions moves money.

Review TR-C7-R1 (fresh adversarial reviewer, same day): two merge blockers fixed before commit (a
statement on an old fact, and a grown registry, halted every later commitment through an
omission error: items 2 and 3 now skip such answers and leave omissions to the chain walk), plus
the chain walk itself, keyless operators, per-entry strongest level, the contest window, the
registry-zero refusal and the commitment's head check (THREATS TR-C7-R1-*). Review TR-C7-R2
(second fresh reviewer, after merging main with C11): a recipe registered for a metric already
stated on halted the record (level 1 now refused in the params), a level-3 source relaying its
own report through a contract can halt the record until the coordinator reads call traces (C40),
and two untested finality guards now have tests (THREATS TR-C7-R2-*).

Consequences: in chain mode a client checks a record's resolutions against its own node, the
latest commitment is public to a first-time client, forks after a known commitment cannot be
the latest, and declared affiliates neither judge nor shield their operator's statements. Still
trusted: the reviewer's operator declarations (an affiliation it does not know of, TR-C7-5), the
coordinator's liveness (it can delay posts or commitments, never backdate or fork), a rewrite's
detection (logs, C20), and an archive node for checking an old commitment. Not yet: level-1
reads (C40), a per-fact index instead of the per-source scan of the book and paged log scans
(C40), the browser reading `trackHead` (C41), a poster key rotation for a commitment log (C41).
Rejected: a new contract (the C2 book already is the one public book every client reads); the
commitment hash alone as calldata to a sink (no contract check of linkage, and a first-time
client cannot find the latest without the poster's transaction list); operators in the Onyx
registry root (changing them would change outcomes); dropping self or affiliate answers until an
independent one exists (would leave others unjudged when the publisher is the only resolver);
a later answer contesting a judged entry (re-opens TR-R1-1).

## Amendment 4 (2026-10-17, ROADMAP C9, branch `track-followups`): period facts, skill, key statuses, receipts
Decides PLAN q-TR-5. Narrows TR-13, TR-I-5 and TR-I-6; closes TR-R4-4. Code: `onyx_track::{Close,
MetricRule::close_at, Baseline, Record::skill_ppm}`, `onyx_track::intake::{KeyLife, KeyLives,
Intake::log_with, Receipt, SignedReceipt, check_receipt}`, `onyx_coordinator::track_chain::{
CastFacts::key_lives_at, parse_key_state}`, `onyx_source_btc::{TRACK_FLOW_PERIOD_SECS,
TRACK_FLOW_LEAD_SECS}`. Spec: proof-ladder.md "Level 5" (R-TR-1, R-TR-6, R-TR-11, R-TI-9,
R-TI-10, "Close-time rules"). Research: RESEARCH.md "Track-record follow-ups (C9)".

1. **A close at the period's start (TR-13).** A rule's close is `Close::AfterAsOf(secs)` (as
   before: `as_of + secs`) or `Close::PeriodStart { period_secs, lead_secs }`: the fact is the total
   over `[as_of - period_secs, as_of)`, `as_of` must be a multiple of `period_secs` (else
   `BadPeriod`, for statements and resolutions alike), and statements close at
   `as_of - period_secs - lead_secs` and stay sealed until `as_of`, the period's end
   (`MetricRule::reveal_at`; review TR-C9-R1-5: a forecast public while its period is open
   invites anyone who can move the total to steer it toward or away from the forecast). So the fact carries its period through its aligned `as_of`:
   with the pinned rule, one `as_of` names exactly one period, and the fact key, the payloads and
   the book's fact key (`signed_fact_key`) keep their form (adding a field to `Fact` would have
   changed all three for no gain while each metric has one period length). Period 1 s to 366 days,
   lead at most 366 days. `Late`, `Void`, `PastClose` apply with the earlier close; sealing and
   R-TR-8 use the reveal time (`max(committed_at, reveal)`, one reveal time per batch).
   **The Bitcoin flow rule** (`crypto.btc.entity_flow`, `crypto.btc.entity_flow_series`), the
   first production metric with a rule: period 86,400 s (UTC days, `as_of` a midnight), lead
   7,200 s. A day's blocks are those whose median time past (BIP 113, which never decreases, so the
   day is one height range) falls in the day. Such a block needs six of its eleven predecessors
   timestamped in the day, and Bitcoin Core accepts a block timestamped at most 2 hours ahead of
   its own clock (`MAX_FUTURE_BLOCK_TIME`), so no block of the day is accepted before
   `start - 7,200`: the close. What is still public before the close: mempool transactions that
   may be mined in the day (partial inputs, not the total: accepted, a forecaster may watch the
   mempool) and the entity's own intentions (an exchange stating its own flows controls the truth:
   TR-C9-2, C92). A record on these metrics still needs a source stating through the intake and a
   level-1 reader for its answers (chain mode refuses level 1 until C40), so none exists yet; the
   website and API say so.
2. **Skill against persistence (q-TR-5, decided: yes, beside the counts).** Each counted entry
   of a period metric carries a `Baseline`: the naive forecast is the last value known at the
   entry's close. Its one candidate is the fact of the same metric and subject whose period ended
   last at or before the close, named by an on-time statement (review TR-C9-R2-1: a first version
   fell back up to 2 periods, so a copier skipping the latest period met a staler baseline and
   scored +100 points); if the answers public by the close, other than the source's own or its
   operator's, judge it uncontested (contest window passed by the close), its value is the baseline;
   `Agreed` when it agrees with the entry's judging answer under the metric's tolerance,
   `Disagreed` otherwise, `None` without one. Point facts (`AfterAsOf`) have no grid, so no
   baseline: "the last value" would be whichever earlier fact someone named, which a sparse
   copier chooses (review TR-C9-R1-2). A record counts `baseline_entries`,
   `baseline_source_agreed` and `baseline_agreed`; `skill_ppm` = (source agreed - baseline agreed)
   x 1e6 / baseline entries, floored, in -1e6..=1e6, shown only when established, with its counts.
   Why persistence: it is the standard naive forecast for a series (Murphy 1988, RESEARCH), needs
   no parameter, and is what a copier of public data can always state; a metric with a seasonal or
   climatological baseline is a later per-rule choice. Why the agreement rate and not the Brier
   score: persistence states no confidence, and the counted event is agreement. Integers only;
   deterministic; never an input to an outcome, a level, a market or money (R-TR-9 and the fence
   unchanged). **Frozen:** the candidate's reveal (its `as_of`) is at or before the entry's
   close, so R-TR-8 admits no later statement on it, and a resolution new to a later log on a fact
   the earlier log named is public after `prev.as_of` (so never by the close). A first version with
   a 256-fact lookback over all named facts was not: late statements flooded the window and
   rewrote a committed baseline (review TR-C9-R1-1, merge blocker, fixed).
3. **Key statuses from the level-3 book (TR-R4-4).** A revealed statement enters the log only if
   its key's `OnyxSignedBook.keyState` at the end of its post's block is active and not suspended
   (`KeyLife::Active`; `Suspended`, `Retired`, `Compromised`, `Unknown` do not count). The client
   reads it per post (`CastFacts::key_lives_at`, by block hash) and passes one `KeyLives` per post
   (exactly the batch book's keys) to `verify_log`; with `params.chain` set they are required (no
   statuses: `BadKeys`), without a chain the coordinator's `revoked` flags stay the only status
   (trusted, as before). The coordinator's log is `Intake::log_with(head, lives)`. With statuses
   given, a book marking revoked a key active at the post is refused, so the coordinator's flag
   can no longer censor a key either (review TR-C9-R1-8). The status is the one at the post's
   block, not back-dated: the book's `markCompromised(id, since)` withdraws level-3 reports from
   `since`, but track statements posted before the mark stand (amendment 1 item 5: a thief's
   statements until the revocation count against the source; TR-C9-R1-4). A served post anchor
   must be the canonical block at its number (review TR-C9-R1-9). Read at the
   post's own block, a revocation and a post race to one deterministic outcome; a batch posted
   before a revocation stands (amendment 1 item 5), the next one loses the key's statements even
   if the coordinator never revoked it. Excluded rows stay in the batch (proven, revealed,
   counted for completeness), so a batch never becomes unverifiable.
4. **A retired outcome (TR-R1-4): not built (C90).** The registry still has no removal path, so
   nothing halts today. Sketch for C90: retirements pinned in the params (`extends` keeps them, a
   new one only with `from > prev.as_of`), registry checks skipped for a retired item, statements
   committed from `from` on `Retired` (not counted), answers public from `from` on refused; key
   books and `verify_log` need the same exemption.
5. **Intake receipts (TR-I-6).** `Intake::receipt(digest, deadline)` gives a `Receipt { intake,
   digest, deadline }` (deadline at or before the statement's close), which the coordinator signs
   with EIP-191 as the intake's poster (`onyx-track-intake-receipt-v1:0x<digest>`), the account
   every payload's intake id names. It is matched by payload digest, not by the signed row, so a
   second signature over the same payload cannot make a kept receipt look broken (review
   TR-C9-R1-3). `check_receipt` shows anyone: `Kept` (a post at or before the
   deadline carries it, proven before the reveal with `Intake::inclusion`'s proof under the posted
   root, or revealed after), `PostedLate`, `Broken` (not shown once the head is past the deadline
   and the reveal time, when an on-time post would be revealed), or `Pending`. A receipt never changes a
   commit time or an outcome (a colluding intake could otherwise backdate with one); it makes a
   delay or a drop attributable to the poster key. Publishing broken receipts beside a record is
   C91. Sources posting to the C2 book themselves stays rejected for now (amendment 1).
   **Sealed rows encrypted until their close (TR-I-11): not built.** C50's drand/tlock IBE fits:
   a row encrypted to the quicknet round at its close cannot be opened by anyone, the coordinator
   included, before that round's signature is public, and the batch root can be over ciphertexts.
   Wiring it (round choice from the close, ciphertext in the leaf, open-and-check in `verify_log`)
   is C93, after C51 wires tlock for sealed reveals; not in this branch.

Review TR-C9-R1 (fresh adversarial reviewer, same day; PoCs in a scratch copy): 1 High (the
lookback flood, TR-C9-R1-1) fixed before commit; Medium: a sparse copier beat a stale baseline
(R1-2, fixed: period grid, 2-period freshness, on-time naming), a kept receipt shown broken by a
second signature (R1-3, fixed), retroactive compromise not applied (R1-4, by design, stated),
forecasts public while their period is open (R1-5, fixed: sealed until `as_of`); Low and Info in
THREATS TR-C9-R1-*.

Consequences: a Bitcoin flow fact can be stated, sealed and judged with a close no block of its
period can beat, and stays sealed until its day ends; a record shows whether a source beats repeating the last known value, which
dilution with easy facts cannot raise; a key the level-3 book compromises stops counting at the next
post whatever the coordinator does; a dropped or delayed statement is provable against the
poster. Still trusted or open: mempool lookahead and an entity stating on itself (C92), the
level-1 reader (C40), a retired outcome (C90), broken receipts unpublished (C91), sealed rows
readable by the coordinator until C93. Rejected: a `period` field in `Fact` (changes every key and
the book's fact key while one period per metric suffices); a close at `as_of` for flows (the day's
blocks are public as they are mined); block timestamps for the day's blocks (not monotonic); a
receipt that sets `committed_at` (a backdating channel); the coordinator's `revoked` flag as the
status in chain mode (TR-R4-4).

Review TR-C9-R2 (second fresh adversarial reviewer, same day): the baseline is the latest period
ended by the close or none, no fallback (TR-C9-R2-1, Medium, fixed); the Bitcoin day's last blocks
come after `as_of`, so a period forecast is public while they can move the total (TR-C9-R2-2, Low,
accepted with TODO C40); the 7,200 s lead holds for Bitcoin Core 27 and later, a pre-27 node with a
peer-shifted clock could see a day's block up to 70 min earlier (TR-C9-R2-3, Low, accepted). The
close arithmetic is pinned on a block-acceptance model in `adapters/source-btc/tests/track_flow_close.rs`.

## Amendment 5 (2026-10-18, ROADMAP C20, branch `track-serve`): logs served, a node's publication path, paged histories, `since` in the browser
Narrows TR-S-13 and TR-S-14 (TR-9's rerun becomes possible), closes TR-S-R1-7's paging TODO and
review TR-S-R2-3's index budget. Code: `onyx_track::publication` (`Publication`, `Step`,
`check_steps`), `onyx_track::intake::{PublishedIntake, Intake::published}`,
`onyx_track::TrackBook::from_committed`, `onyx_track::wire::{log_body, check_log_body,
history_page_body, check_history_page, complete_history}`, `onyx_api::track_serve`
(`/v1/track/log`, `/v1/track/{source}/{metric}/history`, `TrackStore::install`/`reading`,
`node_routes`), `onyx_coordinator::track_chain::write_publication`, onyx-node
`ONYX_TRACK_PUBLICATIONS`, `apps/web/verify/src/track_view.rs` and `apps/web/src/lib/trackSeen.ts`.
Spec: docs/spec/delivery.md "Track records".

1. **One publication carries every commitment with its logs.** Every later commitment keeps every
   earlier statement and resolution (`check_extends`), so the logs are kept once, in publication
   order, and each step names how many rows of each it covers: step `i`'s log is the first
   `steps[i].statements` statements (and resolutions), sorted into the canonical order. A served
   run of steps is checked by `check_steps`: each prefix hashes to its commitment, the params to
   `params_hash`, and each step extends the one before. So a client that knew an earlier
   commitment and asks `GET /v1/track/log?since=` it checks `check_extends` all the way to the
   latest: a rewrite chained to the known commitment is refused (TR-S-14), whatever order the
   server lays the logs out in. With the registry, the client reruns `compute` on the latest log
   and must get the served commitment again (TR-S-13); with the level-3 registry's reviewed keys
   (and, in chain mode, the key statuses it reads itself) it runs `verify_log` on the published
   intake (`PublishedIntake`: head, posts, revealed batches, key books). Without the registry it
   checks hashes and extensions only and says so (`recomputed: false`). In chain mode the
   resolutions are still the server's word until the client reruns `chain::verify_resolutions`
   against its own node (`resolutions_from_chain`).
2. **Priced answers are not given away by the logs (decided here, conservatively).** A level-1
   row or a level-2 result is a value the paid API sells (ATT-R2-F1, SIG-R1-F2); a log holding one
   would give it away free, and the commitment hashes the whole resolution list, so it cannot be
   served with values redacted. So `GET /v1/track/log` answers 403 `track_log_priced` when any
   resolution is at level 1 or 2; the records and their proofs are served as before. Level-3
   answers (free, public in `OnyxSignedBook`) and statements (revealed by the intake's own rule)
   are served. Whether priced answers may be released in logs (after a delay, or to buyers, or
   under a commitment format that hashes each resolution so values can be redacted) is the lead's
   decision: ROADMAP C22. Until then a record answered at level 1 or 2 cannot be rerun by a client.
3. **Rights.** The logs cover every record, so in `ONYX_RIGHTS=fallback` they are 403
   `label_rights_withheld` when any ruled metric is label-derived (registry `needs_label_set`, an
   `entity` name segment, or unknown). A node has no registry: every ruled metric of a file it
   serves counts as label-derived (fail closed).
4. **The publication path.** The coordinator appends each computation with `Publication::push`
   (refused unless it extends the latest step) and writes the file with `write_publication`
   (checked, refused unless it starts with the file already there, written whole and renamed into
   place). The node reads `ONYX_TRACK_PUBLICATIONS` at start and again whenever the file changes
   (modification time, length, inode or change time), on the blocking pool; it serves a file only
   if it checks (`Publication::check`: a chain from a zero `prev`, every log and the latest rows,
   `TrackBook::from_committed`, the intake at the head; after the first load only the new steps,
   `check_after`) and starts with what it served before (`starts_with`). The head it served
   (step count, latest commitment hash, log row counts and a digest of those bytes; review
   C20-R2-2) is kept in `ONYX_STATE_DIR` (`track-served-head.borsh`), so after a restart a file
   that does not start with those bytes, or no file, is refused too. Anything else answers 503 `track_store_unreadable` on the track routes only
   (never stopping the node, whose paid routes do not depend on the coordinator's file) until
   the file is fixed: a node never serves a rewritten or truncated history, and never goes
   quietly back, across restarts too. Losing the state directory loses that floor (as for the
   node's receipt journals). The node computes nothing (no registry): the rows are what the commitment
   commits to, and whether the logs give them is each client's rerun. The node's `app.rs` still
   names track code on one line, `track_serve::node_routes(rights, publications, state_dir)`, paths in and a
   `Router` out (fence updated). Statements on the Bitcoin flow metrics have a rule (amendment 4)
   but no source states yet and no level-1 reader exists (C40), so no node has a file to read
   today.
5. **Paged histories.** A record past 1,024 entries is still served without its history; its
   client fetches `GET /v1/track/{source}/{metric}/history?at=<commitment>&from=<n>` (1,024
   entries a page with the neighbours just outside it; 409 `commitment_moved` when `at` is not
   the latest), checks each page against the record's commitment (`check_history_page`) and
   recounts the record over all pages (`complete_history`, `verify_history` unchanged). The index
   is paged too (`?from=`, 1,024 records a page, `next`).
6. **`since` in the browser.** The page remembers, per record, the commitment it checked and its
   `as_of` (browser storage, per viewer, never shared) and asks with `since` next time; the served
   commitment must chain back to it by hash with `as_of` rising, and be no older. A remembered
   commitment the server does not know shows nothing and says so (it may have rewritten its
   records, or it is another server), with a button to forget it. The site has no live node, so
   its since file holds the example as the server answers `?since=` each example commitment; the
   same code checks a node's answer (`check_track_record` takes `known`). The browser also reruns
   the example's logs (`compute` with the example registry, both commitments, the pinned one last).
7. **Cost.** A log body can reach tens of megabytes: at most 16,384 rows per body (413
   `track_log_too_large` beyond; paged logs are C21) and its own free budget, 2 answers per client
   a minute (304 free), apart from the records' 30. Checking hashes every served commitment's whole
   log, so a body costs links times rows: a client refuses one past 16,384 rows or 1,048,576
   (`MAX_LOG_CHECK`, about 5 s native; review C20-R2-1) before hashing (`TooLarge`), and the
   server answers 413 instead of building it (ask since a later commitment). Each step's log is
   grown from the one before, not rebuilt; the canonical sorts compute each row's key once. A
   node refuses a file whose check would pass 4,194,304 (`MAX_INSTALL_CHECK`, about 20 s), and its
   kept head names the served bytes (row counts and a digest), so a restart checks only the steps
   after it (review C20-R2-2).
8. **Still never a level or money.** `Publication::priced_answers` only counts; the new routes
   are free and bound to no market; the fence is unchanged except for the mount line.

Consequences: a client with the registry checks that each served outcome follows from the
published logs, and one that knew an earlier commitment checks that nothing committed since was
dropped or rewritten. Still trusted or open: priced answers in logs (C22), the registry a client
reruns with (not served; C23), paged logs (C21), the browser reading the on-chain latest and its
own chain view of the intake's posts (C41). Rejected: serving logs with priced values redacted
(the commitment hashes the whole list, so a redacted log cannot be checked); a node recomputing
with its own registry (a node has none, and the clients' rerun is the check that matters); one
budget for logs and records (a log answer costs up to a thousand record answers).

Review C20-R1 (fresh adversarial reviewer, same day, read-only): the rollback guard did not
survive a restart (High; the served head is now kept in the state directory), a reload ran on
the runtime's workers and rechecked every step (Medium; blocking pool, `check_after`), a server
could make a client hash links times rows without bound (Medium; client and server budgets), a
bad file stopped the whole node (Medium; 503 on the track routes only), `write_publication` was
weaker than the node (Low; same rule), the change stamp missed a same-length rename in one tick
(Low; inode and change time), no client test reached `check_extends` (Low; a stronger answer
rewriting a committed outcome now does), an e2e name overclaimed (Low; renamed), two doc
overclaims (Info; fixed). THREATS TR-C20-R1-*.

Review C20-R2 (second fresh adversarial reviewer, same day): the canonical sorts recomputed two
Keccak hashes per comparison, so a body inside the client budget cost 37 s native (Medium;
cached-key sorts, budget lowered to 64 links over 16,384 rows); a node's check of a file was
unbounded, ran at start, and a restart rechecked the whole chain against only the latest hash
(Medium; `MAX_INSTALL_CHECK`, the head names the served bytes); a request could park a runtime
worker on the read lock (Medium; requests answered from the last finished read); a history page
was built before the budget (Low); a node that served before and names no file said "none
published" (Low; 503). Item 2 (C22) kept: the 403 is stricter than the paid API, which serves a
level-1 row or level-2 result free once past its metric's fresh window, but it gives nothing
away; ROADMAP C22 now names the consistent release rule. THREATS TR-C20-R2-*.

## Amendment 6 (2026-10-18, ROADMAP C21 and C23, branch `track-pages`): chained logs in pages, the registry served
Closes TR-C20-5 (logs past 16,384 rows) and TR-C20-R2-2's TODO (a log format a node checks in
linear time); narrows TR-C20-4 (the registry a rerun needs is served). Code:
`onyx_track::chained` (`LogRow`, `ChainedCheck`, `check_chained`, `chained_statements_hash`,
`chained_resolutions_hash`, `rows_from`), `onyx_track::publication` (`PUBLICATION_VERSION_2`,
`Publication::{chained, with_format, push_with, check_cost}`, `chained_check_cost`),
`onyx_track::wire` (`wire_paged.rs`: `chained_log_body`, `log_rows_page`, `log_posts_page`,
`PagedLog`, `registry_body`, `check_registry_body`, `peek_commitment`), `onyx_core::registry`
(`registry_data.rs`: `RegistryData`, `RuleBuilder`, `NoRules`, `StaticRegistry::data`),
`onyx_source_btc::BtcRules` as a `RuleBuilder`, `onyx_api::track_serve` (pages on
`GET /v1/track/log`, `GET /v1/track/registry`), the browser's `check_track_log` and
`check_track_log_paged`. Spec: docs/spec/delivery.md "Track records". Research: RESEARCH.md
"Paged track logs (C21)".

1. **Design picked: a chained prefix hash, not a Merkle root over the logs.** In format 1 a
   commitment's `statements_hash` is one Keccak over its whole canonical (sorted) log, so every
   commitment of a chain hashes its whole log again: checking a chain costs links times rows, and
   a body could not be split, since a sponge's state halfway is not a commitment a client can
   check (a server could hand any 200-byte state; Keccak-f is a permutation). Format 2 keeps the
   logs in publication order (each commitment covers a prefix: the rows of the one before, then
   its own new rows; the coordinator's `push_with` appends them sorted, a check accepts any order
   within a link since the chain binds the order and the rerun sorts) and hashes them row by row: `head_0 = 0^32`,
   `head_i = keccak256(D_row ‖ head_{i-1} ‖ borsh(row_i))`, and the commitment field is
   `keccak256(D ‖ borsh((n: u64, head_n)))`, statements and resolutions each with their own two
   domains (`onyx-track-statement-row-v2`, `onyx-track-statements-v2`,
   `onyx-track-resolution-row-v2`, `onyx-track-resolutions-v2`). Every commitment is then a
   checkpoint of one stream: a client feeds the rows in order and checks each commitment's two
   hashes when its last row passes. A Merkle root over the logs (RFC 9162 style, with consistency
   proofs between commitments) would let a client check any page alone, but the extension rules
   (`check_extends`: no backdated statement, no answer rewriting a judged entry, no operator for a
   held source) need every earlier row anyway, and `compute` needs the whole latest log, so random
   access buys nothing a rerun can use; the chain is the smaller change (no new tree, no proof per
   page) and has the same linear cost. Rejected: per-page heads stated by the server (unverifiable
   until a commitment, so they catch nothing a lying server would not keep consistent).
2. **Versioned, never reinterpreted.** The commitment's Borsh layout and its hash domain
   (`onyx-track-commitment-v1`) do not change: `OnyxSignedBook.postTrackCommitment` checks both,
   and every format-1 commitment keeps its hash. What changes is what the two log hashes are over,
   and the domains tell the formats apart: a format-1 hash never checks as a format-2 one or the
   other way round (tested both ways). The format is named where the logs travel: a version-2
   `Publication` carries a format byte after its version byte (version 1 files are read and
   written byte for byte as before, always format 1), a format-2 log head says `format: 2`, and a
   format-1 client refuses a format-2 head as malformed. A chain never changes format (no real
   chain exists yet; the example stays format 1 with its pinned commitments). The commitment
   itself does not say its format: a client takes the format from the served form, and a wrong
   one fails the hashes.
3. **One check, linear.** `chained::ChainedCheck` takes the stream row by row and is, link by
   link, format 1's `check_steps`: each link's log hashes to it, its params to `params_hash`; each
   link is chained, later, keeps the params (`TrackParams::extends`), declares no operator for a
   source the earlier links hold, and every row new to it passes `check_extends`'s rules
   (statements revealable only after the link before; an answer on a fact the earlier links
   resolved no stronger than its weakest committed answer and public after the link before; one on
   a fact they named public after it). Earlier rows are kept by construction (a prefix), and a row
   seen before is refused. Per row: three Keccak hashes and a few ordered-set operations,
   so a page costs its own rows; the rerun of `compute` and `verify_log` runs once at the end over
   the whole log. 3,000 generated scenarios (backdated statements, early and stronger answers at
   both levels, operators declared later, sealed and unsealed params, changed resolver levels)
   give the same accept or refuse as `check_extends` (`the_stream_check_refuses_what_check_extends_refuses`).
   A node checks a whole chained file the same way (`Publication::check`), so a restart rechecks
   in linear time, and `check_cost` is rows plus steps (2,000 links over 40,000 rows: 42,000,
   where format 1 would cost 80 million).
4. **Pages.** On chained logs `GET /v1/track/log[?since=]` answers a head (the links, the stream's
   length, the intake's head without its batches) and the rows come from
   `?at=<latest>&from=<n>&since=<first link>`, 4,096 a page (exactly, but the last), the intake's
   posts from `?at=<latest>&posts=<n>` (whole posts, more than one only within 4,096 revealed
   statements, and at most 4,096 posts). One batch is never split: a batch whose page would pass
   48 MiB (about 27,000 revealed statements with their proofs; the intake allows 65,536) is 413
   `track_log_too_large`, so such an intake cannot be checked page by page yet (ROADMAP C25).
   Pages name the latest commitment and the run; a
   moved commitment is 409 `commitment_moved` and the client starts again from the head. Pages
   have their own free budget, 60 a minute per client; the head keeps the logs' 2.
5. **Cost caps kept.** A client refuses, before fetching any row, a head whose rows plus links pass
   `MAX_LOG_CHECK` (1,048,576), and counts revealed intake rows and posts against it as posts
   arrive; the server counts the same (rows, revealed rows, posts, links) and answers 413 instead
   of serving such a head or page. The client also counts every page's hex before decoding it,
   48 MiB a page and 512 MiB over the whole log (`MAX_PAGED_LOG_BYTES`). A node refuses a file whose
   `check_cost` passes `MAX_INSTALL_CHECK` (4,194,304): for chained logs about four million rows
   (a count bound; the time per chained row was not measured at that size, only the 42,000-row
   test, and the check holds a set of every row hash in memory).
   Format-1 bodies keep the 16,384-row cap and the links-times-rows budget unchanged. The client
   holds the whole log in memory (`compute` needs it), bounded by the same cap.
6. **The registry served (C23).** A `StaticRegistry` holds its address rules as code, so it had no
   byte form. `RegistryData` is everything else with each rule named by id; a client rebuilds the
   registry with the rule types it trusts (`RuleBuilder`, chosen at compile time like `RuleSet`;
   `NoRules`, `onyx_source_btc::BtcRules`) and uses it only when the rebuilt root is the
   commitment's `registry_root` (`check_registry_body`). The root hashes each rule's namespace,
   id, vectors and results, so a rule with other vectors or results is refused; one that agrees
   on every vector but differs elsewhere shares the root, which is why only trusted types are
   used (as for settlement, L25-F2). A version-2 publication carries the latest commitment's
   registry (`push_with` checks its root against the book's), and the node serves it at
   `GET /v1/track/registry` without checking it (the node has no address rules; every client
   checks). The registry holds no metric value (nothing priced); a recorded revocation names an
   address with an entity (a label row, covered by the root so it cannot be redacted), so the
   registry is withheld whenever it holds one, in every rights mode (review C23-R2-2), and
   wherever the logs are withheld. The browser reruns the example with the registry as the server
   serves it (`level5/track-example-registry.json`), rebuilt and checked first.
7. **Not done here.** A live node URL for the website's track section needs a deployed node and a
   domain (lead item LC-8): ROADMAP C24. `chain::verify_chain` (the chain-mode walk of every
   on-chain commitment from the first) still takes format-1 logs: ROADMAP C41. C22 is unchanged:
   chained logs holding a level-1 row or a level-2 result are 403 `track_log_priced` on the head
   and on every page.

Consequences: logs of any size up to the client budget are served and rerun, a long chain costs
one pass on every client and node, and the registry a rerun needs no longer has to be had out of
band. Still trusted or open: priced answers (C22), the browser's view of the chain (C41), a live
node for the site (C24).

Review C21-R1 (fresh adversarial reviewer, same day, read-only): no format-2 input accepted that
the format-1 rules refuse, formats never confused, the registry rebuild reproduces the root.
Fixed: a client held every page with no byte bound across pages (Medium; a running 512 MiB cap
counted before decoding, `with_byte_cap` tested); one large intake batch did not fit a page
(Medium; 413 named, TODO C25, docs corrected); the browser parsed every page before the head's
budget check (Medium; head first, input caps); empty posts uncounted and unbounded per page (Low;
counted, 4,096 posts a page); the server's cost check left out the intake (Low; counted); each
page rehashed the chain to find `since` (Low; hashes kept at install). Accepted: a restart keeps
the served head over steps and rows only, so a file with the same steps but another registry or
intake is served (availability only; clients check both). THREATS TR-C21-R1-*.

## Amendment 7 (2026-10-28, ROADMAP C66, north-star loop NL4): daily level-2 facts, resolutions only from recorded rounds

Slice 1 of docs/spec/track-records-served.md, built by `onyx_track::daily`.

1. **Daily rule.** A daily level-2 metric (`SubjectRule::Daily` or `Dated`, one fact per calendar
   day) gets `Close::PeriodStart { period_secs: 86_400, lead_secs: 3_600 }`: the fact for day `D`
   is the UTC period `[D, D + 1)`, as of the next UTC midnight, and statements close an hour
   before `D` starts in UTC. A station's local day west of UTC starts later still, so no part of
   it is public at the close. Tolerances, a reviewer decision recorded here: 0 for an earthquake
   count, 5 mantissa units (0.5 °C) for a daily high (`daily_rule`, `daily_fact`).
2. **Level 2 resolves these metrics.** Their params set `resolver_levels` to level 2, the level
   their values are checked at. The default (levels 1 and 3) stays for every other metric. The
   risk is the one of item 2: a colluding level-2 majority has no dispute path; level 5 moves no
   money, and the observers of the Base Sepolia rounds are one operator (THREATS B6-5), so these
   records are a demonstration until NS4 (THREATS TRS-1).
3. **Only recorded rounds resolve (R-TRS-1).** `resolution_of` builds a resolution only when the
   client's own node shows the round recorded at level 2 (`Recorded`, from
   `verify_observation_onchain`), after the day ended, with a value and a non-zero result hash.
   The evidence is the round's result hash, the resolver the template's source, `public_from` the
   record's block time. An open, failed or early round gives none
   (`daily::tests::no_resolution_from_an_open_round`). The module lives in `onyx-track` (no new crate
   depends on it), so the track-level fence of item 12 holds: callers pass the chain's recorded
   level in, and nothing outside `onyx-track` names a level next to a track type
   (`scripts/check-boundaries.sh`).
4. **The forecaster is parked.** The NWS forecast of the Denver high
   (`api.weather.gov/gridpoints/BOU/75,66`, `maxTemperature` in °C over the local daytime) passes
   the rights review: NWS information is public domain, may be served unaltered with the NWS
   identified, and must not be presented as NWS-endorsed (https://www.weather.gov/disclaimer;
   https://www.weather.gov/documentation/services-web-api). It fails TR-R2-4 instead: the NWS and
   NCEI are both NOAA, so the honest operator declaration makes every NWS statement on the NCEI
   high self-resolved, and an undeclared pair would hide that. The NWS also asks callers to put a
   contact in the User-Agent, which Onyx does not send. Parked until the owner decides: declare the
   two NOAA offices separate operators with this reason on record, or name another forecaster of
   a public value. Earthquake counts have no public forecaster (spec open question 3).
5. **Served.** A publication under these rules is served by the existing `/v1/track` routes and
   checks (`track_serve_tests::a_live_publication_serves_the_daily_records`). No node has one yet:
   it needs a forecaster (item 4). The site keeps the labelled example until then.

## Amendment 8 (2026-10-28, ROADMAP C66, north-star loop NL4): a testnet forecaster

Slice 2 of docs/spec/track-records-served.md. The decision of amendment 7 item 4 was made by the
coordinator under the owner's delegation ("whatever you want to do", 2026-10-27).

1. **NOAA grades NOAA, on testnet only.** The NWS gridpoint forecast of the Denver high
   (`noaa.nws.forecast.bou`) is stated on `noaa.ghcnd.usw00003017.tmax` and graded by the NCEI
   record. The two NOAA offices are not declared one operator for this Base Sepolia record, so
   TR-R2-4 does not turn its entries self-resolved; every place the record shows says it is a
   demonstration of the mechanism, not an independent track record (THREATS TRS-4). No record of
   this kind leaves testnet.
2. **A relay states for the NWS.** The NWS signs nothing. An Onyx relay key, listed in the
   review record docs/ops/nws-relay.md (the key's provenance is that document's hash under this
   deployment's origin), states each coming local day's forecast high, rounded half away from zero
   to a tenth of a °C from the JSON number's text (`onyx_source_ncei::nws`; no float reaches a
   value, ADR-0009). Each fetch is kept with the run (THREATS TRS-6, TRS-7).
3. **Confidence.** The NWS states a value, not a probability: every relayed statement declares
   500,000 ppm (spec open question 1). Scoring counts agreement within 0.5 °C; the Brier figure is
   shown only once a record is established (30 graded days and 5 subjects), and the site says "too
   few to score" until then (R-TRS-4; the existing `MIN_RESOLVED` rule, not a new number).
4. **Through the intake on Base Sepolia.** `adapters/source-ncei/examples/track-nws.rs`
   (`scripts/track-nws-sepolia.sh`) signs the statements, seals one batch per day and posts each
   header from a dedicated poster key (funded once by the deployer; its nonce counts the posts) to
   a sink no one holds a key for; `committed_at` is the final block time of each post. The run's
   state replays the same statements into the same headers, so a later run reveals and publishes
   without resending. A statement is sent only while its close is at least two hours away.
5. **Resolutions.** Only final level-2 rounds grade (amendment 7 item 3). NCEI days are
   observable 30 to 60 days after the day, so the first grade can come in December 2026; until
   then the record holds statements only, each sealed until its day ends.
6. **Served.** The publication goes to the export's `track/` folder; the node serves it on
   `/v1/track` once deployed (it carries its registry, so the metric is not label-derived under
   `ONYX_RIGHTS=fallback`). The data page asks the node and checks the record in the browser
   (`check_track_record`), and shows nothing when the check fails (apps/web/tests/ns16.mjs).
7. **First run, 2026-10-28.** Six days stated (2026-10-29 to 2026-11-03) in six batches, posted
   in Base Sepolia blocks 51,232,516 to 51,232,594, plus one funding transfer to the poster.
