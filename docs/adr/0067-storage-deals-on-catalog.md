# ADR-0067: Storage deals on catalog-anchored datasets (READINESS B3, THREATS CAT-2)

## Status
Proposed, draft 1 (2026-11-19, test network only). Slices 2, 3 and 4 are built and tested
locally (2026-11-19): contract v4, its stateless reader `OnyxStorageAnchor`, the Rust mirror and
the shared vectors; the node serves and accepts catalog proofs and follows rules 4; the tool
opens catalog deals; the local demo runs a catalog deal from creation to close. A review of
slice 2 added one rule (check 7: no deal through a catalog that has a successor). Slice 5 is
prepared (2026-11-19): the move from v3 is scripted, tested with fake tools and written up as a
runbook. Nothing is deployed: the live run of slice 5 and slice 6 are open.

Words used here: an **own report** is one signed report in the book for one manifest (ADR-0059
item 3). A **catalog** is one signed report for many datasets (ADR-0059 amendment 1). **v3** is
the deployed `OnyxStorage` (`RULES_VERSION` 3). **v4** is the contract this note proposes.

## Context

### What exists

- **The contract.** `OnyxStorage` v3 is live on Base Sepolia at
  `0x9244eeaf5053f90ca2fbce157a3446f828c5ee43` (block 53457522, deployed 2026-11-02;
  docs/STORAGE-NETWORK.md:310). It has no admin key, no proxy, and every parameter is fixed
  (contracts/src/OnyxStorage.sol:25, :210-220). It is 22,057 bytes; the limit is 24,576.
- **The nodes.** Five storage nodes ran two live runs: 612 of 612 checks, then 85 of 90
  (docs/STORAGE-NETWORK.md:312-333). They still run and tick (docs/TRACKER.md:209).
- **The tools.** `onyx-storage` (register, deal, epoch, settle, withdraw, exit:
  crates/onyx-node/src/bin/onyx-storage.rs), `scripts/storage-sepolia.sh` (live) and
  `scripts/storage-network-anvil.sh` (local demo with five nodes).
- **Catalogs.** A publisher anchors about 9,000 datasets with one report a day. Catalog nodes
  take a manifest when a held catalog lists its leaf (ADR-0059 amendment 1 item 5;
  crates/onyx-api/src/bulk_serve.rs:2716).

### What is switched off, and why

- **No deal is open.** Both runs were closed on 2026-11-02 (docs/TRACKER.md:555). The next run
  waited for test ETH: node keys held 0.000026 to 0.000083 test ETH
  (docs/STORAGE-NETWORK.md:333). That block is gone: `scripts/testnet-faucet.py` gives 0.0001
  test ETH per request (READINESS A11).
- **Deals on widened datasets.** The node refuses a deal on a format-2 manifest
  (crates/onyx-api/src/storage.rs:1841), because the contract reads format 1 only
  (contracts/src/OnyxStorageLib.sol:130).
- **Deals on catalog datasets.** Not possible at all: see the gap below.

### The gap (CAT-2)

A deal cannot name a dataset that is anchored only through a catalog. There are four reasons.

1. **The contract reads one kind of report.** `createDeal` calls `_checkAnchor`
   (contracts/src/OnyxStorage.sol:536, :585). That calls `checkPayload`, which accepts only a
   payload with the metric `bulk.manifest`, the subject `Aggregate(dataset)` and the value
   `Bytes(manifest root)` (contracts/src/OnyxStorageLib.sol:34, :158-180). A catalog's report
   has the metric `bulk.catalog`, the subject `Aggregate("catalog")` and the catalog root as
   its value (crates/onyx-bulk/src/catalog.rs:461). The contract has no Merkle check from a
   manifest to a catalog.
2. **A late own report is refused.** The book refuses a report observed more than 7 days ago
   (`MAX_REPORT_AGE_SECS`, contracts/src/OnyxSignedBook.sol:32, :366). A manifest's own report
   must carry the manifest's creation time (crates/onyx-bulk/src/anchor.rs, `payload`). So an
   older catalog-only manifest can never get an own report (review CAT-R1 M4, THREATS.md:3352).
   A second own report per dataset would also undo what catalogs are for: one anchor for all.
3. **The node fetch expects an own report.** A storage node fetches a deal's manifest from
   `GET /v1/bulk/manifests/{root}` and checks the answer's `report` as an own report
   (crates/onyx-api/src/storage.rs:1863-1901, `anchor::verify`). For a catalog version that
   route returns the catalog's report and no leaf proof
   (crates/onyx-api/src/bulk_serve.rs:5163-5201), so the check fails.
4. **The deal tool expects `report.bin`.** `onyx-storage deal` reads `manifest.bin` and
   `report.bin` from a folder (crates/onyx-node/src/bin/onyx-storage.rs:284-314).

### Is a new contract needed?

Yes, for catalog deals. v3 cannot be changed: there is no admin key and no proxy, and
`_checkAnchor` is internal. No call sequence on v3 makes it accept a catalog report.

No, for "live deals again". A deal on a manifest that has its own report still works on v3
today. It needs only gas. Slice 1 uses this to prove the five nodes still work before any
contract work.

## Decision

Build `OnyxStorage` v4: v3 plus one new function, `createDealInCatalog`. Everything after deal
creation stays the same.

### 1. The contract change (smallest)

```solidity
function createDealInCatalog(
    bytes calldata header,      // the manifest's Borsh header, as in createDeal
    bytes calldata catalog,     // the catalog's Borsh header
    uint64 leafIndex,           // the manifest's leaf position in the catalog
    bytes32[] calldata siblings,// the leaf's Merkle path
    bytes calldata payload,     // the catalog's signed payload, as posted to the book
    uint8 r,
    uint32 epochs,
    string calldata sourceUrl
) external nonReentrant returns (uint64 id);
```

The checks, in order:

1. Read the manifest header as today (`readHeader`). Also read `version` (4 bytes) and `prev`
   (32 bytes): they sit right after the publisher string
   (crates/onyx-bulk/src/manifest.rs:136-143).
2. Read the catalog header: format byte 1, the publisher string, then 84 fixed bytes (number,
   previous root, creation time, leaf count, leaves root; crates/onyx-bulk/src/catalog.rs:113).
   The length must be exact. The catalog's publisher must equal the manifest's publisher.
3. Build the leaf row: `"onyx-bulk-catalog-leaf-v1" ‖ borsh(publisher, dataset, version, prev,
   manifest root)` (catalog.rs:104). Check it with `OnyxMerkle.verify(leavesRoot, row,
   leafIndex, leafCount, siblings)` (contracts/src/OnyxMerkle.sol:25). The library exists and
   the storage proofs already use it.
4. Compute the catalog root: `keccak256("onyx-bulk-catalog-v1" ‖ catalog)`.
5. Check the payload: source = publisher, metric `bulk.catalog`, subject
   `Aggregate("catalog")`, value `Bytes(catalog root)`. Also check the observed time equals the
   catalog's creation time, and the sequence equals the fixed sequence of the catalog number
   (catalog.rs:451). Then the contract accepts exactly the reports a node accepts.
6. Ask the book the same three questions as `_checkAnchor` today: the report is recorded, its
   fact is accepted on exactly this report, the key is registered and not suspended.
7. **Not through an old catalog** (added by the review of slice 2). A catalog's report has one
   fixed sequence for its number, and the book's `sequenceOf(key, sequence)` is public. So the
   contract asks the book for the report under the same key and the sequence of catalog
   number + 1. If there is one and it holds (the three questions of check 6 all pass for it),
   the call reverts with `CatalogSuperseded`. A next catalog that is disputed, decided against
   or signed by a suspended key does not count: nodes do not take such a catalog either, so
   they still serve the one before it (the node-side rule is `next_catalog_unheld`,
   crates/onyx-api/src/bulk_successor.rs, which also treats only a report that holds as "the
   next catalog exists"). The largest catalog number has no next one: the contract then asks
   about the reserved sequence, which no report can use, so nothing overflows. The rule in one
   line: **a deal must use the publisher's newest catalog, and a node that holds the catalogs
   serves that proof** (its manifest answer, section 4). Known limits:
   a next catalog signed by a second key of the same publisher is not seen; and a deal sent
   while a new catalog lands is refused and must be sent again with the new catalog.
8. Open the deal with the same code as `createDeal` (one shared internal function).

Other changes in v4:

- `RULES_VERSION` becomes 4. A node refuses a contract whose rules it does not know
  (crates/onyx-api/src/storage.rs:1026).
- A new event `DealAnchor(uint64 indexed dealId, bytes32 digest, bool inCatalog)` names the
  report that backs each deal. `DealCreated` keeps its shape, so log readers keep working.
- `readHeader` accepts format 1 and format 2. Both formats have the same first fields and the
  same last 84 bytes (manifest.rs:136-160). This lifts the "widened dataset" block in the same
  deployment (ROADMAP.md:1166).
- `createDeal` (own report) stays, with the same signature. It also emits `DealAnchor`, with
  `inCatalog` false.
- **The size stop rule applied (slice 2).** With the new checks inside it, `OnyxStorage` was
  25,672 bytes: 1,096 over the limit. So the header and payload reads moved to a second
  contract, `OnyxStorageAnchor` (contracts/src/OnyxStorageAnchor.sol). It has two pure
  functions (`own` for a manifest's own report, `inCatalog` for a catalog), no state, no owner
  and no funds. `OnyxStorage` deploys it in its constructor and keeps the address in the
  immutable `anchor`, so the constructor's arguments do not change and nobody can swap the
  reader. The three questions to the book (check 6) and the question of check 7 stay in
  `OnyxStorage`; the reader only computes which sequence check 7 asks about. No check was
  dropped. Sizes now, with check 7: `OnyxStorage` 23,098 bytes (1,478 free),
  `OnyxStorageAnchor` 4,646 bytes.
- **One rule added in slice 2.** `createDealInCatalog` refuses a manifest header that has no
  room for the version and the previous root between the publisher and the tail (`BadHeader`).
  `createDeal` does not read those 36 bytes, so it is unchanged. No real header is that short.

New error: `CatalogSuperseded` (check 7).

What v4 does not check: the leaf's neighbours. A catalog node checks them so that a reader
knows the leaf is the only one for its dataset. A deal needs less: proof that the publisher
signed this manifest root. Any leaf of an anchored catalog gives that.

Cost: one keccak per path level. A catalog of 9,000 leaves has 14 levels; the cap of 50,000
leaves has 16. Measured in slice 2 (forge, the vectors' manifest of 393 header bytes, a real
catalog of 9,000 leaves, a path of 14 levels, 3 replicas):

| Call | Execution gas | Calldata bytes | Transaction gas |
|---|---|---|---|
| `createDeal` on v4, the same manifest with its own report | 307,934 | 868 | about 337,700 |
| `createDealInCatalog`, 14 levels | 347,756 | 1,604 | about 386,800 |

Measured again after check 7 was added (it costs about 10,000 gas: two more reads of the book).
So the catalog path costs 39,822 more execution gas, and about 49,100 more for the whole
transaction (it carries 736 more bytes). Transaction gas here is 21,000 plus the calldata
(16 per byte, 4 per zero byte) plus execution. The tests are
`OnyxStorageCatalog.t.sol::test_gasOfACatalogDealWith14Levels` and
`test_gasOfAnOwnReportDealOnTheSameManifest`. `createDeal` itself costs about 6,800 more gas
on v4 than on v3: one call to the reader contract and the `DealAnchor` event.

### 2. How a deal names a catalog-anchored dataset

The deal stores what it stores today: the manifest root, the shards root, the shard count and
the byte count (`Deal`, OnyxStorage.sol:194). It stores nothing about the catalog. The catalog
is only the proof, at creation, that the publisher signed that manifest root. `DealAnchor`
records which report was used.

A deal pins one version. A later version of the dataset needs a new deal, as today.

### 3. Proofs and settlement

No change. Challenges, answers, misses, slashes, pay, decline and close read only the deal's
`shardsRoot`, `shardCount` and `salt`. A catalog deal and an own-report deal are the same
after creation. The storage proof is over row bytes, so it does not depend on the schema.

### 4. Node and tool changes

- **`crates/onyx-storage`.** `deal.rs` mirrors the new contract reads: `version` and `prev`
  from the header, the catalog header, the leaf row, the catalog payload check. `abi.rs` gets
  `create_deal_in_catalog` and the `DealAnchor` event. The shared vectors
  (`fixtures/storage/vectors.json`) get catalog cases, so Solidity and Rust agree byte for byte.
- **The manifest route.** `GET /v1/bulk/manifests/{root}` adds a `catalog` field (a catalog
  header and the leaf proof) when the held version is catalog-anchored. The proof comes from
  the publisher's newest catalog the node holds, when that catalog lists exactly this
  manifest: check 7 lets a deal through the newest catalog only, and most datasets stay
  unchanged from one daily catalog to the next. When the newest catalog does not list it (a
  later version, a removal) or the node holds no catalog (a storage node), the answer carries
  the proof the version was taken under (`Held.catalog`). A rows answer is not changed: its
  `anchor.catalog` stays the catalog the version is held under.
- **The storage node.** `fetch_manifest_until` accepts either an own report or a catalog
  proof with the catalog's report. `ensure_manifest` stores a catalog version through a new
  store entry (`accept_manifest_with_proof`) that takes the carried header, proof and report,
  and reads the catalog's anchor on chain. It does not need the whole catalog. The node's
  `RULES_VERSION` becomes 4, and only 4: challenges and pay are the same under 3, but a
  rules-3 contract reads format 1 only and knows no catalog, and section 5 moves every node off
  it first. The format gate (`deal_format`) is gone: a test shows a format-2 deal passes its
  checks.
- **`onyx-storage deal`.** Two new forms. `--dir DIR --catalog FILE`: the tool reads the
  publisher's catalog file and its report, finds the leaf, builds the path and sends
  `createDealInCatalog`. `--root 0x… --node URL`: the same from a node's manifest answer, for
  a payer who has no local files. Without them, the tool works as today. It reads
  `RULES_VERSION()` first and refuses a catalog deal or a format-2 manifest on a contract
  below 4, before any approval is sent.
- **Scripts.** `scripts/storage-network-anvil.sh` adds one catalog deal to the demo.
  `scripts/storage-sepolia.sh` learns the new deal form and the move from v3.

### 5. Migration from v3

v3 cannot be stopped or upgraded. It needs no stop: it holds no open deal.

1. Before any node runs the v4 build: each node operator calls `requestExit` on v3, waits, then
   calls `withdrawBond`. Two transactions per node. The wait (corrected in slice 5): a node that
   asks in epoch E is off the list from E + 1, and `withdrawBond` needs an epoch above E + 1 +
   `SETTLE_EPOCHS` (2). So the bond is free from epoch E + 4: 72 to 96 minutes with v3's
   24-minute epochs, not 48.
2. Deploy v4 with `contracts/script/DeployStorage.s.sol` (the constructor's arguments do not
   change). The deployment also creates `OnyxStorageAnchor`: read its address from `anchor()`
   and verify both contracts' source.
3. Each node registers and activates on v4 (approve, `register`, `activate`), then restarts
   with the new contract address, node id and deploy block. The saved log file is named by
   contract address (storage.rs:295), so old and new logs do not mix.
4. Add v4 to `deploy/early-record/contracts.json`. Keep the v3 entry: its history stays valid.
   Found in slice 5: the file's reader refuses a second contract with a counted ABI and knows
   `storage-v3` only (`onyx_api::early`, `Contracts::parse`). So this step needs a `storage-v4`
   ABI in that reader first (the same five counted events), and a decision on days already
   published under the old file's hash. Until then the early record does not count work on v4.
5. Mark v3 retired in docs/STORAGE-NETWORK.md and the runbook, as was done for v0.

### 6. Threat notes

THREATS.md would need these new rows (ids are proposals):

| Id | Threat | Planned answer |
|---|---|---|
| STOR-CAT-1 | A forged leaf: a deal on a manifest the publisher never listed (wrong index, wrong leaf count, a path from another catalog, another publisher's catalog) | The Merkle path binds index and count; the leaf, the catalog and the header must name one publisher; forge tests for each case |
| STOR-CAT-2 | The contract and the Rust code read a catalog header or leaf differently, so the contract accepts what nodes refuse | Shared vectors, exact lengths, a fuzz test that compares both |
| STOR-CAT-3 | A stale catalog: anyone opens a deal with an old catalog on a version that was later replaced or removed | The contract refuses a catalog whose next number has a report that holds under the same key (check 7, `CatalogSuperseded`). Not covered: a next catalog under a second key of the publisher. For that case, and for a dataset removed after the deal began, nodes learn removals and decline (CAT-ERASE-2). See open question 3 |
| STOR-CAT-4 | The book withdraws the catalog after the deal starts (a stolen key). The deal keeps paying, and `extendDeal` does not check the anchor again | The same holds for own reports today. Nodes drop the version when the book withdraws it; the payer closes at the end |
| STOR-CAT-5 | A storage node takes a manifest from a carried proof without the whole catalog, so it cannot see a catalog fork (CAT-6) | It checks the proof with the contract's rules, the leaf's neighbours, and the book's record of that catalog at a final block. Fork checks stay with catalog nodes. The node does not refuse a proof from an older catalog: the contract did that when the deal was made (check 7), and a deal made yesterday must still be fetched and repaired after today's catalog lands |
| STOR-CAT-6 | Two leaves for one dataset in a signed catalog (the contract does not check neighbours) | Only the publisher can sign that, and nodes refuse an unsorted catalog. The deal still binds a root the publisher signed |
| STOR-CAT-7 | A dead or hostile source URL never serves the catalog proof, so each placed node pays one decline | Same class as STORAGE-16. Nodes also ask the catalog nodes and their peers |
| STOR-CAT-8 | Migration: a v4 node pointed at v3, bonds left in v3, work counted twice in the early record | The rules check refuses the wrong contract; exit before upgrade; one entry per address in the record |

Rows to update: CAT-2 (done when slice 5 is live), CAT-ERASE-2 (what v4 does and does not
refuse), STORAGE-16.

## Build slices

Gas figures are rough. They use the measured gas of v3 (docs/ops/runbooks/deploy-storage.md)
at the price seen on 2026-11-03 (about 0.006 gwei). One faucet request gives 0.0001 test ETH.

| # | Slice | Size | Test ETH |
|---|---|---|---|
| 1 | A live smoke run on v3 | 3 hours of work, 3.5 hours of waiting | about 0.0002 |
| 2 | Contract v4 and its Rust mirror | 8 to 10 hours | none |
| 3 | Node: serve and accept catalog proofs | 10 to 14 hours | none |
| 4 | Tool, scripts, local demo | 5 to 6 hours | none |
| 5 | Deploy v4, move the nodes, first catalog deal | 4 to 6 hours, then 3 days of watching | about 0.0003 |
| 6 | Docs, threat rows, close B3 | 2 to 3 hours | none |

A fresh design review of this note comes between slices 1 and 2 (about 2 hours).

### Slice 1: live deals again, on v3 (checked 2026-11-19: not possible as written)

- **Checked live, 2026-11-19:** the five storage nodes run and each reports `lastTickOk: true`.
  But the live node lists no dataset with its own report any more: both publishers anchor by
  catalog only, and a sample of the smallest datasets of each publisher (8 of 9,054) had a
  `bulk.catalog` report in every case.
- **So:** a deal on v3 would need a new test dataset published only for this, with its own
  report. That dataset would show on the public site with no use for a reader. Not done.
- **Instead:** the proof that the nodes still take a deal moves to the local demo (slice 4) and
  to the first deal on v4 (slice 5). "Live deals again" and "deals on catalog-anchored
  datasets" are one piece of work: v4.

### Slice 2: contract v4 and the Rust mirror (built 2026-11-19)

- **Files:** `contracts/src/OnyxStorage.sol`, `contracts/src/OnyxStorageLib.sol`,
  `contracts/src/OnyxStorageAnchor.sol` (new, from the stop rule),
  `contracts/test/OnyxStorageCatalog.t.sol` (new), `contracts/test/OnyxStorage.t.sol`,
  `contracts/test/OnyxStorageNotice.t.sol`, `contracts/test/OnyxStorageVectors.t.sol`,
  `contracts/test/OnyxStorageFixture.sol`, `contracts/test/invariant/StorageInvariants.t.sol`,
  `crates/onyx-storage/src/deal.rs`, `abi.rs`, `vectors.rs`, `catalog_vectors.rs` (new),
  `fixtures/storage/vectors.json`, `contracts/.gas-snapshot`.
- **The vectors.** `catalog_vectors.rs` builds them with the real encoders of `onyx-bulk`
  (`Manifest::build`, `Catalog::build`, `catalog::sign`, Borsh of a `SignedPayload`): two
  catalogs of one publisher (5 leaves, then 9,000 leaves), 4 good deals and 37 refused cases.
  Each refused case changes one thing in a good one and names the error both sides must give.
  Seven refused cases have bytes no encoder makes (a cut header, one extra byte); their names
  say so.
- **Tests added, Solidity.** A catalog deal opens and takes the deposit; every good vector
  opens a deal (a promoted level, 14 levels, a format-2 header at version 2); every refused
  vector is refused with its error (a wrong index, a wrong leaf count, a changed, missing or
  extra sibling, a path from another catalog, a tombstone leaf, a replaced version, another
  publisher's catalog or manifest, a catalog header of another length or format, a manifest
  header of format 0 or 3, a payload with another metric, subject, value, time, sequence or
  source); two fuzz tests (any changed bit of the path, any other index, any changed byte of
  the catalog header or the payload); a catalog the book did not record; a fact not accepted
  on this report (disputed, then accepted on another report); a suspended key, and the deal
  after the key is reinstated; a compromised key; bad parameters and no approval; a catalog
  deal challenged, paid and closed like any deal; a format-2 header on `createDeal`; the
  money invariants with catalog deals in the action set.
- **Tests added, Rust.** The mirror builds the same leaf row, catalog root, sequence, payload
  and digest as the encoders; the file on disk passes the mirror case by case with the same
  errors; a format-2 header reads and format 3 is refused.
- **Tests changed.** Two tests set the format byte to 2 and expected a refusal. They now use
  format 3 (and 0), because v4 reads format 2. Two tests expected `RULES_VERSION` 3; now 4.
- **Result:** the forge tests and the vector tests pass. `OnyxStorage` is 23,311 bytes, 1,265
  under the limit, after the stop rule moved the reads to `OnyxStorageAnchor`. The gas is in
  section 1.
- **Not built here:** a test that feeds the same random bytes to Rust and Solidity (THREATS
  STOR-CAT-2 names one). The shared vectors and the Solidity fuzz tests cover the same reads.
  The tool `onyx-storage deal` now reads a format-2 header too, but the node still refuses a
  format-2 deal until slice 3. The local demo script runs a node built for rules 3, so it does
  not work with this contract until slice 3 and slice 4.
- **Stop rule:** if the size goes over the limit, move the anchor checks into a small second
  contract with no state (open question 2). Do not cut checks. (Applied: see section 1.)
- **After the review of slice 2 (2026-11-19).** Check 7 added (`CatalogSuperseded`), with
  tests: catalog 0 then catalog 1 anchored, a deal through catalog 0 is refused and one through
  catalog 1 opens; the same again when catalog 2 lands; a next catalog that the book did not
  accept does not count; a catalog with the largest number opens a deal (no overflow). The
  vectors now carry their own counts (5 good deals, 37 refused cases; both sides assert the
  exact numbers), a good case with a catalog of one leaf and an empty path, the whole calldata
  of one deal as `abi::create_deal_in_catalog` writes it (a Solidity test sends those bytes and
  the deal opens), and the sequence check 7 asks about for each catalog. The deploy test
  asserts that the reader contract has code. `OnyxStorage` is 23,098 bytes (1,478 free).

### Slice 3: the node (built 2026-11-19)

- **Files:** `crates/onyx-api/src/bulk_serve.rs` (the manifest route, `manifest_answer`, the
  new store entry `accept_manifest_with_proof`), `crates/onyx-api/src/storage.rs`
  (`ManifestAnchor`, `check_catalog_proof`, `manifest_from_answer`, `fetch_manifest_until`,
  `ensure_manifest`, `RULES_VERSION`; `deal_format` removed),
  `crates/onyx-api/src/storage_tests.rs`, `docs/api/openapi.json` and its two copies,
  `docs/spec/storage-network.md`, `docs/spec/bulk-datasets.md`, `docs/ops/storage-node.md`.
- **The HTTP answer.** `GET /v1/bulk/manifests/{root}` answers `{root, manifest, header,
  report}` as before. For a version that a catalog anchors it also has `catalog`: `{number,
  root, header, leaf}`, a catalog's header and the leaf proof, and `report` is that catalog's
  report. It has the shape of `anchor.catalog` of a rows answer. Which catalog (fixed after
  the second review, which found that the first build served the first catalog a version was
  taken under, so that every deal on a dataset unchanged since an older catalog reverted): the
  newest catalog the node holds that lists exactly this manifest; else the one the version was
  taken under. Tested: catalog 0 then 1 list the manifest unchanged, the answer proves from
  catalog 1 and passes the mirror, while the rows answer still names catalog 0; a version
  that catalog 2 replaced is served with catalog 0's proof and the new version with catalog
  2's; after catalog 3 removes the dataset both keep the proofs they were taken under. The
  answer is copied under the store's lock and encoded outside it. The client's cap on the
  answer now covers the header's second copy and the largest proof (tested). A version with its own
  report answers as before, with no `catalog`. `POST /v1/bulk/manifests` takes `catalog` as a
  number or as that object (it reads its `number`).
- **The node's check.** One set of rules, no third copy: `check_catalog_proof` calls the
  mirror of the contract (`onyx_storage::deal::check_deal_in_catalog`, checks 1 to 5), then
  checks the signature. Then the node checks what the contract does not: the leaf's
  neighbours, because readers of its rows answers check them. The store entry checks the proof
  again by itself and reads the catalog's report in the book at a final block before it keeps
  the version.
- **Rules version.** The node accepts only 4. A rules-3 contract is refused with both numbers
  named, and nothing is sent to it.
- **Not built: a node that refuses a proof from an older catalog.** The review of slice 2
  asked for it. It would break working deals: a deal made today is fetched by a new holder
  tomorrow, after the next daily catalog, and that holder would refuse it and then decline the
  deal. The contract already refuses the old catalog when the deal is made (check 7). What is
  left (a dataset removed later) is the removal learning of CAT-3, which exists.
- **Tests added** (`cargo test -p onyx-api --lib storage::`): the manifest answer of a catalog
  version carries a proof that checks, and equals `anchor.catalog` of the rows answer; a
  storage node that holds no catalog takes the manifest from the carried proof, fetches its
  shards, serves rows that pass the client check with the catalog in the anchor, and answers
  its challenges with proofs the contract's rule accepts; the proof is refused for a wrong
  leaf, a wrong catalog, another catalog's report, another publisher's catalog, a report signed
  by another key, a leaf without its neighbours, the catalog's report passed off as an own
  report, and a manifest of format 3; a node whose book does not hold the catalog, or whose
  book withdrew it, stores nothing and fetches nothing; the store entry refuses the same by
  itself and keeps the proof over a restart; the shared vectors pass the node's check (every
  good deal accepted, every refused case refused for the contract's reason, exact counts); a
  format-2 deal is taken, fetched and answered; a rules-3 contract is refused.
- **Not built here:** the fuzz target for the new answer shape
  (`fuzz/fuzz_targets/bulk_parsers.rs`). The answer is parsed by the same hex and Borsh
  decoders the other answers use; the vectors and the refusal tests cover its rules.
- **Result:** `cargo test -p onyx-api -p onyx-node` passes after the second review (933 tests
  in `onyx-api`, none failed); clippy and fmt are clean.

### Slice 4: tool, scripts, local demo (built 2026-11-19)

- **Files:** `crates/onyx-node/src/bin/onyx-storage.rs` (`deal --catalog`, `deal --root
  --node`, the bounded reads), `crates/onyx-api/src/storage.rs` (`anchor_in_catalog`,
  `deal_call`, `deal_fits_rules`, `view_snapshot`), `scripts/storage-network-anvil.sh`,
  `scripts/storage-sepolia.sh` (`deal-catalog`), `scripts/storage-sepolia-tests.sh`,
  `scripts/README.md` (generated).
- **Before the approval (second review).** The tool asks the book what the contract will ask:
  the anchor's report holds, and for a catalog no report holds under the same key for the next
  catalog number. It stops with the contract's reason (`CatalogSuperseded`, `NotAnchored`,
  `KeySuspended`) before the token approval is sent. `--book-check no` skips this (a test
  switch: the demo uses it to see the contract refuse). Refusals that depend on the nodes or
  the payer's tokens are still found only by the transaction.
- **The tool.** `onyx-storage deal --dir DIR --catalog FILE` builds the call from the
  publisher's catalog file and `FILE.report`. `onyx-storage deal --root 0x… --node URL` builds
  it from a node's manifest answer. Both run the contract's checks first and refuse a catalog
  that does not list the manifest. The tool reads `RULES_VERSION()` first and refuses a catalog
  deal or a format-2 manifest on a contract below 4, before the approval.
- **The live script.** `scripts/storage-sepolia.sh deal-catalog --yes ROOT ...` pays for a manifest
  that the public node serves with a catalog proof. It has the same guards as `deal` (no
  unmined transaction of the payer, no second deal for a manifest). Tested with fakes only; no
  live run.
- **The demo.** `scripts/storage-network-anvil.sh` now runs two deals next to each other: one
  with its own report and one on a dataset that only a catalog anchors (a catalog of two
  leaves, one report for both). The demo anchors catalog 0, then catalog 1 (one more
  dataset, the first two unchanged), before the deal. Measured on the run after the second
  review (2026-11-19, local anvil, epochs of 40 blocks, 459 s in all with the build): the
  publisher node's manifest answer proved the manifest from catalog 0, and from catalog 1 once
  it held catalog 1, while its rows answer kept catalog 0; the tool refused a catalog that does
  not list the manifest and sent nothing; a deal through the old catalog 0 was refused by the
  tool before any approval (`CatalogSuperseded`, allowance still 0) and, sent anyway with
  `--book-check no`, by the contract; the deal through catalog 1 opened from the node's answer
  (deal 1, 4 shards, 3 copies, epochs 2 to 4; `DealAnchor` says `inCatalog` true for it and
  false for deal 0); the five nodes, which hold no catalog, fetched its shards from the carried
  proof; rows of it from a storage node passed `onyx-check --bulk` and name catalog 1; its
  checks passed 6 of 6 in epoch 2, then 2 of 6 in epochs 3 and 4 (the rest were the killed
  node and the node that dropped its data, slashed as the demo intends); it was closed after
  epoch 4 with 0.005 of its 0.009 tONYX deposit paid and 0.004 back to the payer;
  `conserved()` was true after the close and at the end, and the events replayed to the same
  books. The deployment (both contracts in one transaction) used 6,111,928 gas.
- **Old contracts: the tool's reads.** Seen live on 2026-11-19: `onyx-storage status` did not
  finish one node in 25 minutes. It read every log since the deploy block, 200 blocks a call:
  about 35,000 calls after 7 million blocks. Now `status`, `epoch`, `settle`, `prove`, `seed`
  and `books` read the contract's view functions at the head block (`dealInfo`, `nodeInfo`,
  `activeNodes`, `setHashAt`, `exclusionFrom`, `books`, `conserved`). Logs are read only where
  no view has the answer: `epoch` reads the `Answered` and `Slashed` logs of the blocks of that
  epoch and the two after it; a node list that is no longer the newest is searched backwards
  from its epoch's first block; node URLs and deal sources (`read`, `status --since-block N`)
  come from the logs since `--since-block`. `--log-span N` sets the blocks per call, and a
  progress line goes to stderr every 50 calls. `declines` and `books --replay yes` still read
  every log: they report the whole history. Exclusions (second review: the first build asked
  `exclusionFrom` for every shard of every running deal on each command, hours for a deal of
  65,536 shards): `epoch` and `settle` read them only for the shards that epoch's slots pick,
  the same few calls whatever the deal's size (tested with 3 and with 65,536 shards); `prove`
  reads none; `read` reads them for the shards its key range needs; `status --node` reads
  them for every shard only while the open deals have at most 1,024 shards, or with `--shards
  yes`. `register` reads `Activated` logs from its registration block only. Not tried against the live contract (no call to a
  public endpoint was made in this work).
- **Tests added:** the deal call from a catalog and from a node's answer is the same bytes and
  decodes to the proof's parts; a catalog that does not list the manifest, and a report that
  does not sign the catalog, are refused; the rules decision (0, 2, 3 against 4, 5); the block
  range of an epoch's events; the backwards ranges; a view snapshot on a chain 7 million blocks
  long reads no log and gives the same lists, holders, challenges and exclusions as the
  snapshot from every log; texts are read only since the given block and never below the
  deploy block; an older node list is found backwards and the search stops at `--since-block`
  with a clear error; the epoch report reads only its three epochs of blocks and equals the
  report on the full snapshot; a long read is cut by the span and reports progress.
- **Result:** `scripts/storage-network-anvil.sh` ends with `STORAGE NETWORK DEMO PASSED`.

### Slice 5: deploy v4 and the first catalog deal (prepared 2026-11-19; the live run is open)

- **Files:** `scripts/storage-sepolia.sh` (`exit-old`, `withdraw-old`, `archive-old`; `deploy`
  records the reader; the bond checks in `fund` and `register`),
  `scripts/storage-sepolia-tests.sh`, `docs/ops/runbooks/deploy-storage.md` ("Move from v3 to
  v4"), `docs/ops/storage-node.md`. In the live run: `deploy/early-record/contracts.json`,
  `docs/STORAGE-NETWORK.md`, the node env files (written by `install`).
- **Do:** the migration steps of section 5, then one deal on a catalog-anchored dataset with
  `scripts/storage-sepolia.sh deal-catalog --yes ROOT` (`onyx-storage deal --root --node`), 3
  copies.
- **Prepared: the move, as steps of the live script.** Each can be run again; each reads the
  chain first and sends only what is missing; a sending step needs `--yes` and without it says
  what it would send.
  - `exit-old --yes`: one `requestExit` per recorded node. It refuses while any deal on the
    contract is not closed and not past its end (it reads `dealCount()` and every `dealInfo`).
  - `withdraw-old --yes`: one `withdrawBond` per node. Too early, it reads `SETTLE_EPOCHS()`,
    `params()` and the head block, says how many epochs, blocks and minutes are left, and sends
    nothing. At the end it prints `conserved()` and the tONYX the old contract still holds.
  - `archive-old`: moves every entry of the state file (the contract, the deploy block, the
    record of the old deploy, node and deal ids) under `old_v3`. Nothing is deleted or sent. It
    refuses while a node's bond is in, a deal is not closed or a node has earnings left.
  - The three steps act only on a contract whose `RULES_VERSION()` is at most 3
    (`STORAGE_OLD_RULES_MAX`), so they never empty the newest contract by mistake.
  - `deploy --yes` reads `anchor()`, checks that the reader has code, records and prints it. It
    does not verify source: the runbook gives the two commands to do it by hand.
  - **The bond.** `onyx-storage register` approves `bondFor(capacity)`: 10 tONYX for 6 GiB.
    Three nodes get less than that back from v3 (they were slashed). `fund --yes` mints each
    node up to 20 tONYX from the deployer, so it covers the gap; it refuses a target below the
    bond. `register` checks each node's tONYX against the bond before its approval.
- **Prepared: the order** (the runbook has the commands, checks, waits, gas and the way back
  for each step): `close`; `exit-old --yes`; wait 72 to 96 minutes; `withdraw-old --yes`;
  `archive-old`; deploy the new API; `plan`, `deploy --yes`; `fund --yes`; `register` and at
  once `install`; record the deployment; `deal-catalog --yes`; watch three epochs. The rule
  behind it: the nodes leave v3 before the host gets the new program (the storage units start
  the same binary as the API, and the new one sends nothing to v3), and no deal is made on v4
  before every node's health shows its new id.
- **Why the exit is safe:** both v3 deals ran for epochs 2 to 10 and were closed on 2026-11-02;
  v3 was at epoch 1032 on 2026-11-19. Read from the documents, not from the chain in this work;
  `exit-old` checks it on chain before it sends.
- **Tests (fake tools, no network, no key):** `bash scripts/storage-sepolia-tests.sh`, 169
  checks. For the new steps: the whole path; no `--yes` sends nothing; a running deal blocks
  the exit; too early says how long and sends nothing; a second run sends nothing; a send that
  fails part way leaves a state a second run finishes from; a failed read or an unmined
  transaction sends nothing; a rules-4 contract is never treated as old; the archive keeps
  every entry and `plan` does not adopt the old deploy after it; a node below the bond is not
  registered, and is after `fund`.
- **Not tested here:** anything against the live contract, the real `onyx-storage exit` and
  `withdraw-bond` (the fakes follow the contract's rules for them), and the gas of
  `requestExit` and `withdrawBond` (the runbook's figures for these two are estimates).
- **Left (the live run):** a decision on v4's epoch length before `deploy --yes` (open question
  4; the script's default is still 7,200 blocks); the twelve steps of the runbook; the
  `storage-v4` ABI of the early record (section 5 step 4); source verification; the
  docs/STORAGE-NETWORK.md status line.
- **Verify live:** `RULES_VERSION()` reads 4; `dealInfo` names the manifest root;
  `DealAnchor` names the catalog's report; five nodes tick ok; every check passes for three
  epochs; `onyx-check --bulk` passes on a rows answer from a storage node; `conserved()` is
  true on v3 and on v4.
- **Cost:** the deployment is one transaction of 6,111,928 gas (measured in the local demo,
  both contracts; v3 was 4,585,344 gas and cost 0.000024 test ETH, so about 0.000032 test ETH
  at the same price); exit and bond withdrawal on v3,
  10 transactions; approve, register and activate on v4, 15 transactions; one deal; then about
  7 transactions per epoch. Under 0.0001 for the move, about 0.000007 per epoch after. Budget
  0.0003: one faucet request for the deployer, one for the payer, one for each node.

### Slice 6: documents and the B3 row

- **Files:** `THREATS.md` (the rows of section 6; CAT-2 and CAT-ERASE-2), `docs/READINESS.md`
  (B3), `docs/STORAGE-NETWORK.md`, `docs/spec/storage-network.md` (R-STOR-3),
  `docs/ops/runbooks/deploy-storage.md`, `docs/ops/bulk-publish.md` (the two "cannot back a
  storage deal yet" notes), ADR-0059 amendment 1 item 7, ADR-0060 (an amendment that points
  here), `docs/TRACKER.md`.
- **Verify:** `bash scripts/check-docs.sh` passes; each new THREATS row names its test.

### State of v3 on chain (read 2026-11-19)

Read with contract views on the public endpoint: epoch 1032, 2 deals, `conserved()` true. All
five nodes are registered and active with 6 GiB each, no misses, bonds of 9.8, 9.8, 8.82, 7.938
and 7.938 tONYX, and each holds gas for its next steps. So the move in slice 5 starts from five
working nodes. Three of them get less than the 10 tONYX bond back: `fund --yes` tops them up. A node of this build follows rules 4 only: the nodes leave v3 before the new
node is installed, and the new node is not deployed before that day.

## Open questions

1. **One function or two?** Recommended: two. `createDeal` keeps its signature, so the 24
   existing test calls and the own-report tool path stay as they are.
2. **What if v4 does not fit in 24,576 bytes?** v3 has about 2,500 bytes free. Recommended:
   measure in slice 2. If it does not fit, move the two anchor checks into a second contract
   with no state and no admin key, which v4 calls. Never drop a check to save bytes.
   Answered in slice 2: it did not fit (25,672 bytes), so `OnyxStorageAnchor` holds the reads.
3. **Should v4 refuse a deal on a removed or replaced version?** Answered by the review of
   slice 2: yes, as far as the book shows it. The first draft said the contract cannot know the
   newest catalog. That was wrong: each catalog number has one sequence, and the book answers
   `sequenceOf(key, sequence)`. So v4 refuses a deal through catalog n when the book holds a
   report that holds for catalog n + 1 under the same key (check 7). Two limits stay: a next
   catalog under a second key of the same publisher is not seen, and a dataset removed after
   the deal began is not the contract's business. Nodes decline removed datasets (CAT-3 slice
   2b). No rule on the catalog's age: it would block deals whenever the publisher pauses.
4. **How long is an epoch on v4?** Parameters are fixed at deploy. With 24-minute epochs, one
   standing deal costs about 0.0004 test ETH a day in gas. With one-day epochs it costs about
   0.000007 a day. Base Sepolia makes a block every 0.2 s, so one day is 432,000 blocks (the
   runbook's default of 43,200 is 2.4 hours). Recommended: 432,000 blocks. The fast feedback
   comes from slice 1 and the local demo.
5. **A missed seed with long epochs.** A seed must be recorded within 256 blocks (51 s) of an
   epoch's start (`SEED_WINDOW`), or the epoch has no checks and no pay. With one-day epochs
   one miss costs a day. Recommended: every node tries to record the seed, not only the
   keeper, and the health watch alerts on an epoch with no seed. Do not change the window in
   v4: it follows the EVM's 256-block limit on block hashes.
6. **Must a storage node hold every earlier version?** Today it fetches earlier versions back
   to version 1, at most 64 steps (storage.rs:2199), because the store takes versions in order
   (bulk_serve.rs:2887). A dataset refreshed daily passes 64 versions in about two months.
   Recommended: for the first live deal pick a dataset under 64 versions. Then let a node hold
   a deal's version alone, served by root only. That is its own slice after B3, with its own
   review.
7. **Format 2 in the same contract?** Recommended: yes. It is one line in the contract, and a
   later contract would mean a second migration. The node keeps its gate until the format-2
   test of slice 3 passes.
8. **Posted answers (ADR-0064) in v4?** Recommended: no. That design is not final. A third
   contract later costs one more migration, which is cheap on the test network.
9. **Should the deal store the catalog root?** Recommended: no. The event is enough, and the
   deal struct stays the same, so checks cost the same gas as today.

## Consequences

- One catalog anchor a day can back deals for every dataset in it. No second report per
  dataset is needed.
- Widened datasets can get deals from the same deployment.
- The five nodes move to a new contract: about 25 transactions and well under 0.001 test ETH.
- The contract's history splits across three addresses (v0, v3, v4). The early record lists
  each.
- The contract did not fit in one piece. v4 is two contracts: `OnyxStorage` (23,098 bytes,
  1,478 free) and the stateless `OnyxStorageAnchor` (4,646 bytes). A deployment creates both,
  and each deal creation makes one more call.
- A deal cannot be opened through a catalog once the book holds the publisher's next catalog
  under the same key. A payer whose deal crosses a new catalog sends it again. What the
  contract cannot see (a second key, a later removal) stays with the nodes.
- Storage nodes start to hold catalog-anchored versions. They check the book for them but
  hold no catalogs, so fork detection stays with catalog nodes.
