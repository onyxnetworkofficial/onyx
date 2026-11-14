# ADR-0059: Bulk datasets: bounded shards, a signed and anchored manifest, proven range reads

## Status
Accepted, 2026-10-29; amendment 1 (catalogs: one anchor for many manifests) accepted
2026-11-03 after review THREATS CAT-R1, built (item 3's withdrawn-catalog and fork-marking rules
on 2026-11-04 after reviews CAT-FIX-R1, CAT-FIX-R2 and CAT-6-R1: THREATS CAT-5, CAT-6);
amendment 2 (catalogs kept small on disk) draft 4 after reviews CAT-RET-R1, CAT-FIX-R1 and
CAT-DIFF-R1, built 2026-11-04; amendment 3 (a client check of a publisher's catalog history)
draft 2 after review CAT-HIST-R1, built 2026-11-05 and fixed after build review CAT-HIST-R2; amendment 4 (taking a dataset down) draft 2 after review CAT-TOMB-R1, built 2026-11-08 without item 5's deletion; its item 5a (erasure) draft 2 after design review CAT-ERASE-R1, slice 1 built 2026-11-09; amendment 5 (widening successors) draft 2 after review BULK-WIDEN-R1, built 2026-11-14 (docs/contributor-loop.md CL5; test network only). Supersedes ADR-0055
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
   the next version, naming the newest of those (CAT-R1 L2). A catalog counts as withdrawn
   only on the book's final word, read at one finalized block: its fact holds no value or
   another report, another signature holds its payload, or it is disputed with its key
   compromised; never a suspended key, an open dispute or a lagging read (THREATS CAT-5).
   A node that sees an anchored
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
   `createDeal` would lift it. No dataset can be taken down (no tombstone leaf; CAT-R1 L5):
   amendment 4 adds tombstones.

**What it costs in trust.** With one report per manifest the book itself refuses a second manifest
for one version. With catalogs it refuses a second catalog with one number under one key; a
publisher who rewrites a dataset across catalogs, or across keys after a rotation, is caught by
every node that holds both catalogs (items 3 and 4) and by anyone comparing them, then by the
reviewer, not by the book alone. A client that sees one catalog knows its leaf is the only one for
its dataset in that catalog (item 6), not that later catalogs kept it; walking the catalog chain
back (an opt-in `onyx-check --bulk --history`) is amendment 3 (THREATS
CAT-1). Both catalogs are signed by the publisher's key, so the publisher cannot deny either.

## Amendment 2 (2026-11-04, draft 2 after review CAT-RET-R1): catalogs that stay small as they repeat

**Why.** Item 3 makes every catalog repeat every dataset of its publisher. With a catalog a day (the
first publisher's daily refresh), every node stores a whole copy each day: about 1 MB a day for
9,044 datasets now, about 6 MB a day at the 50,000-leaf cap (THREATS CAT-4).

**Draft 1 and its review.** Draft 1 kept only the newest two catalogs whole and let a node with no
catalog of a publisher start at any anchored catalog n. The review (THREATS CAT-RET-R1, verdict
"redesign") showed that starting late reopens CAT-R1 H1 (two groups of nodes serving two roots for
one version), and that pruning blocks recovery from a stolen key and strands versions a slow node
has not taken yet. Draft 1 is withdrawn.

**Draft 2** (2026-11-04): item 1, the missing parts of item 3, came first; every catalog kept but
stored small; joining from catalog 0 as built. Review CAT-FIX-R1 asked three changes before it is
built (a diff whose base is set aside, a diff route for joining, checkpoints for old rebuilds).

**Draft 3** (2026-11-04) kept the newest catalog whole and rewrote each older one as a reverse
diff against the next. Review THREATS CAT-DIFF-R1 ("build with changes") found that an
interrupted set-aside could leave diffs whose base was gone (the node would not start), that
joining nodes read catalogs upward while reverse diffs rebuild downward, and that rewriting
files deletes a whole catalog before its diff is durable. It proposed forward diffs; draft 4
takes them.

**Draft 4** (2026-11-04, after review CAT-DIFF-R1; built the same day, `catalog_checkpoint` 32):

1. **Item 3 complete** (THREATS CAT-5, CAT-6, built 2026-11-04 after reviews CAT-FIX-R1,
   CAT-FIX-R2 and CAT-6-R1).
2. **Forward diffs, each file written once.** A catalog whose number is a multiple of 32 is
   stored whole (`{n}.bin`, as today); any other is stored as `{n}.diff`: its header, report and
   anchor read, and the leaves of n that differ from catalog n - 1 (a version moved or a dataset
   added; item 3 lets nothing else change). Files are never rewritten or removed, except by a
   set-aside, which moves them newest first, so the files left are always a prefix 0..k. A whole
   file may exist at any number (the catalogs taken before this change are whole).
3. **Rebuild.** Catalog n is rebuilt from the nearest whole file at or below it plus every diff
   after it, at most 31; each step is checked with `validate()` (leaf count, leaves root, order)
   and against its stored header. The newest catalog is held whole in memory, as now; an older
   one goes through a cache of one rebuilt catalog per publisher, so a node joining (reading 0, 1,
   2, ...) pays one diff per step. Each step checks its leaf count and link; the catalog returned
   is checked whole. A `GET` rebuilds off the async threads; an upload naming an older catalog
   rebuilds in place; at most two rebuilds at a time (review of the build, CAT-DIFF-R2).
4. **Durability.** A new diff or whole file is written to a temporary name, synced, renamed, and
   the folder synced, before the catalog counts as taken. A failed write refuses the upload (the
   publisher sends it again); nothing held is touched.
5. **Start-up.** The newest catalog is rebuilt and checked whole (at most 31 diffs, about what
   SCALE-5's two-catalog check costs); older ones are checked by header, signature and link, as
   now. A diff that cannot be applied when asked answers 500, never "not held".
6. **Unchanged for clients and peers.** `GET /v1/bulk/catalogs/{publisher}/{n}` answers the same
   bytes (Borsh is canonical). A new node still takes catalogs from 0 in order.
7. **Quota.** Catalog bytes (whole and diffs) count against the publisher's byte quota.

**Cost.** The first refresh changed 483 of 9,044 leaves: a diff of about 55 KB plus a whole 1 MB
catalog every 32 days, about 86 KB a day instead of 1 MB (live 2026-11-04: `0.bin` 1,016,139
bytes, `1.bin` 1,018,286 bytes). At the 50,000-leaf cap with 5 % of leaves changing a day: about
0.46 MB a day instead of 6 MB. At 100 % change a diff is about a whole catalog (3 % worse than
today). Storing a moved leaf as its index and new root only would make diffs about 3 times
smaller; left for later.

## Amendment 3 (2026-11-05, draft 2 after review CAT-HIST-R1): a client check of a publisher's catalog history

**Why** (THREATS CAT-1). `onyx-check --bulk` proves rows against one catalog: the leaf is the only
one for its dataset there (amendment 1 item 6). It does not prove that the catalogs before it
moved the dataset append only. A node checks that for each catalog it takes (item 3), but a reader
who trusts no node cannot see it, and a publisher who rewrote a dataset across catalogs (or under
another key) is caught only by nodes that hold both chains.

**Draft 1** walked leaf proofs, one catalog at a time. Review CAT-HIST-R1 ("build with changes")
showed that a bracket proof assumes sorted leaves, which a client cannot check from three leaves
(an anchored catalog listing one dataset twice is refused by honest nodes without a fork mark, and
a colluding node could show each client one root), that the key check refused every honest
history after a stolen-key recovery (CAT-5), that keys seen in the walk miss a fork under a third
key, and that the route cost the node a rebuild and two tree builds for each 2 KB answer.

**Draft 2** (built 2026-11-05; the build review CAT-HIST-R2, "not ready", found two High holes and
eight smaller issues, the review of those fixes, CAT-HIST-R3, two more, and a third look,
CAT-HIST-R4, test gaps and one wrong claim; all fixed before deploy and folded in below):

1. **Route.** `GET /v1/bulk/catalogs/{publisher}/files?from=n` answers, as Borsh, the node's
   newest catalog number and the stored catalogs from n on, each its header, its signed report and
   either every leaf (a whole file) or the leaves that differ from the catalog before (a diff,
   amendment 2): up to the end of n's block of 32, at most 16 MiB (at least one catalog). The node
   writes the answer from its stored bytes: no rebuild, no tree, no leaf decoded. Two answers at a
   time; each is sent in 256 KiB frames and holds its permit until its last frame, or until its
   deadline (60 s, longer for a large answer at 128 KiB/s), when its bytes and permit are let go
   (a reader that stalls then holds only its connection, about 0.7 MiB of queued frames and the
   kernel's buffer, until the node's write stall timeout, 30 s by default, or the hosted proxy's
   75 s write timeout closes it: THREATS SERVE-WRITE-1);
   a third gets 503 `bulk_busy` with `retry-after`. A year of daily catalogs of the first publisher is about 12 requests and 31 MB
   (a whole catalog every 32 days).
2. **Walk** (`onyx-check --bulk <rows> --history <node url>`). From catalog 0 up to the newest
   number of the first answer, the client rebuilds each catalog (a diff on the one before) and
   checks it as a node does: the leaves make the signed leaves root, the report signs the header
   (`catalog::verify`), `validate()` (sorted, unique, shape) and from 1 on `check_successor`
   (every dataset, not only the asked one). The rows answer's catalog must be catalog m of the
   walk (same header root) and list the rows' version. Own-report manifests are followed down
   (`GET /v1/bulk/manifests/{root}`) from below the version the dataset entered the catalogs at,
   and from a rows answer anchored by its own report (its first step taken from the checked
   answer), until a version the catalogs list or another chain met (the roots must match) or
   version 1; each must be that version of the dataset, with its own report. Every key is
   collected before any anchor is read. A manifest the node does not serve leaves the check
   undecided (the node may lag the rows node).
3. **Chain reads, at one finalized block B** (read after the first answer; every read at B). Each
   catalog's and each followed manifest's report as the node checks it (`read_anchor_at`). The
   publisher's keys: every key that signed a catalog or an own-report manifest of the walk, the
   rows answer's key, every key the caller adds (`--keys`), and, when the caller's RPC allows wide
   log ranges, every `KeyRegistered` key of the publisher from `--scan-keys-from <block>` (a log
   whose source is another publisher's is skipped; each key confirmed by `keyState(key).source`).
   For every such key and every catalog number k: `sequenceOf(key, sequence_for_catalog(k))` is
   zero or catalog k's digest; for every version v of the dataset seen: `sequenceOf(key,
   sequence_for(dataset, v))` is zero or a report whose value hash is `keccak(borsh(Value::Bytes(root
   of v)))`. A differing report counts only when it holds at B: withdrawn for good (the CAT-5 rule)
   it is listed as set aside; live it is a rewrite with two signed reports; anything else (open
   dispute, suspended key, not yet final) leaves the check undecided. Last, for every key, the
   catalog after the newest served (catalog 0 when the node serves none) and the version after the
   newest seen: a report there that holds means the node did not serve the newest (undecided).
4. **Outcomes.** Verified (exit 0); refused (exit 1) with `fault` "publisher" (two live signed
   reports, two anchored catalogs with one number, a signed manifest naming another root than the
   catalogs, or an anchored signed catalog that breaks item 3) or "node" (an answer that does not
   make the signed catalog or manifest, or a rule-breaking catalog that is not anchored);
   undecided (exit 3) when something can still change: the caller's node lags, a dispute or
   suspension is open, a report the book withdrew for good is still served (the node has not set
   it aside yet), the history node lags the rows answer's node, a set-aside happened during the
   walk. The report lists catalogs walked, the catalog the dataset entered, each version with its
   root and first catalog, every key with how it was found and the first, last and count of its
   catalogs, set-aside reports, the creation span, the block read and the key scan's range.
5. **What it does not catch.** A key that signed nothing in the walk, that the rows answer did not
   use, and that the caller neither names nor scans is not read: a second catalog, a second root
   for a version, or a newer catalog under it passes; nodes holding the original chain still mark
   a rewrite forked (item 3). The public test-network RPC allows 200 blocks a log query (about
   13,700 queries since the book's deploy block 52,118,336), so the scan is for a local or paid
   RPC. The check also trusts the caller's RPC and the book's reviewer actions, as every check
   does.
6. **Cost.** Chain reads, one at a time: about 3 per catalog, plus keys x (catalogs + versions +
   2): about 2,500 for a year of daily catalogs with two keys. A busy node (429 or 503) is asked
   again every 10 s, up to 7 times. Opt-in; the default check is unchanged.

Not in this amendment: tombstones (CAT-3) and storage deals for catalog-anchored datasets (CAT-2).

## Amendment 4 (2026-11-08, draft 2 after review CAT-TOMB-R1, built without item 5's deletion): taking a dataset down (tombstones)

**Why.** Amendment 1 item 7 and THREATS CAT-3: a catalog keeps every dataset of the one before,
so a publisher cannot take a dataset down, not even one it must withdraw (a licence it lost, a
column it should never have published, a dataset it no longer wants served). The one way out
today is to stop publishing catalogs, which freezes every other dataset too. Draft 1 was
reviewed before any code (THREATS CAT-TOMB-R1: "build with the listed changes"); every finding is
taken below.

**Decision.**

1. **A tombstone leaf.** A catalog may list, for a dataset at version v with root r, the leaf
   `(publisher, dataset, v + 1, prev = r, root = zero)`: the dataset is removed from that catalog
   on. The leaf format and the successor rule do not change: `check_successor`'s one-step rule
   already admits it, and `Catalog::validate` takes a zero root only at version 2 or more, so no
   later leaf can follow a tombstone (it would need a zero previous root past version 1). A
   dataset whose versions so far have their own reports may enter a catalog as a tombstone at
   the next version, naming the newest held one, by amendment 1 item 3's entry rule (CAT-TOMB-R1
   L8); never at version 1.
2. **Final unless the book withdraws it.** A successor keeps a tombstone as it is, so the dataset
   id cannot be used again by that publisher. Only the book's final word on the catalog itself
   (amendment 1 item 3, THREATS CAT-5) undoes it: when the tombstone's catalog is set aside, the
   dataset is back as it was (CAT-TOMB-R1 L9). The tombstone counts against the dataset cap like
   any leaf.
3. **Removal is a derived state** (CAT-TOMB-R1 H1). A dataset is removed on a node while the newest
   catalog it holds for the publisher lists it with a zero root. Nothing held is deleted when the
   catalog is taken: serving, the dataset's info, `GET /v1/bulk` and its search, the rows route,
   and every upload path (`POST /v1/bulk/manifests` with or without `catalog`, shard `PUT`)
   check the state and answer `410 bulk_dataset_removed` with the catalog number, so no re-upload
   of the old manifests or a later own report brings the dataset back. A set-aside of the
   tombstone's catalog ends the state with no other step.
4. **The tombstone is always taken** (CAT-TOMB-R1 M5). A zero-root leaf skips the held-version
   clash and the per-version chain read of amendment 1 item 4: a node that holds an own-report
   version v + 1 still takes the catalog and removes the dataset (and logs the clash), so one
   stray report can neither freeze a publisher's catalogs on a node nor make a dataset
   impossible to remove. Every node then ends at what the publisher signed: removed.
5. **Bytes are kept for now** (CAT-TOMB-R1 H2, M3). The book can withdraw a catalog later
   (`markCompromised` takes any past time), so a thief's tombstones must not cost the owner its
   bytes, and the content store is shared by root across publishers and storage deals. In this
   first build a removed dataset's manifests and shards stay held and counted, and the peer
   routes by root (`GET /v1/bulk/manifests/{root}`, `GET /v1/bulk/shards/{root}`) keep answering,
   so storage deals keep their challenges and the history check can read own-report manifests
   (CAT-TOMB-R1 L7). Erasure after a grace period (item 5a) deletes the shards later.

5a. **Erasure** (draft 2, 2026-11-09, after design review CAT-ERASE-R1; slice 1 built with it).
   Two slices. Slice 1 erases on a node that holds the publisher's catalogs (the query API node)
   and needs no chain write. Slice 2 brings in storage nodes (they hold deals' manifests by root
   and no catalogs) and files the ADR-0060 declines; it is drafted after slice 1.
   - **What is erased.** Only shard files. Manifests, catalogs and set-aside (withdrawn) files are
     never erased: they are small, carry no rows, and the history check reads them (amendment 3;
     CAT-TOMB-R1 L7). Rows are what a takedown is about.
   - **The record.** For each dataset the newest held catalog of its publisher lists as removed,
     the node keeps one durable record `<bulk dir>/removals/<publisher>/<dataset>.bin` (written
     like every bulk file: a temporary file, synced, renamed; loaded at start before any route
     serves): the run's first catalog (number and root), the newest catalog's key, `since`, and a
     phase `Waiting`, `Erasing` (block, time, the shard roots chosen) or `Erased` (the same, plus
     the roots kept as shared). The run is the newest held catalogs, walked back from the newest,
     that list the dataset's tombstone; its first catalog under the newest catalog's key starts
     the grace (two keys taking turns cannot restart it; CAT-ERASE-R1 M2).
   - **`since`** is the later of the chain time the book recorded that first catalog's report and
     the time of the block at which this node took that catalog (its anchor read), so a catalog
     anchored early and uploaded late still shows its removal for the whole grace before any
     erase (CAT-ERASE-R1 M2). Never the catalog's own creation time or the host clock.
   - **When a dataset is due.** An hourly pass (after the bulk sweep) reads at one finalized block
     and takes `now` from that block's own answer. The dataset is due only when: `now >= since +
     grace`; the block is at or past the highest anchor block of the publisher's held catalogs;
     the run's first catalog and the newest both read anchored at that block; the newest
     catalog's key reads `life == 1` (active) and not suspended at that block (`keyState`: an
     anchored report alone is not enough, a retired or compromised key's past facts stay
     anchored; CAT-ERASE-R1 M3); no catalog fork of the publisher stands; and no operator hold
     names the publisher or the dataset.
   - **Candidates and references** (CAT-ERASE-R1 M1; build review CAT-ERASE-R2). The candidates
     are the shard roots of every version of the due dataset, held and set aside. A candidate is
     kept when a held (not set aside) manifest of another dataset names it, of any publisher,
     unless that dataset is `Erasing` or `Erased` and still removed (one that came back is a
     reference again). A dataset due in the same pass is a reference, so a root two removed
     datasets share goes with the last of them, one plan later. Set-aside manifests are no
     references (they are not served). A held manifest that does not decode stops the whole
     pass. The kept roots are written in the record. A node in storage mode
     (`ONYX_STORAGE_CONTRACT` set) does not erase in slice 1: its deals' placements are
     references only slice 2 counts.
   - **One lock** (CAT-ERASE-R1 H1; CAT-ERASE-R2). A store-wide erase lock: every manifest
     upload, every shard write (publisher `PUT`, a storage peer's fetch; taken once the body is
     read and checked) and every catalog upload (a set-aside comes with one) takes it to read,
     always before a publisher lock and never nested; the pass takes it to write from the
     reference count to the end, and under the due publisher's lock re-checks that `removed()`
     still names the dataset with the newest catalog and the run's first held unchanged. A record
     whose dataset came back moves aside at the next pass. Storing an erased root again clears its
     erased mark and its record's entry (written under the record lock).
   - **Crash-safe order.** (1) write `Erasing` with the roots; (2) mark them erased in memory (no
     route serves them; their cached manifest entries are dropped); (3) remove them from the tier
     index (`TieredStore::forget_bulk`) and save it once, so a restart never finds a tracked shard
     without its file; (4) unlink the files, a missing one counting as done; (5) write `Erased`.
     On restart an `Erasing` record redoes (2) to (5) after a fresh reference count, so a root
     referenced meanwhile is kept.
   - **Dry-run and the first erase** (CAT-ERASE-R1 L2; CAT-ERASE-R2). `dry-run` (the default)
     writes only `Waiting` records and logs each plan with a digest of its roots; it marks nothing
     in memory and deletes nothing. With `on`, a pass erases a plan only when an earlier pass
     logged the same digest at an earlier block while the dataset stayed due (a hold or a guard
     that fails drops the plan; a dry run's plan counts), and at most `ONYX_BULK_ERASE_MAX_SHARDS`
     shards a pass (10,000; the first dataset of a pass goes even when larger). A held `Erasing`
     record is not finished, and its roots stay unserved.
   - **Answers.** Rows and info answer `410 bulk_dataset_removed` as before, with the phase and the
     due time; the listing leaves removed datasets out, as before. `GET /v1/bulk/shards/{root}`
     for an erased root answers `410 bulk_shard_removed`. Manifest and catalog routes are
     unchanged. An erased dataset counts toward the publisher's dataset cap and no longer toward
     its byte quota or the node's total (CAT-ERASE-R1 L4).
   - **Withdrawal after erasure** (CAT-ERASE-R1 M4). When the book withdraws the tombstone's
     catalog and a set-aside brings the dataset back, the record moves aside and its roots stop
     being erased; a shard `PUT` then takes any root of that record that a held version names
     (not only the latest version's; versions only append, so the latest names the earlier
     ones' shards), so the publisher's `upload` restores them; until then rows answer
     `409 bulk_shard_missing`. The residual (a compromise found after the grace costs the owner
     that re-upload) is THREATS CAT-ERASE-1.
   - **Config.** `ONYX_BULK_ERASE` = `off`, `dry-run` (default) or `on`;
     `ONYX_BULK_ERASE_GRACE_DAYS` (30, at least 30); `ONYX_BULK_ERASE_HOLD` (publishers or
     `publisher/dataset` names the node keeps); `ONYX_BULK_ERASE_MAX_SHARDS`. `on` is refused in
     storage mode. No "erase now" (a legal order) in this slice: it would skip the guard against
     a thief's tombstone and needs its own review.
   - **Logs.** One line per change of phase and per hold: `bulk: erase: <p>/<d>: waiting, due
     <time>`, `held: <reason>`, `plan <digest>: <n> shards, <k> shared kept, block <b>`,
     `erasing`, `erased`, `withdrawn after erasure: re-upload <n> shards`. `GET /v1/bulk`'s
     `catalogs` gains the counts waiting and erased per publisher.
   - **Storage deals in slice 1** (CAT-ERASE-R1 L3). A storage node fetches a deal's shards from
     its source, possibly this node: once erased here they answer 410, so a placed node that
     finds no other holder fails its challenges and declines. A new deal on a removed own-report
     manifest costs each placed node one decline (THREATS CAT-ERASE-2) until OnyxStorage v2
     (with CAT-2) refuses such deals.
   - **Slice 2a: storage nodes learn of a removal** (draft 2, 2026-11-09, after design review
     CAT-LEARN-R1, built 2026-11-09). No chain write. Kept apart from `removed()`, which stays catalog-only
     (the erase pass reads it); a storage node's own fetches (manifests, shards) never consult it, so
     challenges and earnings are untouched, and shards stay readable by root (THREATS CAT-ERASE-2).
     - *The route.* `GET /v1/bulk/catalogs/{publisher}/leaf/{dataset}` (a peer route, `limited()`):
       the newest held catalog's header, its signed report and the dataset's leaf with its proof,
       built from the newest in-memory catalog body (no rebuild, no chain read, no fetch on a
       request, so nothing loops or amplifies); a node with no catalog of the publisher answers
       from a learned proof it keeps. `404 bulk_no_leaf` when it lists none, `409
       bulk_publisher_forked` under a fork mark (`fork_marked()`), 500 when the newest catalog is not
       held whole. A node holding the catalogs answers from them, never from a learned proof.
     - *Sources* (CAT-LEARN-R1 H1). A paced task beside the storage tick (hourly, a few datasets a
       pass, answers capped at 64 KiB, a short per-call timeout) asks, for each (publisher, dataset)
       its placed deals' held manifests name: first `ONYX_STORAGE_CATALOG_NODES` (the deploy sets
       the hosted query node), then each deal's source, then its peers; every URL until one gives a
       proof that holds (a 404, 409 or live leaf from one never ends the search). `status_json`
       counts datasets never learned.
     - *What it keeps* (CAT-LEARN-R1 M1, L1). Read at one finalized block, every check spelled out:
       the header's publisher is the held manifest's; the header proves the leaf (`proves`: path,
       index, count, neighbours); the leaf's publisher and dataset are the asked ones and it is a
       tombstone (root zero, version 2 or later); the report verifies against the header
       (`cat::verify`) and reads anchored at that block (`read_anchor_at`, which already refuses a
       catalog under a key compromised from before it). Whatever versions it holds: dataset ids are
       final (item 2). Only one version rule: if it holds version t - 1, that root must be the
       tombstone's `prev`, otherwise it is a fork, not kept, and logged. Held versions at t or later
       are logged as the publisher's fault (item 6) and do not stop the removal.
     - *Keeping it* (CAT-LEARN-R1 M2, M3). Each pass reads the kept proof again (one reading per
       report digest, shared by all its tombstones). It is dropped only on the book's final word
       (`Reading.withdrawn`: a withdrawal, another signature); a failed read, a suspension or a
       retired key keeps the last state, as a catalog node keeps a taken catalog. The record keeps
       `first` (number, root, key, recorded time, the time of the block this node first verified it
       at), replaced only on a withdrawal or a new key, beside `newest` (the highest-numbered proof
       that holds), so slice 2b's grace cannot be restarted by newer catalogs. Records live under
       `<bulk dir>/learned/` (a temporary file, synced, renamed; `.bin` only; loaded before any route
       serves). While a proof is kept, rows, info and listing answer `410 bulk_dataset_removed`
       with `learned`.
     - *Config.* `ONYX_STORAGE_REMOVALS` = `on` (default), `log` (records and logs only) or `off`;
       `ONYX_STORAGE_CATALOG_NODES`; both refused without `ONYX_STORAGE_CONTRACT`.
     - *Build review CAT-LEARN-R2.* `first` is kept per key with its report and read again each
       pass (slice 1's rule: the first under the newest key; keys taking turns never restart it); a
       dataset whose last proof goes is asked again in the same pass before it serves; a held catalog
       never stops the learning (anyone may post an old anchored one), and the route prefers a newer
       learned proof; a damaged record moves aside and never stops the start.
   - **Slice 2b (outline).** Declines of each deal on a due removal (`decline(deal, WHOLE_DEAL,
     node)`, paid by the operator, with the ADR-0060 notice), never before the grace counted from
     `first`, with the key active (life 1, as slice 1 needs for anything irreversible), and only once
     no fork stands (a relay can hand out one forked side); erasure only once every placement on a
     root is out.

6. **The history check** (amendment 3). The walk accepts a tombstone as a dataset step. When the
   walk ends the answer's dataset in a tombstone, the outcome is `Removed` (exit 4): the rows
   were the publisher's, and the publisher has withdrawn them; the report names the tombstone's
   catalog number, its creation time and the version it takes. It ranks below every fault and below
   undecided (CAT-TOMB-R1 M4: a publisher cannot turn a rewrite into "removed" by tombstoning
   it). A live report under any key at the tombstone's version or the next one is the
   publisher's fault (CAT-TOMB-R1 L7; the check reads those two, as it reads the version after
   the newest for any dataset). The plain `onyx-check --bulk` does not walk catalogs and so does not
   see a removal: only `--history` does.
7. **The tool.** `onyx-publish catalog-build ... --remove <dataset>[,<dataset>...]` writes the
   tombstones; it is refused for a dataset the previous catalog does not hold, holds as a
   tombstone already, or that `--dirs` also lists; a folder `--dirs` lists for a dataset the
   previous catalog holds as a tombstone is skipped with `SKIPPED ... removed` (CAT-TOMB-R1 M6), and
   `upload` takes a `410 bulk_dataset_removed` as done.

**What it does not do.** It cannot force any copy off another machine; on this node, any held manifest of another dataset that names a removed dataset's shards keeps them, fetchable by root (CAT-ERASE-R1 L5); and it does not remove the
dataset from catalogs already signed (the history check needs them). Until a node runs item 5a
with `ONYX_BULK_ERASE=on`, and on every storage node until slice 2, a removed dataset's bytes stay
and anyone can still fetch its files by root; only its rows, its info and its listing stop.

## Amendment 5 (2026-11-14, draft 2 after review BULK-WIDEN-R1, built 2026-11-14): widening successors (a schema that grows)

**Why** (ROADMAP S23, THREATS BULK-SCHEMA-1, BULK-SRC-1). Item 5 keeps a dataset's schema for good
(`Manifest::check_successor`, crates/onyx-bulk/src/manifest.rs:307-309). A decimal is canonical at
exactly one scale, and extra places are refused, never rounded (`canonical_decimal`, row.rs:70-110).
Each shard is checked against one schema (`check_shard`, shard.rs:142). Since 2026-11-14 the first
publisher's source writes full binary-float digits (`711.5473167999999` where it wrote
`711.5473168`), so 467 datasets' new rows need more places than their version-1 schemas allow, a
few gained a column, and none can be refreshed.

**Decision.** A version may widen the schema for its new shards. Every earlier shard keeps its
bytes, its root, its entry and the schema it was checked under.

1. **Format 2** (`onyx-bulk`).
   - The header (manifest.rs:67-99) keeps every format-1 field in the same order. `schema` is the
     newest schema: new shards are built under it, and answers show values in it.
   - Format 2 adds one field right after `schema` (review BULK-WIDEN-R1 finding 1: `OnyxStorageLib.readHeader` reads
     `shard_count`..`shards_root` as the header's last 84 bytes and refuses any format byte but 1, so that tail stays last):
     `earlier: Vec<EarlierSchema>`, with
     `EarlierSchema { until: u32, schema: Schema }`, oldest first. An entry covers the shards from
     the previous entry's `until` (or 0) up to its own `until`; the newest schema covers
     `[last until, shard_count)`. `schema_of(i)` gives the schema of shard i.
   - `validate` requires: format 1 has no `earlier`; format 2 has 1 to 7 earlier schemas; `until`
     strictly increasing, at least 1 and below `shard_count`; every schema valid; each schema widens
     to the next (item 2), and the last earlier one widens to `schema`.
   - Borsh: a hand-written impl writes and reads the tail only in format 2. Format-1 bytes stay
     exactly as today, so every existing manifest, and every dataset that never widens, keeps its
     bytes, root and readers. A manifest with one schema is always format 1.
   - The root formula stays `keccak256("onyx-bulk-manifest-v1" ‖ borsh(header))`
     (manifest.rs:103-107); its first byte is the format, which keeps format-1 and format-2
     preimages apart. Anchors (anchor.rs:74-84), sequences and catalog leaves do not change.
   - A widened dataset cannot back a storage deal until an OnyxStorage that accepts format 2 is deployed (the
     deployed contract is immutable and refuses it); storage nodes refuse a deal on one rather than fail its checks.
   - `validate` refuses more than 7 earlier schemas before decoding them, and a shard root that appears in two
     schema ranges (the node checks an upload against the first index that names a root, bulk_serve.rs:3021-3042).
   - Shard entries do not change (84 bytes): a shard's schema follows from its index, and the entry
     proof binds the index (`proves_entry`, manifest.rs:157-162).

2. **Widening rule** (`Schema::widens(old, new)`).
   - Allowed at an old column's position: a decimal's scale grows (up to 38, schema.rs:12); a
     non-key `int` becomes `decimal`. Nothing else: nullability stays (review finding 3: a not-null text column
     stores an empty field as `""`, a nullable one as null, input.rs:97-98, so relaxing it would change how the same
     source value is stored).
   - Allowed after the old columns: new columns, each nullable, up to 64 in all (schema.rs:8).
   - At least one change is required.
   - Refused: renaming, reordering or dropping a column; a smaller scale; decimal to int; any change
     to or from text; any change of nullability; an appended column that is not nullable; any change
     to the key column or its index (schema.rs:97-106).
   - Why this is safe: every row of an old shard maps by one exact rule (item 4's `present`) to a
     row shown under `new`, and no value changes: fraction zeros are added (input already pads this
     way, row.rs:96-101); an int is written with zero fraction digits; a column the row lacked reads
     null. Shown rows are for display and checks only: a padded decimal can pass the 80-character cap of stored
     cells (row.rs:72-77), so nothing validates shown cells as input (review finding 5). `present` composes:
     showing s0 at s2 equals showing s0 at s1 and then at s2 (a test), since `validate` checks only adjacent pairs. Names and positions never move, so `values` keys (`cells_json`, row.rs:203-214) and the
     key (`key_of`, row.rs:194-199) read the same under every schema of a dataset. Text has no exact
     widening. The key stays a non-null `int` for range reads and shard order.

3. **Successor rule** (replaces manifest.rs:307-309; the rest of `check_successor` stays).
   - If `schema` equals the previous version's, `earlier` must equal the previous `earlier`.
   - Otherwise `widens(prev.schema, schema)` must hold, and `earlier` must be the previous `earlier`
     followed by exactly `{until: prev.shard_count, schema: prev.schema}`.
   - So earlier schemas and their boundaries stay byte for byte, an old shard can never be
     relabelled, and a version widens at most once. The newest schema covers the version's new
     shards (at least one is required, manifest.rs:316-320). `Manifest::build` (manifest.rs:219-248)
     derives `earlier` from `prev`. Nodes take versions in order through this check
     (bulk_serve.rs:2753-2768) and recheck chains at start (bulk_serve.rs:2505).

4. **Readers.** Each shard is checked and decoded under `schema_of(index)`, never under the newest
   schema. `row::present(from, to, cells)` then shows its rows under `schema`: a decimal is padded to
   the new scale; an int is written as `i.000…`; appended columns are null. Showing a value this way
   never rounds and never fails. `MAX_DECIMAL_CHARS` (80, row.rs:14) bounds stored cells only, so a
   shown decimal can reach 117 characters (80 plus 37 places); readers must not cut it. Without the
   change, decoding an old shard under the newest schema fails loudly at `check_shard`; it never
   silently gives wrong values.
   - *Node.* `entry_of` returns `schema_of(entry.index)` of the version that names the root
     (bulk_serve.rs:3042), which covers the publisher's `PUT`, a peer's `put_shard`, and the restore
     after a withdrawn tombstone (amendment 4 item 5a). `rows` loads each shard under that schema
     (bulk_serve.rs:3586), so the load cache key `(root, schema hash)` (bulk_serve.rs:905-906, 3239)
     stays the same across widenings and no shard is reloaded. `row_json` (wire.rs:120-130) decodes
     under that schema, presents under `schema`, and in format 2 adds `"schema": k` (the shard's
     schema index). `header_json` (bulk_serve.rs:3643-3664) adds `format` and `schemas` (each
     `until` with its schema); `schema` stays the newest. Storage `read_rows` checks each shard under
     `schema_of(e.index)` (storage.rs:3324-3336).
   - *Client check* (`check_rows_answer`, wire.rs:156-287; `onyx-check --bulk`,
     crates/onyx-check/src/lib.rs:813). `validate` (wire.rs:164) checks the whole schema chain. Each
     row is decoded under the schema of its proven shard index (wire.rs:245). `values`, if sent, must
     equal `cells_json(schema, present(...))`; `schema`, if sent, must equal the index. So every
     `values` object in an answer fits `manifest.schema`, and a node can neither show an old row
     under another schema nor choose between the stored and padded forms.
   - *Padding at serve time.* Proofs cover `row`, the stored bytes. `values` was always derived from
     them and checked against them (wire.rs:247-253), so every proof stays verifiable. Across
     versions a value's text may gain zeros (`711.5473168`, later `711.5473168000000`); its number
     never changes.
   - *Website.* `apps/web/verify` has no bulk check (its Cargo.toml has no `onyx-bulk`). The
     datasets page shows `values` by `manifest.schema.columns` (apps/web/src/lib/datasets.ts:144-160)
     and trims padding zeros first (`trimDec`, datasets.ts:133-135), so padded and stored forms look
     alike and its 80-character cut (datasets.ts:158) never meets a padded value. A future bulk
     check in the WASM verifier must call `check_rows_answer`.
   - *AI connector.* It passes on the node's answer and checks it with the `onyx-check` binary
     (apps/agent/src/check.mjs:1-2).

5. **Proofs, roots, catalogs.** The header root covers every schema and boundary, so the publisher's
   one signature fixes the schema of every shard. A row of an old shard is checked against the
   newest manifest as before (row to shard root, unchanged entry to `shards_root`, header to root,
   root to the signature); only the decoding schema changes, and the proven index chooses it.
   Amendments 1 to 4 are not affected: a leaf names a manifest by its root (catalog.rs:49-60,
   98-100); catalog successors, diffs, tombstones and erasure work by roots; the history walk decodes
   own-report manifests with `Manifest::from_bytes` (bulk_history.rs:285).

6. **Limits.** At most 8 schemas per dataset (7 widenings, at most one per version). A schema is at
   most 4,486 Borsh bytes (64 columns of 70 bytes), so a header stays under 40 KB (80 KB as hex in
   each rows answer) and a manifest well under 8 MiB (manifest.rs:24). Scales stay within 1 to 38. A
   publisher that widens every day hits `bulk_not_successor` ("8 schemas") within a week and must
   start a new id; the tool suggests headroom so that one widening lasts. Each widening closes the
   old schema's last shard, possibly small: at most 7 short shards per dataset.

7. **Tool.** `onyx-publish build --prev DIR --schema new.json` refuses a schema that differs from
   the previous version's unless `--widen` is given; the refusal names the first column and the
   decimal places its rows need. New: `onyx-publish schema-fit --prev DIR --input FILE --out new.json`
   writes one widening meant to last (review finding 4: binary-float text needs more places as values get
   smaller, so headroom 2 would spend the 7 widenings): a decimal column gets the places 17 significant digits need
   at the column's smallest non-zero magnitude in the input, plus 2, at most 38; new input columns are appended as
   nullable; rows no widening can take (a value past 38 places or 80 characters) are named.

8. **Rollout.** (0) A release that reads format 2 but refuses a format-2 upload unless
   `ONYX_BULK_FORMAT2=on` (review finding 2: `BulkStore::open` decodes every held manifest and stops the start on any
   error, bulk_serve.rs:1136-1137, so a node that took one format-2 manifest cannot be rolled back to a format-1
   binary); the flag goes on only once that release is the rollback target. (1) Nodes first, query and storage: they
   take format 2 and serve format 1 byte for byte as today. A node not yet upgraded refuses a format-2 manifest (manifest.rs:112-114); it
   still takes the catalog (leaves only) and keeps serving the earlier version, so that dataset lags;
   there is no fork. (2) Then `onyx-check` and the connector's binary: an older check refuses an
   honest format-2 answer (wire.rs:161-163, exit 1), so the publisher guide says so; from now on a
   header format the check does not know gives exit 3, "update onyx-check", but only when the raw header's root
   verifies against the signed report (review finding 6: else any node could turn a refusal into "undecided" with a
   fake format byte) and, on the reader's node, the book holds that report for the publisher's registered key
   (review BULK-WIDEN-R2 finding 1: any fresh key verifies off chain; `--unchecked` says it was not read on chain);
   the history walk (bulk_history.rs:283-286) treats an unknown format the same way (finding 7), its bytes tied to
   the root by the header the manifest answer carries (`header`; review BULK-WIDEN-R2 finding 2).
   A node that upgrades after a widening refuses later versions ("versions are taken in order",
   bulk_serve.rs:2779-2785) until the missed widened version is uploaded again with its catalog number; `upload`
   steps back on that 409 (finding 9). (3) Then publishers: the
   first publisher widens the 467 datasets in one catalog (one `postReport`). Storage nodes must
   upgrade before a widened own-report manifest backs a deal; catalog-only datasets cannot back deals
   yet (CAT-2).

**Tests.** `onyx-bulk`: `widen_1_format_1_bytes_and_roots_are_unchanged` (a golden format-1 root;
refuses a format-1 header with a tail and a format-2 header with no earlier schema),
`widen_2_scale_int_to_decimal_nullable_and_appended_columns_widen`,
`widen_3_every_other_change_is_refused`, `widen_4_earlier_schemas_and_boundaries_are_kept_exactly`,
`widen_5_a_ninth_schema_is_refused`, `present_pads_exactly_and_never_rounds`,
`widen_6_an_answer_across_three_schemas_checks`,
`widen_7_rows_under_another_schema_or_unpadded_values_are_refused`. `onyx-api`:
`widen_old_shards_load_under_their_own_schema_without_reload`,
`widen_shard_put_and_restore_check_an_old_root_under_its_schema`,
`widen_a_relabelling_successor_is_refused_409`, `widen_a_restart_rechecks_a_format_2_chain`,
`widen_storage_read_rows_checks_each_shard_under_its_schema`. `onyx-publish`:
`build_widen_needs_the_flag_and_writes_format_2`, `schema_fit_writes_the_narrowest_widening`.
`onyx-check`: `bulk_widened_answer_verifies_and_an_unknown_format_is_undecided`,
`bulk_an_unknown_format_with_a_wrong_root_is_refused`. Fuzz: `bulk_parsers` arm 0 asserts that a decoded manifest
encodes back to the same bytes (the hand-written Borsh). Website: `sample` of
a widened answer shows the same text as before.

**THREATS** (beside BULK-SCHEMA-1): BULK-WIDEN-1 (a successor relabels an earlier shard's schema or
boundary), BULK-WIDEN-2 (a node decodes an old shard under another schema or sends `values` that are
not the proven row's; High), BULK-WIDEN-3 (a padded value rounded or cut), BULK-WIDEN-4 (header
growth), BULK-WIDEN-5 (older readers refuse an honest format-2 answer; accepted with item 8's
order), BULK-WIDEN-6 (a reader that typed a column as int or not null meets decimals or nulls;
accepted, each answer's `schema` describes every value).

**Not in this amendment.** S23's link from an ended id to a new one; dropping or renaming columns;
narrowing a schema.

**Built** (2026-11-14) as decided, with every test named above, plus: `format_2_validate_checks_the_schema_chain`,
`present_composes_across_widenings` and `an_unknown_format_is_undecided_only_when_its_root_is_signed` (`onyx-bulk`),
`widen_a_format_2_upload_is_refused_until_the_flag_is_on`,
`widen_the_history_walk_blames_unknown_format_bytes_that_are_not_the_root`,
`widen_an_unknown_format_is_undecided_only_when_its_header_is_the_root` and
`widen_with_the_flag_off_a_held_format_2_version_answers_200_and_its_fork_is_recorded` (`onyx-api`),
`bulk_an_unknown_format_signed_by_another_key_is_refused_on_chain` (`onyx-check`), `widen_format_2_uploads_are_off_until_set_on` (`onyx-node`) and
`upload_steps_back_when_a_node_lacks_the_previous_version` (`onyx-publish`). Choices the text left open: a format-2
upload with the setting off is `422 bulk_format_not_taken`, checked only where a manifest would be stored (a new
version, or one in a withdrawn version's place): the same manifest again is still `200`, and a second anchored
manifest for a held version still marks the dataset forked (the mark and `forked.txt`, never that manifest; review
BULK-WIDEN-R2 finding 3); `header_json` names `schemas` for format 1 too (one
entry); a ninth schema is refused by `Manifest::build` and `check_successor` ("8 schemas"), but a node meets it first
in the decoder's cap (item 1), so it answers `422 bulk_bad_manifest` ("at most 8 schemas"), not item 6's
`bulk_not_successor`; the history walk cannot split an unknown format's header from its shard list, so
`GET /v1/bulk/manifests/{root}` carries the header's own bytes (`header`) and the walk requires them to hash to the
root it followed and the manifest to start with them, else the node is at fault; `schema-fit` keeps a column whose
input fits its scale (the narrowest widening), types an added column by its values (int, decimal at the fit
scale, else text) and caps the fit scale so the longest value stays within 80 characters (an added column gets
the same checks as a widened one; review BULK-WIDEN-R2 finding 4); `build --prev` records the previous folder in `manifest.json` (`prev_dir`, relative to the new folder, so the file names no absolute path) and `upload` records the
catalog it used (`uploaded.json`), which the step back reads.

**Alternatives considered.** Re-encode old shards under the new schema (every root changes, every
served proof, storage deal, erasure record and own-report history breaks, GBs uploaded again;
rejected). A new dataset id and a tombstone (the whole history of 467 datasets uploaded again,
every link to the old id breaks; kept for the changes item 2 refuses). Round to the old scale
(changes the source's values; forbidden). A schema index in each shard entry (changes the entry
layout and every carried-over leaf, while the proven index already fixes the schema; rejected).
Show stored forms unpadded (one column would mix scales within an answer; rejected).

## Alternatives
- A new anchor contract (a registry of dataset roots): duplicates the key registry, life cycle and
  replay book `OnyxSignedBook` already has, and needs its own review; rejected.
- One Merkle tree over all rows of a dataset: an append would rebuild it, and a node would need
  every row to prove one; shards bound the work and the memory.
- Fixed byte chunks instead of rows: a buyer would get a chunk, not a row with a proof (ADR-0055).
