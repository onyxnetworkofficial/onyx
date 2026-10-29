# Spec: the proof ladder, level 2 "re-observed" feeds and level 3 source-signed data

## Scope
How strongly each value is checked, as protocol data, and the first lane beyond rerun: level 2,
where several bonded observers read the same public source in the same window. Decided in
ADR-0027. Built in loop 57: `onyx_core::proof` (types), `onyx_coordinator::observe` (round and
settlement), `adapters/source-page` (an example fetcher). Not yet on chain, not yet served by the
API. Research: RESEARCH.md "Proof ladder level 2". Level 3 lane (a), source-signed data, is
decided in ADR-0028 and built in loop 62: `onyx_core::signed` (types),
`onyx_coordinator::signed` (key registry, verification, intake), `adapters/source-signed` (an
example source); see "Level 3" below. Level 5, a source's track record against later stronger
answers, is decided in ADR-0036 and built in `crates/onyx-track` (computation, commitment,
intake, served form, on-chain resolutions and commitment log, C7), served at `/v1/track` and
shown on the website (C6); see "Level 5" below.

## Levels
| Level | Name | How it is checked | Methods today |
|---|---|---|---|
| 1 | Recomputed | Anyone reruns a pinned recipe on pinned public inputs | `Recompute` |
| 2 | Re-observed | A strict majority of all assigned bonded observers saw the same value at the same public source in the same window | `ReObserved` |
| 3 | Source-proven | The source signs it, or a zkTLS proof of the exact HTTPS response, or a device signature | `FirstPartyAttestation` (labels), `SignedSource` (data, ADR-0028) |
| 4 | Claim (bond planned) | A submitter's value that anyone can check; no bonded observers, no proof; bond and challenge window planned | `OfficialSourceMatch`, `CrossSourceQuorum` |
| 5 | Track record | Judged now, scored later: labels by Brier (ADR-0016); a source's values by its track record against later level 1/3 answers (ADR-0036) | `CalibratedJudgment` |

`OfficialSourceMatch` and `CrossSourceQuorum` are level 4 (L2R-F8): anyone can redo the
comparison, but no bonded observers did, and nothing proves the compared bytes came from the
publisher. Only a settled round of bonded observers is level 2.

## Interfaces
- `ProofLevel { Recomputed, ReObserved, SourceProven, BondedClaim, TrackRecord }`, `number()`
  1..=5, `meets(minimum)`: true when at least as strong.
- `VerificationMethod::proof_level()`: the level of any claim. Not stored in the claim, so it can
  never disagree with the method (same rule as the lane).
- `MarketPolicy::of(levels)`, `accepts(level)`: a set of accepted levels, not a threshold.
  `MarketPolicy::money_default()` (also `Default`) accepts levels 1 and 3 only (L2R-F8): level 2
  joins once level-2 results can be disputed. Checked by the API, **advisory** (L2R2-F2):
  `Catalog::market_policy`; `GET /v1/settlement/{market}` answers 409 `proof_level_not_accepted`
  when the market's shard (`ShardMeta::proof_level`) is a level the policy refuses. The API's
  check is advisory: `OnyxSettlement.verifyRow` and the shard binding carry no proof level, and
  `GET /v1/proofs/...` returns the same `verifyRow` arguments for any row (`/v1/proofs` is not
  market-scoped, so it has no policy to apply). The level is on chain in `OnyxRegistry`: a market
  contract that wants the rule calls `verifyRowLeveled` and checks the attested level itself
  (below; AUD-2-F12).
- `ShardMeta::proof_level` is the weakest of the shard's inputs (L2R2-F3,
  `ProofLevel::weakest`): the recipe rerun is level 1, and a pinned label set has its own level
  (entity-signed: 3; a published list: 4; scored judgment: 5) because rerunning the recipe does not
  check the labels. `ShardMeta::from_spec` sets level 5 for a shard that pins a label set (fail
  closed); the caller supplies the set's reviewed level with `with_label_set_level` (for Bitcoin,
  `onyx_source_btc::label_set_proof_level`, from fixtures/labels/PROVENANCE.md: both reviewed test
  sets are level 4).
- `VerificationMethod::ReObserved { spec, result }`: a claim whose value is the settled result of
  a level-2 round of the feed `spec` (an `ObservationSpec::id`); `result` is that round's
  `ObservationSettlement::result_hash()` (L2R-F3). Appended as the last variant (tag 5), so the
  encodings of existing methods do not change (pinned in fixtures/claims/vectors.json, L2R-F10).
  Neither hash may be zero (`Claim::validate` checks only that shape).
  `ObservationSettlement::backs(feed, method, value)` takes the round's feed itself and checks its
  id, so kind and scale never come from the caller (L2R2-F6); it accepts the claim only if it names
  this feed and this result, the round settled a value (not `NoResult` or `SourceUnavailable`),
  and the claimed value equals it (`Bytes`; `Int` at scale 0; `Fixed` at the feed's scale).
- **Claim intake** (L2R2-F1): `ObservationResults` is the coordinator's store of results it
  settled itself. `ObservationResults::settle(round, now)` is the only way a result enters (each
  round once, `AlreadySettled` otherwise; and one result per fact, `FeedAlreadySettled` otherwise,
  L2R3-F5 and L2R4-F1: the fact is `fact_key` = (metric, subject, window end), whatever the other
  spec fields, so a coordinator cannot settle competing observer sets or competing feeds for one
  fact and keep the result it likes; `ObservationRound::settle` is crate-private and the store is
  not `Clone`, L2R4-F2). `accept_claim` refuses claims of every other method (`NotLevelTwo`,
  L2R4-F4); `ObservationSettlement::backs` is crate-private (L2R4-F3). `ObservationResults::accept_claim(claim, registry)` is the one intake, and
  `ObservationResults::decode_claim(bytes, registry)` its byte form; `Claim::decode` refuses
  `ReObserved` (`NeedsObservationIntake`, L2R3-F3). For `ReObserved` the intake looks the result up
  in the store, calls `backs`, and binds every claim field a settled value could be reused for
  (L2R3-F1):
  | Claim field | Must be |
  |---|---|
  | `method` | `ReObserved { spec: feed id, result: this result }`, a round that settled a value |
  | `value` | exactly the settled value (`backs`) |
  | `metric` | the feed's `metric` |
  | `subject` | the feed's `subject` |
  | `source.source` | the feed's source id (`rights.source`) |
  | `source.snapshot` | `Document { published_unix: window_end, content_hash: result_hash }` |
  | `rights` | the feed's rights |
  | `label_set` | none (a level-2 value depends on no label set) |
  | `evidence` | empty (the result hash is the evidence) |
  | `confidence` | none (objective lane; `validate` refuses one) |

  `ObservationSettlement` has private fields and no decoder, so a claim submitter can never supply
  or edit a settlement. Every path that takes in claims must use the intake. Across coordinators,
  one result per feed is a store rule only: the on-chain port must enforce it (TODO).
- `ObservationSpec { feed, metric, subject, fetcher, url, selector, kind, window_start,
  window_end, min_observers, rights }`: `metric` (a dotted id) and `subject` are what a settled
  value is a claim about (L2R3-F1); `id() = keccak256("onyx-observe-spec-v1" ‖ borsh(spec))`.
- `ObservationKind::Exact { max_len }` or `Numeric { scale, tolerance }` (mantissa units).
- `Answer::{Bytes, Number, Unavailable}`; `Observation { answer, observed_at, page_hash }`.
- `observation_commit(round_id, observer, observation, salt) = keccak256("onyx-observe-v1" ‖
  round_id ‖ observer(20) ‖ borsh(observation) ‖ salt)`, where `round_id =
  keccak256("onyx-observe-round-v2" ‖ spec_id ‖ borsh(cfg) ‖ borsh(observers) ‖ draw_hash)`
  (L2R-F6, L2-F2).
- `DrawSeed { pool, beacon_height, beacon, attempt }`, `pool_hash(pool) =
  keccak256("onyx-observer-pool-v1" ‖ borsh(pool))`, `draw_hash(seed) =
  keccak256("onyx-observe-draw-v1" ‖ borsh(seed))`, `draw_observers(spec_id, pool, seed, size,
  min_bond)` and `check_draw` (R-OBS-2a).
- `Fetcher { id(), extract(spec, page) -> Answer }`; `observe(fetcher, spec, page, now)` builds
  an observation (refuses a fetcher the feed does not pin).
- `ObservationResults::announce_draw(spec, cfg, members, beacon_height, chain)` then
  `open_round(spec, cfg, snapshot, chain)` open a round (R-OBS-2a; `chain: &dyn ChainView` gives
  the height and the beacon block hash); runners use `observe::draw_and_open(store, spec, cfg,
  members, chain: &mut dyn DrawChain, save)`, which also announces and fixes the draw on
  `OnyxRegistry` and saves the book; `commit(now, observer, hash)`,
  `reveal(now, observer, observation, salt)`; settled only through `ObservationResults::settle`
  (L2R3-F5) into an `ObservationSettlement` (read with
  `spec_id()`, `round_id()`, `outcome()`, `statuses()`, `entries()`); `result_hash() = keccak256("onyx-observe-result-v1" ‖
  borsh(spec_id, round_id, outcome))` is what a level-2 claim points at.
- `aggregate(kind, members, answers)`: the pure aggregation rule below.

## Rules (level 2)
- **R-OBS-1 Feed.** `feed` is a dotted id; `fetcher` is a non-zero content hash; `url` is
  `https://` and visible ASCII (at most 512 bytes); `selector` visible ASCII (at most 256);
  `window_start < window_end`, at most one day apart for an exact feed and at most 300 s for a
  numeric feed (L2R-F1: a moving number drifts within the window, and the feed's tolerance must
  cover that drift; a short window keeps it small); a numeric `tolerance` is at most 10^30
  mantissa units (L2R-F7; feed review still judges it per source); `min_observers` 3..=255. The
  source's `DataRights` must pass `check_ingest`: a source that forbids redistribution is never a
  feed.
- **R-OBS-2 Round.** Exactly `min_observers` observers, drawn from a bonded pool (R-OBS-2a; the
  spec itself must equal the template, L2SRC-R3-F3), strictly ascending by address,
  each with a bond of at least `outlier_slash`. `commit_close` is at or after `window_end` and at
  most 600 s later; reveals run for at least 60 s after it; `miss_slash > 0` and `outlier_slash
  >= miss_slash`; `reward`, `outlier_slash` and `miss_slash` fit `i128` (no silent clamp,
  L2R-F7).
- **R-OBS-2a Draw (L2-F2, ADR-0027 amendment 6).** The coordinator never lists observers. It
  announces the spec, config, pool snapshot (1..=1024 members, ascending) and a beacon height above
  the current chain height, once per fact and attempt; the snapshot's bonds are free bonds (bond
  minus slashes at stake in open rounds). The round opens with the beacon (the block hash at that
  height), once, matching the announcement. Members whose free bond covers `outlier_slash` get
  the key `keccak256("onyx-observe-draw-v1" ‖ spec_id ‖ draw_hash ‖ address)`; the
  `min_observers` smallest keys are the observers. Opening puts `outlier_slash` of each observer's
  bond at stake until settle (`ObserverExposed` past the full bond). A dated fact's next round after
  one without a value, or after `abandon_draw`, is the next attempt with a fresh announcement and
  beacon, counted toward the cap (SEC-R2-F1); attempts never reset (at most 64: 0..=63). Each
  attempt is counted at announcement and the book saved before the beacon height; a restarted
  store follows the registry's `drawCount`. An abandoned fixed-window draw does not close its
  fact. A consumer recomputes the draw from the served `pool` and `draw`, refuses any other
  observer set, and asks `OnyxRegistry.drawMatches` (announced before the beacon height, latest
  attempt, beacon fixed from `blockhash`; L2RND-R1, ADR-0030 amendment) with its own draw
  commitment `draw_commit(pool_hash, spec_id, cfg_hash)`: `announceDraw` commits the pool, the
  spec (so the window) and the config (so `min_bond`) before the beacon exists, and
  `recordObservation` refuses any other (L2RND-R3, ADR-0027 amendment 6 point 11). The
  beacon is an L2 block hash: the sequencer can bias it (ADR-0022 item 5). Per (metric, subject),
  across all windows, at most `MAX_DRAWS` = 3 announced draws may be without a recorded value
  until the registry owner records `reopenDraws` (each grants 3 more); `announceDraw` enforces
  it and `drawBudget` reads it (L2RND-R2). Runners check it before announcing, announce on chain
  before counting locally, treat an already-fixed beacon as success, and raise a dated fact's
  non-value count to the chain's strikes. A fixed feed's `window_start` is a multiple of
  `window_secs`. Clients use `client::verify_observation_onchain` (level 2, `drawMatches`, attempt
  below the allowance). This caps, but does not remove, the coordinator's choice among the
  public draws of a subject.
- **R-OBS-2b Polite fetching (partner tracker T2, ADR-0027 amendment 16).** An observer fetches
  at its own slot, `onyx_core::fetch::fetch_time` = `window_start` + (the first 8 bytes of
  `keccak256("onyx-fetch-slot:v1" || round id || observer address)` as a big-endian `u64`, modulo
  `window_len - FETCH_MARGIN_SECS`), not at the window start; a window no longer than
  `FETCH_MARGIN_SECS` (30 s) fetches at the start. The slot is a rule for honest observers: the
  coordinator sees commits, not fetches. A coordinator admits a round only while the observers of
  the open rounds on the same origin (`onyx_core::fetch::url_origin`: lowercase DNS host, no IP,
  no user information) whose windows overlap it stay within a budget
  (`onyx_coordinator::origin_budget::OriginLoad`, default 16).
- **R-OBS-3 Commit.** Accepted from `window_start` to `commit_close`, once per observer. The
  commit contains the observer's address, so a copied hash cannot be revealed by the copier; a
  hash another observer already posted is NOT refused (L2R-F5: refusing it would let whoever saw
  a hash first block its owner). The commit binds the round id, so a reveal of one round never
  opens a commit in another round of the same feed (L2R-F6).
- **R-OBS-4 Reveal.** Accepted only when it ARRIVES after `commit_close` and until `reveal_close`,
  by the coordinator's own clock, so no answer is visible while anyone can still commit. The
  arrival time is kept with the round (saved with the round book before any receipt) and the
  reveal is checked at it, since receipts wait for the finalized log block (R-OBS-10), on Base
  Sepolia ~20 minutes after `commit_close` (review REVLOG-R4-3). It must hash to the commit; its shape must match the
  feed (`Bytes` for exact, `Number` for numeric, `Unavailable` for either); `observed_at` must lie
  in the window; `page_hash` is the Keccak-256 of the raw fetched bytes, zero only for
  `Unavailable`.
- **R-OBS-5 Observation time.** The settled value is "as of `window_end`". `observed_at` is
  self-reported and only checked to lie in the window; the protocol never trusts an observer's
  clock beyond that. Sources whose value changes faster than the window are not level-2 feeds
  (exact) or need a tolerance that covers the drift (numeric).
- **R-OBS-6 Quorum.** `quorum = floor(members / 2) + 1`, counted over ALL assigned observers, not
  over reveals. Silent observers can never let a minority decide.
- **R-OBS-7 Aggregation.** Unavailable wins if at least `quorum` observers answered it. Exact: the
  most common byte string wins if at least `quorum` observers revealed it. Numeric: the band is
  `center ± tolerance`, where `center` is the lower median of all numeric reveals; the value wins if
  at least `quorum` numbers lie in the band, and the settled value is the lower median of those
  numbers. Property (tested on 2,000 random cases): if a strict majority reports numbers within
  `tolerance` of each other, all of them are in the band, whatever the minority reports. A
  minority inside the band can move the settled value, but only within the honest majority's own
  range (L2R-F4, tested). Anything else is `NoResult`. Integers only (ADR-0009).
- **R-OBS-8 Payouts.** Value outcome: an answer that agrees (exact: same bytes; numeric: within
  `tolerance` of the band center) is `Agreed` (`+reward`); a number between one and two
  tolerances from the center is `NoCall` (L2R-F1: honest drift, so a minority cannot choose which
  honest edge is slashed); a different value beyond that is `Outlier` (`-outlier_slash`);
  `Unavailable` is `Missed` (`-miss_slash`, honest outages are cheap, permanent laziness is not).
  Unavailable outcome: nobody is paid (L2R-F2: `Unavailable` costs no fetch and leaves no
  evidence, so paying it would reward a lazy majority); `Unavailable` is `NoCall`; a value that at
  least one other revealed value corroborates (identical bytes, or a number within `tolerance`) is
  `NoCall` (L2R3-F4: evidence the source was up, so a lazy majority cannot slash the honest
  fetchers); a lone, uncorroborated value is `Missed` (`-miss_slash`, L2R2-F4: a stale guess must
  never beat an honest `Unavailable`). `NoResult`: every revealer is
  `NoCall`, except in a numeric round where a strict majority of ALL assigned observers revealed
  numbers within twice the tolerance of the center: then a number beyond twice the tolerance is
  `Outlier` (L2R2-F5: a far minority cannot force `NoResult` for free when honest answers drift
  between one and two tolerances apart; tested, including a property that such an honest majority
  is never an outlier). An honest split with no such majority judges nobody. Always: committed but never revealed is `Unrevealed`; never committed is `Absent`.
  Both are neither paid nor slashed (SIG-R1-F1): the coordinator alone says a reveal or commit is
  missing, and it can withhold a signed one, so a slash needs a public record that the observer did
  not answer. The transcript log (ADR-0027 amendment 8) proves the opposite direction only: a
  commit an observer posted on chain, or a reveal the coordinator's receipt key acknowledged,
  must be in the served round, or clients refuse the round (`transcriptComplete`). A missing
  receipt does not prove silence (the coordinator may refuse to sign), so the rule stands and
  withholding a wrong answer still costs nothing but the reward. Slashes never exceed the bond.
- **R-OBS-10 Transcript log (ADR-0027 amendment 8).** Every draw names its receipt signer on
  chain when it is announced (`drawReceiptSigner`). An observer posts its signed commit on chain
  (`postCommit`, anyone may relay; the block time is kept) and, when the coordinator accepts its
  reveal, gets a receipt (`reveal_receipt_message`, signed by the receipt key) and posts it
  (`postReceipt`). Neither shows the answer. A coordinator reads posted commits ONCE per round, from one
  block N at or below the chain's finalized block whose time is past `commit_close`, before it
  takes any reveal into the round or signs any receipt, keeps that snapshot with the round (saved
  with the round book) and never reads the log again (review REVLOG-R3-1); it uses them
  (`observe::absorb_posted_commits`: a commit posted in [window start, `commit_close`] replaces
  one sent off chain; a reveal that does not open it is a non-reveal), so no posting observer
  can veto a round (review REVLOG-R1-H1). It asks `transcriptComplete` before it records or
  publishes a round and stops if false. A client passes every drawn observer of every served
  round (value round and attempts) with its served commit and reveal flag to
  `transcriptComplete`; false means a posted in-phase commit or a receipted reveal is missing,
  and the client refuses the answer. Limits: the coordinator sees a reveal's value before it
  signs the receipt, so it can refuse receipts selectively (THREATS REVLOG-1, Medium); a sealed
  round's `revealed` list is the server's word until its fact closes (REVLOG-5).
- **R-OBS-11 Sealed reveal posts and attempt records (ADR-0027 amendments 10 and 11; protocol
  in `onyx_coordinator::sealed` and `OnyxRegistry`, wired through the observer and coordinator
  processes, the round book, served rounds and the Rust and web clients by ROADMAP C11; not
  deployed).**
  A draw announced after `setRevealKey` snapshots the coordinator's reveal key
  (`drawRevealKey`). An observer that seals posts its signed reveal on chain encrypted to that key
  (`postSealedReveal`: ephemeral key `R`, ciphertext, a Schnorr proof of knowledge of `R`'s secret
  bound to round, attempt and observer (`keyProofValid`, one `ecrecover`; an ECDSA signature by
  `R` is not such a proof, review C4-R1-1), and the observer's signature over
  "Onyx observe sealed reveal v1" naming `R` and the ciphertext hash), only once the chain is
  past `commit_close`; the plaintext is padded to one length per feed (`sealed_len`, review
  C4-R2-1). The registry keeps the first post per slot with its block time. A post with
  a block time in (`commit_close`, `reveal_close`] must be in the round: revealed (status 1) or
  dismissed (status 2) with `S` and a Chaum-Pedersen proof that clients check and that a valid
  reveal can never pass (`check_dismissal`); `revealLogComplete` (the amendment-8 rules plus these)
  is false otherwise (also for entries not strictly ascending). That the entries are exactly the
  drawn set, the window is the round's config and the round id is the recomputed one is the
  client's check (the chain knows none of them, review C4-R1-2). Before a non-value round is served, the coordinator records it
  (`recordAttempt`, only after `reveal_close` and only while `revealLogComplete` holds): the record
  binds every drawn observer's commit and status and the result hash; a client asks
  `attemptMatches` for the sealed round and again, with the entries it rebuilds, once the round is
  unsealed. The coordinator reads every drawn slot ONCE, at its chain's finalized block whose time
  is past `reveal_close`, re-checks each stored key proof before opening anything, and keeps the
  read only when the chain's `revealLogComplete` accepts the round with it; it records every round
  it settles (`recordObservation`, `recordAttempt`) before serving it. A client asks
  `drawRevealKey` (the served key must be the draw's), `revealLogComplete`, each served post
  (`sealedRevealOf`, `sealedKeyProofOf`: byte for byte the chain's) and, for every round served
  sealed and every round without a value of a keyed draw, `attemptMatches` (amendment 11 item 8;
  delivery.md "Sealed reveals"). Limits: the coordinator can open a post before `reveal_close`
  (THREATS C4-7, time-lock C12); a sealed outcome is still not recomputable while sealed (C4-8);
  spool reveals keep REVLOG-1.
- **R-OBS-9 Fetcher contract.** A fetcher turns page bytes into an answer and nothing else; same
  bytes, same answer. A selector that finds nothing usable answers `Unavailable`, never a guess.
  The example `JsonPointerFetcher` (id `keccak256("onyx-fetcher:json-pointer:v1")`): exact feeds
  take a trimmed JSON string, a boolean or a JSON integer; numeric feeds take a plain decimal
  string or a JSON integer, truncating digits beyond the scale toward zero; JSON floats are
  refused.

## Level 3 lane (a): source-signed data (ADR-0028)
### Interfaces
- `SourceKey { source, signer: [u8; 20], provenance: KeyProvenance }`; `id() =
  keccak256("onyx-source-key-v1" ‖ borsh(source, signer))` (provenance not in the id, so a
  re-review keeps it). `KeyProvenance::WellKnown { origin, document_hash, fetched_unix }`
  (`https://<origin>/.well-known/onyx-source-keys.json`, RFC 8615) or `DnsTxt { origin,
  record_hash, fetched_unix }` (`_onyx-source-keys.<origin>`). `origin` is a lowercase DNS host
  name with at least two labels, never an IP (`is_origin_host`).
- `SignedPayload { source, key, metric, subject, value, observed_unix, sequence }`; `digest() =
  keccak256("onyx-signed-source-v1" ‖ borsh(payload))`; `message() =
  "onyx-signed-source-v1:0x" ‖ hex(digest)` (88 ASCII bytes), which the key signs with EIP-191
  `personal_sign`.
- `SignedReport { payload, signature: [u8; 65] }` (`r ‖ s ‖ v`); `hash() =
  keccak256("onyx-signed-report-v1" ‖ borsh(report))`.
- `VerificationMethod::SignedSource { key, report }` (tag 6): `key` is `SourceKey::id`, `report`
  is `SignedReport::hash`. Level 3, objective lane. `Claim::decode` refuses it
  (`NeedsSignedSourceIntake`).
- `SignedSources` (not `Clone`): `new()` (intake only) or `with_reviewer(name) -> (store,
  Reviewer)`; reviewer actions `register_key(by, key, reg, now, reason)`, `retire_key(by, id,
  at, now, reason)`,
  `mark_compromised(by, id, since, now, reason) -> report hashes to re-review`,
  `reinstate_key(by, id, now, reason)`, `resolve_fact(by, fact, keep, now, reason)`; reads
  `key_status(id)` (life cycle), `key_state(id)`, `is_suspended(id)`, `fact_status(fact)`,
  `settleable(fact, now)`, `accepted(report_hash)`, `review_list()`, `equivocations()`,
  `decisions()`, `book_sizes()`; and the one intake `accept_claim(claim, report, reg, now) ->
  Intake { level, fact, report }` / `decode_claim(claim_bytes, report_bytes, reg, now)`.

### Rules
- **R-SIG-1 Key registration.** The key's source is in the registry (rights reviewed); signer
  non-zero; provenance well formed (origin host, non-zero hash and time); a key id registers once;
  every key of one source is published under the same origin (`OriginMismatch`: moving a source to
  another origin is a new review), an origin publishes keys for one source only (`OriginTaken`,
  L3R-F7), and a signer (private key) is registered once, under one source, in any life-cycle
  state, for good (`SignerTaken`, L3R4-F1): a compromise of the signer can never leave it active
  under another source. The intake cannot fetch the web: the provenance is the review
  record that the origin, and so the key, belongs to the publisher.
- **R-SIG-2 Signature.** EIP-191 over `message()`, canonical form only (`v` 27 or 28, `s` at most
  n/2, EIP-2), recovered signer equal to the key's (`onyx_labels::attest::eip191_recover`).
- **R-SIG-3 Key life cycle and suspension (ADR-0028 amendment 2).** The life cycle only moves
  forward: `Active` -> `Retired { at }` -> `Compromised { since }` (any state may jump to
  `Compromised`; a retirement is final and `at <= now`; `since <= now` and may only move earlier).
  Suspension is a separate flag set by an equivocation; reinstating clears the flag only. A report
  counts only if: `Active`; or `Retired { at }` with `observed_unix` in `[at - RETIRED_GRACE_SECS,
  at)` and arriving before `at + RETIRED_GRACE_SECS` (1 h; the signer chooses `observed_unix`,
  L3R-F1, L3R2-F4); never `Compromised` whatever the stated time; and never while suspended. No
  sequence of reviewer actions re-enables a retired or compromised key (L3R2-F1). Retiring a key
  withdraws its accepted reports observed at or after `at` (they were accepted while it was active,
  dated ahead or under a retroactive `at`): they go to re-review and their facts are cleared
  (L3R5-F1).
- **R-SIG-4 Freshness.** `observed_unix` at most `MAX_CLOCK_SKEW_SECS` (300) after `now` and at
  most `MAX_REPORT_AGE_SECS` (7 days) before it.
- **R-SIG-5 Replay.** Each `(key, sequence)` carries one payload: the book of used sequences and
  the 7-day window are the whole replay guard. Entries observed more than 7 days + 300 s ago are
  pruned (such a report is refused as stale anyway), except evidence (L3R2-F5). A payload the
  store has recorded (accepted, evidence, or either side of an equivocation) again, under any
  signature, is `Replay` and changes nothing (L3R4-F2). There is no high-water mark, so
  reports may arrive in any order and a later report delivered first blocks nothing (L3R-F5). The
  sequence `u64::MAX` is reserved (`SequenceReserved`, L3R-F8).
- **R-SIG-6 Fact book (ADR-0028 amendment 1).** A fact is `signed_fact_key = keccak256(
  "onyx-signed-fact-v1" ‖ borsh(source, metric, subject, observed_unix))`, across all keys of the
  source. Its status is `Accepted(report)` or `Disputed(reports)`. Every validly signed report is
  recorded as evidence. Checked after the signature verifies, so only key holders reach it, and
  before acceptance, so an old unused sequence cannot hide a contradiction (L3R-F6):
  - fact unknown: the report is accepted;
  - fact accepted with the **same value**: `AlreadyStated`, a harmless restatement (a re-publish,
    or two keys during a rotation): nobody is suspended and nothing is stored (L3R-F2, L3R2-F5);
  - fact accepted with a **different value**: `Equivocation`. The fact becomes `Disputed` with both
    reports, the accepted one is withdrawn to the re-review list, and **every key involved** is
    suspended, the first signer as well as the second, so a thief who states a lie first cannot
    get the honest key alone suspended (L3R-F3);
  - fact disputed: the report joins the dispute as evidence and nothing is accepted. One report
    per key: the same value again is `AlreadyStated`, another value from a key already in the
    dispute is that key equivocating. A new key whose value differs from any report in the
    dispute took a side in a contradiction: `Equivocation`, and it is suspended like the keys
    already in it (L3R3-F2); a new key agreeing with every report gets `FactDisputed`.
  - A different payload under a used `(key, sequence)` is an `Equivocation` too: the key is
    suspended, and the report accepted under that sequence, if still accepted, is withdrawn and its
    fact disputed (with both reports when the two state the same fact). The second report is then
    checked against the fact book on its own fact, without being accepted (L3R5-F2): a different
    value from another key's accepted report disputes that fact and suspends that key too; a
    dispute it is not in takes it as evidence (its key is suspended already); the same value or an
    unknown fact changes nothing more.
  `Equivocation { fact, withdrawn }` names the fact and the accepted reports it withdrew.
  Consumers must treat a disputed fact as having no value, and settle only on `settleable(fact,
  now)`: an accepted fact that has stood for `DISPUTE_WINDOW_SECS` (1 h), time for a contradiction
  to arrive (L3R2-F7). A reviewer settles a dispute with `resolve_fact(fact, Some(report))` (a
  report of the dispute whose key may sign it now: life cycle and no suspension, L3R2-F6) or
  `resolve_fact(fact, None)` (cleared; the source may state it again).
- **R-SIG-6a Compromise recovery.** `mark_compromised(key, since, now)` withdraws everything the
  key stated from `since` on (observed or recorded at or after `since`): its accepted reports leave
  the book for the re-review list, their facts are cleared, and its reports drop out of open
  disputes. Nothing is restored on its own (ADR-0028 amendments 3-4, L3R3-F1, F3): a dispute stays
  disputed (even with no reports left) and every suspension stays until the
  reviewer resolves the fact (`resolve_fact`) and reinstates the keys (`reinstate_key`). The
  source's new key can then state the true values (L3R-F4).
- **R-SIG-6b Reviewer actions.** Register, retire, compromise, reinstate and resolve need the
  store's `Reviewer` (`NotReviewer` otherwise) and append to the decision log with the reviewer's
  name, the time and the reason (L3R2-F2, F6, L3R3-F4). The store takes no decision on its own.
- **R-SIG-6c Invariants (ADR-0028 amendments 3-4, checked exhaustively against an independent
  model).** (1) A report is accepted only if its key's life cycle permits it (R-SIG-3) and the key
  is not suspended. (2) A suspension clears only by `reinstate_key` of that key; a dispute ends
  only by `resolve_fact` (even when a compromise left it with no reports). (3) The life cycle only
  moves forward: no retired or compromised key is re-enabled, and a signer is registered once
  (R-SIG-1). An honest key that joins a two-sided dispute is suspended too, because its value
  differs from one side: the price of joining a contradiction, cleared by the reviewer. Every
  time rule runs on one monotonic clock: the store refuses a `now` earlier than one it already
  used (`ClockWentBack`), and callers pass chain time, the latest block's timestamp; on chain the
  clock is `block.timestamp` (R-SIG-ON-4, L3R-F8).
- **R-SIG-6d Book bounds (L3R3-F6).** Sequence and report entries: one 7-day + 300 s window plus
  evidence held for open disputes and the re-review list, released by the next sweep after the
  dispute or review ends. Fact book and accepted map: one entry per accepted fact, growing with
  what sources publish (restatements are not stored). Re-review list: grows only with compromises
  and equivocations. The clock never steps back (R-SIG-6c), so a pruned report can never be
  replayed against an emptied entry; the chain book prunes nothing (R-SIG-ON-2).
- **R-SIG-7 Claim binding.** Every claim field must be exactly the payload's:
  | Claim field | Must be |
  |---|---|
  | `method` | `SignedSource { key: payload.key, report: report hash }` |
  | `value` | `payload.value` |
  | `metric` | `payload.metric` |
  | `subject` | `payload.subject` |
  | `source.source` | `payload.source` (the key's source) |
  | `source.snapshot` | `Document { published_unix: observed_unix, content_hash: report hash }` |
  | `rights` | the registry's rights for the source (`Claim::validate`) |
  | `label_set` | none |
  | `evidence` | exactly `[{ kind: "signed-report", content_hash: report hash }]` |
  | `confidence` | none (objective) |
  Refusals before the books are updated consume nothing (no sequence, no fact) in this store. On
  chain a validly signed payload burns its `(key, sequence)` for good whatever this table says
  (R-SIG-ON-1): a source never signs a second payload under a sequence it has used, even for a
  payload the intake refused (THREATS L3C-1).

### Level 3 on chain: the signed book (ADR-0028 amendment 6)
`contracts/src/OnyxSignedBook.sol` is the book of record for every coordinator and client
(THREATS L3-F7, L3R-F8). `SignedSources` stays as each coordinator's pre-check and as the model
the contract is checked against.
- **R-SIG-ON-1 Intake.** `postReport(payload, signature)`, open to anyone (a relayer cannot forge,
  only deliver). `payload` is the Borsh `SignedPayload` the source signed. The contract hashes
  those bytes (`digest = keccak256("onyx-signed-source-v1" ‖ payload)`), verifies the EIP-191
  signature over `message()` with `OnyxSig.recover` (canonical form only), and parses the fixed
  layout: source string (u32 LE length, bytes), 32-byte key id, metric string, subject (tag 0..=3,
  string), value (tag 0..=4 and exactly its variant's bytes: 16 for `Int`/`UInt`, 17 with scale
  1..=38 for `Fixed`, one byte 0 or 1 for `Bool`, a u32 length up to 1024 and the bytes for
  `Bytes`), then `observed_unix` and `sequence` (u64 LE). Anything else, a zero key or a zero
  observation time is `BadPayload`. Derived on chain exactly as Rust does:
  `fact = keccak256("onyx-signed-fact-v1" ‖ borsh(source, metric, subject, observed_unix))` (the
  payload's bytes without the key, value and sequence), `valueHash = keccak256(borsh(value))`,
  `reportHash = keccak256("onyx-signed-report-v1" ‖ payload ‖ signature)` (`SignedReport::hash`),
  `keyId(source, signer) = keccak256("onyx-source-key-v1" ‖ borsh(source, signer))`. Check order
  as `accept_claim`: layout, reserved sequence, key registered (`UnknownKey`), key's source
  (`KeySourceMismatch`), life cycle and suspension (R-SIG-3), signature, freshness (R-SIG-4),
  replay, sequence book, fact book. Claim binding (R-SIG-7) stays off chain: a consumer binds a
  claim to `reportHash`. The contract does not check UTF-8 or the registry's metric and subject
  rules; only a key holder can sign such a payload, and it burns that key's sequence: anyone
  holding it can post it, so a corrected payload re-signed under the same sequence is an
  equivocation (THREATS L3C-1). The signed message names no chain id and no book address: a
  report is the source's statement, not an authorization, and the same bytes verify off chain
  (R-SIG-2). Anyone may therefore post it to every book that registered its key. A key has one
  sequence space across every deployment and the off-chain store, so a source never runs a
  separate counter per chain (a rehearsal or testnet key is never registered in a production
  book), or two books record an equivocation (THREATS L3C-11).
- **R-SIG-ON-2 Books.** `sequenceOf[key][sequence]` holds the first payload digest for good and
  the payload book (`report(digest)`) every recorded report: nothing is pruned, so a sequence
  reused after any time is still an equivocation (stricter than the off-chain store, which forgets
  after the 7-day window). There is no high-water mark (L3R-F5): reports land in any order. Facts
  and disputes follow R-SIG-6 exactly; a key adds at most two reports to a dispute (a
  contradiction suspends it, and a suspended key posts nothing), so dispute loops are bounded by
  twice the source's key count, which only the reviewer grows.
- **R-SIG-ON-3 Withdrawals read lazily.** Retiring and compromising cost O(1). An accepted report
  reads as withdrawn (its fact as having no value, open to a new statement) when its key was
  retired at a point at or before its observation time, or compromised from a point at or before
  its observation or acceptance time; a dispute's report drops out when its key was compromised
  from a point at or before its observation or recording time. `retiredAt` is kept after a
  compromise. This gives the eager store's results: `OnyxSignedBookTraces.t.sol` replays 160
  seeded traces of 20 events from `SignedSources` (`tests/level3_chain_book.rs`) and requires the
  same result, key states and fact states (status, accepted report and time, settleability,
  dispute list) after every step.
- **R-SIG-ON-4 Clock.** Every time rule uses `block.timestamp`: freshness (`observed_unix` at most
  300 s after it, at most 7 days before), the retirement window and grace, `retireKey(at)` and
  `markCompromised(since)` not after it, `recordedAt`, `acceptedAt` and the dispute window. On
  Base (OP Stack) each block's timestamp is the previous one plus the block time, never before
  its L1 origin's and at most the sequencer drift ahead of it: the sequencer does not choose a
  block's timestamp, but by producing blocks faster than their block time it can run block time
  up to `max_sequencer_drift` (1800 s since Fjord) ahead of the L1 clock, which shortens the
  dispute window and the retirement grace in real time by as much (THREATS L3C-12). It can also
  delay or reorder transactions (Assumptions).
- **R-SIG-ON-5 Reviewer.** One immutable `reviewer` address registers (one signer once, one
  origin per source, one source per origin; `provenance` is the keys document's Keccak, the
  review record), retires, compromises, reinstates and resolves, as R-SIG-6b; each decision is an
  event with the reviewer's reason. Single key on testnet (multisig or timelock: ROADMAP H1).
- **R-SIG-ON-6 Reads.** `settleable(fact) -> (ok, reportHash, valueHash)`: the accepted report
  once it has stood `DISPUTE_WINDOW_SECS` of block time. A client checks a claim by reading it at
  a finalized block and comparing `reportHash` with the claim's report (the claim names the
  report hash, R-SIG-7, so another signature over the same payload does not match) and
  `valueHash` with its value (`signed_chain::SignedBookCast::check_settleable`). `factView`, `keyState`, `report`,
  `sequenceOf` for the rest. No Onyx contract reads the book: level-3 money settlement waits for
  ROADMAP H6, and the money default (levels 1 and 3) is unchanged.
- **R-SIG-ON-7 Wiring: posts, clock, serving, censorship (ROADMAP C10, ADR-0028 amendment 7).**
  - *Deploy.* The book is deployed on its own (`scripts/deploy-signed-book.sh`,
    `DeploySignedBook.s.sol`) next to a running ledger. The reviewer is never the deployer, which
    is the settlement's coordinator: the script's dry run, the constructor (`ReviewerIsDeployer`)
    and the node's start refuse it (THREATS C10-1). The node's manifest pins `signedBook`,
    `signedBookCodeHash` (written only after the code at the address matched this checkout's
    build with `reviewer` masked) and `signedBookReviewer`; the node checks the chain id, the
    code hash and `reviewer()` at start.
  - *Posts.* A coordinator takes a report only through `onyx_coordinator::signed_intake::
    BookIntake::take`: it posts every report it takes to the book, whatever its own store says
    (a contradiction only it saw must reach the chain, L3-F7), logs the post (recorded with its
    outcome and block, refused with the book's error, or pending), and then runs its
    `SignedSources` pre-check with `now` = the latest block's timestamp, never earlier than the
    store's clock. It refuses to act when that block time is more than 2,100 s (sequencer drift
    plus skew) ahead of its host clock or more than 300 s behind (THREATS C10-4), and a payload
    over 4,096 bytes, which no valid report reaches (review C10-R2-1). A pending post
    is retried, and so is a refusal that may clear (`FromFuture`, `UnknownKey`, `KeySuspended`;
    review C10-R1-1).
  - *Serving.* `GET /v1/signed/{fact}` (onyx-api `signed_serve`, free) answers only when, at the
    node's finalized block (every read by that block's hash; no `finalized` tag means no answer):
    no report the operator's coordinator took for the fact is pending, every one it saw land is
    in the book at that block, no report of it the book refused as `FromFuture` states another
    value, `settleable(fact)` names this server's report and value, the block's time is at least
    `acceptedAt + DISPUTE_WINDOW_SECS + 1,800`, and as many seconds have passed on the host's
    clock since the operator took the report (block time runs fast after a sequencer stall, and a
    node can lie about it; review C10-R1-2) (THREATS C10-5).
    Otherwise a 409 `level3_not_servable` with a `reason` (`unknown`, `disputed`,
    `in_dispute_window`, `in_serving_margin`, `in_host_margin`, `other_report`, `post_pending`,
    `post_not_final`, `post_retrying`),
    a 503 `level3_chain_unavailable`, `level3_store_unreadable`, `level3_posts_delayed` or
    `level3_clock_skew`, or a
    404 `no_signed_report`.
  - *Censorship policy (L3C-2, L3C-12).* (a) The serving margin: 1,800 s of block time past the
    window. The arithmetic (review C10-R2): OP Stack keeps every block's time `t` within
    `o <= t <= o + 1,800` of its L1 origin's time `o`. Accepted at `t_a`, served at `t_s >= t_a +
    5,400`: `o_s >= t_s - 1,800 >= t_a + 3,600 >= o_a + 3,600`, so at least the 3,600 s window of
    L1-origin time passes, the worst case being acceptance with no drift and serving at full drift.
    A margin of 1,800 s is the least that gives this; the window alone gives only 1,800 s. The L1
    origin can itself lag the L1 head (by up to the sequencing window, during a stall), so origin
    time is not real time: the second margin, 5,400 s on the host's clock since the operator took
    the report, covers real time (review C10-R1-2). It is as good as the clocks: the API refuses
    (503 `level3_clock_skew`) when a time the coordinator wrote is more than 300 s ahead of its
    own clock (a clock behind would trip (c) late; review C10-R2-2); a clock running ahead cannot
    be seen and serves early by its error, still never before the block-time margin, so the node
    runs NTP (docs/ops). (b) A delayed post extends
    its fact's window: while a report the coordinator took for the fact has not landed and become
    final, the fact is not served, so a contradiction the sequencer holds back is never outrun by
    this operator. (c) A breaker: a post pending more than 600 s (host clock) stops every level-3
    answer, since a sequencer holding our posts may be holding others'. It is global on purpose
    and clears by itself when the post lands (`retry_pending`); one oversized report cannot trip
    it, since a coordinator refuses a payload over 4,096 bytes before posting (review C10-R2-1);
    the runbook deploy-signed-book.md "Level 3 stopped" has the recovery. (d) Not covered: a
    contradiction held back that this operator never saw, for longer than the margin; forced
    inclusion through L1 can take up to the sequencing window (hours). Level 3 settles no money
    here; before it does, the window must exceed forced inclusion plus the drift (ROADMAP H6).
- **R-SIG-CL-1 The client's check of a level-3 answer (ROADMAP C61, ADR-0028 amendment 8).**
  A `/v1/signed` body is the operator's word; a client takes it only after
  `onyx_web_verify::signed_view::check` (the code the website's verifier runs, also usable from
  Rust) passes, against reads it made itself. Its pin: chain id, book address and the keccak of
  the book's runtime code (the build with its reviewer filled in). (a) Body: exactly the keys the
  API writes, `level` 3, the pinned chain and book; `payload` a canonical Borsh `SignedPayload`
  that validates; the signature canonical EIP-191 over the payload's text, and the payload's
  `key` the `SourceKey::id` of (its source, the recovered signer); `fact`, `reportHash`,
  `valueHash` recomputed; source, key, metric, subject, value, `observedUnix` and `sequence`
  equal to the payload's. (b) Chain, through the client's own RPC: `eth_chainId` is the pin's;
  the `finalized` block, every other read at its hash (EIP-1898); that block at most 3,600 s old
  and at most 2,100 s ahead of the client's clock (a lagging or stuck RPC may miss a contradiction
  that landed since; Base Sepolia's finalized head was measured up to about 2,008 s behind; an
  RPC that lies about the timestamp is not caught, C63);
  `eth_getCode(book)` hashes to the pin (block reads ask `requireCanonical`); returns decoded
  exactly (`settleable` 96 bytes; `factView` its ABI layout, nothing after); `settleable(fact)` is (true, the recomputed report
  hash, the recomputed value hash); `factView(fact)` is accepted, on that report, at the body's
  `acceptedAt`. (c) Margin: the block's time and the client's own clock are both at least
  `acceptedAt + 5,400` (R-SIG-ON-7 (a) and the host margin of review C10-R1-2). Anything else
  refuses the body and shows nothing of it. Not covered: an RPC that lies about state at a
  fresh block (the reader chose it; two RPCs or a proof against an L1-anchored output root is
  ROADMAP C63). The website (no live book yet) checks one answer recorded in the local-anvil
  rehearsal against the chain answers recorded with it, both pinned in the verifier (the reads
  by `keccak256("onyx-signed-reads-v1" ‖ borsh(reads))`), and labels it a rehearsal; the
  reader's own RPC read (a public Base Sepolia RPC by default, no key, changeable) is refused on
  the chain id until a live book is pinned.
- **Vectors.** `fixtures/signed/vectors.json` (24 payloads over every subject and value variant,
  12 malformed ones) and `fixtures/signed/traces.json`, both written by
  `crates/onyx-coordinator/tests/level3_chain_book.rs` (`ONYX_BLESS=1`, only with an ADR change).

### Assumptions
- The reviewer confirms the origin is the publisher named in the source's reviewed rights. A
  lookalike domain registered by someone else is caught only by that review.
- The source keeps its key safe and reports a compromise; until it does, a stolen key signs
  anything within the 7-day window (THREATS L3-F3).
- Level 3 is in the money default, so a market settling on a signed value trusts the source's key.
- On chain (R-SIG-ON-4): the Base sequencer can delay or censor a post. A contradiction held back
  past the dispute window lets the first value become settleable meanwhile; it still disputes the
  fact when it lands (THREATS L3C-2). The API's margin, its hold on facts with pending posts and
  its breaker (R-SIG-ON-7) narrow this for one operator; they cannot see a contradiction that
  never reached it. Consumers read `settleable` at a finalized block, so a
  reorg cannot take back what they settled on.

## Feed templates (level 2, L2SRC-R2-F1)
- Each level-2 metric has exactly one registered `FeedTemplate` (crates/onyx-core/src/feed.rs):
  fetcher id, `https://` URL template, selector, kind, window length, observer count, rights and a
  subject rule. `Fixed(subject)`: no `{date}` in the URL, one fact per window end. `Dated { prefix,
  lag }`: subject `Aggregate(prefix ":" YYYY-MM-DD)`, the URL holds `{date}` once, one fact per date
  whatever the window (`fact_key_dated`, L2SRC-R2-F2). `Period { prefix, min_days, max_days }`
  (SEC-1): subject `Aggregate(prefix ":" YYYY-MM-DD)` naming a reporting period's end, which may be
  any calendar day (weekends included); the URL holds no `{date}` and the selector holds it once
  (one document lists every period); the window's UTC day must be `min_days..=max_days` calendar
  days after the period end; one fact per date like `Dated`. Its price clock
  (`decay_start`) is `(period end + min_days) * 86400 + window_secs`, a function of the template
  and the subject only.
- Registration is a reviewer action: `ObservationResults::new()` returns the store and its one
  `FeedAdmin` capability (not `Clone`, no public constructor); `register_feed` needs it, records
  (time, metric, template id, reason) in an append-only log, and is final (a metric's template is
  never replaced; a new feed is a new metric).
- Rounds open only through `ObservationResults::open_round` (`ObservationRound::open` is
  crate-private), which requires the spec to equal the metric's template instantiated for the
  spec's own subject and window start, field for field (`FeedTemplate::matches`); `settle` and
  `accept_claim` recheck it. A spec naming a registered metric but another fetcher, host, field,
  selector, window, observer count or rights never opens, settles or is accepted. A metric without
  a template has no level-2 rounds at all (`UnknownFeed`).
- Date lag (`DateLag`): the record date must be a business day on the source's calendar (weekends
  plus a holiday table the adapter supplies), at least `min_business_days` business days must lie
  strictly between it and the window's UTC day (published before the window opens), it is at most
  `max_calendar_days` before it, and every day involved must be inside the table's coverage (fail
  closed outside it).
- Trust: one store is one coordinator's book. A coordinator that registers its own template for a
  metric in its own store can settle what it likes there; another store refuses those results. The
  on-chain port must register templates and enforce one result per fact on chain (TODO, THREATS
  L2R3-F5, L2SRC-R2-F1 residual).

## Real sources (level 2)
- `us.treasury.fiscaldata` (adapters/source-fiscaldata, loops 62-63): the Debt to the Penny record
  for one date. Fetcher `onyx-fetcher:fiscaldata-record:v1` reads only the canonical feed for the
  date its URL pins (same host, fields, selector, metric, dated subject, kind, rights, observer count
  and window length; anything else is `Unavailable`, L2SRC-R1-F2), and exactly one record whose date
  equals that date; tolerance 0 (a published record does not move); the amount must be dollars and
  cents with exactly two decimals (L2SRC-R1-F6).
  - Its `FeedTemplate` (`onyx_source_fiscaldata::template`) is the metric's one feed: dated
    subject `us.treasury.federal_debt:YYYY-MM-DD`, so each record date is one fact whatever the
    window, and an old record can never be stated as a new date (L2SRC-R1-F1, R2-F1, R2-F2).
  - The record date must be a U.S. federal business day (5 U.S.C. 6103 holidays, observed days,
    2026-2035; refused outside), with at least one business day between it and the window's UTC
    day (the Treasury publishes on the next business day) and at most 10 calendar days before it
    (L2SRC-R2-F4). A Friday record before a Monday holiday is out on Tuesday and observable from
    Wednesday. A date not yet published makes every honest observer answer `Unavailable` (the
    round settles `SourceUnavailable`, which pays nobody).
  - Recorded rehearsal: five observers drawn from a simulated pool of nine with a simulated
    beacon, value 40305316210829.72 USD for 2026-10-08, spec id
    `0x0cef170f5595649bfc63fa3a01be5f23d55012d31098f03cff92a76b4850ce24`, result
    `0xf65720c7a4c2358f3c8d8047066846994e117bc0daa307e29009ca2da3f2cec9`. Observers are simulated on
    one machine; each page is a real HTTPS response. The browser recomputes the draw from the pool
    and the beacon, then the round id and the
    result hash from the feed, the config, the observers' bonds, the draw and the settled value, and requires
    the pinned spec id, response hash and result (L2SRC-R1-F3). The card shows only the check's
    output: value, date and window from the rebuilt feed, counts from the checked reveals, and the
    source name, terms link and quote and attribution from the adapter's constants; a summary whose
    own copies differ fails (L2SRC-R2-F3).

- `sec.edgar` (adapters/source-edgar, indexer Phase 2 item 4): the crypto-asset units a company
  tags as `us-gaap:CryptoAssetNumberOfUnits` in its SEC filings, read from EDGAR's
  `companyconcept` API (public information, reuse without permission: <https://www.sec.gov/privacy>).
  One metric per company (`sec.edgar.crypto_asset_units.<key>`, Strategy Inc and Riot Platforms),
  period subject `sec.cik<CIK>.crypto_asset_units:YYYY-MM-DD`, observable 110..=730 days after the
  period end (every 10-Q and 10-K deadline, including a Rule 12b-25 extension, has passed by
  110 days). The value is the company-wide crypto-asset unit total the filing tags (EDGAR's
  `companyconcept` serves only facts without dimensions), not a per-asset bitcoin figure; it is
  registered only for companies whose tagged totals match their own public bitcoin totals
  (`fixtures/PROVENANCE.md`). Only a value consumes a period: a non-value round leaves it open
  (SEC-R1-F1). Fetcher
  `onyx-fetcher:edgar-companyconcept:v1` reads only the company's canonical feed, requires the
  document's `cik`, taxonomy and tag, at least one row of the template's unit for the period, all
  such rows equal, and no other unit tagging that period; values must be JSON integers (no
  fraction, exponent or sign). Claims are `Value::Int`. A fact states what the filing says, not
  that the coins exist (SEC-2). Recorded rehearsal 2026-10-11 (period 2026-03-31, three observers
  each, drawn from a simulated pool of nine): Strategy Inc 762099 (unit `Bitcoin`), Riot Platforms 15679 (unit `item`). The browser
  rechecks both rounds against pinned spec ids, response hashes and result hashes
  (`check_edgar_rounds`).

## On chain (ADR-0030)
- `OnyxRegistry.metrics[keccak256(utf8(metric))] = (kind, template)`: kind `Recipe` (level 1,
  template = recipe id), `Feed` (level 2, fixed subject, one fact per window end) or `DatedFeed`
  (level 2, dated subject, one fact per date), template = `FeedTemplate::id`. The level-2 kind is
  `MetricKind::of(template)` on every registration path; a consumer refuses a registered kind or
  template id that differs from its own copy (REG-R2-F2). Owner only, once per metric, never
  changed (R-REG-ON-1).
- Level-2 facts: the coordinator records value outcomes only (attested, not enforced: the
  contract sees a result hash, not an outcome; SEC-R1-F1, SEC-R2-F3). A non-value round leaves a
  dated or period fact open off chain, at most `MAX_NON_VALUE_ROUNDS` times (SEC-R2-F1).
- Before a shard opens, the coordinator attests (preimage, runtime bytes, metric, fuel used,
  level). Enforced on chain: the shard id is recomputed from the preimage; its recipe is the
  metric's registered recipe; the runtime bytes hash to the preimage's runtime hash and the fuel
  limit is read from them (REG-R2-F1); a labelled shard's label set is registered and the level is
  no better than the label set's; level 1..=5; `fuel used * 1e6 <= fuel limit * 500_000`
  (`within_preflight_budget`); once, only while the shard is `None`. Attested only: the fuel used
  and the level above its floor; an unlabelled shard has no floor (R-REG-ON-2).
- `verifyRowLeveled` returns the row check, the attested level and the metric; `(false, 0, 0)` for
  a shard that is not final or not attested. Consumers that need a level verify through it
  (R-REG-ON-3).
- A level-2 fact key is computed on chain from the metric's registered template, the subject key
  (`keccak256(borsh(subject))`) and the window end (0 for a dated feed); each fact is set once. The
  spec id and result hash are recorded, not derived: a consumer builds both itself
  (`onchain_observation_check`: registration, instantiation, the observer draw, round id, reveals
  against commits, re-aggregation) and checks `observationLevel(metric, subject, window, spec, result) == 2`; reading
  them back from the chain proves nothing (R-REG-ON-4, REG-R2-F3). The pool snapshot and the
  beacon are bound through the result hash (round id v2), not stored separately; that the beacon is
  the block hash at its height is not checked on chain (TODO, THREATS L2-F2).
- The registry owner's key is never the settlement's coordinator's key (R-REG-ON-5); an owner and a
  coordinator who collude can still give any level (REG-R2-F5).
- Vectors: `fixtures/registry/vectors.json` (metric keys, the fuel rule, shard ids from their
  preimage, fact keys built with the Treasury template id), every array checked to its length.

## Level 5: track record (ADR-0036)
A level-5 value cannot be checked today. Its source's track record shows how the source's earlier
values turned out once a stronger answer for the same fact existed. The record is information
for a reader; it never changes any value's level (R-TR-9).

### Interfaces (`crates/onyx-track`, depends on `onyx-core`, `onyx-merkle` and, for EIP-191 recovery in the intake, `onyx-labels`)
- `Fact { metric, subject, as_of }`, `key() = keccak256("onyx-track-fact-v1" ‖ borsh(fact))`. No
  source in the key.
- `Statement { source, fact, value, confidence, committed_at }`, `hash()` with domain
  `onyx-track-statement-v1`: what a registered source stated, with its confidence (ppm) that the
  value will agree, and when Onyx first committed it.
- `Resolution { fact, value, level, resolver, evidence, public_from }`, `hash()` with domain
  `onyx-track-resolution-v1`: a later answer at an accepted level, the registered source it comes
  from, the hash of that answer (claim, level-2 result or level-3 report) and the earliest time it
  was public.
- `MetricRule { metric, close: Close, tolerance }` (`MetricRule::after`, `period_start`,
  `close_at(as_of)`); `Close { AfterAsOf(secs), PeriodStart { period_secs, lead_secs } }` (C9,
  ADR-0036 amendment 4; `MAX_PERIOD_SECS` = 366 days); `TrackParams { resolver_levels, rules }`,
  `TrackParams::new(rules)` (resolver levels 1 and 3), `validate(reg)`, `hash()` (domain
  `onyx-track-params-v1`).
- `compute(params, reg, statements, resolutions, as_of) -> TrackBook` with `entries()` (one
  `Entry` per (source, fact)), `records()` (one `Record` per (source, metric)), `metrics()` (one
  `MetricSummary { metric, sources, resolved_facts }` per metric), `record(source, metric)`,
  `metric(metric)`, `record_proof(i)`, `entry_proof(i)`, `metric_proof(i)`,
  `commitment(prev) -> TrackCommitment`.
  `sort_statements`, `sort_resolutions` put logs into canonical order.
- `Outcome { Agreed, Disagreed, Equivocated, Late, Void, Contested, Unresolved, SelfResolved }`; `Record`
  counts each, plus `agreed_first`, `brier_sum_e12`, `distinct_subjects`, first and last commit
  times, `metric_sources`, `metric_resolved_facts`, `baseline_entries`, `baseline_source_agreed`,
  `baseline_agreed` (C9); `resolved()`, `agreement_ppm()`, `mean_brier_e12()`, `cmp_brier(other)`,
  `skill_ppm()`, `established()`. `Entry.baseline: Baseline { None, Agreed, Disagreed }` (C9,
  R-TR-11). `MetricRule::reveal_at`, `TrackParams::reveal_of` (C9).
- Client: `verify_record(c, (record, proof))`, `verify_summary(c, (summary, proof))`,
  `verify_history(c, (record, proof), (summary, proof), entries, before, after)`,
  `check_extends(prev, TrackLog { params, statements, resolutions }, next, next_log)`.
- Served form (ROADMAP C6, feature `wire`): `wire::record_body(Served { book, commitment,
  params, chain }, source, metric)` and the client's `wire::check_record_body(body, Expect {
  source, metric, min_as_of, known, latest })` (docs/spec/delivery.md "Track records").
- On chain (ROADMAP C7, ADR-0036 amendment 3): `TrackParams { .., chain, operators }`,
  `OperatorOf { source, operator }`, `with_chain`, `with_operators`, `operator_of`,
  `same_operator`; `chain::{FactChain, ChainView, Settled, Recorded, Registered, Preimages,
  Level2Preimage, ObservedOutcome, derive_resolutions, check_operators, verify_resolutions,
  CommitmentLog, LogHead, commitment_bytes, check_latest, check_in_log, book_fact_key}`; see "On
  chain" below.
- Example (ROADMAP C6): `example::{registry, params, logs, book, chain}`, invented sources and
  values on `example.signed.reserve`; `example::is_example_id` marks every id it uses.

### Rules
- **R-TR-1 Inputs.** Params: resolver levels non-empty, strictly ascending, each level 1, 2 or 3
  (default 1 and 3); rules strictly ascending by metric, every metric registered, tolerance at
  most `MAX_TOLERANCE` and 0 for `Bool` and `Bytes`. Statements: source registered with
  ingestible rights, a rule for the metric (else `NoRule`: no record without a reviewed rule),
  subject valid in the registry, value well formed and of the metric's kind, confidence
  500,000..=1,000,000 ppm (`MIN_CONFIDENCE_PPM`: a statement claims its value is more likely right
  than not, TR-R1-3), `committed_at <= as_of`, the close (`MetricRule::close_at`) without
  overflow (`Overflow`) and, for a `PeriodStart` rule, `as_of` a multiple of the period with the
  period and lead after time 0 (`BadPeriod`); a `PeriodStart` rule has a period of 1 s to 366 days
  and a lead of at most 366 days (`BadParams("period")`).
  Resolutions: level accepted, evidence non-zero, resolver registered with ingestible rights,
  `public_from <= as_of` (an answer is committed only once public, TR-R2-2), same fact and value
  checks. Statements strictly
  ascending by (source, metric, fact key, statement hash), resolutions by (fact key, resolution
  hash); at most 2^24 of each.
- **R-TR-2 Fact state.** For one source's entry the answers of the source and of sources with its
  declared operator are left out first (R-TR-3); of the rest, only those at the strongest level
  count (a weaker answer never contests a stronger one, TR-R1-7), and of those only the ones public
  within `CONTEST_WINDOW_SECS` (3,600 s) of the earliest; the entry is judged once that window has
  passed at `as_of` (amendment 3 item 6). So neither its own nor an affiliate's answer contests,
  outranks or shields the ones that judge it (TR-R2-1, TR-R2-4, TR-C7-4, TR-C7-R1-5), and a fast
  answer never judges alone (TR-C7-R1-6). For the metric summary the same with nothing left out:
  agreeing, resolved; different values, contested.
- **R-TR-3 One entry per (source, fact).** Its earliest statement (ties: lowest hash) is the one
  judged. Close = `as_of + secs` for `AfterAsOf(secs)`, `as_of - period_secs - lead_secs` for
  `PeriodStart` (the fact is the total over `[as_of - period_secs, as_of)`; C9). The usable answers are R-TR-2's for the entry:
  resolver neither the source nor sharing its declared operator (`TrackParams::same_operator`),
  strongest level among those, within the contest window of the earliest (the first in order is
  named; the window still open at `as_of`: `Unresolved`). In order: earliest commit after the close: `Late`; no resolution: `Unresolved`; no
  usable answer: `SelfResolved`; usable answers with different values: `Contested`; earliest usable `public_from <= close`: `Void`; any statement of the
  group with another value or confidence: `Equivocated` (only now, so an unresolved hedge counts
  nothing, TR-R1-10); else `Agreed` when the value is within the tolerance (numbers: absolute
  difference in the value's units, same scale; others: equal), otherwise `Disagreed`.
- **R-TR-4 Score.** Counted (resolved) = agreed, disagreed, equivocated. Brier term in 1e-12:
  agreed `(10^6 - p)^2`, disagreed `p^2`, equivocated `10^12`; others 0. Compared by
  cross-multiplied sums, never by a rounded mean, and only after the agreement count (TR-R1-3).
  Helpers never overflow on an unverified record (`None` or saturating, TR-R1-9).
- **R-TR-5 First.** An agreed entry is `first` when no agreed entry of another source on the same
  fact has an earlier commit time and was judged no later (its usable answers' time at or before
  this entry's; amendment 3 item 6). Ties are all first.
- **R-TR-6 Records.** Entries are sorted by (source, metric, fact key); each record covers a
  contiguous range (`entries_from`, `entries_count`). `distinct_subjects` counts subjects of
  counted entries. Per metric, `MetricSummary.sources` = records on the metric and
  `resolved_facts` = resolved (not contested) facts of the metric; the three baseline counts as
  R-TR-11; each record copies its metric's
  two numbers into `metric_sources` and `metric_resolved_facts`. Established: at least 30 resolved over at least 5 distinct
  subjects (ADR-0016 item 6's numbers; PLAN q-TR-1).
- **R-TR-7 Commitment.** Entry and record rows are Borsh, in `onyx-merkle` trees.
  `TrackCommitment { prev, as_of, registry_root, params_hash, statements_hash, resolutions_hash,
  entries_root, entry_count, records_root, record_count, metrics_root, metric_count }`, hashed with domain
  `onyx-track-commitment-v1`; `statements_hash` and `resolutions_hash` are Keccak of the domain
  and the Borsh list.
- **R-TR-8 Append-only.** `check_extends`: `next.prev` is `prev`'s hash, `next.as_of > prev.as_of`,
  both logs (params, statements, resolutions) match their hashes, the later params keep the
  resolver levels and every earlier rule (TR-R1-2), every earlier statement and resolution is in
  the later log unchanged, every statement new to it has `max(committed_at, reveal) > prev.as_of`
  (reveal = `reveal_of(fact)`: the close, or a period fact's `as_of`, C9)
  (it was sealed until its close or not made yet; ADR-0036 amendment 1 item 4: before C5 the rule
  was `committed_at > prev.as_of`, and the intake's anchor check now bounds `committed_at`; only
  when `params.intake` is set: without an intake the rule stays `committed_at > prev.as_of`,
  review TR-R4-1), every
  resolution new to it has `public_from > prev.as_of` (unless its fact is neither resolved nor
  named by a statement in the earlier log: no committed entry exists on it) and, for a fact the
  earlier log resolved, a level no stronger than its weakest committed answer (TR-R1-1: with
  R-TR-2's window a later answer never changes an entry already judged, it judges only one left
  self-resolved or pending, amendment 3 item 6), the later
  params keep `intake`, `chain` and every declared operator, and no operator is declared for a
  source the earlier logs hold (`OperatorRegrouped`, TR-R2-4). The registry root is not compared: the registry
  only grows and no outcome reads it.
- **R-TR-9 Never a level, never money.** `onyx-track` returns no `ProofLevel` and no
  `MarketPolicy`; a level-5 value stays level 5 whatever the record, and the money default stays
  levels 1 and 3 (L2R-F8, L16-R4). No record picks a value among sources, and none drives rewards
  or slashing before an ADR binds sources to stake. `scripts/check-boundaries.sh` refuses a file
  that uses `onyx_track` together with level or market-policy code, any manifest depending on
  `onyx-track` that is not listed in its `TRACK_DEPENDENTS`, any manifest that renames it
  (`package = "onyx-track"`), and any package that `cargo metadata` shows depending on it that is
  not in `TRACK_DEPENDENT_PACKAGES` (TR-R2-3).
- **R-TR-11 Skill against persistence (C9, q-TR-5; reviews TR-C9-R1-1, R1-2, R2-1).** Only on a metric
  with a `PeriodStart` rule (point facts have no grid, so "the last value" is undefined): a counted
  entry of `source` on fact `(metric, subject, as_of)` closing at `c`, judged by value `v`. Its
  candidate is the one fact of the same metric and subject with `as_of' = c - (c mod period)` (the
  period that ended last by the close), if a statement committed at or before its own close names
  it (on time: a `Late` statement never makes a baseline). No older period stands in for it, so a
  source cannot choose a staler baseline by leaving the latest period unnamed (review TR-C9-R2-1).
  If the answers with `public_from <= c`, resolver neither `source` nor sharing its operator, judge
  it (R-TR-2 with `as_of = c`: strongest level, contest window passed by `c`) with one value `w`,
  that is the baseline: `Agreed` if `agrees(w, v, tolerance)`, else `Disagreed`; `None` when the
  candidate does not qualify or the entry is not counted. One fact is looked at. A record counts `baseline_entries` (baseline not `None`), `baseline_agreed`
  (`Agreed`) and `baseline_source_agreed` (of those entries, outcome `Agreed`). `skill_ppm` =
  floor((baseline_source_agreed - baseline_agreed) x 10^6 / baseline_entries), `None` with no
  baseline entry or counts above it. Display only (R-TR-9). Frozen once the entry is judged: the
  candidate's reveal is at or before `c` (its `as_of`), so R-TR-8 admits no later statement on
  it, and no later answer public by `c` on a fact the earlier log named.
- **R-TR-10 Client check.** One record: `verify_record` (proof and counts in bounds). Its
  history: `verify_history` needs the metric's committed summary (equal to the record's two
  context numbers), every entry of the range in order with proofs and strictly ascending fact keys,
  and the entries just outside it (or the tree's ends) from another (source, metric); the recount
  must equal the record. Limit: this proves the record adds up over committed entries; that each
  outcome and the summary are right needs a rerun of `compute` on the published logs.

### Intake (ADR-0036 amendment 1, `onyx_track::intake`; chain: `onyx_coordinator::track_chain`)
Interfaces:
- `IntakeId { chain_id, sink, poster, first_nonce }`, `id()` (domain `onyx-track-intake-v1`).
- `StatementPayload { intake, key, source, fact, value, confidence }`, `digest()` (domain
  `onyx-track-signed-statement-v1`), `message()` = `onyx-track-signed-statement-v1:0x<digest>`
  (EIP-191 personal_sign text). `SignedStatement { payload, signature }`, `statement(committed_at)`.
- `TrackKey { source, signer, revoked }`, `id()` = `SourceKey::id` of (source, signer);
  `KeyBook { keys }` strictly ascending by id, `validate(reg, reviewed)` (every key a reviewed
  level-3 `SourceKey`), `hash()` (domain `onyx-track-keys-v1`), `extends(earlier)`.
- `TrackParams.intake`: the intake id every statement comes through (zero: no intake), pinned for
  a commitment chain (`extends` requires it equal; review TR-R3-3).
- `BatchHeader { intake, seq, prev, keys, statements_root, statement_count, reveal_by }`, `hash()`
  (domain `onyx-track-batch-v1`). `Anchor { block, block_hash, timestamp }`, `Post { seq,
  header_hash, anchor }`.
- `trait FinalizedRecord { head() -> Anchor; posts(head) -> Vec<Post> }`: every post at or below
  the finalized head, in order, or an error when the record shows more posts than it can return.
- `Intake::new(id, book, reg, reviewed)`, `set_keys(book, reg, reviewed)`, `submit(params, reg,
  signed, final_time)`, `seal() -> Option<BatchHeader>` (one close per call), `anchor(post)`,
  `sync(record)`, `log_at(head)` (every key active), `log_with(head, lives)` (C9),
  `revealed_at(head) -> Vec<RevealedBatch>`, `books()`, `receipt(digest, deadline)`,
  `inclusion(digest) -> Option<(seq, Proof)>` (C9).
- `KeyLife { Active, Suspended, Retired, Compromised, Unknown }`, `KeyLives { block, keys: Vec<(key
  id, KeyLife)> }`, `KeyLives::all_active(book, block)` (C9, TR-R4-4).
- `Receipt { intake, digest, deadline }`, `digest()` (domain `onyx-track-intake-receipt-v1`),
  `message()` = `onyx-track-intake-receipt-v1:0x<digest>`; `SignedReceipt { receipt, signature }`;
  `ReceiptStatus { Kept { seq, committed_at }, PostedLate { seq, committed_at }, Broken, Pending }`
  (C9, TR-I-6).
- Client: `verify_log(IntakeView { intake, head, posts, batches, books, reviewed, lives }, params,
  reg, as_of, statements)`; `check_receipt(id, view, params, reg, receipt, statement, inclusion)`.

Rules:
- **R-TI-1 Key.** A statement is admitted only with a valid EIP-191 signature (canonical: `v` 27 or
  28, low `s`) over its payload's message by the signer of `payload.key`, which must be an
  unrevoked key of `payload.source` in the key book in force (the batch's). No other key adds a
  statement under a source (q-TR-2). Every key of every book is a reviewed level-3 `SourceKey` of
  the same source and signer (with its provenance record), so the intake cannot add a key of its
  own (review TR-R3-2).
- **R-TI-2 Intake id and replay.** `payload.intake` and `params.intake` are the intake's id. A payload digest is taken
  at most once over the intake's life (pending or sealed), whatever the signature; a second one is
  `Replay`. The client refuses a log in which a digest appears twice.
- **R-TI-3 Admission.** The same checks as `compute` (R-TR-1: source, rule, subject, value kind,
  confidence 500,000..=1,000,000 ppm). The intake also refuses a statement whose close is at or
  before the latest finalized time it knows (`PastClose`), at most `MAX_PENDING_PER_SOURCE`
  (4,096) pending per source and `MAX_BATCH` (65,536) pending in all (`Flood`).
- **R-TI-4 Batch.** `seal` puts the pending statements with the earliest reveal time
  (`TrackParams::reveal_of`: the close, or for a `PeriodStart` rule the fact's `as_of`, C9) (Borsh
  rows sorted ascending) into an `onyx-merkle` tree; the header names the intake, `seq` (0, 1,
  ...), `prev` (the previous header's hash, zero for the first), the key book's hash, the root and
  count, and `reveal_by` = that reveal time, shared by every statement of the batch (one per batch,
  so no statement can stay sealed past its own reveal behind a far one; review TR-R3-1). Only
  `header.hash()` is posted.
- **R-TI-5 Time.** A batch's statements have `committed_at` = the `timestamp` of the block that
  carries its post, read at the finalized head. A track commitment fed by the intake has `as_of` =
  the head's timestamp.
- **R-TI-6 Reveal.** A statement's reveal is `max(committed_at, reveal_of(fact))`: its close, or
  for a period fact the period's end (`as_of`), so no forecast is public while the period's wall
  clock is open (review TR-C9-R1-5). For the Bitcoin flow rule the day's last blocks (median time
  past still before `as_of`) are mined after `as_of`, about an hour with honest miners and without
  bound if miners hold timestamps low, so a forecast is public while those blocks can still move
  the total (review TR-C9-R2-2; accepted: an exact `Bytes` total is moved off every forecast by
  any transfer anyway; TODO C40: reveal once the day's last block is final). `log_at(head)` holds exactly the statements of
  posts at or below the head with reveal at or before the head's timestamp, in canonical order. A
  batch with `max(post time, reveal_by) <= as_of` is fully revealed. A revealed statement's reveal
  time equals its header's `reveal_by`.
- **R-TI-7 Keys forward.** A new book keeps every key of the one before with the same source and
  signer, never un-revokes one, and never gives a signer to a second key. `set_keys` takes effect
  at the next `seal` and drops pending statements whose key it revokes. Sealed statements stand.
- **R-TI-8 Client check.** `verify_log` refuses unless: posts are `0..n` at or below the head, in
  block order; batch `i`'s header hashes to post `i` and has the intake, `seq = i` and the
  previous header's hash; each header's book is given, valid and extends the one before; every
  revealed statement is proven at a distinct index under its root, R-TI-1..3 hold for it in its
  batch's book, and its reveal is at or before `as_of`; R-TI-6's completeness holds; no digest
  twice; `as_of` equals the head's timestamp; `params.intake` is the view's intake; and the log
  equals the revealed statements with their post times whose keys R-TI-9 lets count.
- **R-TI-9 Key status (C9, TR-R4-4).** `IntakeView.lives` holds one `KeyLives` per post: for each
  key of the batch's book, in id order and nothing else, its `OnyxSignedBook.keyState` at the end
  of the post's block (`block` = the post's); life 1 and not suspended is `Active`. A revealed
  statement counts in the log only if its key is `Active` there; it stays proven and revealed
  (completeness unchanged). With `params.chain` set `lives` is required (`BadKeys`); without one,
  `None` leaves the book's `revoked` flags as the only status (trusted). With `lives` given, a book
  that marks revoked a key `Active` at the post is refused (`BadKeys`, review TR-C9-R1-8): the
  coordinator's flag can neither keep nor censor a key. The status is the book's at the post's
  block, never back-dated: a later `markCompromised(since)` with an earlier `since` does not remove
  statements already posted (amendment 1 item 5; TR-C9-R1-4).
- **R-TI-10 Receipt (C9, TR-I-6).** `Intake::receipt` names a pending statement with a deadline at
  or before its close; the poster (`IntakeId.poster`) signs `message()` with EIP-191.
  `check_receipt`: the receipt's intake and the view's are the id's, its digest is the statement's
  payload digest, the signature recovers to the poster, the deadline is at or before the
  statement's close (else `WrongReceipt`, `BadSignature`, `BadDeadline`). The statement is shown
  in a post's batch (revealed with a valid proof, or `inclusion`'s proof under that post's root; a
  wrong inclusion is `BadInclusion`): `Kept` if that post's block time is at or before the
  deadline, else `PostedLate`. Not shown: `Broken` when the head's time is after the deadline and
  at or after the close, else `Pending`. No commit time or outcome reads a receipt.

Chain record today (`CastRecord`, local anvil and later Base Sepolia): the header hash is the
32-byte calldata of a transaction from the dedicated poster to the sink address; a post counts once
its block is at or below the `finalized` block (a pinned depth only on a loopback RPC), and the
node's chain id must be the intake's (review TR-R3-4); `posts` checks each transaction's sender, recipient, nonce (`first_nonce + seq`) and data, and
that the poster's nonce at the head equals `first_nonce + n` (`HiddenPost` otherwise). On C2 the
signed-source book can store the headers and count them itself, behind the same trait.

### On chain (ADR-0036 amendment 3, ROADMAP C7; `onyx_track::chain`, `onyx_coordinator::track_chain`, `OnyxSignedBook`)
Interfaces:
- `FactChain { chain_id, book, registry }`, `id()` (domain `onyx-track-chain-v1`) =
  `TrackParams.chain` (zero: resolutions are caller input). `registry` zero: no level-1 or
  level-2 fact exists on that chain.
- `trait ChainView { chain(); head() -> Anchor; has_registry(); book_sources(); operator_of(source);
  settled(book_fact) -> Option<Settled { report: Option<SignedReport>, report_hash, value_hash,
  accepted_at }>; registered(metric) -> (Registered, template); recorded(metric, subject,
  window_end) -> Option<Recorded { template, window_end, spec, result, recorded_time }> }`, every
  read at `head()`. `CastFacts::pin(cast, rpc, work, chain, finality)` implements it.
- `Preimages { reports, observations: Vec<Level2Preimage { template, spec, round, outcome }> }`;
  `Level2Preimage::result_hash()` = `onyx_coordinator::observe::result_hash_of`;
  `book_fact_key` = `onyx_coordinator::signed::signed_fact_key` (both tested equal).
- `derive_resolutions(params, reg, view, statements, prev: Option<(as_of, resolutions)>,
  preimages)`, `check_operators(params, view, statements, resolutions)`,
  `verify_resolutions(reg, view, as_of, log, prev: Option<(as_of, log)>, preimages)`.
- `CommitmentLog { chain_id, book, poster, track }`, `LogHead { count, latest, as_of }`,
  `commitment_bytes(c)` (288 bytes), `check_latest(c, head)`, `check_in_log(known, at)`;
  `CommitmentLogCast::{post(head, c), head(block), at(block, index)}`.
- `OnyxSignedBook`: `postTrackCommitment(bytes32 track, uint64 head, bytes commitment) returns
  (uint64 index)`, `trackHead(address poster, bytes32 track) returns (uint64 count, bytes32
  latest, uint64 asOf)`, `trackCommitmentAt(poster, track, index)`, event `TrackCommitted(poster,
  track, index, commitmentHash, asOf, head, commitment)`; `declareOperator(string source, bytes32
  operator, string reason)` (reviewer), `operatorOf(bytes32 sourceHash)`, event
  `OperatorDeclared`.

Rules:
- **R-TC-1 Chain mode.** With `params.chain` set, a commitment's resolutions are exactly
  `derive_resolutions` at its head; `params.chain` equals the view's chain and never changes along
  the commitment chain; `as_of` is the head block's timestamp.
- **R-TC-2 Derivation.** For each distinct fact the statement log names: level 3 (if accepted),
  for each source with a key in the book, the book fact `(source, metric, subject, as_of)`
  settleable at the head, its report (calldata or preimage) hashing to the settled report and
  value hashes and naming that source and fact; `public_from = acceptedAt + 3600`. Level 2 (if
  accepted) on a metric registered `Feed`/`DatedFeed`: the recorded observation whose window end
  is `as_of`, opened by a preimage whose template is the registered one (`id`, kind), whose spec
  `matches` it, is of this fact and hashes to the recorded spec id, and whose result hash is the
  recorded one; a `Value` outcome resolves, others resolve nothing; `public_from` = the recording
  block's time. Params accepting level 1: `Level1Unsupported` (whatever the metrics; TR-C7-R2-1).
  Level 2 accepted with no registry on the chain: `NoRegistry`. An on-chain answer without a
  preimage: `Withheld`. A derived resolution `compute` would refuse: skipped. The previous
  commitment's resolutions are kept; for a fact it resolved, only new answers no stronger than its
  weakest committed one; for a fact it resolved or its statements named, a new answer with
  `public_from <=` its `as_of` is skipped (an omission there is found by R-TC-4's walk); a fact
  first named now takes any answer. Canonical order.
- **R-TC-3 Operators.** `params.operators` strictly ascending by source, operator non-zero. In
  chain mode every stating source has a key in the book (`NotInBook`) and, for every source
  stating, resolving or listed, the params' operator equals the book's `operatorOf` when the
  source has a book key, and is none when it has not (zero: none; `OperatorMismatch`). The book takes a declaration once per source, from the reviewer, before
  the source's first key.
- **R-TC-4 Client check.** `verify_resolutions` = `as_of` is the view's head time
  (`NotTheHead`), R-TC-3, and R-TC-2's list equals the log's (`Mismatch`); it takes the previous
  log on trust. The full check is `verify_chain` over every log entry from the first, each with
  its own registry and a view at its own head (`CastFacts::at_commitment`: its event's `head`,
  final, timed at `as_of`): the entry at its index (`NotTheLogs`), chained, `verify_resolutions`,
  `check_extends` (`NotExtended`), equal to a rerun of `compute` (`Mismatch`), the last the log's
  latest. With (an intake's) `verify_log` a record then rests on chain data only.
- **R-TC-5 Commitment log.** The book appends `keccak256("onyx-track-commitment-v1" ‖ bytes)` to
  `(msg.sender, track)` only if the bytes are 288 long, `track` non-zero, `head < block.number`,
  `prev` (bytes 0..32) is the log's latest (zero when empty), and `as_of` (u64 LE at 32..40) is
  above the latest's and at most `block.timestamp`. Entries never change. A client pins
  `(chain_id, book, poster, track)`, reads `trackHead` at a finalized block and accepts only its
  latest (`NotLatest`), and finds a known commitment at its index (`NotInLog`).
- **R-TC-6 Reads.** A view reads one finalized head (a pinned depth only on a loopback RPC; the
  configured chain id only); calls name the head by hash; after each log scan the head's hash is
  re-read and a replaced head is refused.

### Close-time rules (reviewed per metric, TR-13; ADR-0036 amendment 1 item 9)
A statement must be committed before its truth can be known anywhere, not only before the
resolution Onyx uses is public (`Void` covers only the latter). Reviewed 2026-10-17:

| Metric | Level today | Earliest the truth is public anywhere | Rule |
| --- | --- | --- | --- |
| `crypto.btc.entity_flow`, `crypto.btc.entity_flow_series` | 1 | Block by block as the day's blocks are mined; a block whose median time past is in the day can be accepted from 2 h before the day starts (`MAX_FUTURE_BLOCK_TIME`) | `PeriodStart { period_secs: 86,400, lead_secs: 7,200 }` (C9, `onyx_source_btc::TRACK_FLOW_*`): the fact is the UTC day ending at `as_of` (a midnight), its blocks those whose median time past (BIP 113) is in the day; statements close 2 h before the day starts and stay sealed until the day ends (its UTC end; the day's last blocks come later, TR-C9-R2-2). The 2 h holds for Bitcoin Core 27 and later (system clock); a pre-27 node whose peers shift its clock (at most 70 min) could accept a day's block up to 70 min before the close, which needs miners stamping six blocks in a row far ahead (TR-C9-R2-3, accepted). Mempool transactions are public earlier (partial inputs, accepted); anyone transacting with the subject moves the truth (TR-C9-2, C92). The value is `Bytes`, exact match; the day's value and block range are pinned by the level-1 reader (C40). No record yet: no source states, and level 1 has no chain reader (C40) |
| `crypto.btc.entity_reserve`, `crypto.btc.pool_blocks` | 4 | Not applicable | None: level 4 never resolves (R-TR-1) |
| `us.treasury.total_public_debt_outstanding` | 2 | Debt to the Penny publishes a record date on the next business day | None while level 2 does not resolve (q-TR-4); then `close_after_secs` 0 with `as_of` the end of the record date (before any publication) |
| EDGAR company holdings (`source-edgar` metrics) | 2 | The company's own earnings release (Form 8-K Item 2.02) or filing, after the period end | None while level 2 does not resolve (q-TR-4); then `close_after_secs` 0 at the period end |
| `example.signed.reserve` (example only) | 3 | The signed report, published after its observation time | `close_after_secs` 0 |

### Display (API and website, C6)
- Always "level 5" and per (source, metric); never one score across metrics.
- Established, with a baseline entry (period metrics only): the skill against repeating the last known value, "+S
  points: on K resolved values with an earlier known value, this source agreed A_s; repeating that
  earlier value would have agreed A_b" (C9, R-TR-11; signed, floored to a tenth of a point).
- Established: "agreed A of R resolved (rate)", agreed first, mean Brier, then late, void,
  contested, self-resolved and unresolved counts, "one of N sources on this metric" and "stated S
  of F resolved facts" (S = the record's resolved count, F = the metric's resolved facts).
  Not established: the counts and "not established: R resolved (30 needed), D subjects (5
  needed)", no percentage and no mean Brier. Rates and the mean Brier are floored (a tenth of a
  percent; four decimals), computed from the checked rows by the verifier.
- Never the words "verified" or "proven" for a level-5 value, whatever its record.
- Only real records as real. While none exists (the Bitcoin flow metrics have a rule, but no
  source states on them and their answers have no chain reader yet), the website says so and why, and shows the example (`onyx_track::example`) labelled "example" on the
  section and on every card, checked against its commitment pinned in the browser verifier.

### Assumptions
- The statement log is checked by `verify_log` (C5): signatures by the source's registered key,
  `committed_at` from the finalized block of the batch's post. What remains trusted: the chain's
  block time (on an OP stack the sequencer sets it, within `max_sequencer_drift` of its L1 origin)
  and that block timestamps strictly increase; the coordinator keeps sealed statements private
  until their reveal and posts batches promptly (it can delay, never backdate).
- The key book mirrors the level-3 registry's reviewed keys; a retirement or compromise there is
  revoked in the next book (amendment 1 item 5). With a chain pinned the level-3 book's status at
  each post decides instead (R-TI-9); without one the coordinator's flags are trusted.
- Resolutions: in chain mode (amendment 3) they are rederived by every client from its own node
  at the commitment's head, so omission and forgery fail the check. Trusted there: the chain's
  contracts at the pinned addresses, the reviewer's operator declarations, an archive node for an
  old head, and that level 1 is not accepted on a recipe metric until C40 (refused, fail closed).
  Without `params.chain` (tests, the example) they remain caller input: complete and authentic by
  assumption.
- The registry only grows (no source, metric or subject is removed and rights never become
  `Forbidden`), so a statement once admitted is admitted by every later computation (TR-R1-4).
- Close times per metric are early enough that the truth is not public anywhere before them
  (the R-SCORE-1 problem; `Void` covers only the resolution's own public time).

## The ladder in the product (ADR-0048, NORTH-STAR NS5)
- **R-LAD-1 Level of a value.** The strongest public proof of exactly that value (same lane-free
  key, same value hash). Proofs only accumulate, so the level only rises; a stronger answer with
  another value is no rise of this one (TODO NS5-5: say so on the weaker value).
- **R-LAD-2 Ladder.** `onyx_core::ladder::Ladder { value, steps }`, `value_key(key, value hash)`
  (domain `onyx-ladder-value-v1`), `fact_key(metric, subject, as_of)` (`onyx-ladder-fact-v1`, no
  source, no lane), `row_key(round, shard, index)` (`onyx-ladder-row-v1`), `value_hash(value) =
  keccak256(borsh(value))`, `observed_value(kind, answer)` (the level-2 value rule, also used by
  `onyx-track`). Steps: one to five, evidence non-zero, each strictly stronger and no older than
  the one before (`check`); `check_stated(level)`: the top is the answer's level;
  `check_extends(prev)`: every step of `prev` kept in place (no fall, no rewrite); `Ladder::of`:
  the rises of any set of proofs in time order (ties: stronger first).
- **R-LAD-3 Trust.** A client takes a step only through its lane's own check (level 2: the round
  rebuilt and asked on chain; rows: `verifyRowLeveled`); a stronger step without such a check is
  refused (`onyx_api::ladder::check_served`). Cross-lane checks: TODO NS5-1.

## Assumptions
- Observers are third parties with no special access: every honest observer sees the same bytes
  for the selected value (no personalised pages, no per-client prices).
- Observers are drawn at random from a bonded pool (R-OBS-2a). The pool's membership and bonds
  are the coordinator's snapshot; the beacon is a chain block hash the client may check.
- A strict majority of the drawn observers is honest, which a random draw turns into: no colluding
  group holds most of the pool. Level 2 does not survive a colluding or
  bribed majority (SchellingCoin's known weakness); bonds and random assignment raise the cost,
  they do not remove the risk. That is why the money default accepts levels 1 and 3 only; level 2
  joins once level-2 disputes exist.
- The off-chain prototype uses unix seconds for all phases; reward funding (who pays `reward`) is
  outside this module, as for validation fees.

## Open questions
- Who funds feed rewards: the buyer of the feed, the revenue split (ADR-0025), or a feed budget?
- When may money markets accept level 2 by default? Decided for now: no (levels 1 and 3), until
  a level-2 dispute path exists (L2R-F8).
- Fetchers as WASM under the recipe host, so the extraction step is content-addressed code like
  recipes (today the example fetcher is pinned by a version tag; see THREATS L2-F5).
- Level-2 disputes: a `NoResult` or a contested value could go to a drawn panel that re-fetches
  (ADR-0021 style) while the source still serves the window's value; not designed.
- On-chain port: map the window and phases to block time; store commits and reveals in a contract.
- Level 3 for the same feeds via zkTLS (TLSNotary), which would let one proven fetch replace a
  majority of unproven ones. Deferred with adoption criteria (ADR-0028 item 8).
- Level 3: Ed25519 sources; a public, reviewed keys-document format beyond the example; key
  provenance re-checks on a schedule (a source that removes a key from its well-known document has
  rotated it); the on-chain port of the sequence and fact books.
- Level 4 as a protocol lane (bonded claim) beyond labels.
- Level 5: priced level-1/2 answers in served logs (C22), paged logs (C21) and a served registry
  for reruns (C23) (the logs themselves are served since C20), level-1 chain reads and a per-fact index
  (C40), the browser reading the on-chain latest and a commitment log's poster rotation (C41),
  a retired outcome before any registry removal (C90), broken receipts published beside a record
  (C91), statements by an entity on itself (C92), sealed rows encrypted to a time-lock (C93, after
  C51) (ROADMAP C21-C23, C40, C41, C90-C93; PLAN q-TR-1, q-TR-4).
