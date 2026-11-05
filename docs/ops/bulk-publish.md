# Publish a large dataset on the test network (Base Sepolia)

For a publisher putting its own data (for example years of on-chain metrics, many GB) on the Onyx
test network. Test network only: no real money, a test key only. Design: ADR-0059; rules:
docs/spec/bulk-datasets.md; a full local rehearsal: `scripts/bulk-demo.sh local`.

**What you end up with:** your rows on an Onyx node, readable by anyone for free, each row with
a proof that it is exactly what you signed. One small transaction per version.

**Time:** about 1 hour the first time (most of it waiting: 20 to 40 minutes for the reviewer and
for Base Sepolia to make your anchor final). Later versions: about 30 minutes, mostly waiting.

## Before you start (once)

1. **Pick a neutral publisher name and domain.** The name is public and permanent. It must not
   be your name, a person's name, a company or product name, or anything that points to Onyx or
   to you. Use a short descriptive id in lowercase with dots or dashes, for example
   `onchain-metrics.pub`, and a domain you control that is equally neutral (for example a new
   domain or a subdomain used only for this). You need to be able to put one file on that domain
   over HTTPS.
   The book records the domain as your *origin*, and one origin belongs to one publisher. An
   origin may also be a domain and a path (`example.org/archive`): the keys document is then at
   `https://<origin>/.well-known/onyx-source-keys.json`.
2. **Ask the Onyx operator for three things:** the node address (`https://...`), the signed book
   address (`0x...`, the `signedBook` of the node's deployment manifest), and who the reviewer
   is. The node must run with `ONYX_BULK_DIR` (docs/ops/config.md).
3. **Get the tools**: on Linux x86-64, download `onyx-publish-linux-x86_64.tar.gz` from the latest
   test-network release (the Add data page links it) and unpack it; the programs are in
   `onyx-publish/bin/`. Elsewhere, build them (about 15 minutes): in the Onyx folder run
   `cargo build --release -p onyx-publish -p onyx-check`; they are then in `target/release/`.
4. **Make sure the data is yours to share.** Only data you own or may share, whose terms let
   anyone redistribute it, and that names no individual person. You will sign a statement saying
   so (step 7 below does it for you).

## Step by step

All commands below are run in one folder, for example `~/onyx-publisher`. Replace the values in
`<...>`.

1. **Make your test key (1 minute).**
   `onyx-publish keygen --out publisher.key`
   It prints only your address (`0x...`). The key file is readable by you only. Copy it to a USB
   stick or another safe offline place now: if you lose it you need a new key (see "If something
   goes wrong"). Never send the key file to anyone; send only the address.
2. **Make your keys document (1 minute).**
   `onyx-publish keys-doc --key publisher.key --publisher <publisher id> --out onyx-source-keys.json`
   Put that file on your domain so that
   `https://<your domain>/.well-known/onyx-source-keys.json` shows it.
3. **Ask the reviewer to register your key (wait: minutes to a day).** Send the reviewer your
   publisher id, your domain and your address. The reviewer opens your keys document, checks it,
   and registers the key in the signed book (see "For the operator").
4. **Get test ETH for your address (5 minutes).** Use a Base Sepolia faucet (Base's documentation
   lists them under "Network faucets"). One anchor costs about 250,000 gas, a tiny amount of
   test ETH.
5. **Prepare your data (depends on you).** One CSV file with a header line (or JSON Lines), sorted
   by its key column (usually a time in unix seconds or a block height). Write a `schema.json`
   that names every column, for example:
   ```json
   {"columns": [
     {"name": "t", "kind": "int"},
     {"name": "metric", "kind": "text"},
     {"name": "value", "kind": "decimal", "scale": 8, "nullable": true}
   ], "key": "t"}
   ```
   Kinds: `int` (whole numbers), `decimal` (with a fixed number of decimals, `scale`; a value with
   more decimals is refused, never rounded), `text`. There are no floating-point numbers. An
   empty field is allowed only in a column marked `"nullable": true`.
6. **Build the shards and the manifest (about 1 minute per GB).**
   `onyx-publish build --schema schema.json --input data.csv --dataset <dataset id> --publisher <publisher id> --license CC0-1.0 --out v1`
   It checks every row and stops at the first bad one with its line number. `--license` is the
   license your data is under (an SPDX id such as `CC0-1.0` or `CC-BY-4.0`; add
   `--attribution "<text>"` if it requires credit). It prints the row and shard counts and the
   manifest root. Shards are about 8 MiB each (`--shard-mib`, at most 64; keep 8 unless your
   upload line is fast).
7. **Sign (seconds).** `onyx-publish sign --key publisher.key --dir v1`
   This signs the manifest root and the rights statement with your key.
8. **Anchor on Base Sepolia (1 minute).**
   `onyx-publish anchor --dir v1 --rpc https://sepolia.base.org --chain-id 84532 --book <signed book address> --gas-key publisher.key`
   It prints `ANCHORED tx=... block=...`. Running it twice is safe: the second time sends nothing.
9. **Upload (wait 20 to 40 minutes, then about 1 minute per GB on a fast line).**
   `onyx-publish upload --dir v1 --node <node address> --wait-secs 3600`
   The node takes your manifest only once Base Sepolia has made the anchor final, so the command
   waits and says so every 15 seconds. Then it sends every shard, asks the node again which shards
   it still misses, sends those again (at most 3 rounds), and prints `UPLOADED ... rounds=N`. If it
   stops halfway, run the same command again: it sends only what the node is still missing.
10. **Check it like a buyer would (2 minutes).**
    `curl -s "<node address>/v1/bulk/<publisher id>/<dataset id>/rows?from=<a key>&to=<a later key>&limit=100" > rows.json`
    then
    `onyx-check --bulk rows.json --publisher <publisher id> --dataset <dataset id> --from <a key> --to <a later key> --rpc https://sepolia.base.org --chain-id 84532 --book <signed book address>`
    It prints `"ok": true` with your publisher, dataset and row count when every row is proven.
    Add `--history <node address>` to check every catalog of the publisher from 0 as well (catalogs
    only; about 12 requests and 31 MB for a year of daily catalogs). It reports `"fault"`
    `publisher` or `node` when it refuses, and says what it cannot catch without
    `--scan-keys-from` (ADR-0059 amendment 3).

## Add more data later

Make a CSV with only the new rows (keys at or after the last key already published), then:

1. `onyx-publish build --schema schema.json --input new.csv --dataset <dataset id> --publisher <publisher id> --license CC0-1.0 --out v2 --prev v1`
2. `onyx-publish sign --key publisher.key --dir v2`
3. `onyx-publish anchor --dir v2 --rpc https://sepolia.base.org --chain-id 84532 --book <signed book address> --gas-key publisher.key`
4. `onyx-publish upload --dir v2 --node <node address> --wait-secs 3600`

Version 2 keeps every shard of version 1 and adds the new ones; only the new shards are uploaded.
The schema, license and names stay the same across versions. Keep every version folder.

Write the new CSV with every column of version 1, and a value in every cell the source has. An
exporter that picks its columns again from only the new rows can lose one: over a day or two, two
columns often hold the same values by chance, or one is empty. `build` publishes an empty cell of a
nullable column as empty, and a published version never changes (THREATS BULK-COL-1). Decide which
columns to drop (duplicates, all-empty) over the whole history, as for version 1.

## Many datasets with one anchor (catalogs)

Each `anchor` above is one transaction. With many datasets, or a refresh of all of them, anchor
one **catalog** instead (ADR-0059 amendment 1): it lists the newest manifest of each dataset, and
one `postReport` covers them all.

1. Build and sign nothing per dataset: run only `onyx-publish build` for each new version folder.
2. List those folders, one per line, in a file (`dirs.txt`).
3. `onyx-publish catalog-build --publisher <publisher id> --dirs dirs.txt --out catalog0.bin`
   (the next time: `--prev catalog0.bin --out catalog1.bin`; datasets you do not list keep the
   version the previous catalog had).
4. `onyx-publish catalog-sign --key publisher.key --catalog catalog0.bin`
5. `onyx-publish catalog-anchor --catalog catalog0.bin --rpc https://sepolia.base.org --chain-id 84532 --book <signed book address> --gas-key publisher.key`
6. `onyx-publish catalog-upload --catalog catalog0.bin --node <node address> --wait-secs 3600`
7. For each folder: `onyx-publish upload --dir <folder> --node <node address> --catalog 0`

Rules: catalogs go to a node in number order; each lists every dataset of the one before and moves
each by at most one version; a dataset enters at version 1, or at the version after the newest one
it already has with its own anchor. Keep every catalog file and its `.report`: a catalog is never
rebuilt or signed twice under one number (the book would suspend your key). A dataset that only a
catalog anchors cannot back a storage deal yet (ADR-0059 amendment 1 item 7).

## If something goes wrong

| Message | What it means | What to do |
|---|---|---|
| `line N: ...` during build | A row does not fit the schema (a letter in a number, too many decimals, an empty required field, keys out of order) | Fix that line (or the schema) and build again into a new folder |
| `postReport: would revert` during anchor | The book does not know your key yet, or the key is retired, compromised or suspended | Wait for the reviewer's registration; check your address with `onyx-publish address --key publisher.key` |
| `this key already anchored another manifest for version N` | You built and anchored a different version N before (each version is signed under a number fixed by the dataset and version) | Never anchor two different manifests for one version: the book would suspend your key. Build the next version with `--prev` instead |
| `bulk_not_anchored` after the wait | The anchor is not final yet, or not in this node's book | Run upload again later; check `--book` is the node's book |
| `bulk_quota_exceeded` | The node's limit for one publisher (16 GiB by default) | Ask the operator to raise `ONYX_BULK_QUOTA_GIB` |
| `bulk_not_servable` when reading | Your key was marked compromised or suspended | See "Lost or stolen key" |
| `bulk_publisher_forked` or `bulk_dataset_forked` with `fork` | Two anchored catalogs of yours cannot both stand (a stolen key, or two machines publishing) | See "Lost or stolen key"; the mark clears by itself once the book withdraws one side |

**Lost or stolen key.** Tell the reviewer at once, with the time you think it was stolen. The
reviewer marks the key compromised from that time: what it signed after that time stops counting
(nodes serve your newest version from before it within 5 minutes; clients refuse the rest at
once). Make a new key (steps 1 to 4 with the same publisher id and domain; add the new address to
your keys document). If a thief published a version, publish your own next version from your last
good folder with the new key: it replaces the thief's. A version of your own made after the theft
time can be signed again with the new key (`sign`, `anchor`, `upload` on its folder) while it is
less than 7 days old. A lost (not stolen) key is only retired: what it signed keeps being served,
and your next version is signed with the new key.

**A stolen key and catalogs** (THREATS CAT-5, CAT-6), in this order:
1. The reviewer marks the stolen key compromised (`markCompromised`) with `since` at or before the
   time the book recorded the thief's first catalog (`report(digest)` gives `recordedAt`). Never
   clear a dispute about your catalog with `resolveFact(fact, 0)` first: that withdraws your side,
   and nodes holding the thief's catalog would keep it.
2. Wait until that transaction is final (about 15 minutes on Base Sepolia).
3. Build your replacement from your last good catalog (`catalog-build --prev` that catalog's
   file), sign it with the new key, anchor it and upload it. Nodes set the thief's catalogs aside
   and take yours; a dataset version yours lists unchanged keeps serving.
4. Run `upload --catalog N` for your datasets again.

Posted before step 2 is final, your replacement and the thief's catalog are both anchored: every
node that holds the thief's catalog marks your catalog datasets forked (not served, uploads
refused) until the compromise is final and read, at most 5 minutes later. While a fork stands
the node serves none of your catalog datasets, even versions signed by another key; with an
equivocation alone (one key, one number) the key is only suspended. Two machines publishing
catalogs under two keys fork you the same way: publish from one place.

## For the operator

- Run the node with `ONYX_BULK_DIR` (and the signed book pinned in its manifest; docs/ops/config.md
  lists the quota settings).
- Register a publisher's key only after opening `https://<its domain>/.well-known/onyx-source-keys.json`
  and checking the name is neutral (no person, company, product or Onyx). The provenance hash is the
  Keccak of the exact document bytes (`onyx-publish keys-doc` prints it). With the reviewer's key:
  `cast send <book> 'registerKey(string,address,string,bytes32,string)' <publisher id> <address> <domain> <provenance hash> "bulk publisher reviewed"`
  (the reviewer key never goes on the node).
- To stop serving a dataset, remove its folder under `ONYX_BULK_DIR/manifests/` and restart.
- `ONYX_BULK_DIR/catalogs/<publisher>/` holds every 32nd catalog whole (`{n}.bin`) and the others
  as diffs (`{n}.diff`); never delete one in the middle (every later diff up to the next whole one
  needs it), and never roll the node back to a release from before diffs once a `.diff` exists
  (it would not see them; a number with both files stops the start). Catalogs set aside go to
  `withdrawn/`; `fork.bin` is a catalog fork record (THREATS
  CAT-6). Catalog bytes count in the publisher's quota.
