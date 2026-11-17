# Publish a large dataset on the test network (Base Sepolia)

For a publisher putting its own data (for example years of on-chain metrics, many GB) on the Onyx
test network. Test network only: no real money, a test key only. Design: ADR-0059; rules:
docs/spec/bulk-datasets.md; a full local rehearsal: `scripts/bulk-demo.sh local`.

**What you end up with:** your rows on an Onyx node, readable by anyone for free, each row with
a proof that it is exactly what you signed. One small transaction per version.

**Time:** about 40 minutes of your own time the first time, and a review of up to 5 days in
between. Later versions: about 30 minutes, mostly waiting for Base Sepolia.

**Two paths.** If you only know of data Onyx should have, *suggest* it on the website's Add data
page: five questions, no key, nothing is published. This guide is the other path: you hold data
and will sign it. That needs a key, an application and a review, once (ADR-0066). The website's
Publish data page (`#/publish`) has the same ten steps in short, and the status list with a lookup.

## Before you start (once)

1. **Pick a neutral publisher id.** The id is public and permanent: it is written into the book
   on chain and into every answer. It is a label for the data, not a signature of who you are.
   So it must describe the data and nothing else: not your name or any person's, not a company
   or product name, and not the word Onyx or anything that reads as if the network itself
   published the data. A reader must not be able to tell from the id who is behind it; what they
   trust is your key and your signed rights statement, never the name. Use a short descriptive
   id in lowercase with dots or dashes, for example (made up) `river-levels.open`.
   The tools refuse an id that holds the word `official`, `verified`, `reviewer` or `operator`,
   that contains `onyx` anywhere, or that equals a registered id once `.`, `:`, `-` and `_` are
   dropped. No id is reserved by asking: the first valid application for an id wins.
2. **Pick a neutral origin.** The origin is where your keys document lives: an HTTPS host you
   control, or a host and a path (`data.example.org/river-levels`), equally neutral (a new
   domain or a subdomain used only for this). You must be able to put files there. The book
   binds one origin to one publisher for good.
   - **Lose this origin and you lose the proof of your id.** Keep the domain paid.
   - An origin is refused when it is a prefix of a registered origin or the other way round
     (`example.org` and `example.org/a`), and when it is on a shared pages host: someone else
     could take the name once you give it up.
   - A domain's registration record can name its owner. Check yours before you apply.
3. **Note the test network's addresses.**
   - Node address: `https://onyx.cryptagraph.com` (the hosted test node; it takes bulk uploads).
   - Signed book address: `0x04bb31d80c8ce9df02c8a03486b5812ec6e5ccc5` (the `OnyxSignedBook`
     contract on Base Sepolia, chain id 84532; listed in `deploy/early-record/contracts.json` and
     docs/ops/deployment.md).
   - The reviewer is reached in one way: an issue on the public repository,
     https://github.com/onyxnetworkofficial/onyx/issues, titled
     `Publisher application: <publisher id>` (step 7 says what goes in it). The issue is public
     and shows the account that opens it.

   On another network or your own node, ask its operator for the same addresses; that node must
   run with `ONYX_BULK_DIR` (docs/ops/config.md).
4. **Get the tools**: on Linux x86-64, download `onyx-publish-linux-x86_64.tar.gz` from the latest
   test-network release (the website's Publish data page links it) and unpack it
   (`tar -xzf onyx-publish-linux-x86_64.tar.gz`); the programs are in `onyx-publish/bin/`. Put
   them on the path of your terminal, since every command below is a bare `onyx-publish ...`:
   `export PATH="$PWD/onyx-publish/bin:$PATH"`
   Elsewhere, build the same release from the source (about 15 minutes, with a Rust toolchain;
   the release tag, such as `v0.5.0-testnet`, is on the release's page):
   `git clone --branch <release tag> https://github.com/onyxnetworkofficial/onyx && cd onyx`, then
   `cargo build --release -p onyx-publish -p onyx-check`; they are then in `target/release/`:
   `export PATH="$PWD/target/release:$PATH"`
   The path line holds for the terminal it is typed in. The onboarding commands (`apply`,
   `doctor`, `verify-application`, `request`) are in the tools from `v0.5.0-testnet` on: a
   download that answers `unknown command apply` is older, and so is a build of an older tag.
5. **Make sure the data is yours to share.** Only data you own or may share, whose terms let
   anyone redistribute it, and that names no individual person. You sign a statement saying so
   (step 4 does it for you), and the reviewer reads your first dataset.

**One reviewer, on a test network.** One person holds the one key that can register a
publisher, and decides within 5 days. A registration cannot be undone, so the review comes
first. What the reviewer checks: [publisher-review.md](publisher-review.md).

## Step by step

All commands are run in one folder, for example `~/onyx-publisher`. Replace the values in
`<...>`. The subfolder `site/` mirrors your origin: what is in it goes online, nothing else.
At any time, `onyx-publish doctor` names the step that is missing and the command for it; it
sends nothing.

| # | Step | Time | Leaves your machine? |
|---|---|---|---|
| 1 | Make two keys | 2 minutes | Never |
| 2 | Make your keys document | 1 minute | Yes, public |
| 3 | Prepare a small first dataset | depends on you | No |
| 4 | Build and sign it | 1 minute | Yes, public |
| 5 | Write your application | 1 minute | Yes, public |
| 6 | Put `site/` on your origin; run the doctor | 10 minutes | Yes, public |
| 7 | Point the reviewer at your origin | 2 minutes | Yes, public |
| 8 | Wait for the decision | up to 5 days | No |
| 9 | Upload your first dataset | 10 minutes | Yes, public |
| 10 | Check it like a buyer would | 2 minutes | No |

1. **Make two test keys (2 minutes).**
   `onyx-publish keygen --out publisher.key`, then `onyx-publish keygen --out spare.key`
   Each prints only its address (`0x...`) and also writes it next to the key
   (`publisher.key.address`, `spare.key.address`: public, safe to keep). The key files are
   readable by you only and are never printed. Move `spare.key` (not `spare.key.address`) to a
   USB stick or another offline place now, and leave `spare.key.address` in the folder:
   `doctor` counts your spare by it (step 6). **If you lose your
   only key, your id is lost**: nobody can prove a new key is yours, and the origin alone is not
   enough. Never send a key file to anyone.
2. **Make your keys document (1 minute).**
   `onyx-publish keys-doc --key publisher.key --publisher <publisher id> --out site/.well-known/onyx-source-keys.json`
3. **Prepare a small first dataset (depends on you).** A real one, small: it must fit one shard
   of at most 8 MiB, and the reviewer reads its rows. It is not a throwaway: it becomes version
   1 of that dataset, and you add the rest as version 2 after approval. One CSV file with a
   header line (or JSON Lines), sorted by its key column (usually a time in unix seconds or a
   block height). Write a `schema.json` that names every column, for example:
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
4. **Build and sign it (1 minute).**
   `onyx-publish build --schema schema.json --input data.csv --dataset <dataset id> --publisher <publisher id> --license CC0-1.0 --out site/sample`
   then `onyx-publish sign --key publisher.key --dir site/sample`
   `build` checks every row and stops at the first bad one with its line number. `--license` is
   the license your data is under (an SPDX id such as `CC0-1.0` or `CC-BY-4.0`; add
   `--attribution "<text>"` if it requires credit). `sign` signs the manifest root and the
   rights statement with your key. Do steps 4 and 5 in one go: a first application's sample is
   built within the hour before it.
5. **Write your application (1 minute).**
   `onyx-publish apply --key publisher.key --publisher <publisher id> --origin <origin> --data "<one sentence about the data>" --chain-id 84532 --book <signed book address>`
   It writes `site/.well-known/onyx-publisher-application.json`: your id, origin and address,
   the hash of your keys document, the sentence, the sample's root, the limits you ask for (1
   GiB and 16 datasets unless you pass `--quota-gib` and `--datasets`) and an expiry 6 days
   later, signed with your key for this chain and this book. It has no name, contact or wallet
   field, and the sentence must hold no e-mail address. Everything in it becomes public.
   From here on, **never build version 1 of this dataset again**: its signed report is public,
   and a second version 1 would make the book suspend your key. `build` refuses it.
6. **Put `site/` on your origin and run the doctor (10 minutes).** Copy the content of `site/`
   so that `https://<origin>/.well-known/onyx-source-keys.json` shows your keys document (HTTPS,
   no redirect). Then `onyx-publish doctor --live`
   It reads the files back from your origin, compares them with the local ones, and prints one
   line per step: `ok`, `missing` or `problem`, each with the next command (`skipped`: a check
   you did not ask for). When nothing is missing it prints the two lines for step 7.
   Three more lines when you name where to read:
   `--rpc https://sepolia.base.org --chain-id 84532 --book <signed book address>`
   reads the registered keys from the book itself (is your id, origin or key taken?) and your
   key's test ETH (none is needed before approval); `--node <node address>` asks the node;
   `--publishers publishers.json` reads the status list (step 8). The public chain node answers
   200 blocks a request, so the first read of the book's whole history takes hours there; the
   tool keeps what it read (`~/.local/state/onyx/book-scan/`, or `--scan-cache FILE`) and a later
   run reads only the new blocks. Leave `--rpc` out if you do not want to wait: the reviewer
   reads the book in any case.
   Your spare key counts as there when `spare.key.address` is in the folder; if you made the
   spare elsewhere, add `--spare-offline`.
7. **Point the reviewer at your origin (2 minutes).** Open the issue named in "Before you start"
   step 3 (the repository offers the template "Publisher application") with exactly two lines:
   your origin and the Keccak of your application file. Never a key file and no personal
   details. You never send the application itself: the reviewer's check fetches it from your
   origin, from a hosted machine, so you learn nothing about the reviewer and the reviewer's
   machine never connects to you.
8. **Wait for the decision (up to 5 days).** The reviewer answers in the issue: approved, or
   refused with a reason code. The same state is in the public record, `deploy/publishers/` of
   the repository (it shows `yes` for each checked line, no text about you), and in its status list `apps/web/static/publishers.json`, which the website's Publish data page shows with a lookup by hash or id: a registered
   publisher under its id, an application that is not approved under its Keccak only (no id,
   origin or text of yours is kept). To read it in your terminal, save the list and let the
   doctor print your line:
   `curl -s -o publishers.json https://onyx.cryptagraph.com/publishers.json`, then
   `onyx-publish doctor --publishers publishers.json`
   A publisher registered before the record began is listed there without its origin. After a refusal, fix the named reason and apply again **with a
   new key** (the refused key is blocked until its refused sample's report is 7 days old). If
   the application expired before a decision, run step 5 again with `--reapply`, the same key
   and the same `site/sample` folder, put the new file on your origin, run `doctor --live`
   (it says "a re-apply after an expiry") and open a new issue with the new Keccak.
9. **Upload your first dataset (wait 20 to 40 minutes, then about 10 minutes).**
   `onyx-publish upload --dir site/sample --node <node address> --wait-secs 3600`
   The node takes your manifest once the sample's report is recorded in the book and Base
   Sepolia has made it final, so the command waits and says so every 15 seconds. The
   reviewer's tool posts your sample's signed report in the same run as your registration, so
   you need no test ETH for it. If the reviewer's answer says the sample is not anchored
   (`"relay": {"not_relayed": "stale"}` in your record), anchor it yourself first (it needs a
   little test ETH from a Base Sepolia faucet; Base's documentation lists them under "Network
   faucets"; one anchor is about 250,000 gas):
   `onyx-publish anchor --dir site/sample --rpc https://sepolia.base.org --chain-id 84532 --book <signed book address> --gas-key publisher.key`
   Fund the key from a faucet, not from a wallet with other history: a transfer links the two
   on chain. If the sample is older than 7 days at approval, its report can no longer be
   posted: anchor the same folder through your catalog 0 instead ("Many datasets with one
   anchor" below) and never build it again.
   When the node answers a 503 that clears by itself (`bulk_chain_unavailable`, `overloaded`,
   `bulk_busy`), a 429, a proxy's 502, or nothing, `upload` says so and tries again, up to 8
   times in a row, 15 s apart or the node's `retry-after` (`upload`, its recheck and shards, and
   `catalog-upload` alike); a full node (503 `bulk_node_full`) is refused at once. If it stops
   halfway, run the same command again: it sends only what the node is still missing.
10. **Check it like a buyer would (2 minutes).**
    `curl -s "<node address>/v1/bulk/<publisher id>/<dataset id>/rows?from=<a key>&to=<a later key>&limit=100" > rows.json`
    then
    `onyx-check --bulk rows.json --publisher <publisher id> --dataset <dataset id> --from <a key> --to <a later key> --rpc https://sepolia.base.org --chain-id 84532 --book <signed book address>`
    It prints `"ok": true` with your publisher, dataset and row count when every row is proven.
    Add `--history <node address>` to check every catalog of the publisher from 0 as well (catalogs
    only; about 12 requests and 31 MB for a year of daily catalogs). It reports `"fault"`
    `publisher` or `node` when it refuses, and says what it cannot catch without
    `--scan-keys-from` (ADR-0059 amendment 3).

After that you work alone, inside your limits: the rest of your history is version 2 ("Add more
data later"). Each later anchor needs a little test ETH.

**Register your spare key soon** (15 minutes, then a review). A further key needs the
cosignature of an active key of your id; control of the origin alone gives nothing.

1. List both keys in your keys document (the spare by its address, from `spare.key.address`):
   `onyx-publish keys-doc --key publisher.key --also-address <spare address> --publisher <publisher id> --out site/.well-known/onyx-source-keys.json`
2. Bring `spare.key` back from its offline place, then:
   `onyx-publish apply --key spare.key --cosign-key publisher.key --publisher <publisher id> --origin <origin> --data "<the same sentence>" --chain-id 84532 --book <signed book address>`
3. Copy `site/` to your origin, run `onyx-publish doctor --live --key spare.key --spare-offline`,
   and open the same two-line issue with the new Keccak.
4. Move `spare.key` offline again. A first application's keys document lists exactly one key;
   only this later one lists two.

## Check an answer yourself

Anyone can check rows a node served, a publisher or a reader, without trusting the node.
`onyx-check` is the program that does it: it proves each row up to the manifest root, checks the
publisher's signature over that root, and reads on a chain node you choose that the book recorded
and accepts that signature.

- **Where it ships:** in `onyx-publish-linux-x86_64.tar.gz` of the latest test-network release
  (https://github.com/onyxnetworkofficial/onyx/releases), next to `onyx-publish` in
  `onyx-publish/bin/`; or built from the source with `cargo build --release -p onyx-check`.
- **One full check** of the newest rows of a dataset on the hosted test node (replace the two
  ids):

  ```sh
  curl -s "https://onyx.cryptagraph.com/v1/bulk/<publisher id>/<dataset id>/rows?order=desc&limit=5" > rows.json
  onyx-check --bulk rows.json --publisher <publisher id> --dataset <dataset id> --order desc \
    --rpc https://sepolia.base.org --chain-id 84532 --book 0x04bb31d80c8ce9df02c8a03486b5812ec6e5ccc5
  ```

  With `--order desc` it refuses an answer in another order, and for page 0 without `--to` it
  proves the page starts at the dataset's newest row (`"newestProven": true` in the report).
  Without that check, "newest" is the node's word. Add `--page N` for a later page.
- **A range, oldest first** (replace the two ids and the two keys; a row's key is a whole number,
  unix seconds for a day or time series, so 1767225600 is 2026-01-01 and 1769817600 is
  2026-01-31):

  ```sh
  curl -s "https://onyx.cryptagraph.com/v1/bulk/<publisher id>/<dataset id>/rows?from=1767225600&to=1769817600&limit=100" > rows.json
  onyx-check --bulk rows.json --publisher <publisher id> --dataset <dataset id> \
    --from 1767225600 --to 1769817600 \
    --rpc https://sepolia.base.org --chain-id 84532 --book 0x04bb31d80c8ce9df02c8a03486b5812ec6e5ccc5
  ```

  `--from` and `--to` also take the dates you asked (`2026-01-01`) for a key that reads as a day.

  It prints one JSON report and exits 0 with `"ok": true` only when every row is proven and the
  book holds the signature. Exit 1: refused (a changed row, a wrong publisher, an unknown key).
  Exit 3: nothing decided (your chain node could not be read): try again.
- Pass the same `--from` and `--to` you asked, so a row outside what you asked is refused.
  `GET /v1/bulk?q=<words>` lists dataset ids.
- `--order`, `--page` and dates need `onyx-check` from v0.4.0-testnet or later. An older one
  refuses a newest-first answer ("rows skip"); it reads only the oldest-first form with
  whole-number keys.
- What no check proves: the answer's `query`, `key` and `next_page` fields (the report's
  `notProven` says so), and that a page other than a proven newest one starts or ends where it
  should.

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

### When new rows do not fit the schema

`build --prev` refuses rows that need more decimal places than a column's `scale`, or a column the
schema lacks (values are padded, never rounded); the refusal names the line, the column and the
places its value needs. A later version may **widen** the schema for its new rows (ADR-0059
amendment 5): every earlier shard keeps its bytes, its root and the schema it was built under.

1. At version 1, give each decimal column a few more decimal places than the data needs today (the
   padding costs a few bytes a value). Later, check new rows against the places their values need,
   not against the scale your exporter would pick for a new dataset: a value with fewer places is
   padded and fits.
2. If new rows still do not fit, widen first:
   `onyx-publish schema-fit --prev <last version folder> --input new-rows.csv --out new.json`
   writes the narrowest widening that takes the rows and lasts: a decimal column whose values need
   more places gets the places 17 significant digits need at its smallest non-zero value, plus 2
   (at most 38, and no more than keeps its longest value within the 80 characters of a stored
   decimal), so a source that writes full binary-float digits does not need another widening
   next week; a non-key `int` column that now holds fractions becomes `decimal`; a column the input
   has and the schema lacks is added at the end as nullable. Columns that fit stay as they are. It
   names every value no widening can take (more than 38 places, past 80 characters, not a number,
   empty in a column that is not nullable) and then exits 1; fix those rows first.
3. Then `onyx-publish build --prev <last version folder> --schema new.json --widen ...` (without
   `--widen` a different schema is refused), and sign, anchor and upload as usual. The version is
   format 2; the next versions under the same schema need no flag.
4. Limits: at most 8 schemas per dataset (7 widenings, at most one per version); the eighth widening
   is refused by `build` ("at most 8 schemas"), and a node refuses such a manifest as
   `bulk_bad_manifest` (its header is refused before the schemas are read). Widening never renames, reorders or drops a
   column, never changes a column's nullability, a text column or the key column, and never
   narrows a scale. For those changes, and after 8 schemas, start a **new dataset id** (for example
   `<dataset id>-2`): build its version 1 from the whole history with the new schema, list it in
   your next catalog, then end the old id with a tombstone that links it to the new one: see
   "Move a dataset to a new id" below.
5. Who can read a widened dataset: nodes store a format-2 manifest only with `ONYX_BULK_FORMAT2=on`
   (docs/ops/config.md); a node that does not yet keeps serving your earlier version, and a node
   that upgrades later refuses your next versions ("versions are taken in order") until it gets the
   widened one: `onyx-publish upload` then uploads the previous version from the folder `build --prev`
   recorded (with the catalog number it was uploaded with) and tries again. Readers need an
   `onyx-check` that reads format 2: an older one refuses an honest widened answer (exit 1); from
   this one on, a header format newer than the check is exit 3 ("update onyx-check") only when its
   root verifies against a report your registered key signed and the reader's own chain node shows
   the book holds it (with `--unchecked` it is exit 3 marked `chainChecked: false`, like every
   unchecked answer); anything else is refused. Answers show every row in the newest schema (old decimals padded with
   zeros, added columns null; each row's `schema` names the schema it is stored under).
6. A widened dataset cannot back a storage deal yet: the deployed OnyxStorage reads format-1
   headers only, and storage nodes refuse a deal on a format-2 manifest. Deals on widened datasets
   need a new OnyxStorage.

### Move a dataset to a new id

A move ends the old id and, in the same catalog, links it to the new one (ADR-0059 amendment 6):
the old id then answers `410` with `successor`, and readers can check where the data went.
Moves need catalogs (see "Many datasets with one anchor" below).

1. Build the new id's version 1 from the whole history under the new schema
   (`onyx-publish build --dataset <new id> ...`).
2. In catalog n, list the new id; the old id stays as it is. Sign, anchor and upload catalog n,
   then `upload --dir <new id folder> --catalog n` to every node.
3. Check the new id on every node: every shard stored, `onyx-check --bulk` on a sample of rows.
   Keep both ids served for an overlap: a day at least; a week lets readers compare rows.
4. In a later catalog, move the old id: `onyx-publish catalog-build --publisher <publisher id>
   --dirs LIST --prev <catalog file> --move <old id>=<new id> --links-out links-1 --out <file>`.
   From the second move on, add `--links <last links folder>` (the version the previous catalog
   lists) and a new `--links-out`. Sign, anchor and upload the catalog, then
   `upload --dir links-1 --catalog <number>` to every node before the other folders.
5. Check: `GET /v1/bulk/<publisher id>/<old id>` answers `410` with `successor`, and `onyx-check
   --successor` on its `/successor` answer exits 0.

Both steps can go in one catalog, but the old id then stops before the new one's shards are up.
Example (made up): `example.flows` moves to `example.flows-2` after its source dropped a column.

- The tool refuses a new id that is not live in the catalog it builds, an id under
  `onyx.successors` (reserved for links), an old id also in `--remove` or `--dirs`, a
  `--links` folder the previous catalog does not list, and an existing `--links-out` folder.
  A link names a dataset live in its own catalog: to move the new id on later, move it in a
  later catalog (a chain follows up to 8 links).
- A wrong link is fixed in the next catalog: `--relink <old id>=<id>`; `--unlink <old id>` takes
  it back (the old id then ends `retracted`). The last row for a tombstone wins; no key is ever
  at stake. One catalog writes all its moves in one links version.
- Keep every links folder: each version names the one before, and `upload` steps back to it.
- A links generation holds 65,536 rows, 32 MiB and 4,096 shards (one shard per catalog that
  writes links). The tool warns at 90 % of each and refuses past them; then
  `--links-next-generation` starts `onyx.successors.2` with only the links that act, and ends
  the full generation in the same catalog.
- Until a node holds the links version the catalog lists, its 410 says `pending`: upload the
  links folder first. Nodes older than successor links answer 410 without `successor`, so say
  the move wherever you announce your data as well.

### When your source changes

A source can rename a file, drop a metric or change what a series means. For a daily refresh of
many datasets:

1. A dataset whose source is gone must be skipped, not stop the run: the others still refresh, and
   the skipped one keeps its last version (it is still correct for the days it covers).
2. Before a renamed source continues a dataset id, compare its rows with the rows Onyx serves for
   the same days (`GET /v1/bulk/<publisher>/<dataset>/rows?from=...`). Continue only if they are
   equal: a version adds days, it never changes the days already published.
3. A source whose values or columns changed for days already published is new data: give it a
   new dataset id (with the whole history) and, if the old one should stop, end it with a tombstone.
4. Retry the anchor and the upload when a node does not answer: both are safe to repeat (a report
   already recorded is not sent again).

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
| `postReport: would revert` during anchor | The book does not know your key yet, or the key is retired, compromised or suspended | Wait for the reviewer's decision (step 8); check your address with `onyx-publish address --key publisher.key` |
| `your application carries version 1 of ...` during build | You are building a second version 1 of your sample's dataset | Add rows with `--prev site/sample` (version 2). Only if the application was never put on your origin: delete it and build again |
| `this key is blocked until unix N` | Your earlier application with this key was refused, and its sample's report can still be relayed | Apply with a new key, or wait until that time |
| `this key already anchored another manifest for version N` | You built and anchored a different version N before (each version is signed under a number fixed by the dataset and version) | Never anchor two different manifests for one version: the book would suspend your key. Build the next version with `--prev` instead |
| `bulk_not_anchored` after the wait | The anchor is not final yet, or not in this node's book | Run upload again later; check `--book` is the node's book |
| `bulk_quota_exceeded` | The node's limit for you: your entry in its limits file, or one number for every publisher (16 GiB by default) | Ask the operator for a higher limit |
| `bulk_publisher_not_listed` | Your key is registered, but this node's operator has not listed your publisher id, so it takes no upload from you | Ask the node's operator for an entry; registration in the book is not a grant of disk on a node |
| `bulk_publisher_paused` | The node's operator paused your publisher id: your data is still served, new data is not taken | You can still remove a dataset (`catalog-build --remove`); ask the operator why |
| `bulk_not_servable` when reading | Your key was marked compromised or suspended | See "Lost or stolen key" |
| `bulk_publisher_forked` or `bulk_dataset_forked` with `fork` | Two anchored catalogs of yours cannot both stand (a stolen key, or two machines publishing) | See "Lost or stolen key"; the mark clears by itself once the book withdraws one side |

**Lost or stolen key.** What you ask of the reviewer is always a signed file
(`onyx-publish request ...`, put at `https://<origin>/.well-known/onyx-publisher-request.json`)
and a two-line issue with your origin and the file's Keccak. A plain issue changes nothing.

- **Stolen.** At once: take the stolen key's address out of your keys document at the origin,
  and run `onyx-publish request --key <the stolen key> --kind key-stolen --at <unix time of the
  theft> ...`. Both are needed: a thief has the key but not your origin. To move to your spare
  in the same step, add its application cosigned by the stolen key (`apply --key spare.key
  --cosign-key publisher.key`). The reviewer registers the new key first, then marks the stolen
  key compromised from the theft time: what it signed after that time stops counting (nodes
  serve your newest version from before it within 5 minutes; clients refuse the rest at once).
  If a thief published a version, publish your own next version from your last good folder with
  the new key: it replaces the thief's. A version of your own made after the theft time can be
  signed again with the new key (`sign`, `anchor`, `upload` on its folder) while it is less than
  7 days old.
- **Lost, and you have another registered key.** `onyx-publish request --key <the other key>
  --kind retire-key --retire <lost key's address> --at <unix time, now>`: what the lost key
  signed before that time keeps being served, and your next version is signed with the other
  key. A time more than an hour back would withdraw what the key signed since then, so it is
  taken only when you also take the key's address out of your keys document.
- **Each request is taken once.** It carries a sequence number (the time you made it); to ask
  again, make a new one. `--kind withdraw` withdraws the application that is in `site/` now,
  and only the key that signed it can.
- **Lost, and it was your only key.** There is no rotation: nobody can prove a new key is yours.
  The data already published stays served. Start a new id on a new origin and publish the
  history again there.

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

**Take a dataset down** (ADR-0059 amendment 4, THREATS CAT-3):
1. Build the next catalog with `--remove <dataset id>` (several: comma-separated) besides your
   usual `--dirs`: it ends each with a tombstone. Sign, anchor and upload it as usual.
2. Nodes that take it stop serving the dataset: its rows, info and listing answer `410
   bulk_dataset_removed`, and no upload brings it back. Later catalogs keep the tombstone; a
   folder of that dataset in a later `--dirs` is skipped (`SKIPPED ... removed`).
3. The id is gone for good: publish the data again under a new id if you need it back.

Removal is not erasure at once: nodes still hold the files for the grace (30 days of chain time
at least; a stolen key's tombstone can be withdrawn meanwhile), then a node that runs
`ONYX_BULK_ERASE=on` deletes the shard files no other dataset names (ADR-0059 amendment 4 item
5a); the manifests stay (the history check reads them), storage nodes keep their copies until
slice 2, copies others made stay, and catalogs already signed keep the old leaves. A reader's
`onyx-check --history` on rows read before the removal reports it (exit 4).

## For the operator

- Run the node with `ONYX_BULK_DIR` (and the signed book pinned in its manifest; docs/ops/config.md
  lists the quota settings).
- Give each publisher its own limit with `ONYX_BULK_PUBLISHERS` (run-api.md "Limits per
  publisher"): list a publisher there before its key is registered, and pause it there
  (`"state": "paused"`) to stop new data while it can still remove a dataset.
- Register a publisher's key only after the review in [publisher-review.md](publisher-review.md):
  `onyx-publish verify-application` on the applicant's files, then the lines only a person can
  answer, then one `book-reviewer approve` (the limit first, `registerKey`, the relay of the
  sample's report, the record in `deploy/publishers/`). Never open an applicant's origin from
  the reviewer's machine or the node. The reviewer key never goes on the node.
- Pause and resume with `book-reviewer pause|resume --publisher <id> --limits publishers.json
  --records deploy/publishers`, then restart the node with the file;
  `python3 -I scripts/check-publishers.py --limits publishers.json --max-gib <ONYX_BULK_MAX_GIB>`
  compares the node's limits file with the record.
- To stop serving a dataset, remove its folder under `ONYX_BULK_DIR/manifests/` and restart.
- `ONYX_BULK_DIR/catalogs/<publisher>/` holds every 32nd catalog whole (`{n}.bin`) and the others
  as diffs (`{n}.diff`); never delete one in the middle (every later diff up to the next whole one
  needs it), and never roll the node back to a release from before diffs once a `.diff` exists
  (it would not see them; a number with both files stops the start). Catalogs set aside go to
  `withdrawn/`; `fork.bin` is a catalog fork record (THREATS
  CAT-6). Catalog bytes count in the publisher's quota.
