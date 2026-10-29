# onyx-source-fiscaldata: a real level-2 source

Proof ladder level 2 ("re-observed", ADR-0027) on real open data: the U.S. Treasury's
Debt to the Penny record for one date, from the Fiscal Data API. Rights review in
`fixtures/PROVENANCE.md` (the site's terms allow copying and redistribution; no account).

- `template()`: the metric's one `FeedTemplate` (L2SRC-R2-F1). The coordinator registers it
  (`ObservationResults::register_feed`, a recorded reviewer action) and opens, settles and
  accepts rounds of this metric only as this template instantiated for a dated subject. The
  record date must be a U.S. federal business day (5 U.S.C. 6103 holidays, observed days,
  2026-2035), with at least one business day before the window's UTC day and at most 10
  calendar days (L2SRC-R2-F4).
- `FiscalDataFetcher` (`onyx-fetcher:fiscaldata-record:v1`): reads only that canonical feed
  (`spec(record_date, window_start)`); the response must hold exactly that one record; the value
  must be dollars and cents (exactly two decimals, no sign, no leading zero). Anything else is
  `Unavailable`.
- The record date is part of the fact: subject `us.treasury.federal_debt:YYYY-MM-DD`
  (`registry(record_date)` registers it).
- Feature `round`: `round::store()` is a store with the template registered; `round::run` opens a round there, five observers commit and reveal what they
  fetched, the round settles in an `ObservationResults` store, and the one claim it backs goes
  through `accept_claim` (level 2).
- Recorded rehearsal: `scripts/level2-rehearsal.sh` runs it on a local anvil: the draw is
  announced on `OnyxRegistry` before its beacon block, the round book saved, the beacon fixed from
  the block hash (L2RND-R1); it writes `apps/web/static/level2/debt-to-penny.json` and
  `fixtures/debt-to-penny-2026-10-08/draw.json`. Replay without a chain:
  `cargo run -p onyx-source-fiscaldata --example level2-round -- --book PATH`.
- Independent observers (ROADMAP B2, ADR-0027 amendment 9): `examples/level2-observer.rs` is
  one observer process with its own 0600 key file; it reads the draw from `OnyxRegistry` and
  checks it commits to its own copy of the pool snapshot (`round::drawn_round`), fetches the
  record itself at the window start, posts its own signed commit on chain and writes its signed
  reveal to the coordinator's spool (`onyx_coordinator::observer_link`), then waits for its
  receipt on chain (exit 3 without one). `examples/level2-coordinator.rs` is the live
  coordinator (`round::Observers::Independent`, system clock, `--receipt-key` required): it
  holds no observer key. `scripts/level2-observers.sh` rehearses a round with seven observer
  processes and distinct anvil dev keys on ONE machine (about 7 minutes); true independence
  (separate hosts, networks, operators) needs real operators (THREATS L2-SRC-1).
- Sealed reveals (ROADMAP C11, ADR-0027 amendments 10 and 11): with `level2-coordinator
  --reveal-key FILE` (a 0600 key of its own, refused when it is the receipt key or another role's
  key) every draw snapshots the coordinator's reveal key; each drawn observer then posts its
  signed reveal on chain sealed to it (`postSealedReveal`, the post is the receipt) instead of
  the spool, and the coordinator reads every drawn slot past reveal close, opens or dismisses each
  post with a proof, checks `revealLogComplete` and records the round (`recordAttempt`,
  `recordObservation`) before writing it. `examples/level2-verify.rs` is the Rust client against a
  live registry. `scripts/level2-sealed.sh` rehearses two draws of one fact on ONE machine
  (about 20 minutes): five garbage posts dismissed and recorded, then five sealed reveals settled,
  then the client's checks and three doctored answers refused.
- Receipt key (ROADMAP B3, THREATS REVLOG-4): `level2-round --receipt-key FILE` signs receipts
  with the deployment's own hot key (0600 file); without it only a replay or a local anvil runs,
  on the public test key. The run refuses to start when that key is also `--coordinator`,
  `--owner`, `--reporter` or a pool member. A new key is set with `setReceiptSigner` before the
  next draw; a round of an earlier draw is finished only with the key that draw snapshotted.
  Runners that share a coordinator address share its receipt key (THREATS B2-R1-L1): two keys
  on one coordinator can swap the signer between a `setReceiptSigner` and the next draw.
- Tests: `cargo test -p onyx-source-fiscaldata` (recorded round, a lying observer outvoted;
  `tests/b2_observers.rs`: independent observers, collusion, a dropped reveal, pool checks;
  `tests/c11_sealed.rs`: sealed observers end to end and the C11 attacks).
  Live: `ONYX_LIVE_FETCH=1 cargo test -p onyx-source-fiscaldata` runs five `curl` processes
  against the API now (5 requests).

**Operator warning: the round book is durable state.** Keep the `--book` file (and the
results store, `--store`) on durable, backed-up storage, with one runner per book (`<book>.lock`
enforces it on one host). Never run a fact that has a draw in progress with a fresh `--book`
path, on a new host, or on ephemeral storage (a container without a volume): a book lost after
its round's receipts are on chain VOIDS that fact for good (every client refuses it; THREATS
REVLOG-R5-1d, REVLOG-R9-F3). A book lost before that point is handled from the chain and the
results store (its draw adopted, followed or given up; REVLOG-R8-2, REVLOG-R9-F1); restoring an older backup can stop the fact until the
latest book is back.
**Results store path:** a missing `--store` file stops the run; pass `--new-store` only on a fact's first run on a deployment (a mistyped path must never read as "nothing published"; REVLOG-R10-3). A run refuses `--new-store` once the chain shows draws of the fact and the book holds none (REVLOG-R11-3). A run refuses a store naming another chain, registry or registry deploy block (`onchain`), or naming none while it has a chain (REVLOG-R10-1, R11-1); tag a store written before that check with `--tag-store` (a mode of its own: it checks every draw the store publishes against the chain's `drawOf`, tags only on a full match, and exits; REVLOG-R11-2), then run again without it.

The recorded rehearsal simulates its observers and their pool of 9 in one process with public
test keys; the independent-observer rehearsal runs them as separate processes with their own
keys, still on one machine; in both the beacon comes from a local test chain. Each one's bytes and fetch time are a real HTTPS response.
