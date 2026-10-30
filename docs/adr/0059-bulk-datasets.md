# ADR-0059: Bulk datasets: bounded shards, a signed and anchored manifest, proven range reads

## Status
Accepted, 2026-10-29 (docs/contributor-loop.md CL5; test network only). Supersedes ADR-0055
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

## Alternatives
- A new anchor contract (a registry of dataset roots): duplicates the key registry, life cycle and
  replay book `OnyxSignedBook` already has, and needs its own review; rejected.
- One Merkle tree over all rows of a dataset: an append would rebuild it, and a node would need
  every row to prove one; shards bound the work and the memory.
- Fixed byte chunks instead of rows: a buyer would get a chunk, not a row with a proof (ADR-0055).
