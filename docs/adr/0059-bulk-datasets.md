# ADR-0059: Bulk datasets: bounded shards, a signed and anchored manifest, proven range reads

## Status
Accepted, 2026-10-29; amendment 1 (catalogs: one anchor for many manifests) accepted
2026-11-03 after review THREATS CAT-R1, built except item 3's withdrawn-catalog and fork-marking rules (THREATS
CAT-5, CAT-6); amendment 2 (catalogs kept small on disk) draft 2 after review CAT-RET-R1,
not built (docs/contributor-loop.md CL5; test network only). Supersedes ADR-0055
(the CL5 design inputs) where they differ. Built in `crates/onyx-bulk` (format, signature,
client check), `onyx_coordinator::cas` (kind `Shard`) and `onyx_coordinator::tiers` (bulk shards
hot or warm), `onyx_api::bulk_serve` (node routes), `crates/onyx-publish` (publisher tool) and
`onyx-check --bulk` (client check). Spec: docs/spec/bulk-datasets.md. Publisher guide:
docs/ops/bulk-publish.md. Demo: `scripts/bulk-demo.sh local`.

## Context
A publisher (the owner first) wants to put years of its own data, many GB, on the test network.
Own-data samples are capped at 8 KiB (ADR-0040) and recipe row batches at 24 MiB (ADR-0018);
neither carries a publisher's signature over a whole dataset, and no route takes or serves one.
Level 3 (ADR-0028) already has what a publisher needs to sign: a key published under its own
domain, reviewed and registered in `OnyxSignedBook`, EIP-191 signatures the EVM can check, and an
on-chain book whose `(key, sequence)` entries are never reused.

## Decision
1. **Format** (`onyx-bulk`). A dataset is a schema (1..=64 named columns of kind `int` (i128),
   `decimal` (a canonical string with exactly `scale` fraction digits, never rounded) or `text`
   (UTF-8 without control characters but tab and line feed); one `int` key column, never null; no
   float kind, ADR-0009) and shards. A row is the Borsh `Vec<Cell>` (one encoding per value, at
   most 64 KiB). A shard is the Borsh `Vec<Vec<u8>>` of rows sorted by key, at most
   `MAX_SHARD_BYTES` = 64 MiB and `MAX_SHARD_ROWS` = 2^18, committed by the ADR-0003 Merkle root
   over its rows. The publisher tool aims at 8 MiB by default.
2. **Manifest.** The header (format, dataset id, publisher source id, version, previous root,
   creation time, schema, rights, the fixed rights statement, counts, key range, and
   `shards_root`, the Merkle root over the Borsh shard entries) is Borsh; the manifest root is
   `keccak256("onyx-bulk-manifest-v1" ‖ borsh(header))`. Entries (`index, root, rows, bytes,
   key_min, key_max`) are indexed 0..n with ordered key ranges. A served row needs the header, its
   shard's entry with a proof and its own proof: never the whole list.
3. **Signature: the level-3 scheme, unchanged.** The publisher signs the `SignedPayload`
   `{source: publisher, key, metric: "bulk.manifest", subject: Aggregate(dataset), value:
   Bytes(manifest root), observed_unix: creation time, sequence}` with EIP-191. The sequence is
   fixed by the dataset and version (`keccak256("onyx-bulk-sequence-v1" ‖ borsh(dataset,
   version))`, first 8 bytes), never chosen by the signer (review CL5-R1): two manifests for one
   version under one key share `(key, sequence)`, which the book records as an equivocation and
   answers by suspending the key, on chain where every node and client sees it. Its key is
   published in `https://<publisher domain>/.well-known/onyx-source-keys.json` and registered by
   the book's reviewer, as for any signed source (ADR-0028 item 5). No new scheme, key format or
   review path.
4. **Anchor: `OnyxSignedBook`, no new contract.** One `postReport` per manifest version
   (about 248,000 gas, measured on anvil). The book already checks the key is the registered one
   of that source, refuses a replay, keeps every `(key, sequence)` for good (a second payload
   under a used sequence is an equivocation that suspends the key), runs the key life cycle
   (retire, compromise, suspend) and reads at a finalized block. No contract changed, so no gas
   snapshot changed.
5. **Versions append.** Version n+1 names version n's root, keeps the same dataset, publisher,
   schema and rights, has a later creation time, keeps every earlier shard unchanged in place and
   adds at least one; its first new key is not below the last old one. Two anchored manifests
   for one version (a key that equivocated or was stolen) fork the dataset: the node stops
   serving it until reviewed.
6. **Node.** `POST /v1/bulk/manifests` keeps a manifest only when the node's finalized block shows
   the signed report recorded, the fact accepted on exactly that report and the key active (or
   retired: a rotation keeps what the key signed before, and the book withdraws the rest) and not
   suspended. `PUT /v1/bulk/{publisher}/{dataset}/shards/{root}` takes a file only for a root
   in a held manifest, exactly the entry's size, no content encoding, and only if it checks
   against the schema and hashes to the root with the entry's rows and key range. Shards live in
   the CAS (kind `Shard`, 64 MiB cap, integrity rechecked on read) under the ADR-0052 tiers: hot,
   warm after `ONYX_BULK_WARM_AFTER_DAYS` without a read, back to hot when read, never cold (no
   recipe rebuilds a publisher's data). Per publisher a byte quota and a dataset count, per node
   a total. Reads are free on the test network: `GET /v1/bulk`, `/v1/bulk/{publisher}/{dataset}`
   and `.../rows?from=&to=&page=&limit=` (at most 1,000 rows), every row with its proofs and the
   anchor as read at the finalized block, rechecked at most every 300 s. The node serves the newest
   version whose anchor still holds: a version the book withdrew (its key compromised from before
   it) is skipped and can be replaced by the publisher's own next version under a new key, which
   moves the withdrawn ones aside (review CL5-R4). Uploads run outside the 12 s request deadline with their own (300 s, two at a time).
7. **Rights.** The header carries `DataRights` with `Redistribution::Allowed` only (free public
   reading cannot honour restricted terms; forbidden data stays refused, CLAUDE.md), the source
   equal to the publisher, and the fixed statement that the publisher owns the data or may share
   it, its terms let anyone redistribute it, and it names no individual person. The publisher
   signs it with the root; the reviewer's key registration is the review.
8. **Client check.** `onyx_bulk::wire::check_rows_answer` proves each row to its shard root, each
   shard entry to `shards_root`, the header to the manifest root and the root to the publisher's
   signature, checks every row fits the schema and the asked range and that the rows are one run
   with nothing skipped inside the page; `onyx-check --bulk` also requires the publisher and
   dataset the caller asked for (and optionally a lowest version), then reads the book on the
   caller's own node at its finalized block (`report`, `factView`, `keyState`).

## Consequences
- A publisher can put a large dataset on the test network with one transaction per version and
  any number of shards; `scripts/bulk-demo.sh local` publishes 100 MiB of synthetic CSV
  (2,451,226 rows, 23 shards, 188 MB of shards) and checks queried rows end to end.
- Every served row is checkable without trusting the node; completeness at the edges of a page
  is not proven yet (THREATS CL5-9).
- What ADR-0055 lists and this ADR leaves open: store on first use (the node here stores what the
  publisher uploads), a storage deposit per GiB, availability strikes, archive/full/pruned node
  roles, and pricing. The owner's datasets are published under a neutral publisher name and
  domain (docs/ops/bulk-publish.md).

## Amendment 1 (2026-11-03): one anchor for many manifests (catalogs)

**Why.** Item 4 costs one `postReport` per manifest version: 247,695 gas, about 0.0000015 test ETH
at Base Sepolia's 0.006 gwei (measured on the first-contributor publish, 2026-10-30; the base fee
alone is 0.005 gwei). The first publisher's next 8,495 datasets and every refresh of its 549 would
need one each, and its key has test ETH for about 124 (docs/TRACKER.md 2026-11-03; no faucet for
the owner). The byte quota is not the limit: those 8,495 fit in 7 GiB. Reviewed before building
(THREATS CAT-R1: "build with the listed changes"); every finding is taken below.

**Decision.** Beside item 4, a publisher may anchor a **catalog**: one signed report that commits
to the newest manifest of each of its datasets.

1. **Catalog format** (`onyx-bulk`). A leaf is the row `"onyx-bulk-catalog-leaf-v1" ‖
   borsh(publisher, dataset, version, previous manifest root, manifest root)` (CAT-R1 H1, H2: the
   publisher and the previous root are in the leaf); leaves are sorted strictly by dataset id (one
   per dataset) and committed by the ADR-0003 Merkle root over rows (`from_rows`, so a path binds
   its index and the leaf count). Header (Borsh): format, publisher source id, catalog number n,
   the root of catalog n - 1 (zero for n = 0), creation time, leaf count, leaves root. Catalog
   root = `keccak256("onyx-bulk-catalog-v1" ‖ borsh(header))`. At most `MAX_CATALOG_LEAVES`
   (50,000; 10,000 until THREATS CAT-4) leaves.
2. **Signature and anchor: the same scheme and the same book.** `SignedPayload {source:
   publisher, key, metric: "bulk.catalog", subject: Aggregate("catalog"), value: Bytes(catalog
   root), observed_unix: creation time, sequence}`, the sequence fixed by n
   (`keccak256("onyx-bulk-catalog-sequence-v1" ‖ borsh(n))`, first 8 bytes). Two catalogs with one
   number under one key share `(key, sequence)`: the book records the equivocation and suspends the
   key. The creation time must be strictly later than catalog n - 1's (CAT-R1 M3: the book files a
   fact by source, metric, subject and observed time, so two catalogs with one time would be a fact
   equivocation that suspends the key and takes every dataset offline); the tool keeps each catalog
   on disk before signing and checks `sequenceOf` before it posts. One `postReport` per catalog.
3. **Between catalogs, append only.** Catalog n + 1 names the root of catalog n (the newest one
   still anchored: a catalog the book withdrew is set aside and never counts, CAT-R1 H3), keeps
   every dataset of n, and moves each by at most one version: the same leaf, or version v + 1
   whose leaf names version v's root as its previous root. A dataset enters a catalog at version
   1 (previous root zero), or, for a dataset whose versions so far have their own reports, at
   the next version, naming the newest of those (CAT-R1 L2). A node that sees an anchored
   catalog break this, or an anchored catalog n that differs from the one it holds, marks the
   publisher's catalog datasets forked (served no more) until the other side reads withdrawn,
   when the mark clears by itself (CAT-R1 L4, as R-BULK-5).
4. **No second root for one version, whichever anchor** (CAT-R1 M1). A leaf `(dataset, v,
   root)` is accepted only when the book's `sequenceOf(key, sequence_for(dataset, v))` is zero or
   holds a report of that same root, and a held manifest of that version has that root. A
   manifest report posted later for a catalog-anchored version with another root forks the
   dataset, as item 5.
5. **Node.** `POST /v1/bulk/catalogs` takes the header, every leaf and the signed report (at most
   `MAX_CATALOG_LEAVES` leaves and 8 MiB; catalogs count against the publisher's dataset quota
   by leaf, CAT-R1 L1), keeps it only when its report is final in the pinned book with the key
   active (or retired) and not suspended, its predecessor is held (or n = 0), and items 3 and 4
   hold. `GET /v1/bulk/catalogs/{publisher}/{n}` serves a held catalog, so a node that missed one
   can fetch it; a refusal for a missing predecessor names the number needed (CAT-R1 L3). `POST
   /v1/bulk/manifests` then takes a manifest without its own report when a held catalog has its
   leaf, with the manifest's publisher, dataset, version, previous root and root equal to the
   leaf's (CAT-R1 H2). From there it is held exactly as an individually anchored manifest
   (quotas, shards, tiers, reads), anchored by the first held catalog that lists it; the anchor
   check is cached by report digest, so one chain read covers every manifest of a catalog.
6. **Answers and the client check.** Rows of a catalog-anchored version carry the catalog header,
   the leaf with its path, the two neighbouring leaves with their paths (or the first or last
   position), and the catalog's report. `check_rows_answer` checks the leaf against the catalog
   root and that the neighbours' dataset ids bracket it, so the leaf is the only one for its
   dataset in that catalog (CAT-R1 M2 a). `onyx-check --bulk` reads the catalog's report on the
   caller's node at its finalized block (`report`, `factView`, `keyState`) and item 4's
   `sequenceOf`.
7. **Unchanged, and what does not work yet.** Per-manifest anchors stay valid (the first 549
   datasets keep theirs). A storage deal (ADR-0060) checks one manifest's own report on chain
   (`OnyxStorage._checkAnchor`): a dataset anchored only through a catalog cannot back a deal,
   and since the book refuses reports observed more than 7 days ago, a manifest older than that
   can no longer get its own (CAT-R1 M4); a catalog path in the planned OnyxStorage v2
   `createDeal` would lift it. No dataset can be taken down (no tombstone leaf; CAT-R1 L5).

**What it costs in trust.** With one report per manifest the book itself refuses a second manifest
for one version. With catalogs it refuses a second catalog with one number under one key; a
publisher who rewrites a dataset across catalogs, or across keys after a rotation, is caught by
every node that holds both catalogs (items 3 and 4) and by anyone comparing them, then by the
reviewer, not by the book alone. A client that sees one catalog knows its leaf is the only one for
its dataset in that catalog (item 6), not that later catalogs kept it; walking the catalog chain
back (an opt-in `onyx-check --bulk --history`, about 0.6 KiB per catalog) is not built yet (THREATS
CAT-2). Both catalogs are signed by the publisher's key, so the publisher cannot deny either.

## Amendment 2 (2026-11-04, draft 2 after review CAT-RET-R1): catalogs that stay small as they repeat

**Why.** Item 3 makes every catalog repeat every dataset of its publisher. With a catalog a day (the
first publisher's daily refresh), every node stores a whole copy each day: about 1 MB a day for
9,044 datasets now, about 6 MB a day at the 50,000-leaf cap (THREATS CAT-4).

**Draft 1 and its review.** Draft 1 kept only the newest two catalogs whole and let a node with no
catalog of a publisher start at any anchored catalog n. The review (THREATS CAT-RET-R1, verdict
"redesign") showed that starting late reopens CAT-R1 H1 (two groups of nodes serving two roots for
one version), and that pruning blocks recovery from a stolen key and strands versions a slow node
has not taken yet. Draft 1 is withdrawn.

**Draft 2** (not built; to be reviewed again):

1. **First, the missing parts of item 3** (THREATS CAT-5, CAT-6): a catalog the book withdrew is
   set aside before a successor is checked, and a conflicting anchored catalog marks the
   publisher's catalog datasets forked, with both headers, both reports and the leaf proofs kept as
   the fork record (about 2 KiB).
2. **Every catalog kept, but small on disk.** A node keeps the newest catalog whole and stores
   each older one as the leaves that differ from the next catalog, rebuilt when asked
   (`GET /v1/bulk/catalogs/{publisher}/{n}` answers the same bytes as today). Nothing a client or a
   peer checks changes. With 483 of 9,044 datasets changed on the first refresh day, a day costs
   about 60 KB instead of 1 MB. Catalog bytes count against the publisher's byte quota.
3. **Joining stays as built:** a new node takes catalogs from 0 in order, so every node checks
   every successor.

Review CAT-FIX-R1 (2026-11-04) asks three changes before draft 2 is built: a catalog stored as a
diff against the next must be written whole first when the next is set aside; joining gets cheaper
only with a diff route beside the unchanged `GET`; and rebuilding an old catalog from a year of
diffs needs periodic whole checkpoints and a cache. A first build of item 1 was reviewed (THREATS
CAT-FIX-R1: it set catalogs aside on temporary refusals and its fork marks could not clear) and
reverted before it shipped; item 1 is rebuilt with the review's rules (THREATS CAT-5, CAT-6).

## Alternatives## Alternatives
- A new anchor contract (a registry of dataset roots): duplicates the key registry, life cycle and
  replay book `OnyxSignedBook` already has, and needs its own review; rejected.
- One Merkle tree over all rows of a dataset: an append would rebuild it, and a node would need
  every row to prove one; shards bound the work and the memory.
- Fixed byte chunks instead of rows: a buyer would get a chunk, not a row with a proof (ADR-0055).
