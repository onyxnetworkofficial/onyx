# Spec: bulk datasets from a publisher (contributor loop CL5)

Status: **Accepted as ADR-0059 (2026-10-29), built** (amendment 5, widening successors, built
2026-11-14: format 2, R-BULK-16). Test network only. Code: `crates/onyx-bulk`,
`onyx_api::bulk_serve`, `crates/onyx-publish`, `onyx-check --bulk`. Publisher guide:
docs/ops/bulk-publish.md. Threats: THREATS.md "CL5".

## Scope

A publisher puts a large dataset of its own (many GB of rows) on the test network: the rows are
held by nodes and served free, every row provable to a manifest the publisher signed and anchored
on chain. Out of scope here: pricing, store on first use, deposits and availability strikes
(ADR-0055), several nodes holding one dataset, data that is not the publisher's to share.

## Interfaces

### Format (`onyx_bulk`)
- **Schema**: `{"columns":[{"name","kind":"int"|"decimal"|"text","scale"?,"nullable"?}],"key":"<column>"}`;
  1..=64 columns, names `[a-z][a-z0-9_]*` (at most 63 bytes, unique), decimal scale 1..=38, the key
  column `int` and not nullable. Rows are sorted by the key (equal keys allowed).
- **Cells**: `Null` (nullable columns only), `Int(i128)`, `Decimal(String)` (canonical:
  `-?(0|[1-9][0-9]*)\.[0-9]{scale}`, no negative zero), `Text(String)` (at most 16 KiB, no
  control characters but tab and line feed). A row is `borsh(Vec<Cell>)`, at most 64 KiB.
  Input (CSV with a header line, or JSON Lines) is normalised to this form; a value with more
  decimals than the scale is refused, never rounded; JSON decimals must be strings.
- **Shard file**: `borsh(Vec<Vec<u8>>)` of rows, 1..=2^18 rows, at most 64 MiB; root =
  `onyx_merkle::MerkleTree::from_rows(rows).root()`.
- **Shard entry**: `{index: u32, root, rows: u64, bytes: u64, key_min: i128, key_max: i128}`,
  indexes 0..n in order, `key_max(i) <= key_min(i+1)`.
- **Header**: `{format: 1, dataset, publisher, version, prev, created_unix, schema, rights,
  rights_statement, shard_count, row_count, byte_count, key_min, key_max, shards_root}`;
  `shards_root` = Merkle root over `borsh(entry)`; manifest root =
  `keccak256("onyx-bulk-manifest-v1" ‖ borsh(header))`. At most 65,536 shards; Borsh manifest at
  most 8 MiB. **Format 2** (a widened dataset, ADR-0059 amendment 5): the same fields in the same
  order with `earlier: Vec<{until: u32, schema}>` (oldest first, 1 to 7 entries) right after
  `schema`, so the last 84 bytes stay `shard_count` to `shards_root`; `schema` is the newest. An
  entry covers the shards from the previous entry's `until` (or 0) up to its own `until`, the
  newest schema `[last until, shard_count)`: `schema_of(i)` is shard i's schema. The Borsh is
  written by hand: `earlier` (its count first, more than 7 refused before any entry is read) only
  in format 2, so a format-1 header is byte for byte what it was; a header with one schema is
  always format 1. The root formula does not change (its first byte is the format).
- **Widening** (`Schema::widens(old, new)`): at an old column's position a decimal's scale grows
  (up to 38) or a non-key `int` becomes `decimal`; after the old columns, nullable columns (64 in
  all); at least one change. Never: renaming, reordering or dropping a column, a smaller scale,
  decimal to int, any change to or from text, any change of nullability, a change to the key.
- **Shown rows** (`row::present(from, to, cells)`): a row stored under `from` shown under `to`
  (a schema `from` widens to): decimals padded with zeros to `to`'s scale, an int in a column that
  became decimal written with zero fraction digits, columns `from` lacks null. Never rounds or
  fails; composes (`from` to `b` to `to` equals `from` to `to`). Shown cells are display only: a
  padded decimal may pass the 80-character cap of stored cells (up to 117 characters).
- **Anchor**: the ADR-0028 `SignedReport` over `{source: publisher, key: key id, metric:
  "bulk.manifest", subject: Aggregate(dataset), value: Bytes(root), observed_unix: created_unix,
  sequence}`, posted with `OnyxSignedBook.postReport(payload, signature)`.

### Rules
- R-BULK-1 (rights): `rights.redistribution` is `Allowed`, `rights.source` is the publisher,
  `rights_statement` is exactly `onyx_bulk::RIGHTS_STATEMENT`, the license is a license id.
- R-BULK-2 (versions): version 1 has `prev` zero; version n+1 names version n's root, keeps
  dataset, publisher and rights, has a later `created_unix`, keeps every earlier entry in place and
  adds at least one. Its schema is version n's, with `earlier` equal to version n's, or widens
  version n's (R-BULK-16), with `earlier` exactly version n's followed by `{until: n's
  shard_count, schema: n's schema}`: no earlier shard is relabelled and a version widens at most
  once.
- R-BULK-3 (anchor): a node holds a manifest only when, at its finalized block, `report(digest)`
  is recorded with this report's hash, `factView(fact)` is accepted on that hash, and
  `keyState(key)` is registered (the book's fact view already withdraws what a retired or
  compromised key may no longer state), not suspended, with the recovered signer. A key the book
  did not know or had suspended is refused without a chain read for 60 s. It reads the anchor again
  at most 300 s (host clock) before serving.
- R-BULK-4 (shards): a node stores a shard only for a root in a held manifest of that dataset,
  with a body of exactly `entry.bytes` and no content encoding, that checks against
  `schema_of(entry.index)` (framing caps read first) and hashes to the root with the entry's rows
  and key range. It loads a shard to serve rows under the same schema, so a load (keyed by root and
  schema hash) stays valid across widenings.
- R-BULK-5 (forks): a second anchored manifest for a held version marks the dataset forked; it is
  not served until an operator reviews it (the file `forked.txt` in its folder). Two anchored
  catalogs of one publisher that cannot both stand (another root under a held number that still
  holds, or an anchored catalog that is not the successor of the anchored newest) mark the
  publisher forked (`catalogs/<publisher>/fork.bin`: both headers and reports): its catalog
  datasets are not served and its uploads refused; both sides are read again at most every
  300 s, and the mark clears when the book withdraws either side for good (THREATS CAT-6).
- R-BULK-6 (quotas): per publisher at most `ONYX_BULK_QUOTA_GIB` over its datasets' latest
  versions and `ONYX_BULK_MAX_DATASETS` datasets; per node `ONYX_BULK_MAX_GIB`.
- R-BULK-7 (tiers): shards hot; warm after `ONYX_BULK_WARM_AFTER_DAYS` unread (copied and read
  back before the hot copy is dropped); hot again when read; never cold.
- R-BULK-9 (sequence): a manifest's anchor uses `sequence_for(dataset, version)`, never another
  number; two manifests for one version under one key are therefore a book equivocation.
- R-BULK-10 (serving version): the node serves the newest version whose anchor holds (R-BULK-3);
  a held version the book withdrew for good (read at the same finalized block as the newcomer)
  may be replaced by another anchored manifest for that version, which moves it and every later
  version to `withdrawn/`, newest first; one the book has not decided on (an open dispute, a
  suspended key) is waited for. The same manifest under another report is re-anchored only when
  the held report no longer holds.
- R-BULK-8 (client): the answer must name the publisher and dataset asked (and at least the
  version asked); a row is accepted only if it is proven to its shard root, the shard entry to
  `shards_root`, the header (its whole schema chain valid) to the root, the root to the
  publisher's signature over the anchor payload, the row fits `schema_of` its proven shard index
  and the asked range, its `values` (if sent) equal `present(schema_of(index), schema, row)` and
  its `schema` (if sent) that index, the page's rows are one run, and the caller's own node shows
  R-BULK-3 at its finalized block. A header format the check does not read is undecided ("update
  onyx-check", exit 3) only when the raw header's root verifies against the signed report (own,
  or through the catalog leaf) for the publisher and dataset asked and the caller's node shows
  R-BULK-3 for that report and its signer (without a node: exit 3 with `chainChecked: false`);
  else refused.

- R-BULK-11 (catalogs, ADR-0059 amendment 1): a catalog commits to one leaf per dataset
  (publisher, dataset, version, previous root, root), sorted strictly by dataset, under the
  ADR-0003 tree; its report (metric `bulk.catalog`, subject `Aggregate("catalog")`, sequence fixed
  by the catalog number, observed time strictly later than the previous catalog's) anchors every
  manifest it lists. A node takes catalogs in number order, each the append-only successor of the
  one before (every dataset kept, each moved by at most one version naming the previous root), a
  dataset entering at version 1 or after a held version its leaf names, no leaf disagreeing with
  a held version; a listed manifest is then taken without its own report when the book holds no
  other report under the manifest's own sequence. An answer carries the catalog header and the
  leaf with its path and both neighbours; the client checks the leaf names exactly the manifest,
  is the only one for its dataset in that catalog, and that the catalog's report and the
  manifest's own sequence hold at its finalized block. A catalog whose number a held one has is
  taken only when the book withdrew every held catalog from that number on for good (THREATS
  CAT-5): those are set aside with the versions they anchor, except a version the new catalog
  lists unchanged, which it then anchors. A node stores every 32nd catalog whole (`{n}.bin`) and
  the others as the leaves that differ from the one before (`{n}.diff`), rebuilt and checked when
  read; the bytes served are the same (ADR-0059 amendment 2).
- R-BULK-12 (history, ADR-0059 amendment 3): `GET /v1/bulk/catalogs/{publisher}/files?from=n`
  answers the stored catalogs from n on as Borsh `CatalogFiles` (the newest number, then each
  header, report, and every leaf or the changed leaves), to the end of n's block of 32, at most
  16 MiB. `onyx-check --bulk ... --history <node>` rebuilds every catalog from 0 and checks each
  as a node does before it takes it (`validate`, the report signs the header, the append-only
  successor), the dataset's entry (version 1, or its own-report chain back to version 1) and that
  the rows answer's catalog is in the chain; then, at one finalized block: every catalog's and own
  report's anchor, and for every known key of the publisher (the walk's, the answer's, `--keys`, and with
  `--scan-keys-from` every `KeyRegistered` key of the publisher) that no other live report holds
  a catalog number, a version of the dataset, the catalog after the newest served (catalog 0 when
  none is served) or the version after the newest seen; the rows answer's own key is always read,
  and an own-report rows answer is followed down to a version the catalogs list. Refused with
  `fault` "publisher" (two live signed reports or roots for one catalog number or version, or an
  anchored signed catalog that breaks the rules) or "node" (bytes that do not make the signed
  catalog or manifest); undecided when something can still change (open dispute, suspended key,
  a report not yet final at the caller's block, a withdrawn report still served, a set-aside
  during the walk, a history node behind the rows node). Without a key scan, a key that signed
  nothing in the walk or the answer is not read; the report says so. The route writes its answer
  from the stored bytes, two at a time, in 256 KiB frames, each answer holding its permit until its
  last frame or its deadline (60 s, longer for a large answer at 128 KiB/s; 503 `bulk_busy`
  otherwise). When every check holds and the walk ends the dataset in a tombstone, the outcome is
  `removed` (exit 4, `removed: {catalog, createdUnix, version}`), ranked below every fault and
  undecided; a live report at the tombstone's version or the next one is the publisher's fault.
- R-BULK-13 (removal, ADR-0059 amendment 4): a catalog may end a dataset at version v with the
  tombstone leaf `(publisher, dataset, v + 1, prev = root of v, root = zero)`; never at version 1,
  kept as it is by every successor, so the id is never used again by that publisher. While the
  publisher's newest held catalog lists it, the dataset is removed on the node: its rows, info and
  listing, and every upload of it (`POST /v1/bulk/manifests` either way, shard `PUT`), answer
  `410 bulk_dataset_removed` naming the catalog. A tombstone is taken even over a held version at
  its number. Nothing held is deleted then: the manifests and shards stay, counted, and the peer
  routes by root still answer (storage deals, the history check); a set-aside of the tombstone's
  catalog (THREATS CAT-5) brings the dataset back.
- R-BULK-14 (erasure, ADR-0059 amendment 4 item 5a, slice 1): a node holding the publisher's
  catalogs (not in storage mode) with `ONYX_BULK_ERASE=on` erases a removed dataset's shard files
  once, read at one finalized block, the grace is out (30 days or more of chain time from the later
  of the book's record of the run's first tombstone catalog under the newest key and the block the
  node took it at), both that catalog and the newest read anchored, the newest key is active and
  not suspended, no catalog fork stands and no operator hold names it; it erases only roots no held
  manifest of another dataset names (one not erasing, erased or due), and only a plan it logged at
  an earlier pass unchanged. Manifests, catalogs and set-aside files are never erased. An erased
  root answers `410 bulk_shard_removed` on the peer route; rows and info carry the phase and due
  time; the dataset counts toward the cap but not the byte quota. A set-aside of the tombstone's
  catalog after erasure moves the record aside and the publisher's shard uploads restore every
  version.
- R-BULK-15 (storage nodes learn of a removal, ADR-0059 amendment 4 item 5a, slice 2a):
  `GET /v1/bulk/catalogs/{publisher}/leaf/{dataset}` gives the newest held catalog's header, its
  report and the dataset's leaf with its path and neighbours, from memory (no chain read, no
  fetch); a node with no catalog of the publisher answers from a learned proof it keeps (`from`).
  A storage node keeps a tombstone for a dataset its placed deals' manifests name only when, at one
  finalized block, the header is the held manifest's publisher's and proves the leaf, the leaf is a
  tombstone of that dataset at version 2 or later, the report signs the header and reads anchored,
  and a held version `t - 1` is the tombstone's `prev` (held versions at `t` or later are the
  publisher's fault and do not stop it). It keeps the newest proof and each key's first (the
  first under the newest key starts slice 2b's grace; keys taking turns never restart it), reads
  every one again each pass and drops each only on the book's final word on its own catalog; a
  dataset whose last proof goes is asked about again in the same pass before it serves. A catalog
  the node holds never stops the learning, and the route prefers a newer learned proof. While kept, rows, info and listing answer `410 bulk_dataset_removed` with
  `learned` and the public uploads are refused; `removed` stays catalog-only and the node's own
  fetches, challenges and peer routes by root are untouched.
- R-BULK-16 (widening, ADR-0059 amendment 5): a later version may widen the schema for its new
  shards (format 2); every earlier shard keeps its bytes, root, entry and the schema it was built
  under. `validate` (format 2): 1 to 7 earlier schemas, `until` strictly increasing within
  `1..shard_count`, every schema valid, each widening the next and the last widening `schema`, and
  no shard root under two schemas. A node stores a format-2 manifest (a new version, or one in a
  withdrawn version's place) only with `ONYX_BULK_FORMAT2=on` (else 422 `bulk_format_not_taken`,
  through its own report or a catalog; the catalog itself is taken), and reads held ones whatever
  the setting: the same manifest again is 200, and a second anchored manifest for a held version
  marks the dataset forked (`forked.txt` only). Answers: `manifest.format`,
  `manifest.schemas` (each schema with the index it ends at, the newest ending at `shard_count`),
  `manifest.schema` the newest; each row decoded under its shard's schema and its `values` shown
  under the newest (R-BULK-8), with `schema` (the shard's schema index) in format 2. Storage
  `read_rows` checks each shard under its own schema; storage nodes refuse a deal on a format-2
  manifest (the deployed OnyxStorage reads format 1 only). `GET /v1/bulk/manifests/{root}` also
  carries the header's own bytes (`header`). The history walk (R-BULK-12) leaves an own-report
  manifest in a format it does not read undecided when that header hashes to the root the walk
  followed, the manifest starts with it and the report is the publisher's for that root and
  version; else the node's fault. At most 8 schemas per dataset.

### Routes
`GET /v1/bulk?q=&limit=&after=` (every dataset, or those whose `publisher/dataset` holds every
word of `q`, `limit` at a time in id order, `after` the `next` of the page before; `total` counts
the matches), `GET /v1/bulk/{publisher}/{dataset}`,
`GET /v1/bulk/{publisher}/{dataset}/rows?from=&to=&page=&limit=`, `POST /v1/bulk/manifests`,
`POST /v1/bulk/catalogs`, `GET /v1/bulk/catalogs/{publisher}/{n}`,
`GET /v1/bulk/catalogs/{publisher}/files?from=`, `GET /v1/bulk/catalogs/{publisher}/leaf/{dataset}`,
`PUT /v1/bulk/{publisher}/{dataset}/shards/{root}`: docs/api/openapi.json and
docs/spec/delivery.md "Bulk datasets".

### Tools
`onyx-publish keygen | address | keys-doc | synth | build [--prev DIR [--widen]] | schema-fit --prev DIR
--input FILE --out new.json | sign | anchor | upload | catalog-build | catalog-sign | catalog-anchor |
catalog-upload` (`upload` steps back to the previous version's folder on "versions are taken in
order");
`onyx-check --bulk answer.json --publisher ID --dataset ID [--min-version N] [--from N] [--to N]
--rpc URL --chain-id N --book 0x… [--history NODE [--keys 0x…,0x…] [--scan-keys-from BLOCK
[--logs-range N]]]`.

## Assumptions
- The publisher's domain and keys document are what the reviewer checks before registering the
  key (ADR-0028 item 5); a stolen key is handled by the book's life cycle (compromise), after
  which nodes and clients stop accepting its manifests.
- One node holds a dataset; the publisher keeps its own copy of the shard files (the node is not
  the only copy).
- Base Sepolia's finalized block trails the head by about 15 to 30 minutes; a node takes a
  manifest only after that, so `onyx-publish upload --wait-secs` waits.

## Open questions
1. Completeness at a page's edges: the rows inside a page are one run, but a node could start a
   page late or end it early. Serve the neighbour row's proof at each edge? (THREATS CL5-9.)
2. Store on first use and deposits (ADR-0055 items 2 to 4): who pays for keeping many GB?
3. Several nodes per dataset: how a client finds a second node that holds the shards.
4. Should a publisher be able to retract a version (a mistake in its data), given versions only
   append?
