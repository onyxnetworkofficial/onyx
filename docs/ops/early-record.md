# The early-contributor record: running the tool

The record is a daily, append-only list of accepted test-network work (ADR-0065, ROADMAP S26).
Early contributors may be rewarded later; this is a record, not a promise. This page tells how to
run the tool by hand, how the daily run publishes it under the record's own publisher id, and what
the operator sets up once before the first run.

## What you need
- A Base Sepolia JSON-RPC node. The first run reads old state (the deploy-block check), and
  every day reads the role getters at that day's last block. For a backfill from the deploy
  blocks, use a node that keeps old state (an archive node). A provider URL can hold a key.
  Put it in the environment variable `ONYX_EARLY_RPC`, keep it out of shell history, and never
  put it in a file in the repository. `--rpc URL` still works, but every user on the machine can
  read it in the process list, so the tool warns. The tool never prints the URL; errors show
  its scheme and host only.
- `deploy/early-record/contracts.json` (the deployments; with the earlier lists it names beside
  it, today `contracts-1.json`: "Changing the contract list" below) and
  `deploy/early-record/exclusions.json` (the reviewed list of the operator's own keys).
- A state directory that only this tool writes. Keep it between runs.

## Build one day

```sh
read -rs ONYX_EARLY_RPC && export ONYX_EARLY_RPC   # paste the node URL; it is not echoed
cargo run -p onyx-api --features http-client --example early-record -- build \
  --contracts deploy/early-record/contracts.json \
  --exclusions deploy/early-record/exclusions.json \
  --state STATE_DIR --out VERSIONS/2026-11-14 --prev VERSIONS/2026-11-13
```

1. The tool takes the state directory's lock (a second run at the same time stops), then reads
   the node's `finalized` block twice and takes the lower one.
2. It checks that the last block of the previous day still has the hash it recorded. If not, it
   stops and writes nothing.
3. It finds the next UTC day after its cursor. It writes the day only when the first block of the
   following day is final. A block at 00:00:00 starts the new day.
4. It reads that day's logs in 200-block ranges, twice. The second read is independent of the
   first; if they differ (a lagging backend), it stops and writes nothing. Then it reads the
   role getters, the observed times it needs and the state of shards that may have ended
   Empty, at the day's last block.
5. It writes the new folder, then saves its state.

One run writes at most one day. For a backfill, run it again with a new `--out` until it prints
`nothing_new`. `--day YYYY-MM-DD` stops the run when the next day is another one. `--prev` must
be the folder of the state's last day.

## What it prints (one JSON line)

| Output | Meaning | Exit |
|---|---|---|
| `{"built": "2026-11-14", "rows": N, ...}` | A new folder. `skipped_publisher_keys` lists role answers that name a publisher key (not excluded; review them). `days_without_blocks` lists earlier days with no block at all (a halted chain): no version for them | 0 |
| `{"nothing_new": ...}` | No new day is final yet. Nothing is written | 0 |
| `{"already_built": "2026-11-14"}` | The folder already holds that day, byte for byte. Nothing is written | 0 |
| `early-record: node: ...` | The node failed or rate-limited past the retries, or its two reads of the day differ. The logs read so far are kept in `STATE_DIR/pending.json` (dropped when the reads differ); run the same command again to go on | 1 |
| `early-record: the previous day's last block changed: ...` | A reorg or another chain. Stop and find out why; do not delete the state | 1 |
| `early-record: folder: ...` | A folder or the state does not match (for example a changed `data.csv`, `day.json` or schema file), or another run holds the state directory. The tool never builds a built folder again | 1 |
| `early-record: contracts: ...` | `contracts.json` does not match the chain (checked on the first run, and for the contracts a new list adds on its first day), or a new list starts on a day that is already built ("Changing the contract list") | 1 |
| `early-record: contracts.json: ...` | The list file is refused before anything is read: a field the tool does not know, two contracts with one counted ABI tag, an earlier list that is missing or changed | 1 |

## The folders

| Path | What |
|---|---|
| `OUT/data.csv`, `OUT/schema.json` | One version of `testnet.early-contributors` (the input of `onyx-publish build`) |
| `OUT/exclusions/data.csv`, `OUT/exclusions/schema.json` | One version of `testnet.early-contributors.exclusions`, only on a day the exclusions change |
| `OUT/day.json` | The day, its block range, the last block hash, the hashes of `contracts.json`, the exclusions in force, `data.csv` and the join's state after the day (`state_keccak`) |
| `STATE_DIR/state.json` | The cursor and the join's cache. Lose it and the next run starts again from the deploy blocks |
| `STATE_DIR/before/YYYY-MM-DD.json` | The state before that day, for `verify --state-before` |
| `STATE_DIR/pending.json` | Logs of an unfinished day; removed when the day is written |

## Check a day

```sh
ONYX_EARLY_RPC="$OTHER_NODE" cargo run -p onyx-api --features http-client --example early-record -- verify \
  --contracts deploy/early-record/contracts.json \
  --exclusions deploy/early-record/exclusions.json --dir VERSIONS/2026-11-14 \
  [--state-before STATE_DIR/before/2026-11-14.json]
```

`verify` rebuilds the folder's day from the chain and compares `data.csv`, `day.json` and the
exclusions rows byte for byte. `day.json` holds the hash of the join's state after the day, so a
log a node left out whose row comes only on a later day (a bet that wins the next day, a report not
accepted yet) still fails the check on its own day, though that day's CSV is the same. When the
folder holds a built `manifest.bin` (or `exclusions/manifest.bin`), it also compares each shard
root with the CSV's rows (8 MiB shards, the `onyx-publish build` default). Without
`--state-before` it starts from the deploy blocks, as anyone can. Use a second node, so one lying node cannot pass its own
check. A version is published only after `verify` against a second node passes: that is the
gate, and the daily run below applies it to every day.

## Changing the reviewed list
Each entry of `deploy/early-record/exclusions.json` has a `from_day` and, when it ends, an
`until_day`. Give a new entry a `from_day` after the last built day: a replay from the deploy
blocks then gives every earlier day the same bytes. Never edit an entry's days after a version
used it.

## Changing the contract list

Every published day names the hash of the list it was built with (in its `day_close` row and in
`day.json`). So a list that a published day used never changes, not by one byte. A new contract
gets a new list, and each day has exactly one list.

**The files.** `contracts.json` is always the newest list. A list that replaces another has two
more fields:

- `from_day`: the first UTC day built with it.
- `previous`: the file name of the list before it (in the same folder) and the keccak256 of that
  file's bytes.

The first list has neither field. Today: `contracts-1.json` is list 1 (every day up to
2026-11-19; hash `0xa5b599c58c19f77f4629277c503570ec8b62dcbad303924065972d9c1fb4e976`), and
`contracts.json` is list 2, from 2026-11-20: list 1 and `OnyxStorage` v4.

**What the tool does.**

1. It reads `contracts.json` and, from the same folder, every earlier list it names. It checks
   each hash. A changed earlier list stops the run.
2. It builds each day with the list in force on that day. So a day before `from_day` gets the
   same bytes as before the new list existed, in `build` and in `verify`. A contract that the
   day's list does not name is not read at all: its address is not in the request to the node.
3. On the first day of a new list it checks the contracts the list adds on the chain (code at
   the deploy block and none one block before; the node must still keep that state). Then it
   reads these contracts from their `scan_from` blocks, not only from the day's first block.
4. Rows that the added contracts accepted before that day go into that day's version. Their days
   are published and never change. Such a row keeps its own `accept_block` and `accept_tx`; its
   `day` cell is the day of the version it is in. Only the added contracts are read again for
   the earlier blocks. The `day_close` row keeps its form: its block range is the day's own.
5. From then on the state is bound to the new list.

**What a new list may change.** It only adds. The tool refuses a list that:

- drops a counted contract of the list before it, or changes its address, ABI tag or `scan_from`;
- has another chain id;
- has a `from_day` that is not after the `from_day` of the list before it;
- starts on a day that is already built (the run stops with `give a new list a from_day after
  the last built day`): a replay would give that day other bytes.

**What a conflict is.** Two contracts with the same counted ABI tag in one list. The record keeps
one set of ids for each tag (deal and node ids, claim ids, market ids), so two contracts of one
tag would mix their ids. A second deployment of a contract gets an ABI tag of its own, as
`storage-v4` beside `storage-v3`: each storage contract numbers its deals from 0, and the record
keeps the two apart by contract address.

**`storage-v4` (`OnyxStorage`, `RULES_VERSION` 4).** The five counted events have the same
signatures and topics as in `storage-v3`: `Activated`, `DealCreated`, `Answered`, `Slashed`,
`MissReward`. The role getter is the same (`treasury()`). The new event
`DealAnchor(uint64,bytes32,bool)` is not read: a deal's payer comes from `DealCreated`. The reader
contract `OnyxStorageAnchor` has no events, no state and no roles, so it has no entry.

**To add a contract later.**

1. Copy `contracts.json` to `contracts-N.json` (N is the number of that list), byte for byte.
2. In `contracts.json` add the new entry at the end. Set `from_day` to a day after the last built
   day, and `previous` to `contracts-N.json` and its hash:
   `cast keccak 0x$(xxd -p deploy/early-record/contracts-N.json | tr -d '\n')`.
3. State the contract's deploy block with its address in the document the entry names. Run
   `cargo test -p onyx-api --lib early`.
4. Install the tool and both files as in the roll-out below.

**Roll-out of list 2 (the daily run reads its own checkout and its own tools).**

1. Build the tool from a commit with this section:
   `cargo build --release -p onyx-api --features http-client --example early-record`. Put
   `target/release/examples/early-record` into the folder `ONYX_EARLY_BIN` names. Do this first:
   the old tool refuses the new file (`unknown field`), and the new tool gives the same bytes as
   the old one with the old file.
2. Check the list the job has used so far: `grep contracts_hash OUT/versions/<the last
   day>/day.json` must show the hash of list 1 above. If it shows another hash, stop: the job's
   list is not the repository's list 1. Put the job's file, byte for byte, in `contracts-1.json`
   and its hash in `previous` first.
3. While the unit is not running, update that checkout to the same commit. It then holds
   `deploy/early-record/contracts.json` and `deploy/early-record/contracts-1.json`.
4. Check a published day with the new tool and the new files, from the state saved before it
   ("Check a day" above, with `--state-before`): it must say `verified`. This reads a node and
   writes nothing.
5. Steps 1 to 3 must be done before the run that builds 2026-11-20 (it starts after 00:30 UTC on
   2026-11-21). Doing them before 2026-11-19 is built is fine too: that day is built with list 1.
6. If that run was missed, 2026-11-20 is built with list 1. Then set `from_day` in
   `contracts.json` (and in the test `the_shipped_list_keeps_the_first_list_and_adds_storage_v4`)
   to the day after the last built day, commit, and install that. No published day changes, and
   no work is lost: the first version of list 2 reads v4 from its deploy block whenever it starts.

With nothing installed the daily run goes on as before, with list 1: it builds and publishes
each day without the v4 rows. Never go back to the old tool after a day was built with list 2:
it cannot read that state.

## The record's own publisher (once, before the first daily run)

The record is published under a publisher id of its own, `early-record`, with a key of its
own: never in the catalog of a data publisher, and never signed by a data publisher's key
(ADR-0065 item 12; THREATS EARLY-9). The id is neutral, as every publisher name must be (no
person, company, product or Onyx; docs/ops/bulk-publish.md step 1). Its origin is
`onyx.cryptagraph.com/record`: the keys document is the site file
`apps/web/static/record/.well-known/onyx-source-keys.json`, served at
`https://onyx.cryptagraph.com/record/.well-known/onyx-source-keys.json`. The operator makes the key
and that file (steps 1 and 2); neither is in the repository before then.

1. **The key (1 minute).** On the machine that runs the timer:
   `onyx-publish keygen --out <the record key file>`. It prints only the address. The key file is
   mode 0600; nothing else reads it: the daily run passes its path to `onyx-publish`
   (`catalog-sign --key`, `catalog-anchor --gas-key`) and never reads or prints it. Keep an offline
   copy (docs/ops/bulk-publish.md "Lost or stolen key").
2. **The keys document (1 minute).**
   `onyx-publish keys-doc --key <the record key file> --publisher early-record --out apps/web/static/record/.well-known/onyx-source-keys.json`,
   then deploy the site so it is served at the origin above.
3. **Registration (the reviewer).** `registerKey("early-record", <address>, "onyx.cryptagraph.com/record",
   <provenance hash>, ...)` in the signed book, as docs/ops/bulk-publish.md "For the operator" says.
4. **Funding (5 minutes).** Test ETH for the address: one `postReport` a day, about 250,000 gas
   (about 0.0000015 test ETH at 0.006 gwei). Send it from one of the operator's own test keys, never
   from a data publisher's key (that would link the two on chain):
   `cargo run -p onyx-level2 --example test-gas -- send --rpc https://sepolia.base.org --from-key <an operator test key file> --to <address> --wei 100000000000000`
   prints what it would send; add `--yes` to send. It reads the key file in its own process, sends
   only on Base Sepolia, at most 0.001 test ETH at a time, and never more than half of what the sender holds after the fee.
5. **The exclusions.** Put the address in `deploy/early-record/exclusions.json` as
   `"record_publisher": {"address": "0x...", "from_day": "<the first day not built yet>"}`
   (a `from_day` after the last built day, so earlier days keep their bytes).

## The daily run

`scripts/early-record-daily.sh` (its header has every variable and exit code) runs once a day
from the user timer `deploy/systemd/user/onyx-early-record.timer`, after 00:30 UTC. Each step is
skipped when it is done, so a run that stopped (a node, the anchor, an upload) goes on where it
stopped when it runs again, and a built version is never built again:

1. `early-record build` of every complete day not built yet, from `ONYX_EARLY_RPC`, one folder a
   day under `OUT/versions/`.
2. Then each day not published yet, in day order, **all the way before the next day starts** (so
   a day is never left anchored and not uploaded when a later day stops the run):
   1. `scripts/audit-privacy-scan.py` on the version folder (`FAIL_ON_HIT=1`, with the entity
      indexer checkout `ONYX_EARLY_INDEXER`): a hit, a missing folder or a folder with no file is
      exit 4, and neither this day nor a later one is published.
   2. `onyx-publish build` of the day's CSV (and of `exclusions/` on a day it changes) with `--prev`,
      into the same folder, each version created at the end of its day (00:00 UTC of the next day).
   3. `early-record verify` against **a second node**, `ONYX_EARLY_RPC2`, of another provider (their
      scheme://host:port must differ), from the state saved before the day: the CSV bytes,
      `day.json` with its state hash, and the shard roots of both versions. Since every day is
      verified in order and its state hash checked, the state the next day starts from is the one
      the second node computes too. A difference is exit 3, the same stop. A node that is down is
      exit 1 (run again).
   4. One catalog a day (`catalog-build --prev` the day before's, `OUT/catalogs/YYYY-MM-DD.bin`).
      `--dry-run` stops here for each day: it uses no key and sends no transaction.
   5. `catalog-sign`, `catalog-anchor` (one `postReport` a day), `catalog-upload`, then `upload
      --catalog N` of the day's folders (docs/ops/bulk-publish.md "Many datasets with one anchor"),
      each waiting up to `ONYX_EARLY_WAIT_SECS` for the anchor to be final. So a backfill takes
      about 20 to 40 minutes a day of the record, mostly waiting.

Config is the environment only (the unit reads `~/.config/onyx/early-record.env`, mode 0600):
`ONYX_EARLY_RPC`, `ONYX_EARLY_RPC2` (never printed; passed to `early-record` in its environment),
`ONYX_EARLY_STATE`, `ONYX_EARLY_OUT` (an existing folder; both made absolute, so the scan always
reads the real folder), `ONYX_EARLY_PUBLISHER=early-record`,
`ONYX_EARLY_INDEXER`, `ONYX_EARLY_KEY`, `ONYX_EARLY_ANCHOR_RPC` (it goes on `onyx-publish`'s command
line, so the script refuses a URL with a path, query or user info, where keys live: use a public
endpoint), `ONYX_EARLY_BOOK`, `ONYX_EARLY_NODE`, and optionally `ONYX_EARLY_BIN` (the folder with
`early-record` and `onyx-publish`), `ONYX_EARLY_REPO`, `ONYX_EARLY_CHAIN_ID`, `ONYX_EARLY_WAIT_SECS`,
and the four retry settings below.

### What the run tries again by itself

The last stages of a day are tried again inside the run (READINESS A27; THREATS EARLY-21 and
RETRY-R1-*), so a stop that passes by itself no longer ends the run failed:

| Stage | Tried again when `onyx-publish` says (exit 75, last line `TRANSIENT class=...`) | Why it is safe |
|---|---|---|
| `catalog-anchor` | `not_mined` (no receipt within 180 s), `earlier_pending` (an earlier transaction of the key is unmined and young, or the node says the nonce is taken), `rpc_unreachable`, `rpc_http` (429, 408, 5xx), `rpc_busy` (a rate limit, a backend behind the head), `rpc_internal`, `pool_full`, `underpriced`, `fee_cap` (a fee spike above the cap), `not_visible` (mined, or refused by the book as a second copy, the record not readable yet) | It reads the book first and sends nothing when the report is recorded. It keeps what it sent in `OUT/catalogs/<day>.bin.anchor-tx.json` (nonce, hashes, fees, the signed bytes; no key) and, run again, waits for that transaction or replaces it at the same nonce ("A stuck transaction" below). Any second transaction carries the same report, which the book takes once |
| `catalog-upload` | `not_final` (the anchor not final in time), `node_busy` (429, 502, a passing 503, eight in a row), `node_error` (another 5xx), `node_unreachable` | The node takes a catalog it holds again without change |
| `upload`, `upload-exclusions` | The same four | It sends only the shards the node misses |

**Never tried again:** anything before these stages (the build, the privacy scan, `verify`, the
catalog, the signature: exits 1, 3 and 4 as before), and every refusal of the three stages, which
ends the run at once with exit 7 and `onyx-publish`'s own line: a report the book would reject
(`would revert`), a gas key with too little test ETH, another catalog already anchored under the
number, a transaction that stayed stuck after its fee raises or one the tool did not send, a chain
node that is another chain, answers 401 or 403, is over a quota (a daily or monthly count, not a
rate) or is not a JSON-RPC node at all, a node that is full or refuses the catalog (4xx), and any
stop the tool does not know. A tool that was killed (exit above 128) is exit 1: it gave no answer.
A stage that is done is never run again: the retry repeats one command, and the markers in
`OUT/done/` skip the rest. One exception, on purpose: while a wait says the anchor is not final,
`catalog-anchor` runs again before each new try, and a wait that gives up clears the day's
`anchored` marker. The marker comes from a read at the newest block, and a block can be
reorganized away; the anchor reads the book first, so it sends only when the record is gone.

Settings (whole numbers; anything else is exit 2 before anything runs):

| Variable | Default | Meaning |
|---|---|---|
| `ONYX_EARLY_RETRY_TRIES` | 5 | Tries of one stage, the first included (1 to 20; 1 means no retry) |
| `ONYX_EARLY_RETRY_SECS` | 300 | The pause before the second try; it doubles before each later one, an hour at most (1 to 3600). With the defaults: 300, 600, 1200, 2400 s, 75 minutes |
| `ONYX_EARLY_RETRY_BUDGET_SECS` | 5400 | The most one run spends on pauses and failed tries, all stages and days together (600 to 6000). Also the longest wait for finality one try gets: the lower of what is left of it and `ONYX_EARLY_WAIT_SECS` |
| `ONYX_EARLY_RUN_SECS` | 7200 | The whole run's time from its start, whatever stage it is in (600 to 8400). Every tool runs under `timeout` with what is left; when the time is up the run ends with exit 8 and one alert line (review RETRY-R2-M4) |

**How long a failing run can be, and why it matters.** The unit starts a failed run again after
20 minutes and gives up at the fifth start inside 12 hours; only then is it failed for good. A run
that failed for 4.6 hours (five waits of an hour for an anchor that never became final) would
never get there. So a failing run is bounded: the budget above (at most 100 minutes) plus its
last try's own work (25 minutes at the very most), about 2 hours. Four such runs, the three
gaps and an hour for the first run's build fit in 10.3 of the 12 hours
(`scripts/early-record-daily-tests.py` computes it from the unit file and fails if a change of
either breaks it). And an exit 6 or 7 does not wait for the unit: the script itself logs one line
with the tag `onyx-early-record-alert` at once (`journalctl --user -t onyx-early-record-alert`),
naming the day, the stage and the class.

One line a try, then one when the stage is done or given up:

```
early-record-daily: 2026-11-17: catalog-anchor try 2 of 5 after 300 s: not_mined
early-record-daily: 2026-11-17: catalog-anchor done at try 2 of 5
early-record-daily: 2026-11-17: catalog-anchor gave up after 5 of 5 tries (not_mined; 4510 s of the run's 5400 s budget used); run again: it goes on, nothing is sent twice
early-record-daily: 2026-11-17: catalog-anchor refused at try 1 (onyx-publish exit 1): not tried again; read its line above before running again
```

The stage's last error text is kept in `OUT/stage.err`. The run asks the tool once whether it has
exit 75 (its usage text names it). Only a tool built before it, which ends 1 for every stop, is
read by its last line instead, and tried again only for the texts of the same stops (`not mined
before the deadline`, `already known`, `node answered HTTP 5xx`, `bulk_not_anchored`, ...); it
keeps no transaction record and never replaces a stuck transaction. Rebuild the tools in
`ONYX_EARLY_BIN` from this version.

| Exit | Meaning | What to do |
|---|---|---|
| 0 | Done, nothing new, or the dry run reached its stop | Nothing |
| 1 | A tool or a node failed before the last stages, or a tool was killed | The unit starts it again after 20 minutes (at most 4 starts in 12 hours); it goes on |
| 2 | Usage or config, or the state has built days but `OUT` holds none | Fix the env file; for a lost `OUT`, see below |
| 3 | The second node's rebuild of a day differs | See below; nothing of that day or later is published meanwhile |
| 4 | The privacy scan refused a version folder | Review it (`SHOW=1` on a private machine shows the name); nothing of that day or later is published meanwhile |
| 5 | Another run holds `OUT/lock` | Wait for it |
| 6 | A last stage kept failing in a way that passes by itself, past its tries or the budget. One alert line is logged at once | Nothing is lost and nothing was sent twice. The unit starts it again like exit 1; or run it by hand (below) once the chain node or the Onyx node answers |
| 7 | `onyx-publish` refused a last stage. One alert line is logged at once | Read its line (just above the `refused at try` line in the journal, and in `OUT/stage.err`), fix the cause (fund the gas key, a wrong book or chain id, a full node, "A stuck transaction" below), then run it by hand. The unit template does not start it again (`RestartPreventExitStatus` names 7); a unit installed before that line starts it again like exit 1, which only repeats the refusal and its alert line, and sends nothing |
| 8 | The run's time was up (`ONYX_EARLY_RUN_SECS`). One alert line is logged at once | Nothing is lost. The unit starts it again like after exit 1; the next start goes on from the stage that was cut |

### When the run stops

The unit restarts after exits 1 and 6 (and 7, until the template's `RestartPreventExitStatus` line
with the 7 is installed: an operator step, `install` and `systemctl --user daemon-reload` as in
the unit file's header). When it ends failed, `onyx-early-record-alert.service` logs one
line with the tag `onyx-early-record-alert` (its result and exit status) to the journal; check it
with `journalctl --user -t onyx-early-record-alert --since -1d` (the health watch runbook names the
same check). Every day before the one that stopped is published.

**Run it again by hand:** `systemctl --user start onyx-early-record.service` (it reads the same env
file; `journalctl --user -u onyx-early-record -f` follows it). It goes on at the first stage that
is not done. After a `not mined` stop wait a few minutes first: the transaction is usually mined
by then and the run prints `ANCHORED already`. A start seconds after it only meets the same
transaction again (`already known`, 2026-11-18).

- **Exit 3 (the nodes disagree on day D).** Read `OUT/verify.err`, then ask a third node. If the
  second node was wrong, set `ONYX_EARLY_RPC2` to another provider and run again. If the first node
  was wrong (day D's folder holds its wrong answer, never signed): move `OUT/versions/D` out of
  `OUT/versions/` (keep it for the record of what happened), delete `OUT/done/D.*`, copy
  `STATE/before/D.json` over `STATE/state.json`, delete `STATE/pending.json` if it is there, set `ONYX_EARLY_RPC` to a good
  node and run again: it builds day D afresh. A day already published is never changed: a wrong
  published day is corrected only by later `revoke` rows.
- **Exit 4.** Review the hit. A real one is a bug in the record (it copies no text from events):
  fix the code, then rebuild the day as for exit 3. Never publish around it.
- **Exit 2, `OUT` lost while the state keeps its days.** Restore `OUT` from its backup. A build
  without it would start version 1 and catalog 0 again after days already published. With no
  backup, start a new state and a new history only after the reviewer and the operator decide it.

### A stuck transaction

The anchor is one transaction from the record's key. `onyx-publish catalog-anchor` handles the
usual cases by itself, each run (THREATS RETRY-R1-H1):

| What it finds | What it does |
|---|---|
| Its own transaction for this report, unmined for less than 30 minutes (`--stuck-secs 1800`) | Hands the same bytes to the node again and waits 180 s; exit 75 `not_mined` |
| Its own, unmined for 30 minutes or more | Asks the book whether it would still take the report, then sends the SAME report at the SAME nonce with both fees 12.5% higher (or the market's, if that is more), never above 10 gwei a gas (`--max-fee-wei 10000000000`); at most 4 times (`--max-bumps 4`). Only one transaction of a nonce can be mined, and the book takes a report once |
| Its own, still unmined after the 4 raises | Exit 1: `this report's transaction (nonce N, ...) is still unmined ... its fee was raised 4 times` |
| An unmined transaction of the key that is not in its record | Never replaces it. Exit 75 `earlier_pending` while it has seen it for less than 30 minutes, then exit 1: `an unmined transaction of the gas key 0x... holds nonce N and this tool did not send it` |

Both refusals are exit 7 of the daily run, with an alert line. What a person does (`OUT` and the
values in capitals are the job's own, from its env file; run these as the job's user, and only
while the unit is not running: `systemctl --user is-active onyx-early-record.service` says
`inactive` or `failed`):

1. **Look.** The record, the key's address and its two nonces (the first is what is mined, the
   second counts the pool when the node shows it):
   ```sh
   python3 -m json.tool OUT/catalogs/<day>.bin.anchor-tx.json
   onyx-publish address --key ONYX_EARLY_KEY
   cast nonce <address> --rpc-url ONYX_EARLY_ANCHOR_RPC
   cast nonce <address> --block pending --rpc-url ONYX_EARLY_ANCHOR_RPC
   cast tx <a hash from the record> --rpc-url ONYX_EARLY_ANCHOR_RPC
   cast base-fee --rpc-url ONYX_EARLY_ANCHOR_RPC
   ```
2. **The tool's own transaction, stuck after its raises.** The base fee is above what it may
   offer, or the node drops it. Allow more raises, and a higher cap only if the base fee in step 1
   is above the newest `max_fee_per_gas` in the record (the cap is what one gas may cost at most;
   an anchor uses about 250,000 gas, so 10 gwei is 0.0025 test ETH at the very most):
   ```sh
   onyx-publish catalog-anchor --catalog OUT/catalogs/<day>.bin --rpc ONYX_EARLY_ANCHOR_RPC      --chain-id 84532 --book ONYX_EARLY_BOOK --gas-key ONYX_EARLY_KEY      --stuck-secs 0 --max-bumps 8 [--max-fee-wei <wei a gas>]
   systemctl --user start onyx-early-record.service
   ```
   The first command replaces it once more at once and waits; `ANCHORED` or exit 75 are both
   fine, the unit's run goes on from there.
3. **A transaction the tool did not send.** The record's key is for this job only, so this is
   either an anchor sent by tools built before the record existed (let it mine: it carries a
   day's report, and the run after it prints `ANCHORED already`), or someone used the key for
   something else (it must not be: find out who). The tool never replaces it and this
   repository has no command that does: replacing a transaction means signing another one at
   nonce N with the record's key, which is the operator's decision. Once nonce N is mined
   (`cast nonce` moves past it), start the unit: the anchor goes out at the next nonce.
4. **The report went stale.** The book refuses a report older than 7 days (`Stale`): a day that
   was stuck that long shows `would revert, not sent` or `would revert, not replaced` with
   `Stale`. Signing the same catalog again gives the same report, so the day's catalog is built
   again (it was never recorded: a catalog's time is when it is built, its number stays):
   ```sh
   cd OUT/catalogs && for f in <day>.bin <day>.bin.report <day>.bin.anchor-tx.json; do mv "$f" "$f.stale"; done
   rm -f ../done/<day>.anchored ../done/<day>.catalog-uploaded
   systemctl --user start onyx-early-record.service
   ```
   If the anchor then says `this key already anchored another catalog`, the old report was
   recorded after all: move the three `.stale` files back and start the unit again. An old
   transaction of the stale report that is still in a pool is case 3 (it can only revert).
5. **A transaction that reverted and the book gives no reason** (`... reverted and the book gives
   no reason: not sent again`): look at it (`cast receipt <hash>`, `cast run <hash>`). Out of
   gas is the usual cause; to send the report anew, move `<day>.bin.anchor-tx.json` aside and
   start the unit.

`OUT` holds `versions/YYYY-MM-DD/` (the tool's folder, then the bulk version in it, and
`exclusions/` on a day they change), `catalogs/YYYY-MM-DD.bin` (with its `.report` once signed and
its `.anchor-tx.json` once sent),
`done/` (one marker per day and step) and `lock`. Keep all of it: a catalog is never built or
signed twice under one number. A lost state directory stops the run (exit 1): rebuild it by hand
with `early-record build --out OUT/versions/<day>` for each built day in order (each folder is read
again from the chain and must match byte for byte), then run the daily run again.

Install (the unit file's header has the commands): the env file, the `OUT` folder, then **one run
by hand** with `--dry-run`, then the first real run by hand (the backfill from the deploy blocks:
it reads every day since the deploy blocks twice and verifies each on the second node, so it needs
nodes that keep old state; each day waits for its anchor), then `systemctl --user enable --now
onyx-early-record.timer`.

Local proof: `scripts/early-record-anvil.sh` (check-all) runs the record on a local anvil, and the
daily run's dry run with the real tools on the same chain (its "second node" is the same anvil as
`localhost`: another scheme://host:port to the check, one chain); `scripts/early-record-daily-tests.py`
runs the daily run's every path with stub tools.

## Not yet
- Ops (ROADMAP S26): the first backfill and the timer (done 2026-11-15: the key, its keys document,
  registration, funding, and the operator's test keys in the reviewed list). A new operator test key
  goes in docs/ops/deployment.md "Operator test keys" and in `exclusions.json` with a `from_day`.
- `l2_agreed` rows: their evidence is the round transcript, which is not public while a value is
  sold (THREATS EARLY-16).
