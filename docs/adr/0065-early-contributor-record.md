# ADR-0065: The early-contributor record (ROADMAP S26)

## Status
Accepted, 2026-11-14 (test network only). Slice 1 is built: `onyx_api::early` (the join, the
rows, the CSV and schema writer and the exclusions model; it reads no chain),
`deploy/early-record/contracts.json`, `deploy/early-record/exclusions.json` (the reviewed
list), `deploy/early-record/schema.json` and `deploy/early-record/exclusions-schema.json`. A
review the same day fixed six findings (THREATS EARLY-13 to EARLY-15 and the updated rows).
Slice 2 is built the same day: `onyx_api::early_chain` (the chain reader and the folder rules)
and the tool `crates/onyx-api/examples/early-record.rs` (runbook: docs/ops/early-record.md).
`l2_agreed` moved to slice 3 (section "Slices"). Slice 3's code is built the same day: the anvil
end-to-end script (design test 10), the daily run with `verify` on a second node and the privacy
scan as gates, its user timer, and the record's own publisher id `early-record`. A review the
same day fixed nine findings (the state hash in `day.json`, each day published before the next,
absolute paths for the scan, and six smaller ones; THREATS EARLY-4, EARLY-12, EARLY-19, EARLY-20).
Its operations are open (section "Slices"). Threats: THREATS.md "EARLY".

## Context
Early contributors may be rewarded later; this is a record, not a promise.

Today no checkable list of the test-network work that the contracts accepted exists. A list made
later from memory cannot be checked. A list kept by hand can be changed. Points and leaderboards
invite farming, and readers take them as promises.

So Onyx keeps a record of facts. Each fact is one accepted on-chain event. The record has no
score, no weight, no ranking and no cut-off date. Those belong to a later reward decision, if one
comes.

## Decision
1. **One row per accepted event.** A row holds the address that did the work, the unit of work
   and two chain references: the work and its acceptance. A reader can check each row alone.
   Totals per day, address or kind are a query, not a row.
2. **Who.** The address the contract checked: a signature (a miner's commit, a key's report) or
   `msg.sender` (a validator, a node operator, a reporter, a bettor). Never the transaction's
   sender. Logs carry no sender. When a reader adds one (`ChainLog::from`), the rules never read
   it. So the record never credits a relayer.
3. **Kinds in rules 1.**

   | Kind | Who | Accepted when |
   |---|---|---|
   | `l1_root` | `Revealed.miner` (the miner signed the commit) | `Finalized` with the same root, then `OnyxDisputes.Released` |
   | `l1_verdict` | `VerdictRevealed.validator`, kind Agree | the same `Finalized` and `Released` |
   | `storage_proof` | the node's operator (`Activated`) | `Answered` with `passed = true` |
   | `storage_miss_report` | `MissReward.reporter` | the same event (its slot comes from the `Slashed` log of its transaction) |
   | `signed_report` | `KeyRegistered.signer` of the report's key | `ReportRecorded` with outcome Accepted |
   | `stake_correct` | `Submitted.submitter` | `Decided` with state Correct (a later Correct claim on the same fact with another value revokes it) |
   | `correction_found` | `Bet.who`, side No | `Resolved` with winner No |
   | `revoke` | the who of the revoked row | `Withdrawn`, `Equivocated`, `KeyCompromised`, `KeyRetired`, or a Correct stake decision that supersedes |

   A shard with a dispute case is never released: `release` needs `caseCount == 0`. Both kinds
   of case count: a challenge of a `Finalized` root, and a rerun of a `Pending` shard. So such a
   shard gives no row in rules 1, whatever the case decides (upheld, overturned, no valid root,
   unresolved). The tool reads `CaseOpened` and `CaseClosed`, and it ignores the `Finalized`
   that the end of a case emits (`dEnd`). A second `Finalized` without its `CaseOpened` log
   also marks the shard as cased. A voided shard or round gives no row.

   `l2_agreed` (an observer whose reveal equals the settled value) needs the round transcripts:
   slice 2. Not counted: panel votes and re-observations (on the live set, seats are open and the
   owner sets the observers; v1), futures bets, a winning Yes bet and insurance (money, not data
   work), relayed proofs of recorded values, revenue credits, token mints and faucet funds. The
   tool never reads bounty-board wants.
4. **Detail codes.** `l1_root`: `winner` (the earliest commit, which the contract pays) or
   `also`. `l1_verdict`: `agree`. `storage_proof`: `self` (the deal's payer is the node's
   operator) or `other`. `storage_miss_report`: `reward`. `signed_report`: `accepted`.
   `stake_correct`: `by=Observation`, `by=Vote`, `by=Fallback` or `by=Timeout`.
   `correction_found`: `no_won`. `revoke`: `withdrawn`, `equivocated`, `compromised`, `retired`
   or `superseded`.
   `counterparty` is the deal's payer (storage kinds) or the market's opener. `amount` is the pay,
   the reward or the bet, in the token's smallest units.
5. **Revocations.** A row never changes. When a later event withdraws the work of a row, the
   record appends one `revoke` row. Its `revokes` cell holds the key of the revoked row. The book
   reads a report as withdrawn when its key was compromised from a time at or before the report's
   observation or acceptance (R-SIG-6a), or retired at or before its observation (L3R5-F1). The
   logs do not hold the observed time; slice 2 reads it with `report(digest)`. Without it, a
   report recorded up to 300 s (`MAX_CLOCK_SKEW_SECS`) before that time is revoked too. So the
   record never keeps work that the book reads as withdrawn. A report the reviewer keeps with
   `resolveFact` gets no row in rules 1. A stake claim decided Correct by Vote or Fallback can
   lose its place to a later Observed claim on the same fact (`OnyxStakes.submit`). When a
   later Correct decision names another value, every earlier live row of that fact gets a
   `superseded` revoke row. The same value revokes nothing. A decision by Observation is final.
6. **Format.** Dataset `testnet.early-contributors`, license CC0-1.0, key column `key`. Columns:
   `key`, `day`, `kind`, `who`, `subject`, `contract`, `accept_tx`, `accept_block`, `accept_log`,
   `work_tx`, `work_block`, `work_log`, `counterparty` (nullable), `amount` (nullable), `detail`,
   `check`, `revokes` (nullable), `rules`. `key = (record_day << 24) + i`, where `record_day` is
   the number of days since 1970-01-01, so keys ascend across versions. Subjects: `round:shard`,
   `deal:epoch:slot:node`, the report digest, the claim id, the market id. Rows sort by (accept
   block, accept log, kind, who), then by the work reference. The last row of each version is
   `day_close`: its subject is `from_block-to_block`, its detail is
   `block=<hash>;contracts=<hash>;exclusions=<hash>;rows=<n>`, its `accept_block` is the last
   block and its `work_block` the first. Text cells hold only addresses, hashes, numbers and
   codes: the record copies no string from any event. The writer refuses a text cell over 256
   bytes. `onyx-publish build` takes `deploy/early-record/schema.json`.
7. **Days.** One version per UTC day. A block at 00:00:00 belongs to the new day. A version holds
   only rows that its block range accepted on its day.
8. **Same input, same bytes.** The tool sorts logs by (block, log index) and drops a repeat. Two
   different logs at one position stop the run. A log from an address that `contracts.json` does
   not list, or before its contract's `scan_from` block, stops the run. A missing earlier log (a
   reveal without its commit, an answer without the node's activation) stops the run. So
   shuffled or repeated logs give the same CSV and the same manifest root.
9. **State.** A JSON cache holds the cursor, the work not accepted yet and the facts later events
   need (node operators, deal payers, key signers, accepted reports, Correct stake rows and
   their row keys, open cases). It keeps only what a later event can still change. Released,
   cased and voided shards, decided claims, closed cases, and resolved or voided markets leave
   at their event. Revoked rows, and facts that an observation made final, leave after the
   version notes its keys. Pruning never changes the output; a test compares the two. The cache
   is bound to the hash of `contracts.json`. A lost cache costs one rebuild from the deploy
   blocks, nothing else. A shard that ends Empty emits no event. So at each day's last block the
   reader asks `shardState` for every cached shard with no `Finalized`, and drops the ones that
   ended without a root (Empty, Voided, NoValidRoot, Unresolved).
10. **Contracts.** `deploy/early-record/contracts.json` lists every live deployment of
    docs/ops/deployment.md and docs/STORAGE-NETWORK.md: name, ABI tag, address, the document that
    lists it and, for each contract whose events count, the `scan_from` block and what that block
    is. `deploy` means the code appeared there; it is used only where a document states the
    block with the address or the deploy transaction. `bound` means no counted event can come
    before it. Each block is either quoted (`block_quote`) from text tied to the contract: text
    that names its address, or the paragraph that introduces its table. Or it is the quoted
    block of the contract named in `bound_by` (one that it needs, or one deployed before it). A
    test compares the file with the documents. Every `day_close` names its hash. The retired sets (2026-10-12 to 10-14, storage v0) are not scanned in v0:
    all their keys are the operator's.
11. **Exclusions.** Dataset `testnet.early-contributors.exclusions`: `key`, `day`, `address`,
    `action` (`add` or `remove`), `reason` (`role`, `fixture` or `operator_key`) and `source` (a
    getter, a file line or a fixture name). It is append-only. The record keeps every accepted
    event of every address; a reader subtracts the addresses in force. Sources: the role getters
    (`coordinator()`, `owner()`, `receiptSigner()`, `reviewer()`, `treasury()`, `minter()`,
    `reporter()`) by `eth_call` at the day's last block; the fixed list (the local test chain's
    ten public dev accounts and the placeholder addresses of the scripts; nobody owns them); and
    the reviewed list `deploy/early-record/exclusions.json` (reason `operator_key`). For v0 the
    reviewed list names the operator's demo publisher `synthetic-demo`, which deployment.md
    lists, and it holds a `record_publisher` field for the record's own key once that key is
    registered. Each reviewed entry has a `from_day` and an optional `until_day`. A day applies
    only the entries in force on it, so a replay from the deploy blocks gives every earlier day
    the same bytes after the list grows. A publisher key (the signer of a key that the signed book registered) is excluded
    only by the reviewed list. A role or fixture entry that names a publisher key is skipped and
    shown to the reviewer; it never stops a run. If that address was in force, a `remove` row
    follows. The reviewed list never names a data publisher, and a test checks every shipped file
    for the addresses of the data publishers in deployment.md. The record states nothing about
    who runs any other publisher.
12. **Publication.** The record goes under a publisher id of its own, never into the catalog of a
    data publisher. A record that the operator publishes under a data publisher's id would link
    the two in public. Its own id also keeps a record bug from suspending a data publisher's key.
    Cost: one key registration, then one `postReport` a day (about 250,000 gas on Base Sepolia).
    The id is `early-record` (slice 3), neutral as every publisher name must be
    (docs/ops/bulk-publish.md); its origin is `onyx.cryptagraph.com/record`, so its keys
    document is the site file `apps/web/static/record/.well-known/onyx-source-keys.json`, made
    with the key by the operator. Each day is one catalog that lists the record's new
    version and, on a day they change, the exclusions' (docs/ops/bulk-publish.md "Many datasets
    with one anchor"): one anchor a day for both datasets. Each version is created at the end of
    its day (00:00 UTC of the next day), so a stopped build that runs again gives the same bytes.

## Slices
- **Slice 1 (built 2026-11-14, review fixes the same day).** Items 1 to 11 as pure code with
  tests (`onyx_api::early`), `contracts.json`, the reviewed `exclusions.json` and the two schema
  files. The role-getter calldata is built; nothing calls it. A log's sender
  (`ChainLog::from`) may be filled in, and the rules never read it.
- **Slice 2 (built 2026-11-14).** `onyx_api::early_chain` and the `early-record` tool:
  - The head is the node's `finalized` block. A UTC day is written only when the first block of
    the next day is at or below it. A binary search on block times finds the day's last block.
  - Before a day is read, the previous `day_close` block hash must still be canonical.
  - On the first run, each counted contract is checked: code at the head; at a `deploy` block,
    code there and none one block before; a deploy transaction's receipt names the address.
  - One `eth_getLogs` filter holds every counted contract (an address array and topic 0 as a
    list), in 200-block ranges. A refused range is split in halves; the indexer and the record
    share that code. Only the node's raw error text decides: HTTP 429 or rate-limit wording is
    retried with a backoff; a span cap or a result limit (JSON-RPC -32005 "more than 10000
    results") splits. When the node keeps failing, the logs read so far are kept, bound to the
    hash of `contracts.json`, and the next run goes on from them.
  - A load-balanced node can answer from a lagging backend. So the head is the lower of two
    `finalized` reads, and each day's logs are read twice; the second pass is independent of the
    first, and any difference stops the run. `verify` against a second node is the gate before a
    version is published (the daily run of slice 3 applies it).
  - Each log's block time comes from its header, read once per block. A log whose block hash is
    not its header's is refused.
  - At the day's last block: the role getters (exclusions), `report(digest)` for an exact
    observed time where a compromise or a retirement needs it, and `shardState` for shards that
    ended Empty.
  - `build` writes `data.csv`, `schema.json` and `day.json` (and `exclusions/` on a day the
    exclusions change) into a new folder. It never builds a built folder again: a second run
    compares and stops on any difference (`data.csv`, `day.json`, the schema files). One run at
    a time holds the state directory (`flock`); files are written with fsync before their
    rename. A day with no block at all is named in the next version's output. `verify`
    rebuilds a day from the chain (from the deploy blocks, or from the state saved before that
    day) and compares the bytes and a built manifest's shard roots.
  - The tool reads the node URL from `ONYX_EARLY_RPC`, because a URL can hold a provider key.
    Errors name a URL by its scheme and host only.
- **Moved to slice 3: `l2_agreed`.** Its evidence is the round transcript. The transcript lives in
  the coordinator's results store, not on chain, and the free route withholds the reveals while
  a value is sold. So a public rebuild cannot give those rows, and the record would no longer be
  a function of public data. They need a published transcript per round first (THREATS
  EARLY-16).
- **Slice 3, code (built 2026-11-14).**
  - `scripts/early-record-anvil.sh` in check-all (design test 10): on a local anvil with
    OnyxSettlement, OnyxDisputes, OnyxRegistry, tONYX, OnyxSignedBook, OnyxStorage and
    OnyxCorrections, one outside address does accepted work of four kinds (a signed report,
    storage proofs paid by another, rewarded miss reports, a winning No bet accepted the next
    day) and two that are not accepted (a forged storage proof, a losing Yes bet); the two days'
    rows are exactly its expected rows, every role key and the record's own key are in the
    exclusions and it is not; `onyx-publish build` makes versions 1 and 2 (the successor check);
    `verify` gives the same bytes, state hash and shard roots of both datasets, and a changed
    row fails as a difference; the daily run's dry run with the real tools gives the same bytes
    from a fresh state and verifies the shard roots after each bulk build. Not on anvil: `l1_root`,
    `l1_verdict`, `stake_correct` and `revoke` (unit tests).
  - `scripts/early-record-daily.sh`, the daily run (runbook: docs/ops/early-record.md "The
    daily run"): build every complete day not built yet; then each day in order, all the way
    before the next: the privacy scan of its folder (a hit, a missing or an empty folder stops
    publication, exit 4), `onyx-publish build --prev`, `verify` against a second node of another
    scheme://host:port (bytes, the state hash, both versions' shard roots; a difference stops
    publication, exit 3), its catalog signed, anchored and uploaded, then its versions. Config by
    environment only; the key file's path is passed to `onyx-publish` and never read; a dry run
    stops before the signing; a rerun goes on where a run stopped; a lost `OUT` with a kept
    state stops it (exit 2). `scripts/early-record-daily-tests.py` runs every path with stub
    tools.
  - `day.json` holds `state_keccak`, the hash of the join's state after the day (review of
    slice 3): a log a node left out whose row comes only later changes no byte of that day's
    CSV but changes that state, so `verify` of that day fails on another node. Every day is
    verified in order, so the state each day starts from is one the second node computes too;
    a reader rebuilding from the deploy blocks checks every day's hash.
  - `deploy/systemd/user/onyx-early-record.service` and `.timer` (daily after 00:30 UTC;
    restarts after exit 1, never after 2 to 5) and `onyx-early-record-alert.service` (its
    `OnFailure=`: one journal line tagged `onyx-early-record-alert`).
- **Slice 3, operations (open).** The record's key (`onyx-publish keygen`), its keys document
  and registration in the signed book, test ETH for its anchors, its address in
  `record_publisher`, the timer's install, the first backfill from the deploy blocks (by hand,
  with nodes that keep old state), and the rest of the reviewed list of operator test keys.
- **Later.** `l2_agreed`, once a transcript per round is public (THREATS EARLY-16).
- **v1.** An optional signed claim address on submissions.

## Consequences
- Anyone can rebuild a day from public logs and compare shard roots (`early-record verify`).
  `l2_agreed` rows will also need the transcripts.
- A backfill from the deploy blocks needs a node that keeps old state (the deploy-block check
  and the role getters read old blocks). It costs one `eth_getLogs` per 200 blocks.
- Versions append. No earlier row changes; a changed row breaks the successor check.
- Today almost every row is the operator's own. The first useful outside rows will come from
  independent observers (docs/join.md).
- The record does not prove that one address is one person. On a test network, bonds cost
  nothing real.
- The exclusions are the operator's judgement. They are public and append-only, so a reader can
  disagree.
- The record promises no reward, token, payment or allocation. Test tokens have no value.
- It records no off-chain work in v0: submissions, code, reviews, documents or storage reads.
- Reads: one `eth_getLogs` per 200 blocks, about 2,160 calls a day at 0.2 s blocks.

## Alternatives
- **Points or a leaderboard.** They invite farming and read as a promise. Refused.
- **One row per day, address and kind.** A revocation must then rewrite an aggregate, and a list
  of references does not fit a bulk cell well. Refused.
- **Credit the transaction's sender.** That credits relayers. Refused.
- **The catalog of a data publisher (no extra transaction).** It links that publisher to the
  operator in public. Refused (item 12).
