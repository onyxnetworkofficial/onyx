# Spec: bulk datasets from a publisher (contributor loop CL5)

Status: **Accepted as ADR-0059 (2026-10-29), built.** Test network only. Code: `crates/onyx-bulk`,
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
  most 8 MiB.
- **Anchor**: the ADR-0028 `SignedReport` over `{source: publisher, key: key id, metric:
  "bulk.manifest", subject: Aggregate(dataset), value: Bytes(root), observed_unix: created_unix,
  sequence}`, posted with `OnyxSignedBook.postReport(payload, signature)`.

### Rules
- R-BULK-1 (rights): `rights.redistribution` is `Allowed`, `rights.source` is the publisher,
  `rights_statement` is exactly `onyx_bulk::RIGHTS_STATEMENT`, the license is a license id.
- R-BULK-2 (versions): version 1 has `prev` zero; version n+1 names version n's root, keeps
  dataset, publisher, schema and rights, has a later `created_unix`, keeps every earlier entry in
  place and adds at least one.
- R-BULK-3 (anchor): a node holds a manifest only when, at its finalized block, `report(digest)`
  is recorded with this report's hash, `factView(fact)` is accepted on that hash, and
  `keyState(key)` is registered (the book's fact view already withdraws what a retired or
  compromised key may no longer state), not suspended, with the recovered signer. A key the book
  did not know or had suspended is refused without a chain read for 60 s. It reads the anchor again
  at most 300 s (host clock) before serving.
- R-BULK-4 (shards): a node stores a shard only for a root in a held manifest of that dataset,
  with a body of exactly `entry.bytes` and no content encoding, that checks against the schema
  (framing caps read first) and hashes to the root with the entry's rows and key range.
- R-BULK-5 (forks): a second anchored manifest for a held version marks the dataset forked; it is
  not served until an operator reviews it (the file `forked.txt` in its folder).
- R-BULK-6 (quotas): per publisher at most `ONYX_BULK_QUOTA_GIB` over its datasets' latest
  versions and `ONYX_BULK_MAX_DATASETS` datasets; per node `ONYX_BULK_MAX_GIB`.
- R-BULK-7 (tiers): shards hot; warm after `ONYX_BULK_WARM_AFTER_DAYS` unread (copied and read
  back before the hot copy is dropped); hot again when read; never cold.
- R-BULK-9 (sequence): a manifest's anchor uses `sequence_for(dataset, version)`, never another
  number; two manifests for one version under one key are therefore a book equivocation.
- R-BULK-10 (serving version): the node serves the newest version whose anchor holds (R-BULK-3);
  a held version the book no longer holds may be replaced by another anchored manifest for that
  version, which moves it and every later version to `withdrawn/`.
- R-BULK-8 (client): the answer must name the publisher and dataset asked (and at least the
  version asked); a row is accepted only if it is proven to its shard root, the shard entry to
  `shards_root`, the header to the root, the root to the publisher's signature over the anchor
  payload, the row fits the schema and the asked range, the page's rows are one run, and the
  caller's own node shows R-BULK-3 at its finalized block.

### Routes
`GET /v1/bulk`, `GET /v1/bulk/{publisher}/{dataset}`,
`GET /v1/bulk/{publisher}/{dataset}/rows?from=&to=&page=&limit=`, `POST /v1/bulk/manifests`,
`PUT /v1/bulk/{publisher}/{dataset}/shards/{root}`: docs/api/openapi.json and
docs/spec/delivery.md "Bulk datasets".

### Tools
`onyx-publish keygen | address | keys-doc | synth | build | sign | anchor | upload`;
`onyx-check --bulk answer.json --publisher ID --dataset ID [--min-version N] [--from N] [--to N]
--rpc URL --chain-id N --book 0x…`.

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
