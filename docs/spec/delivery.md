# Spec: delivery

## Scope
How customers get data: pay-per-call API, per-row Merkle proofs, bulk export and a settlement
endpoint for prediction markets. Built in M7 (`crates/onyx-api`, ADR-0023 with its loop-39
amendment). Run book: docs/ops/run-api.md.

## Interfaces
All JSON. Hashes and rows are `0x` hex. Every row comes with its proof and a `verify` bundle.
The machine-readable description of every route below is [docs/api/openapi.json](../api/openapi.json)
(OpenAPI 3.1, ADR-0045), served by each node at `GET /v1/openapi.json` (free, ETag); a check keeps
it equal to the code, and every answer the tests see is validated against it.
- `GET /v1/health` -> `{status, x402Version, roots, chainId, contract, payments, payTo,
  contributorPass, receiptsPending, ..., level3}`. Always free. `level3` (ROADMAP C62) is null
  without a level-3 store, else the post log as the node last read it: `breakerSecs` (600),
  `hostSkewSecs` (300), `now` (this host's clock), `logReadable`, `logError`, `posts`,
  `postsRecorded`, `postsPending`, `postsRefused`, `postsRefusedRetrying` (refusals that may
  clear and are retried), `postsRefusedFinal` (the rest: never retried, only grows while the log
  is kept), `oldestPendingSince`, `oldestPendingSecs` (on this host's clock,
  saturating at 0) and `stopped` (null, `level3_posts_delayed`, `level3_clock_skew` or
  `level3_store_unreadable`: exactly the stop a `/v1/signed` request would get now). Counts are
  null while the log is unreadable. No report content.
- `GET /v1/proofs/{round}/{shard}/{index}[?root=]` -> one row:
  `{round, shard, index, row, leafCount, proof{index, leafCount, siblings}, root, finalizedAt,
  recipeId, labelSet, tx, license, attribution, snapshotUnix, verify}` (`attribution` and
  `snapshotUnix` may be null; a gated metric's row adds `servingCheck`).
- `GET /v1/metrics/{metric}?subject=&as_of=&min_as_of=` -> the row whose key is `subject`
  (UTF-8, e.g. `entity:okx`) in the newest finalized shard of `metric` with
  `min_as_of <= as_of <= as_of (asked)` that has a row for `subject` (older shards are tried
  when a newer one lacks it, at most 16 shards). The price is quoted from the newest finalized
  shard, and only that shard and older ones are searched, so a shard that finalizes after the
  quote is never served at the quoted price (M7-L47-F1). When no finalized shard meets
  `min_as_of`, the answer is 404 before any 402: nothing is charged for an answer the client
  would refuse (M7-L47-F5). Same
  shape plus `{metric, subject, asOf}`. Metric and market names are `[a-z0-9._-]`, at most 128
  characters.
- `GET /v1/export/{round}/{shard}[?root=]` -> `{round, shard, root, leafCount, rows[],
  finalizedAt, recipeId, labelSet, tx, license, attribution, verify}`. Capped at
  `export_max_rows` and `export_max_bytes` (413 `export_too_large`; before the 402 when the
  rows are already in memory, else after verify with nothing settled and the unit given back).
  Free exports are limited per client (`free_exports_per_minute`, 429 `rate_limited`;
  M7-L47-F2).
- Root lookups (AUD-1-F4): a lookup that finds no final root, or fails, is remembered for
  `root_miss_ttl` (default 5 s); concurrent lookups of one shard share one node call; lookups that
  miss every cache cost the client one unit of `root_lookups_per_minute` (default 120, 429
  `rate_limited` beyond). A shard that finalizes is therefore served at most `root_miss_ttl` late.
  Node sends (self-settle, reporter) cap `max_fee_per_gas` (default 10 gwei) and the gas limit
  (default 15M) whatever fees the node reports (AUD-1-F3).
- `GET /v1/settlement/{market}` -> `{market, subject, value, round, shard, row, index, leafCount,
  siblings, root, finalizedAt, recipeId, labelSet, tx, verifyCall, verify}`. `verifyCall` names
  `OnyxSettlement.verifyRow(uint64,bytes32,bytes,uint64,uint64,bytes32[])`; the same payload
  passes `OnyxMerkle.verify` (`contracts/test/OnyxApiVector.t.sol`, 2-leaf BTC and 13-leaf vectors).
- `GET /v1/served/{metric}` (ROADMAP D3, ADR-0034 Amendment 2) -> free, for a metric with a
  serving check on this server: `{metric, gate, shards[{round, shard, from, to, root}]}`, the
  shards the check cleared at the roots it judged, in snapshot order (start, end, round). No row.
  A hint for a client re-running the check (G-6): nothing in it is trusted. 404
  `no_serving_check` when this server runs no check for the metric; 403
  `label_rights_withheld` under `ONYX_RIGHTS=fallback` for a label-derived metric (before
  anything is read); a shard that fails the serve-time checks (rights, test labels, catalog
  binding) is left out. Free, so each client has a budget (`free_exports_per_minute`, its own
  bucket): 429 `rate_limited` beyond.
- `GET /v1/observations/{metric}?subject=[&window_end=]` -> a level-2 value with its round's
  public transcript (ADR-0027 amendment 4): `{metric, subject, subjectText, windowStart,
  windowEnd, config{commitClose, revealClose, reward, outlierSlash, missSlash},
  pool[{address, bond}], draw{beaconHeight, beacon, attempt}, observers[{address, bond}],
  commits[{observer, commit, sig}], reveals[{observer, observation, salt, sig}] (`sig`: the observer's EIP-191 signature, ADR-0027 amendment 7),
  answer, value, proofLevel, templateId, specId, roundId, resultHash, factKey, registryFactKey,
  settledUnix, rights, license, attribution, onchain, limits}` (`limits`: §Level-2 limits; also in
  its 402 body and in the `PAYMENT-REQUIRED` offer's `extensions.limits.info.limits`, so it is seen before paying). `subject` is the Borsh `Subject` (hex),
  asked for by its text (`subjectText`, e.g. `us.treasury.federal_debt:2026-10-08`); `answer` is
  the Borsh `Answer` and `observation` the Borsh `Observation`; `value` is the answer as text (a
  decimal at the feed's scale). `window_end` picks a fixed feed's round, or `as_of` the newest
  whose window ends at or before it (default: the newest; not both, 400); a dated feed has one
  round per subject. `min_window_end` (optional on the server, required in the client) is the
  client's freshness floor: a round whose window ends earlier is a 404 `too_old` before any 402
  (L2API-R4-F2, as `min_as_of`). `onchain` is `null` or `{chainId, registry, call, drawMatches,
  drawBudget, transcriptComplete, drawRevealKey, revealLogComplete}` (the last two since ROADMAP
  C11; see "Sealed reveals" below): the four `OnyxRegistry` views a client asks together
  (`observationLevel`, `drawMatches`, `drawBudget`, `transcriptComplete`; level 2 alone is not
  enough, L2RND-R3-F3; REVLOG LOW-1). `transcriptComplete` is optional in the hint (a server from
  before ADR-0027 amendment 8 omits it) and, when present, must name exactly that call; the
  client asks it whether or not the hint names it. Clients from before this key refuse the
  extra key (fail closed) and must update. Priced like a row of the metric (per-row policy),
  decaying from `settledUnix` = `FeedTemplate::decay_start` (a pure function of the template and
  the subject: a dated subject's earliest allowed window end, a fixed subject's own window end;
  never a round input, L2API-R2-F2, R3-F1; a round whose reveal close is after the checker's clock
  plus 300 s is not served, L2API-R1-F2),
  and billed to the revenue book `(OBSERVATION_ROUND, registryFactKey)` (revenue.md R-REV-12).
  `ETag: "obs-{keccak(registryFactKey)}"` (the fact, never the result hash: a result hash lets
  anyone confirm a guessed value, L2API-R1-F5), `immutable` when the URL names one round (a
  dated subject or `window_end`). A paid observation answers no `If-None-Match` before payment
  (as M7-L39-F3); a free one revalidates with 304.
  `outcome=all` (the only other value of `outcome`; `value` is the default and has no
  parameter) adds `attempts`: every published round of the same fact that settled no value
  (SEC-R2-F1), by strictly ascending draw attempt, each before the value round's, as
  `{subject, windowStart, windowEnd, config, pool, draw, observers, commits, reveals, outcome,
  roundId, resultHash}` with `outcome` `sourceUnavailable` or `noResult`. Same price; its ETag
  ends `-all`.
- Observe on payment (ADR-0049, docs/spec/paid-observation.md; only with `ONYX_OBSERVE`): a
  `GET /v1/observations/{metric}?subject=` (no `window_end`, `as_of` or `outcome=all`) for a
  subject no round settled yet, which the metric's pinned dated template allows a round for now,
  is a 402 `observe_payment_required` with `observe: {price, deadlineSecs, pays, becomesPublic,
  refund}` (the same for every such subject). With `ONYX-PREPAID-VOUCHER` (exactly the last total
  plus `price`) the voucher is held, not taken, and the answer is a 202 `{ticket, state: "queued",
  deadline, retryAfter, status}`; while the fact's round is queued or running every other request
  for it is a 202 `{state, retryAfter}` with no ticket and no charge. A buyer with a hold pending
  gets `prepaid_refused` for any other voucher until it is charged or released.
- `GET /v1/tickets/{ticket}` -> free, `no-store`: `{state, metric, subject, deadline}` with
  `retryAfter` while `queued` or `running`, `observation` (the paid answer, the
  `/v1/observations` body) once `observed`, `refund` once `released` (nothing was charged). An
  unknown ticket is a 404 `unknown_ticket`.
- `GET /v1/observations/{metric}/attempts?subject=[&window_end=]` -> free: every published round
  of a fact that settled NO value, `{metric, subjectText, templateId, factKey, registryFactKey,
  attempts, onchain, limits}` (`onchain`: `null` or `{chainId, registry, drawCount, drawOf,
  drawBudget, transcriptComplete}`, the last optional as above). A fact with a served value is a 409 `settled_value`: its rounds come only with
  the paid value (`outcome=all`), since a disagreeing round's reveals can hold the value a later
  round sold. The same holds BEFORE the value (ATT-R1-F2): a round's reveals are served only once
  its fact is closed for good (`ObservationFeed::fact_closed`: a fixed fact with a round, which
  never takes a value after one; the store refuses it). Until then (a dated or period fact; a
  capped one can be reopened) each round is served SEALED: `reveals` is replaced by `revealed`
  (the observers who revealed), so no observation, page hash or salt leaves the server; the
  client checks draw, committee, commits, round id and that `resultHash` is the hash of the
  served outcome, but the outcome itself is the server's word (`CheckedAttempt::sealed`). A
  client refuses a sealed round in `outcome=all` and for a fixed fact. Each client has a budget
  of free answers (`free_attempts_per_minute`, 30; 429 `rate_limited`, ATT-R1-F5). Without
  `window_end`, the subject's newest window with published rounds.
  Level-2 values are never bound to a market: markets bind shard rows, and level 2 is outside
  the money policy (levels 1 and 3); the settlement endpoint keeps answering 409
  `proof_level_not_accepted` for any market on a shard below it.
- Level-2 client check: `client::verify_observation(body, &ObservationExpect{template,
  subject, window_end, min_window_end, now})` with the client's OWN copy of the metric's
  registered `FeedTemplate` (reviewed with the source's rights), never one the server sends.
  `min_window_end` is required (the client's freshness floor, like `min_as_of`, L2API-R1-F3)
  and `now` is the client's clock. A client clock more than 300 s slow refuses a round that
  settled just now (`NotSettledYet`); the client retries later or fixes its clock. The risk
  is the client's only: it never makes a bad round pass (L2API-R2-F4). It applies every rule a round must meet to open
  (`observe::check_round_params`: config timing and slashes, at least `min_observers` and at
  most `MAX_OBSERVERS` observers in ascending address order, every bond covering the outlier
  slash, L2API-R1-F1), refuses a round settling after `now` (+300 s) or a window ending before
  `min_window_end`, instantiates the spec, recomputes the observer draw from `pool` and `draw`
  and refuses any other observer set (`observe::check_draw`, L2-F2), recomputes the round id
  (v2, with the draw), requires every commit to be
  an assigned observer's, once, in ascending address order (L2API-R2-F3), opens every commit,
  applies the reveal-time rule to every reveal (`Observation::check`: kind, size, observation
  time inside the window, a page hash for a value; L2API-R2-F1), re-aggregates the reveals into the result hash
  (`observe::onchain_observation_check`), requires `answer` to be the value that hash commits to
  (`observe::result_hash_of`), and requires every derived field (`templateId`, `specId`,
  `roundId`, `resultHash`, `factKey`, `registryFactKey`, `windowEnd`, `settledUnix`,
  `proofLevel` 2, `rights`, `license`, `attribution`, `value`, `subjectText`, `metric`) to be
  what that recomputation yields; unknown keys are refused. With a node,
  `VerifiedObservation::observation_level_calldata()` asks `OnyxRegistry.observationLevel` with
  the client's own arguments; `client::observation_level_from_return` reads the answer (2: the
  chain recorded exactly this result for this fact). Without that on-chain answer the check
  proves the transcript is consistent and well formed, not who the observers were: an operator
  holding enough observer keys can write a coherent round (THREATS L2-F2, L2API-1). The draw
  stops a hand-picked set within a served pool; with a node,
  `VerifiedObservation::draw_matches_calldata()` asks `OnyxRegistry.drawMatches` and
  `client::draw_matches_from_return` reads it (true: the draw was announced on chain before its
  beacon height, is the fact's latest attempt, committed to the pool, spec and config the client
  recomputed (`OnchainObservationCheck::draw_commit`, L2RND-R3), and its beacon is the block hash
  `fixBeacon` read; L2RND-R1). `VerifiedObservation::onchain_calls()` gives the four
  `eth_call`s (observationLevel, drawMatches, drawBudget, transcriptComplete) and
  `client::verify_observation_onchain` checks their returns together: level 2, the draw matches,
  the subject's draw bound holds (attempt below the allowance, strikes never above it;
  L2RND-R2), and the served round leaves out no commit posted in its commit phase and no
  receipted reveal (ADR-0027 amendment 8). Without the node, the beacon is the operator's word.
- Rounds without a value (SEC-R2-F1): with `outcome=all`, `verify_observation` rebuilds every
  `attempts` entry like the value round (`observations::check_attempt`: draw, commits, reveals,
  aggregation; a round whose reveals settle a value is refused), requires them all of the value's
  fact, strictly ascending and below its draw attempt, and every served `outcome`, `roundId`,
  `resultHash` and `windowEnd` to be the rebuilt one (`VerifiedObservation::attempts`);
  `client::verify_attempts` does the same for the free endpoint. With a node, read at one block
  `drawCount(fact)`, then `drawOf(fact, i)` for every `i` below
  `VerifiedAttempts::draws_without_value(drawCount)`, `drawBudget`, and `transcriptComplete` for
  every published round (`VerifiedAttempts::transcript_complete_calldatas`);
  `client::verify_attempts_onchain` requires: with a value, `drawCount` = its attempt + 1; every
  published round's draw is the chain's at its attempt (commitment, beacon height, beacon,
  announced before the beacon height); every other draw has no beacon on chain; the subject's
  strikes cover these draws (exactly, for a dated feed) within the allowance. It returns the
  tally `{draws, without_value, published}`: the served value round is the only value round,
  and every draw the fact used before it is accounted for. Limits: a draw without a fixed beacon
  had no committee only because honest observers commit after `drawOf` shows the beacon fixed
  (`observe::check_commit_allowed`, ATT-R1-F4). Every commit and reveal carries its drawn
  observer's EIP-191 signature (ADR-0027 amendment 7, ATT-R1-F3), so a coordinator cannot
  replace a fixed draw's real round (say a dissenting one) with a fabricated round: a missing
  round and a substituted round are both refused. A left-out commit posted on chain in its
  commit phase, or a left-out reveal with a posted receipt, is refused (`transcriptComplete`).
  Not proven: that every OTHER reveal is published (a left-out reveal shows its observer as
  committed, not revealed): an observer that did not post, or whose reveal the coordinator
  declined to receipt after seeing it (THREATS REVLOG-1), can still be dropped; and while a
  round is sealed its `revealed` list is the server's word (REVLOG-5). The website shows "N draws, M without a value" from the recorded
  announcement's checked `draw_count` (the rehearsals' anvil chain is gone).
- Sealed reveals (ROADMAP C11, ADR-0027 amendments 10 and 11): a round of a draw with a reveal
  key (and a round without a value the coordinator recorded) carries `sealed`: `{revealKey,
  posts[{observer, ephemeral, keyProof, postedAt, ciphertext}], dismissals[{observer, shared,
  proof}], record}` (`revealKey` `x ‖ y` or null; `posts` every sealed post with a block time in
  (`commitClose`, `revealClose`], ascending, as `sealedRevealOf`/`sealedKeyProofOf` hold it;
  `dismissals` each garbage post's `S` and Chaum-Pedersen proof; `record` the `attemptDigest`
  the round was recorded with by `recordAttempt`, null for a value round). The key is left out
  when the round has none of these, so older answers are unchanged; a round served sealed keeps
  `sealed` (the ciphertexts show nothing; a dismissed post's plaintext is public by design,
  amendment 10 item 8h). Off chain (`observations::check`, `check_attempt`, on the server at
  load and in the client) every post must be a drawn observer's, in the reveal phase, with a key
  proof valid for this round; every dismissal must be of a posted observer whose reveal is not
  served, check against `revealKey`, and open a plaintext that is NOT a valid reveal of the
  served commit; every post must be revealed or dismissed; for a draw with a reveal key every observer served
  (or listed) as revealed must have such a post (sealed only, review C11-R1-3); a served record
  must be the digest of the round's own query. The query (`observations::SealedChecked::query`) is built from the
  client's draw (every drawn observer), the config's window (`windowStart`, `commitClose`,
  `revealClose`) and the recomputed round id, never from what the server says about them
  (review C4-R1-2). With a node, `client::sealed_calls` (or `VerifiedObservation::sealed_calls`,
  `VerifiedAttempts::sealed_calls`) gives, per round, `drawRevealKey`, `revealLogComplete`,
  `attemptMatches` (a round without a value) and `sealedRevealOf` + `sealedKeyProofOf` per served
  post; `verify_observation_onchain` and `verify_attempts_onchain` take their returns as a
  required argument and require: the draw's reveal key is exactly the served one (a keyed draw
  served without its posts is refused), `revealLogComplete` is true, every served post is the
  chain's byte for byte (a server's own garbage under an observer's name is refused), and
  `attemptMatches` is true for every round served sealed and every round without a value of a
  keyed draw (a record for every sealed round, REVLOG-5). `onchain` may name the three calls
  (`drawRevealKey`, `revealLogComplete`, `attemptMatches` on `/attempts`), each exactly as in
  `observations::*_CALL`; the client asks them whatever the hint says.
- Time-locked sealed reveals (ROADMAP C51, ADR-0027 amendment 14): a round of a draw announced
  while the registry required time-locks (`drawTimeLocked`) carries `sealed.timeLock`:
  `{lockRound, farRound, beacon, openings[{observer, sigma, tau}]}` (`lockRound` the first drand
  quicknet round at or after `revealClose + 60`; `farRound` the round every post's far-date copy
  is locked to, ROADMAP C52, ADR-0027 amendment 15: the first quicknet round at or after the
  fact's latest possible reveal close, from the template's last observable day of the subject,
  plus one day; `beacon` drand's 48-byte signature of the lock round; `openings` the time-lock
  opening of each dismissal whose time-lock opened, ascending). The key is left out for a draw
  that is not time-locked, so older answers are unchanged. Off chain the client requires
  `lockRound` and `farRound` to be its own computation from the config and its OWN template, the
  signature to verify against the pinned quicknet key (constants in code, never served), every
  post to be longer than 272 bytes (the time-lock, the far copy and the wrapped body key), each
  opening to be of a dismissal, and checks every dismissal as a time-locked one: an opening must
  re-encrypt to the post's time-lock byte for byte; the body key it unwraps with `S` and `tau`
  must then fail its far copy (exact re-encryption to `farRound`) or open a body that is no
  valid reveal; without an opening, the time-lock must not open canonically under the signature
  (the coordinator owed the opening otherwise). Once `farRound` is published anyone opens every
  post's far copy with its signature alone (`timelock::unseal_posts`) and recomputes the round
  without the coordinator. With a node,
  `sealed_calls` adds `drawTimeLocked(fact, attempt)`, and the round must be served time-locked
  exactly when the draw is (`onchain` may name `drawTimeLocked`, exactly as
  `observations::DRAW_TIME_LOCKED_CALL`). The web page mirrors these checks
  (`apps/web/verify/src/sealed.rs`, `sealed_reveals.time_lock` and a recorded
  `chain.draw_time_locked` answer), re-encrypting with `onyx-tlock` built for wasm32.
- Server side: `observations::ObservationStore::from_json` loads a results store file
  (`ONYX_OBSERVATIONS`; written by the level-2 runner, e.g. `cargo run -p
  onyx-source-fiscaldata --example level2-round -- --store PATH`), refuses it unless its template
  is the one pinned for the metric (`ONYX_OBSERVATION_TEMPLATES`; with a registry pointer, also
  `OnyxRegistry.metrics`, L2API-R4-F1: the lag is not in the spec, so an edited lag would move
  the price clock without changing any spec, round or result), and re-checks every round with
  the same function the client uses, at the server's clock (a round settling in the future
  never loads); nothing derived is read from the file. A source whose
  rights forbid redistribution never loads; `Restricted` gets 451 like a shard.
- `verify` bundle: `{ledger, chainId, contract, call, round, shard, root, leafCount, binding,
  bindingInputs{recipeId, runtimeHash, labelSetHash, sourceHash, schemaId, rightsHash},
  shardInputs{paramsHash, registryRoot}, sourceRef, rights, finalizedAt, finalizedUnix, tx}`.
  `sourceRef` and `rights` are the Borsh bytes of the pinned `SourceRef` and `DataRights`;
  they hash to `sourceHash` and `rightsHash` (M7-L40-F1). `ledger` is `chain`
  (with `chainId`, `contract`) or `off-chain` (both `null`). `call` is
  `getFinalizedRoot(uint64,bytes32)`.
- Client check (M7 loop 3, ADR-0023 Amendment 2): by default
  `onyx_api::client::ClientNode::new(rpc, chain_id, contract).verify(body, &Expect)`, which
  reads `getFinalizedRoot` on the client's own node; or
  `verify_response(body, &Expect, Option<&ChainAnswer>)` with an answer the client read itself
  (`ChainAnswer::from_return(round, shard, chain_id, contract, eth_call_return)`; `None`, not
  final, is refused with `NotFinalOnChain`); the explicit opt-out is
  `verify_response_unchecked(body, &Expect)` (`chain_checked: false`). The answer
  names the `(round, shard)` it was read for and must match the response (M7-L41-F4). Each
  endpoint's answer may carry only its own keys (`client::allowed_keys`); an extra key, at the
  top level, in `verify`, its inputs or `proof`, is refused (`UnexpectedField`).
- Metric definition (`client::MetricDef`, M7-L41-F2): source id, chain (`None` for documents),
  window (`Blocks(n)`: exactly n blocks; `FromHeight(h)`: starts at h; `Document`), recipe,
  runtime hash, schema, params hash, label set (required; `ZERO` means no labels) and registry
  root. A shard that differs in any of them is refused, a `test.*` label set included.
  Limit (PLAN q14, open): one response cannot prove it is the most recent shard for a metric,
  or that none exists.
- Roots: `trait RootSource` (`LedgerRoots` from a settled ledger; `ChainRoots::connect` checks
  `eth_chainId` and the contract code, then reads `getFinalizedRoot` at block tag `finalized`).
  Rows: CAS `Kind::Rows` by root.
- Payment (x402 v2, ADR-0023): `PAYMENT-REQUIRED` (402 challenge), `PAYMENT-SIGNATURE`
  (client payment), `PAYMENT-RESPONSE` (settle receipt). `trait Facilitator { verify, settle }`.
- Contributor pass (ADR-0025, revenue.md R-REV-5): request header `ONYX-CONTRIBUTOR`; with a
  valid proof, fresh data is discounted (default free) and answered `private`. Each settled
  payment leaves a usage receipt for the shard served.
- Caching: see Rules. `ETag: "{root}:{index}"` (export `"{root}:all"`, settlement
  `"{root}:{keccak(subject)}:settlement"`, observation `"obs-{keccak(registryFactKey)}"`),
  `If-None-Match` -> 304 (free data only; paid data answers no conditional request before payment).

### Track records (level 5, ROADMAP C6, ADR-0036 amendment 2)
Free, never priced, never bound to a market (a record never raises a level or settles money,
ADR-0036 item 12). Code: `onyx_api::track_serve` (store and routes) and `onyx_track::wire` (the
body, built by the server and checked by every client, the browser included).
- `GET /v1/track[?from=<n>]` -> `{level: 5, published, commitmentHash, asOf, rules[{metric,
  closeAfterSecs, periodSecs, leadSecs, tolerance}], records[{source, metric, resolved,
  established, example}],
  withheld, truncated, next, reason}`. `reason` says why there is no record (no publication, no
  close-time rule, no statement yet), else `null`. 1,024 records a page from record `from`
  (canonical decimal, default 0); `next` is the following page's `from` (`truncated` true), else
  `null` (C20, review TR-S-R2-3).
  A rule has `closeAfterSecs` (an `AfterAsOf` rule) or `periodSecs` and `leadSecs` (a
  `PeriodStart` rule, C9); the other fields are `null`. A `Record` row carries the C9 baseline
  counts (`baseline_entries`, `baseline_source_agreed`, `baseline_agreed`, proof-ladder.md R-TR-11)
  under the same proofs.
- `GET /v1/track/{source}/{metric}[?since=0x<commitment hash>]` -> `{level: 5, example, source,
  metric, asOf, commitmentHash, commitment, params, record{row, proof}, summary{row, proof},
  history{entries[{row, proof}], before, after} | null, chain}`. `commitment`, `params` and every
  `row` are Borsh hex (`TrackCommitment`, `TrackParams`, `Record`, `MetricSummary`, `Entry`), so
  no decoded field escapes the proofs; `proof` is `{index, leafCount, siblings}` as for rows.
  `history` holds every entry of the record's range plus the entries just outside it (`null`
  at the tree's ends); it is `null` only past 1,024 entries (`onyx_track::wire::MAX_HISTORY`),
  and then the client checks the record and summary only and says the history was not checked.
  `chain` is `null` without `since`; with it, the commitments after `since` (Borsh hex, oldest
  first, at most 4,096) ending with the served one. `example` is true exactly when the source or
  the metric is an example id (`example`, `example.*`, `entity:example*`).
- Ids are dotted ids (`onyx_core::ids::is_dotted_id`); the only query parameter is `since`, in
  canonical form (`0x` and 64 lowercase hex digits); anything else is 400 `bad_request`.
- Order: ids and query; rights (403 `label_rights_withheld` in `ONYX_RIGHTS=fallback`, before
  any lookup, for a metric with a name segment starting with `entity`, or a ruled metric of the
  publication that the registry marks as needing a label set or does not know; a metric with no
  rule has no record to withhold and answers its 404);
  404 `no_track_record` with `reason` `none_published`, `no_rule` (the published params have no
  close-time rule for the metric) or `no_record` (the source stated nothing on it); 409
  `unknown_commitment` (`since` is not in the server's chain); 413 `track_chain_too_long`; the
  ETag; then the per-client free budget (30 per minute, same burst, IPv6 by prefix as for
  exports; 429 `rate_limited`). `ETag: "track-{first 16 bytes of keccak(commitment hash ‖
  source ‖ 0 ‖ metric ‖ 0 ‖ since)}"`, `Cache-Control: public, no-cache`; a matching
  `If-None-Match` is a 304 and costs no budget.
- Server: `TrackStore::publish(params, registry, statements, resolutions, as_of)` computes the
  book and appends its commitment only if it extends the published one (`check_extends`:
  chained, later, params and every earlier statement and resolution kept), so a server never
  goes back to an older commitment or forks its own chain. A node's store refuses example ids;
  only a test store (`allowing_examples`) publishes the example. A node without
  `ONYX_TRACK_PUBLICATIONS` starts with an empty store: the Bitcoin flow metrics have a reviewed close-time rule (proof-ladder.md "Close-time
  rules", C9), but no source states on them and their level-1 answers have no chain reader yet
  (C40), so today every record is a 404 `no_track_record` (`none_published`, reason text
  `track_serve::NOTHING_PUBLISHED`).
- Client check: `onyx_track::wire::check_record_body(body, &Expect{source, metric, min_as_of,
  known, latest})`: the shape (no extra key; `chain` null without `known`), the record asked for,
  the `example` flag, the commitment (hash, `asOf`) and params (`params_hash`), with `latest` (the
  record's on-chain log head, read by the client at a finalized block, ADR-0036 amendment 3)
  exactly that commitment (`NotLatest`), the floor `min_as_of` (`TooOld`), with `known` the hash chain from it to the served commitment with `as_of` rising
  (`NotDescendant`: stale or not chained; a commitment over a rewritten log chained to the known
  one passes here, and only the logs refuse it: `check_log_body` with `known`, below), then
  `verify_history` (or, past `MAX_HISTORY`, `verify_record` and `verify_summary`). The browser
  runs the same function (`check_track_record`; the site's example with its commitment pinned
  in the verifier, `check_track_example`).

- `GET /v1/track/{source}/{metric}/history?at=0x<commitment hash>&from=<n>` (C20, ADR-0036
  amendment 5 item 5) -> `{level: 5, source, metric, commitmentHash, from, entries[{row, proof}],
  before, after}`: up to 1,024 entries of the record from its `from`-th, with the entries just
  outside the page (`null` at the tree's ends). Both parameters, in this order, canonical (else
  400); a record that does not exist answers as above; `at` not the latest commitment is 409
  `commitment_moved` (fetch the record again); `from` past the record's last entry is 400. Same
  ETag rule (over `history ‖ at ‖ source ‖ 0 ‖ metric ‖ 0 ‖ from`) and budget as a record. Client:
  `check_history_page(page, &checked, from)` per page (same record and commitment, entries at the
  asked indices), then `complete_history(&mut checked, &pages)`: pages from 0 tiling the record's
  range, `verify_history` over all of them with the first page's `before` and the last page's
  `after`.
- `GET /v1/track/log[?since=0x<commitment hash>]` (C20, ADR-0036 amendment 5 items 1-3) ->
  `{level: 5, example, asOf, commitmentHash, links[], statements, resolutions, intake}`. `links`
  are Borsh `Step`s (`commitment`, `params`, `statements` and `resolutions` row counts), oldest
  first, from the commitment `since` names (inclusive) or the latest only, ending with the latest;
  `statements` and `resolutions` are the Borsh `Vec`s of every row in publication order (a step's
  log is the first `n` rows, sorted canonically); `intake` is the Borsh `PublishedIntake` at the
  latest head (`null` when the params name no intake). `example` is true when every ruled metric
  is an example id. Order: the query (`since` only, canonical); 404 `no_track_record`
  (`none_published`); 403 `label_rights_withheld` in fallback when any ruled metric is
  label-derived; 403 `track_log_priced` when any resolution is at level 1 or 2 (a value the paid
  API sells; ADR-0036 amendment 5 item 2, C22); 413 `track_log_too_large` past 16,384 rows
  (statements, resolutions and revealed intake rows; `onyx_track::wire::MAX_LOG_ROWS`, C21); 409
  `unknown_commitment`; 413 `track_chain_too_long`; 413 `track_log_too_large` when links times
  rows passes `MAX_LOG_CHECK` (1,048,576, 64 links over the full 16,384 rows, about 5 s of native work; ask since a later commitment; review C20-R2-1); the ETag (over `log ‖ latest ‖ since`); then
  its own free budget, 2 answers per client a minute (`LOG_FREE_PER_MINUTE`; 429). Client:
  `check_log_body(body, &LogExpect{min_as_of, known, latest, registry, reviewed, lives})`: exact
  shape; at most `MAX_LOG_ROWS` rows and `MAX_LOG_CHECK` links times rows (`TooLarge`, before any
  hashing); the last link hashes to `commitmentHash` with `asOf`; `example`; `latest` pin; floor;
  `known` among the links (`NotDescendant`); `check_steps` (each link's log hashes to it, params
  to `params_hash`, each link extends the one before, the last covers every row); the intake
  present exactly when the params name one, for that intake, at the head; with `registry`,
  `compute` again gives the latest commitment (`Recompute`; `RegistryDiffers` when the registry
  root is not the commitment's); with `reviewed` too, `verify_log` on the intake (in chain mode
  only with `lives`). It returns `recomputed`, `intake_checked` and `resolutions_from_chain` (chain
  mode: rerun `chain::verify_resolutions` against your own node). Largest body parsed:
  `MAX_LOG_BODY_BYTES` (48 MiB).
- Chained logs (C21, ADR-0036 amendment 6): a publication in log format 2 (`Publication::chained`)
  keeps its logs in publication order and hashes them row by row (`statements_hash` =
  `keccak256("onyx-track-statements-v2" ‖ borsh((n: u64, head_n)))`, `head_i` =
  `keccak256("onyx-track-statement-row-v2" ‖ head_{i-1} ‖ borsh(row_i))` from `head_0` zero; the
  same for resolutions with `-resolution-row-v2` and `-resolutions-v2`). `GET /v1/track/log[?since=]`
  then answers its head, `{level: 5, example, asOf, commitmentHash, format: 2, links[], rows,
  pageRows, intake}`: `rows` is the length of the run's row stream (segment 0: every statement,
  then every resolution, the first link covers; segment `j`: the statements, then the resolutions,
  new to link `j`), `pageRows` 4,096, `intake` the Borsh `IntakeHead` (`intake`, `head`, `books`,
  `posts`: the post count) or `null`. Pages: `GET /v1/track/log?at=0x<latest>&from=<n>&since=0x<first
  link>` -> `{level: 5, commitmentHash, since, from, rows}` (`rows`: Borsh `Vec<LogRow>`, exactly
  4,096 rows but at the stream's end) and `GET /v1/track/log?at=0x<latest>&posts=<n>` -> `{level: 5,
  commitmentHash, from, posts}` (Borsh `Vec<(Post, RevealedBatch)>`: whole posts from `n`, more than
  one only within 4,096 revealed statements). Parameters in this order and canonical (else 400).
  Order for pages: as for the head (publication, rights, priced); 400 when the logs are format 1;
  409 `commitment_moved` when `at` is not the latest; 409 `unknown_commitment`, 413
  `track_chain_too_long` for `since`; 400 for a position past the end; for rows pages, 413
  `track_log_too_large` past the client's budget (rows plus links over `MAX_LOG_CHECK`; the client
  counts revealed intake rows against the same budget as posts arrive); the ETag (over `log-rows ‖ at ‖
  since ‖ from` or `log-posts ‖ at ‖ from`); then the pages' own budget, 60 per client a minute
  (`LOG_PAGES_PER_MINUTE`; 429). No 413 for rows past 16,384 on chained logs: that cap is format 1's.
  Client: `PagedLog::start(head, &expect)` (exact shape, latest, example flag, pin, floor, `known`
  among the links, `rows` equal to the links' stream, budget, then each link's params hash,
  chaining, `as_of` rising, params kept), `take_rows(page)` per page in order (same `at` and
  `since`, the asked `from`, the exact row count; each row checked as it is taken:
  `chained::ChainedCheck` absorbs it into its log's chain, refuses a repeated row and runs
  `check_extends`'s rules for a row new to its link; a link's two hashes are checked when its last
  row is in), `take_posts(page)` for the intake (pages filled as the server fills them: after `P`
  pages holding `R` revealed statements over `K` posts, `(P - 1) × 4,096 ≤ 2R + K`, else
  `Malformed("posts pages cut short")`, review C21-R2-4), then `finish(&expect)`: the rerun as for format 1
  (with the registry `compute` must give the commitment, the two log hashes being the chained ones
  just checked; with the reviewed keys `verify_log`). Largest page parsed: `MAX_LOG_PAGE_BYTES`
  (48 MiB). Format-1 publications are served and checked exactly as before.
- `GET /v1/track/registry` (C23, ADR-0036 amendment 6 item 6) -> `{level: 5, commitmentHash,
  registryRoot, registry}`: the registry the latest commitment names, as Borsh
  `onyx_core::RegistryData` (entities, sources' reviewed rights, canon and proof ids, aggregates,
  instrument namespaces, metrics, entity keys, revocations, domain). Only a version-2 publication
  carries one (`Publication::push_with`, its root checked against the book's `registry_root` by the
  coordinator; the node has no address rules and cannot check it). No query (else 400). Order: 404
  `no_track_record` (`none_published`); 403 `label_rights_withheld` where the logs are withheld;
  403 `track_registry_names_addresses` when the registry names an address (a recorded revocation,
  a label row the root covers), in every rights mode (review C23-R2-2); 404
  `no_track_registry` (a version-1 file); the ETag (over `registry ‖ latest`); the records' free
  budget. Client: `check_registry_body(body, &commitment, &rules)`: exact shape, the same
  commitment (`Commitment`) and `registry_root` (`RegistryDiffers`), then
  `RegistryData::build_checked(rules, registry_root)`: the client rebuilds the registry with its
  own address rules (`onyx_core::RuleBuilder`; the browser and Bitcoin clients use
  `onyx_source_btc::BtcRules`) and uses it only when its root is `registry_root` (`Registry`). The
  registry holds no value of any metric.
- Publication path (C20, ADR-0036 amendment 5 item 4): the coordinator appends each computation
  to an `onyx_track::publication::Publication` (`push`: refused unless it extends the latest) and
  writes it with `onyx_coordinator::track_chain::write_publication` (checked; refused unless it
  starts with the file already there, and with the same steps only unchanged; whole file renamed
  into place). onyx-node reads `ONYX_TRACK_PUBLICATIONS` (docs/ops/config.md) at start and
  whenever it changes (modification time, length, inode, change time; on the blocking pool, one
  read at a time: a request that arrives during a read is answered from the last finished one and
  never waits, review C20-R2-3): a missing file publishes nothing yet; a file that does not parse
  or check, does not start with what was served, (after a restart) does not start with the bytes
  named by the head kept in `ONYX_STATE_DIR/track-served-head.borsh` (step count, latest
  commitment, log row counts and a Keccak digest of that prefix; only the steps after it are
  checked again), or would cost more than `MAX_INSTALL_CHECK` (4,194,304 steps checked times rows,
  about 20 s; review C20-R2-2) answers every track route 503 `track_store_unreadable` until fixed;
  so does a node whose state directory holds a head but that names no file (C20-R2-5); the node
  itself always starts. The node serves the rows the commitment commits to
  (`TrackBook::from_committed`); it computes nothing (no registry), so with a file every ruled
  metric counts as label-derived in fallback.
- Browser (C20 item 6): the page remembers the last commitment it checked per record
  (`apps/web/src/lib/trackSeen.ts`, browser storage, per viewer) and asks with `?since=`; the
  verifier's `check_track_record(body, source, metric, min_as_of, known)` and, for the site's
  example, `check_track_example_since` (the since file holds the example as the server answers
  `?since=` each example commitment); an unknown remembered commitment shows nothing and offers
  to forget it. `check_track_example_log` reruns the example's logs with the example registry;
  `check_track_record_paged` checks a paged history.

### Signed facts (level 3, ROADMAP C10, ADR-0028 amendment 7)
- `GET /v1/signed/{fact}` (`fact` = `signed_fact_key`, `0x` and 64 lowercase hex digits; no query
  parameters): mounted only when the manifest pins `OnyxSignedBook` and `ONYX_SIGNED_REPORTS`
  names the level-3 coordinator's post log (`onyx_coordinator::signed_intake::PostLog`, re-read
  when the file changes; a file that does not parse or names another chain or book refuses every
  answer, 503 `level3_store_unreadable`). Free, never priced, 30 answers per client per minute
  (429 `rate_limited`), `Cache-Control: no-store`. Not money: no market settles on it.
  `ONYX_RIGHTS` does not gate it: `fallback` withholds what is derived from the entity label sets
  (L8-5), and a signed fact is the source's own data under the rights its review records (THREATS
  L3-F8; no real source is registered yet). Review C10-R2-4.
- 200 only by docs/spec/proof-ladder.md R-SIG-ON-7: the fact's report with `level` 3, the payload
  and signature, `reportHash`, `valueHash`, `acceptedAt`, `checkedAt` (finalized block number,
  hash, time), `book`, `chainId`, `verifyCall` and `moneySettlement`. A client checks it by
  calling `OnyxSignedBook.settleable(fact)` at a finalized block of its own node and comparing
  both hashes: the full client check is docs/spec/proof-ladder.md R-SIG-CL-1
  (`onyx_web_verify::signed_view::check`, the code the website runs; ROADMAP C61).
- Refusals: 404 `no_signed_report`; 409 `level3_not_servable` with `reason` `unknown`,
  `disputed`, `in_dispute_window` or `in_serving_margin` (both with `servableAt`, block time),
  `in_host_margin` (with `servableAtHostUnix`), `other_report`, `post_pending`, `post_not_final`,
  `post_retrying`; 503 `level3_chain_unavailable` (no answer, no
  finalized block; `Retry-After`), `level3_posts_delayed` (a post pending more than 600 s),
  `level3_clock_skew` (`Retry-After`: a time in the post log is more than 300 s ahead of this
  server's clock, review C10-R2-2).

### Ladder (ADR-0048, NORTH-STAR NS5)
Every value answer states its level: `proofLevel` on `/v1/proofs`, `/v1/metrics`, `/v1/export`,
`/v1/settlement` and `/v1/observations` (level 3 keeps its `level`), and `ladder` on the row,
metric and observation answers: `{value, level, steps: [{level, name, evidence, at, record}]}`.
`value` is `onyx_core::ladder::value_key` (the value itself is never in it); `level` is the top
step's and equals `proofLevel`; each step is strictly stronger and no older than the one before
and names its evidence (a row: the shard root; level 2: the result hash) and its chain record
(`OnyxRegistry.ShardAttested`, `OnyxRegistry.ObservationRecorded`). A node serves its own lane's
one step (`null` only if the answer is no value of its kind). Client: `verify_observation`
returns the accepted `ladder` (keep it; `Ladder::check_extends` refuses a later fall);
`client::check_row_ladder(body, verified, chain_level)` binds a row's level to the client's own
`verifyRowLeveled` return. Refused (`VerifyError::Ladder`, onyx-check code `ladder`): another
value, a top that is not the stated level, no own step, a step stronger than the own step, a
dropped or rewritten step. Row ladder `at` is the server's `finalizedUnix` hint, not bound.

### Level-2 limits
Every level-2 answer (`/v1/observations/{metric}`, with or without `outcome=all`, its 402 body
and the `PAYMENT-REQUIRED` offer, and every `/attempts` answer) carries `limits`:
two known limits of level 2, stated in THREATS.md, that no client check removes today (ROADMAP
B4; fix planned in C4). Shard rows (levels 1 and 3) carry none. Shape, stable ids, in this order:
`[{id: "REVLOG-1", applies, text, docs}, {id: "REVLOG-5", applies, text, docs}]`, `docs` =
`docs/spec/delivery.md#level-2-limits` (`observations::limits_json`). `applies` says whether
the limit holds for the rounds of this answer.
- REVLOG-1 (`applies` always true): the coordinator sees each reveal's value before it signs the
  receipt, so it can refuse receipts for values it dislikes and drop those reveals (or drop
  observers that never posted their commit), steering the value or forcing a re-draw. Bounded by
  the fact's draw budget (3 draws, 3 more per registry-owner reopen, ADR-0027); not provable to
  third parties.
- REVLOG-5 (`applies` true only when the answer serves rounds sealed: a free `/attempts` answer
  of a fact not closed): while a fact is not closed its rounds are served sealed; the `revealed`
  list stands in for the reveals and is the server's word, so a withheld receipted reveal can be
  listed as revealed with outcome `noResult` and `transcriptComplete` still passes. It is caught
  only once the fact closes and its rounds are served with their reveals (a value answer with
  `outcome=all`, or `/attempts` of a closed fact) and checked with `transcriptComplete`.
`client::verify_observation` and `client::verify_attempts` accept an answer without `limits`
(a server from before B4) and refuse one whose `limits` is not exactly this list for how its
rounds were served (one dropped, a changed text, REVLOG-5 said not to apply to a sealed round).
Clients from before B4 refuse the extra key (fail closed) and must update, as with
`transcriptComplete` (announced in docs/ops/upgrades.md §API answer compatibility). Texts change
only with the THREATS rows; a changed text is a new client.

**Not authenticated.** No signature, Merkle proof or on-chain value covers `limits`: the server,
or anyone between it and the buyer, can strip it (the client accepts its absence) or change it
(on an answer the client refuses any list but its own constants, so a changed list is a refused
answer, never a softer statement; the copies in the 402 body and offer are not checked by any
client, so before paying they are the server's word). It is advisory text for people. No safety decision may read `applies`: the
client takes the limits from its own constants and its checks from the rounds it verifies, never
from this field.

**In the x402 offer.** x402 v2 §5.1.2 ([core spec](https://github.com/x402-foundation/x402/blob/main/specs/x402-specification-v2.md))
makes `extensions` a map of extension id to `{info, schema}`, `schema` a JSON Schema of `info`
(the spec's [bazaar extension](https://github.com/x402-foundation/x402/blob/main/specs/extensions/bazaar.md)
uses draft 2020-12). The offer carries `extensions.limits = {info: {limits: [...]}, schema}`
(`observations::limits_extension`). The spec says clients echo extensions in their
`PaymentPayload`; the server ignores an echoed copy and serves the payment as without it. The
spec sets no naming rule for extension ids; `limits` is ours, not a registered extension.

### Verification (no trust in the server)
**Step 5 is the only check against a lying operator node, and it is on by default** (IDX-R5-3;
M7 loop 3, ADR-0023 Amendment 2). The server reads each root on chain before answering
(`ChainRoots`), but through the operator's own RPC node, the same one its indexer uses; a node
that lies can make the server serve a wrong or unfinal root. Only a client that reads the root
on its own node catches that.
- Default: `ClientNode::verify(body, expect)` reads `getFinalizedRoot(round, shard)` on the
  client's own node (its chain id checked first, block tag `finalized`) and runs every step:
  `chain_checked: true`. A root not final there is refused (`NotFinalOnChain`), another root,
  binding, height, chain or contract is `ChainMismatch`, a node that cannot be read is
  `ChainUnavailable` (nothing decided). `NotFinalOnChain` can be transient (M7L3-R1-3): the
  client's node may lag the operator's, or the root may have been finalized moments ago (the
  `finalized` tag trails the head by minutes on Base). Retry later on the same node; a root
  that never becomes final there is not verified.
- `verify_response(body, expect, chain)` does the same with an answer the client read itself;
  `chain = None` is refused (`NotFinalOnChain`), after the response's own checks.
- Explicit opt-out: `verify_response_unchecked(body, expect)` checks everything else and
  returns `chain_checked: false` (its text starts `chain_checked=false (NOT VERIFIED ON
  CHAIN`). Treat such a result as unverified.
- Backward compatibility: before M7 loop 3, `verify_response(body, expect, None)` passed with
  `chain_checked: false`. That call now fails closed; callers that meant to skip the chain read
  switch to `verify_response_unchecked`. Calls that passed an answer are unchanged.

A client that trusts only its own node checks a response like this (`verify_response` does
steps 1 to 4, 6 and 7 and compares step 5's answer; `ClientNode::verify` also makes the step 5
`eth_call`):
1. Recompute the shard id from `bindingInputs` and `shardInputs`
   (`keccak("onyx-shard-v1" ‖ shardFieldsHash ‖ paramsHash ‖ registryRoot)`, ADR-0022); it must
   equal `shard`.
2. Recompute the binding (`keccak("onyx-commitment-v1" ‖ root ‖ leafCount u64 BE ‖ recipeId ‖
   runtimeHash ‖ labelSetHash ‖ sourceHash ‖ schemaId ‖ rightsHash)`); it must equal `binding`.
3. Check the data: the row's proof verifies against `root` with `leafCount` leaves, or the export
   rows rebuild `root` and `leafCount`.
4. The outer `round`, `shard` and `root` equal the bundle's.
5. Call `getFinalizedRoot(round, shard)` on the client's own node, at block tag `finalized`, on
   the chain and contract the client expects (never the ones the response names). The returned
   `(root, binding, finalizedAt)` must equal the response's `root`, recomputed binding and
   `finalizedAt`. A zero root means not finalized. If the bundle names a chain or contract, they
   must be the client's.
6. The answer is to the question asked (`Expect`, M7-L39-F2). Proof: `round`, `shard` and
   `index` as asked, and the outer `index` equals the proof's. Export: `round` and `shard` as
   asked. Metric: `metric` and `subject` as asked, the row decodes (`OutputRow`) with
   `key` = subject, and `min_as_of` <= `asOf` <= the asked `as_of`. Settlement: `market` as asked, `subject` is
   the market's (from the market contract), the row's key is it, and `value` equals the row's
   value. A valid proof of some other row is refused.
7. The shard is the one the client asked for (M7-L40-F1). Metric: the client knows the
   metric's definition (`MetricDef`: source id, recipe, schema, params hash, optional label
   set); the bundle's inputs must equal it, so a row of another metric's shard is refused.
   `sourceRef` must hash to `sourceHash`; `asOf` must equal the end of its snapshot (block
   height, range end or publish time), and that must be <= the asked `as_of` and >= the client's
   `min_as_of` (its freshness floor, PLAN q14 a). Settlement: the
   client knows the market's `round` and `shard` (from the market contract) and both must
   match. `rights` must hash to `rightsHash`; `license` and `attribution` must equal it.
   `leafCount`, `finalizedAt`, `recipeId` and `labelSet` must equal the bundle.

Completeness by construction: `client::BOUND_FIELDS` lists every field `verify_response` checks;
`client::INFORMATIONAL_FIELDS` lists the rest (`metric`, `market`, `tx`, `verifyCall`, `ledger`,
`call`, `finalizedUnix`), which a client must not rely on. A test fails if a response grows a
field that is in neither list. `metric` and `market` are names the client already chose; the
check uses the client's own name and definition, not the echo.
What one response cannot prove (PLAN q14 a, M7-L41-F1): that a metric answer comes from the
*newest* finalized shard, or that a subject has no row. A server can answer "latest" with an
older finalized shard that is still a true, verifiable row. The client therefore sets its own
freshness floor (`Expect::Metric.min_as_of`, required) and refuses any shard whose snapshot ends
before it; staleness within the floor is accepted as the client's choice. A `404 unknown_subject`
is the server's word, not a proof of absence. Anything that settles money (a market, a contract
payout) must not use the metric endpoint: it pins `(round, shard)` like `/v1/settlement`, so the
answer is exactly one finalized shard.
A metric answer is served only from a shard whose price at serve time equals the quote (M7-L48-F1);
a row only in a shard at another price is a 404 there, reached with that shard's `as_of`.

Then the row is the one the network finalized, produced by the named recipe, runtime and label set
from the named source under the named rights. For `ledger: off-chain` (demo, M4/M5 model) step 5
is not available and the client is trusting the operator's ledger.

### Error codes
Body: `{"error":{"code","message"}}`. Codes are stable; messages are not. The machine-readable list is
`x-onyx-error-codes` in [docs/api/openapi.json](../api/openapi.json) (OpenAPI 3.1, every route;
`scripts/check-openapi.py` checks it, this table and the Caddy matchers against the code).

| Status | Codes |
|---|---|
| 400 | the proposal validation codes of `/v1/submit` and `/v1/submit/check` (`bad_json`, `empty_field`, `field_too_long`, ...: `x-onyx-error-codes` in docs/api/openapi.json lists them), `bad_request` (also every path or query rejection, unknown query parameters and bad metric or market names, and any `HEAD` carrying `ONYX-CONTRIBUTOR`, ADR-0025), `invalid_payment` (also any `HEAD` carrying `PAYMENT-SIGNATURE`: refused before verify or settle, M7-L50-F2), `prepaid_not_offered` (an `ONYX-PREPAID-VOUCHER` header on a server that takes x402 only), `invalid_prepaid` (a voucher header that does not parse), `bounty_too_specific` and `bad_want_value` (`/v1/wants`: anything but four closed-list values, north-star loop NL9) |
| 402 | `payment_required` (with `PAYMENT-REQUIRED`), `payment_failed` (with `PAYMENT-RESPONSE`), `observe_payment_required` (no round settled this level-2 subject yet; `observe` offers one for a prepaid hold, ADR-0049), `prepaid_refused` (a prepaid voucher that is not the buyer's, not exactly the last total plus the price, over the deposit or during an exit; `last` and `price` beside `error`; ADR-0046 amendment 1) |
| 403 | `test_labels`, `invalid_contributor_proof`, `not_a_contributor` (ADR-0025), `serving_check_unavailable`, `serving_check_failed`, `not_cross_checked` (serving checks, ADR-0034 Amendment 1; all before any 402), `label_rights_withheld` (`ONYX_RIGHTS=fallback`: a label-derived shard or track record, ROADMAP L14, C6), `track_log_priced` (`/v1/track/log`: a resolution at level 1 or 2), `track_registry_names_addresses` (`/v1/track/registry`), `bad_want_signature` and `no_prepaid_account` (`/v1/wants`: a want must be signed for this week by an open prepaid deposit) |
| 404 | `not_found`, `unknown_ticket` (`/v1/tickets`: no such paid observation ticket, ADR-0049), `unknown_shard`, `unknown_metric`, `no_serving_check` (`/v1/served`: no serving check for the metric here), `no_shard_at_as_of` (`/v1/metrics`: the metric exists, but every shard is newer than `as_of`; the message names the oldest `as_of`, THREATS SOAK-5; a metric whose oldest shard this server refuses answers that 403 or 451 instead), `unknown_subject`, `unknown_market`, `row_not_found`, `not_finalized`, `too_old` (observations: no settled round at or after `min_window_end`), `no_track_record` (`/v1/track`: with `reason` `none_published`, `no_rule` or `no_record`), `no_signed_report` (`/v1/signed`), `unknown_label_set`, `no_label_row` (`/v1/labels`), `no_track_registry` (`/v1/track/registry`: a version-1 publication) |
| 405 | `method_not_allowed` (`Allow` names the route's own methods: `GET,HEAD`, or `POST` on the intake routes) |
| 409 | `level3_not_servable` (`/v1/signed`, with `reason`, C10), `unknown_commitment` (`/v1/track`: `since` names no commitment of the server's chain), `commitment_moved` (`/v1/track` pages: `at` is not the latest), `root_mismatch`, `settled_value` (`/v1/observations/{metric}/attempts`: the fact settled a value; ask with `outcome=all`), `proof_level_not_accepted` (`/v1/settlement`: the market's shard is a level its policy refuses), `payment_in_use` (this authorization already has an open settle for another request; this request adds no charge, OPS-L4-F4; the identical retry of an unknown settle is checked instead, M7L3-R1-1, or with self-settle gets 503 `payment_pending`, SOAK-1) |
| 413 | `export_too_large`, `track_chain_too_long`, `track_log_too_large`, `body_too_large` (`/v1/submit*`: over 64 KiB) |
| 422 | `refused` (`/v1/submit`: the route refuses the proposal; the body carries `route`) |
| 429 | `rate_limited`, `payer_limited`, `daily_quota` (`/v1/submit*`: the client's or the server's daily quota), `payer_busy` (all with `Retry-After`; `payer_busy`: the payer already has 4 unsettled payments waiting for the resolver, retry after 60 s; settles in flight do not count, OPS-L4-F5), `too_many_wants` (`/v1/wants`: 16 live wants per account) |
| 451 | `restricted_data`, `forbidden_data` |
| 500 | `rows_mismatch`, `binding_mismatch`, `catalog_mismatch`, `internal` |
| 503 | `root_source_unavailable`, `rows_unavailable`, `facilitator_unavailable`, `facilitator_busy` (with `Retry-After`), `contributor_checks_busy` (with `Retry-After`, ADR-0025 REV-L2-F2), `settle_intent_unavailable`, `receipt_log_unavailable` (a receipt or facilitator state write failed or the log is full: new payments refused before verify, nothing charged, REV-L3-F5), `clock_skew` (with `Retry-After`: the server's clock is more than 300 s off the chain's, OPS-L4-F3), `serving_check_pending` (with `Retry-After`: the serving check has not judged the shard's current root, ADR-0034 Amendment 1), `level3_chain_unavailable`, `level3_store_unreadable`, `level3_posts_delayed`, `level3_clock_skew` (`/v1/signed`, C10), `track_store_unreadable` (`/v1/track*`), `intake_unavailable`, `intake_full`, `intake_busy` (`/v1/submit*`: the journal cannot be used, is at its cap, or has 16 writes in flight), `prepaid_unavailable` (with `Retry-After`: the buyer's account cannot be read or the prepaid usage journal cannot be written; nothing charged), `payment_pending` (with `Retry-After`: self-settle only; the identical retry of a paid request whose settle ended unknown; retry the same request, served once the transfer into OnyxRevenue is proven, nothing more charged; SOAK-1), `overloaded`, `wants_closed` (`/v1/wants` on a node without a prepaid contract), `chain_unavailable` (the prepaid account could not be read), `journal_unavailable` (a want could not be kept) |
| 504 | `timeout` |

## Rules
- Every row served comes with its proof, the finalized root and the `verify` bundle.
- The root comes from the root source only (on chain: the `finalized` block). Rows that do not
  hash to it, or whose binding differs from the ledger's, are never served.
- Path parameters have one spelling: decimal integers without sign or leading zeros, hashes as
  `0x` and 64 lowercase hex digits. Other valid spellings get 308 to the canonical URL; invalid
  ones 400. `?root=` must already be canonical. The query has one spelling too: known
  parameters only (others 400), in the order of the route's definition (`subject`, `as_of`, `min_as_of`),
  values percent-encoded except `A-Z a-z 0-9 - . _ ~ :`. Other spellings get 308. The payment
  `resource` is this canonical URL (M7-L39-F6).
- Cache policy: a URL with `?root=` equal to the finalized root is content-addressed and
  `max-age=31536000, immutable`; a different `?root=` is 409. Every other success is `no-cache`
  with an ETag (revalidate). Paid answers are `private`, free ones `public`. Errors, 402 and
  `/v1/health` are `no-store`.
- Order of work: parse, catalog, admission (rights, test labels, catalog consistency), root
  (cached), price, 402, payment checks, facilitator verify, rows (cached by root), body, settle,
  respond. An unpaid request reads no row. Paid metrics price from the newest finalized
  candidate shard without looking at the subject, never answer 304 before payment, and look the
  subject up after payment; an unknown subject is then 404 and nothing is settled (M7-L39-F3).
  The row found may be in an older shard; the price is the one quoted.
- Rights are checked at serve time: `Restricted` and `Forbidden` sources answer 451.
- Shards pinned to a `test.*` label set are refused (403) unless the server runs with
  `allow_test_labels` (dev and demo only).
- Payments: the client's `accepted` requirements must equal ours exactly, `resource.url` must be
  this exact URL, and the EIP-3009 authorization must pass local checks (amount, recipient,
  validity window, a 65-byte EOA signature that recovers to `from`) before the facilitator is
  called. Smart-wallet signatures are refused for now, and with self-settle so is any payer whose
  address carries code (an ERC-1271 wallet or an EIP-7702 delegation, OPS-L5-F1). Paid data is served only after the
  facilitator settles. The settle runs in its own task: if the request deadline passes first,
  the settle goes on and its answer is stored. An identical retry (same header, same URL, same
  client: IPv4 address or IPv6 /64) within 10 minutes gets the stored response, at most 3 times;
  a retry during the settle waits for it (M7-L39-F1, F5). One request owns a payment at a time:
  the claim is atomic, so identical concurrent requests verify and settle once and the others
  wait (M7-L40-F4). The claim is a drop guard: any end of the request or the settle task,
  including a panic, releases it and wakes the waiters (M7-L40-F5). Caveat: replays are keyed
  by client address, so a client whose address changes (mobile, NAT, a new /64) before its
  retry gets a 402, not the stored answer; the payment is already spent. The store holds
  at most `settled_entries` responses (default 65,536) and `settled_bytes` of response bodies
  (default 256 MiB), both configurable; the least recent go first (M7-L41-F3).
- Client address: the peer, or behind a configured trusted proxy the right-most
  `X-Forwarded-For` address outside the trusted ranges (M7-L48-F2); `ip:port` and `[v6]:port`
  entries count as their address (M7-L50-F4).
- Limits: payment attempts per client (IPv4 address or IPv6 /64; token bucket, default 30 per
  minute; at most 65,536 clients tracked, least recent dropped). An IPv6 /64 also draws from its
  /56 (4 times the budget) and its /48 (16 times); an attempt takes from all three or from none,
  so rotating /64s inside one allocation runs out (M7-L40-F2). Per payer (`from` of the
  authorization): payments verified but never settled (a 404 after verify, a refused settle, an
  abandoned request) are counted, default 20 per hour; when the budget is empty the payer gets
  429 `payer_limited` before the facilitator is called (M7-L40-F2, F3). The unit is taken
  atomically when the payment is accepted, so concurrent requests cannot spend past the budget
  (M7-L41-F5). It is given back when the payment settles, when the facilitator refuses it at
  verify (a replayed, already settled header; an unfunded key), when the facilitator is busy,
  and when verify returns a facilitator error. It stays spent on a 404 after verify, a refused
  settle, a settle that times out or returns a facilitator error (the outcome is unknown: the
  transfer may have been sent; nothing is stored as a success and the retry re-verifies; with
  the settle check below, an unknown outcome is first read from the token), and
  an abandoned request (M7-L42-F2, M7-L47-F3, which supersedes M7-L42-F1's refund at settle). A
  facilitator answer with a non-2xx status counts only as a refusal; an approval under an error
  status is an outage (503). A payer back at its full budget is not stored, so keys refused at verify do
  not hold slots (M7-L42-F3). Sybil limit: the
  budget is per `from` key, and keys are free; a payer with many fresh keys (a key costs nothing to
  make; the facilitator may also require a balance) is capped by the per-client attempt
  buckets and the facilitator slots, not by this budget. Also: requests in flight (default 256,
  then 503), facilitator calls in flight (default 32, then 503 `facilitator_busy`), a
  whole-request deadline (default 12 s, 504) and a facilitator deadline (default 4 s, 503; a
  settle may take 4 times that). The config refuses a request deadline under 2.5 times the
  facilitator deadline. Roots are cached 30 s; row batches by root (256 entries, 256 MiB), each
  with a key index.
- Settle check (M7-F5 residual, ADR-0023 Amendment 2; `ApiState::with_settle_check`, on in
  `onyx-node` facilitator mode): after a facilitator settle whose outcome is unknown, the
  server reads the token's `authorizationState(from, nonce)` at the latest block. Used and its
  logs show the exact value moved into `OnyxRevenue` (`AuthorizationUsed` then `Transfer`, one
  successful transaction): served as settled (receipt written, `PAYMENT-RESPONSE` names that
  transaction). Used otherwise (paid someone else, cancelled): 402 `payment_failed`, nothing
  served. Unused: the same signed authorization is sent once more (its nonce moves money at
  most once; no re-send when `validBefore` would not outlive another settle), then checked
  once more, about one block later (`ApiConfig::settle_recheck_wait`, default 2 s), if that
  does not succeed. A re-send the token refused as used (`invalid_transaction_state`) means the
  first settle landed: that check reads the token's logs even when a lagging node still reads
  the authorization unused (M7L3-R1-1). Each check reads again while a use is not found or the
  node fails, within its budget. Undecided (a node error, a use not found yet, still unused):
  503 as before, the intent left to the resolver. The identical retry (same header, resource
  and client) gets the stored answer of a settle that completed after its request gave up; while
  its intent is still open it runs this check again instead of getting 409, with no new verify
  or settle, and is served once the transfer is proven (it takes no new payer unit; one
  authorization is re-sent at most 3 times in all, `intents::MAX_RESENDS`). Once the resolver
  (every 30 s by default) has recovered the intent, the identical retry is served from the
  resolver's proof (the paying transaction in `PAYMENT-RESPONSE`, no verify, settle or check)
  for 10 minutes after the recovery, once; further identical retries replay that answer (3
  times, as for any settled payment). The request key and the proof are kept in the intent
  journal, so a restart keeps both (M7L3-R2-1); the proof is deleted from the journal before its
  answer leaves, so a restart does not serve it again (M7L3-R3-2). A request served though its
  receipt could not be written leaves its intent to the resolver without the request key: no
  retry is served from it after a restart (M7L3-R3-4). The retry and proof paths check the
  payment against the requirements offered now: after a price change the retry gets 402 with
  the new terms, not the data at the old price (M7L3-R3-5). Never served without a facilitator
  success or a transfer of the exact value into `OnyxRevenue` in the token's logs. Charges: one
  signed authorization moves money at most once (its EIP-3009 nonce), so the server never
  charges it twice. With a lagging node, a payer whose payment landed may get 503 first; the
  data comes on an identical retry once the transfer is proven, within those 10 minutes. A
  retry later than that, or after the cached replays are spent or lost (memory only), takes
  the fresh path: a real facilitator answers 402 on the used nonce, though the receipt is
  written. A payer who signs a new authorization instead pays again.
- A facilitator outage serves nothing (503), never free data. A catalog that cannot be read is a
  500, never free data.

### Indexer (ADR-0033)
With the chain indexer on (`onyx-node`: `ONYX_INDEXER=on`; `onyx_api::indexer`), the catalog's
shards are discovered from `OnyxSettlement` events, not listed by the operator. The catalog file
names only what the operator holds (`held`: spec, rights, row count, end header; no round).
- I-1 Scan: logs of the configured settlement contract with topic
  `Finalized(uint64,bytes32,bytes32,bytes32)`, from the manifest's `deployBlock` up to the number of
  the node's `finalized` block in that poll's answer, in ranges of at most `span` blocks (default
  200). A range the node refuses is split in halves down to one block; a block the node will not
  answer whole is read for the held shards only (header first, then the held shard ids as a
  topic-2 filter, 64 per call) and counted (`partialBlocks`); if that fails too the poll ends.
  The indexer's transport reads answers up to 16 MiB. Each poll reads the last `overlap` blocks
  (default 100) again. The manifest's `deployBlock` is checked at start: no code at
  `deployBlock - 1`, code at `deployBlock` (IDX-R1-4); node errors are retried, and the error
  names the cause (node failing, head below `deployBlock`, no old state, mismatch; IDX-R2-2).
  An answer holding more than 262,144 JSON values is refused while read (IDX-R2-3). A log must come from
  the contract, carry exactly that topic and two indexed words, 64 data bytes, a block inside the
  asked range, and not be `removed`; otherwise the range is an error.
- I-2 Cursor: the next block and every event are kept in a state file bound to (chain id,
  contract, deploy block), written atomically once per poll (and every 50 ranges of a long scan)
  on a blocking thread; a restart resumes there. An older `finalized` head (lagging backend)
  never moves the cursor back. Two different logs at one (block, log index) are both kept and
  counted (`conflicts`); the contract view in I-3 decides (IDX-R1-1). A position keeps at most
  4: at a full one a newcomer is refused (`candidatesDropped`), changing nothing, when its
  (round, shard) is already named or not held, and otherwise evicts (`candidatesEvicted`) a log
  whose shard is not held, then one whose (round, shard) another kept log names; never, on a
  scan's read, the last log naming a held (round, shard), whatever its chain status reads said
  (ADR-0033 Amendment 4, IDX-R4-1). Judging (I-3) runs even when the scan failed, and before the
  scan while any log is unjudged; a failed judging `eth_call` is sent once more. A poll reads
  at most `max_jump` blocks (`ONYX_INDEX_MAX_JUMP`, default 100) past the `finalized` head this
  poll and the previous one both reported (or past the cursor), so one lying head far ahead
  moves the cursor that far at most and the overlap reads those blocks again (IDX-R5-5).
- I-2a Held-shard rescan (ADR-0033 Amendments 4 and 5): from the third poll after every start
  and then `ONYX_INDEX_RESCAN_POLLS` polls (default 240; 0: after a start only) or
  `ONYX_INDEX_RESCAN_SECS` after the previous one ended, `eth_getLogs` from the deploy block to
  the cursor with the held shard ids as a topic-2 filter (64 per call), in the scan's capped
  ranges and split, at most `ONYX_INDEX_RESCAN_CALLS` calls per poll (default 100, apart from
  the scan's budget; continued on the next poll, IDX-R5-4). Found logs are candidates; one may
  replace a held log at its position that the same answer did not return and whose end two
  polls confirmed. A rescan reads every confirmed end again, drops cached views of shards not
  served, and reads every served shard's final view again: two reads on two polls naming the
  same other view (or none) take it out of the catalog and judge it from the start (IDX-R5-2).
  A read at a `finalized` head below the one the shard was admitted at (a lagging backend) is
  no refutation; it is read again on a later poll (IDX-R6-1). A rescan in progress is kept in
  the state file (`rescan: {next, to}`), so the rescan after a restart resumes where it
  stopped instead of the deploy block (IDX-R6-2). Gates are judged again on every poll that
  admits or removes a shard (RESAPI-R2-2). `ONYX_INDEX_MAX_JUMP` must be at most
  `ONYX_INDEX_OVERLAP` (refused at start, IDX-R6-3).
  `rescans`, `rescanning`, `removed` on `/v1/health`.
- I-3 Servable: a (round, shard) enters the catalog only when (a) a kept log names it and the
  operator holds a spec with that shard id, (b) `getFinalizedRoot(round, shard)` read at this
  poll's `finalized` block (never a remembered higher one, IDX-R5-1; a key whose logs are all
  above it waits) returns a non-zero root and binding (no event has to name them;
  the contract decides, IDX-R2-1), and `shardState` reads Finalized or two reads on two polls
  name that view (IDX-R4-2), (c) the held rows hash to that
  root, their count equals any stated `leafCount`, and the spec's binding inputs with that count
  give that binding, (d) a pinned label set that is not `test.*` is held (name, version and
  content hash) and passes `LabelSet::check_servable` against the store (every label cites
  evidence; `attestation` evidence resolves to a stored attestation, other kinds to a stored
  blob); a `test.*` set only with `allow_test_labels`, as before, and (e) the metric is not a
  level-2 metric. With a registry pinned, the shard is then floored at its attested level.
- I-4 Known but not served: every (round, shard) seen is listed on `/v1/health` `indexer`
  (`servable`, `pending`, `not_held`, `unchecked`, or a refusal code:
  `rows_missing`, `leaf_count_mismatch`, `binding_mismatch`, `label_set_missing`,
  `evidence_missing`, `test_labels`, `metric_conflict`, or an end: `no_valid_root`,
  `unresolved`, `voided`, `empty`, `not_on_chain`), with its events and how many are
  superseded (root not the contract's). A held shard that is not final is checked with
  `shardState`: a case that ended `NoValidRoot` or `Unresolved`, a voided or empty shard, or
  one the contract never opened (a forged event; read again if a new event naming it is kept)
  is ended (IDX-R1-3), and not polled again once a read on a later poll also says ended, with
  no live read between (IDX-R3-2). Pending and refused shards are judged again every poll; a
  final view is cached once a read on a later poll names the same one (or it was admitted).
  No single answer is final (ADR-0033 Amendment 3). A lying operator node can delay serving
  until it answers honestly and a rescan (I-2a) or restart runs, and can make a wrong or unfinal
  root be listed and served (I-5 reads through the same node); clients catch that only by
  checking roots on their own nodes ("Verification" step 5; ADR-0033 Amendment 5). `ONYX_INDEX_HEALTH=summary` shows counts
  only (IDX-R1-5).
- I-5 Serving is unchanged: the API still reads the root through `ChainRoots` at `finalized` and
  checks the rows and binding before every answer; the indexer only decides what is listed.
  `ChainRoots` uses the same `ONYX_RPC_URL` as the indexer, so it guards against an indexer bug,
  not against a lying node (IDX-R5-3).

### Serving checks (ADR-0034 Amendment 1)
A metric whose rows a rerun cannot fully check is served only through a serving check
(`onyx_api::gate`): `btc.entity-reserve` (the base sits in the params, and miner and validators
agree on any base), gate `btc-entity-reserve-v1` (`onyx-reserve-serve`, run by `onyx-node-btc`).
- G-1 Which shards: a shard whose catalog entry names a check (`servingCheck`, set for every
  reserve shard) or whose metric has a registered gate. A server without that gate refuses the
  shard on every endpoint (403 `serving_check_unavailable`): fail closed.
- G-2 What is judged: after each indexer poll that admitted a shard (and every 20 polls), and at
  start, every finalized shard of the metric this server holds: its binding inputs, the source
  snapshot's heights, its params (from the row store, by the params hash its id commits to; the
  node stores every catalog spec's params there at start) and the leaves that hash to the root
  the chain finalized. The answer is kept per (round, shard) and root; a shard whose current
  root was not judged is 503 `serving_check_pending`.
- G-3 The reserve check (`reserve::serving_check`), non-circular: from the pinned start height,
  finalized ranges must tile the blocks up to the committed scan (no gap, no overlap, one range
  per start); the start base is the scan's entity totals minus the finalized rows' net flows
  (received and sent do not depend on the base; every shard of a range must agree on them); each
  shard is served only if its params are exactly the expected ones (start: that derived base and
  `base_ref` = the scan's hash; then the previous served shard's end rows, last height and root),
  and `check_params_chain` passes over the selected shards from the derived start; the chain must
  land on the scan's totals. A chain that does not reach the scan serves nothing. A shard on any
  other base is 403 `serving_check_failed` on every endpoint; `/v1/metrics` skips it.
- G-4 Rows (THREATS RES-1): a row is served only if its entity's base is cross-checked
  independently: a committed record for this scan and label set (`fixtures/reserves/
  cross-checks.json`: OKX against its proof-of-reserves file, BITB against the issuer's wallet
  list; every compared address exact), or an operator-configured second-node re-scan that
  reconciles at every address of the entity. Other rows are 403 `not_cross_checked` before any
  402 (which entities are cross-checked is public policy). An export of a shard with such a row
  is refused.
- G-5 Answers: a served row carries `servingCheck: {gate, crossChecked: true, basis, source}` (an
  export: `{gate, rows: [...]}`); the client reads it into `Verified.serving` and
  `Verified::require_cross_checked` refuses an answer without it. It is the operator's claim, not
  bound to the root: a client that does not trust the operator re-runs the check (G-6).
- G-6 The client's reserve check (ROADMAP D3 and D22, THREATS RES-10, ADR-0034 Amendments 2
  and 3), with no operator input trusted: (1) the client pins a committed scan itself
  (`reserve::ReservePin::from_scan` over the scan file and undated label set it holds, or
  `from_dated_scan` with the dated set, its history and the flows file; the scan's and the flows
  file's hashes must be ones it trusts, e.g. the website's `RESERVES_SNAPSHOT`); any later scan it
  trusts will do; (2) reads `/v1/served/btc.entity-reserve` (a hint, checked for shape before any
  paid fetch: consecutive ranges, one ending at the scan's height, a range listed twice only with
  one root, kept once; no cap on its length) and fetches the entity's row only in the links of the
  span from the answer to its anchor (`onyx_reserve_serve::client::link_requests`: walking toward
  the landing, stopping there or before a range where the entity has no address; every range on
  the walk one window of the pinned set, the leaf index the entity's in that window; refused before
  any fetch above the client's budget, and so are a pin not on the client's headers with six
  confirmations, an answer older than `min_as_of` or not yet six deep, and a list longer than the
  client parses: `Limits`); (3) verifies each answer on its own node
  (`onyx_api::client::ClientNode::verify_row`: root final, shard id and binding recomputed, row
  under the root); (4) `check_reserve_answer` builds each link from the binding alone (recipe,
  label set, schema, source snapshot range and tip) and runs `reserve::client_check` against the
  client's own header chain: the pin's scan and flows file on it with six confirmations; every
  link on the pinned recipe and reserve schema, bound to the pinned set's window for its blocks,
  not past the set's publication, its tip the header chain's block with six confirmations, the
  landing on the scan's block; then `reserve::derive_entity_span`: the span's anchor (the landing,
  whose balance is the scan's total; or, for a dated set, the entity's exit or entry, from the scan
  and the flows file) fixes the first base, and each row's copied params (`base_sats`,
  `base_height`, `base_ref`) are the derived base (first), then the previous link's end plus the
  entity's membership-change delta and the previous root; balances add up. The answer checked
  must hold a link's row (same root, index and row bytes; the same root is the same rows, so the
  same work finalized in another round is the same answer) and end at or after the client's
  `min_as_of` (the operator chooses how far it lists, so a list cut after an old link is otherwise
  a stale but true answer).
  Not checkable by the client: that the scan saw every output (RES-1), and the flows file's
  per-address split at a membership change (RES-13: its word, under a hash the client trusts).
  Cost: one paid row per link from the answer to its anchor, `|answer - landing| + 1`, at most the
  client's budget (`DEFAULT_SPAN_BUDGET` 4,096 rows, 10.24 USDC at 2,500 micro-USDC); a newer pin
  shortens it. `min_as_of` is a Bitcoin height the client takes from its own headers, never the
  server's clock. The browser runs the same `reserve::derive_entity_span` on the week export
  (`apps/web/verify` `reserves_check`; its tips against headers are D12).

## Assumptions
- Clients verify proofs and bundles themselves; the API is not trusted for correctness.
- Time-locked sealed reveals (ROADMAP C51): drand quicknet's threshold of League of Entropy
  nodes does not sign a round early, and the chain's block time does not lag the wall clock by
  more than 60 s across a round's reveal close (THREATS C12-6, the rule is ROADMAP C53).
- Serving checks: the operator runs the check honestly (it holds the committed scan and the
  finalized rows); the check is an operator-side filter, so a client relying on it either trusts
  the operator or re-runs it (G-6, which trusts only the client's own node and its own scan pin).
  A second-node re-scan is the operator's statement that it came from another node.
- Indexer: the node's `finalized` block is final (Base: L1 batch finality, about 20 minutes); a
  shard id finalizes at most once on chain (`everFinalized`), so a held spec needs no round; the
  state file is bound to (chain id, contract, deploy block), which a fresh anvil with the same
  deployer can repeat: use a new state file per local chain.
- A finalized root never changes; only root-pinned URLs are cached as immutable.
- The facilitator is trusted to tell the truth about verify and settle (it holds no data).
  After an unknown settle the token, read through the operator's node, decides (settle check);
  a payment proven at the latest block is served like a facilitator success (both are undone
  only by a reorg of that block).
- The client's own node is honest about `getFinalizedRoot` at its `finalized` block (the
  default client check trusts it, and nothing else).
- Behind a reverse proxy, client addresses come from `X-Forwarded-For` only when the peer is in
  the configured trusted ranges (M7-L48-F2); otherwise every client shares the proxy's address.
- Track records: a client that reads the commitment log in `OnyxSignedBook` (`trackHead`, ADR-0036
  amendment 3) accepts only its latest; one that cannot (the browser today, C41) has the server's
  word for it, bounded by its own floor and the commitment it saw last (`since`; THREATS
  TR-R2-5). A record checks against its commitment; that each outcome is right needs a rerun of
  `compute` on the served logs (`GET /v1/track/log`) with the registry the commitment names
  (served since C23 at `GET /v1/track/registry`, used only once it rebuilds to `registry_root`
  with the client's own address rules); logs holding priced level-1/2 answers are not served (C22).

## Open questions
- Indexer: confirm a range's completeness against the backend that answered (a JSON-RPC batch
  with the `to` block's header), instead of the overlap re-read?
- Track records: priced level-1/2 answers in served logs (ROADMAP C22, the lead's decision); a
  live node URL for the website's track section (C24, needs LC-8); chain-mode full walks
  (`chain::verify_chain`) over chained logs (C41).
- Response encoding for bulk clients: JSON with hex rows now; raw Borsh with a JSON envelope later?
- Streaming export above the cap (now: 413, use `/v1/proofs`).
- `Restricted` data with a paid license: serve with terms, or never (now: never)?
- A settle that times out after landing on chain: decided M7 loop 3, serve on proof of
  transfer (settle check, ADR-0023 Amendment 2).
