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
- `deploy/early-record/contracts.json` (the deployments) and `deploy/early-record/exclusions.json`
  (the reviewed list of the operator's own keys).
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
| `early-record: contracts: ...` | `contracts.json` does not match the chain (first run only) | 1 |

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
`early-record` and `onyx-publish`), `ONYX_EARLY_REPO`, `ONYX_EARLY_CHAIN_ID`, `ONYX_EARLY_WAIT_SECS`.

| Exit | Meaning | What to do |
|---|---|---|
| 0 | Done, nothing new, or the dry run reached its stop | Nothing |
| 1 | A tool or a node failed | The unit starts it again after 20 minutes (at most 4 starts in 12 hours); it goes on |
| 2 | Usage or config, or the state has built days but `OUT` holds none | Fix the env file; for a lost `OUT`, see below |
| 3 | The second node's rebuild of a day differs | See below; nothing of that day or later is published meanwhile |
| 4 | The privacy scan refused a version folder | Review it (`SHOW=1` on a private machine shows the name); nothing of that day or later is published meanwhile |
| 5 | Another run holds `OUT/lock` | Wait for it |

### When the run stops

The unit restarts only after exit 1. When it ends failed, `onyx-early-record-alert.service` logs one
line with the tag `onyx-early-record-alert` (its result and exit status) to the journal; check it
with `journalctl --user -t onyx-early-record-alert --since -1d` (the health watch runbook names the
same check). Every day before the one that stopped is published.

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

`OUT` holds `versions/YYYY-MM-DD/` (the tool's folder, then the bulk version in it, and
`exclusions/` on a day they change), `catalogs/YYYY-MM-DD.bin` (with its `.report` once signed),
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
