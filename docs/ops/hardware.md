# Hardware requirements

What a machine needs for each Onyx role, before you start (ROADMAP P5). Check yours with
`onyx-node doctor --role <role> --data-dir <dir>` (section 5).

Every number here has one of these tags:

- **[M]** measured: on what machine, when and how is said next to it.
- **[L]** linked: copied from the client's own requirements page, with the URL and the date we
  read it.
- **[C]** a limit in Onyx's code or shipped config (file named).
- **[D]** derived: the sum or the rounding of tagged numbers, with the arithmetic shown.
- **not measured yet**: nobody has measured it. No number is guessed.

Machines behind the [M] numbers:

| Name here | CPU | RAM | Disk | Used for |
|---|---|---|---|---|
| Reference machine (the dev box) | AMD Ryzen 5 5600X, 6 cores / 12 threads | 60 GiB (`MemTotal` 63,659,732 kB) | 3.6 TiB NVMe | Every Onyx benchmark in docs/ops/benchmarks.md, the soak (docs/audit/2026-10-14-soak.md) and this page's own runs (section 4) |
| Maintainer's indexer | AMD Ryzen 9 9900X, 12 cores / 24 threads | 91 GiB (+71 GiB swap, 23 GiB in use) | NVMe, 4 + 8 + 4 TB, 80-94% full | The entity indexer and its unpruned Bitcoin Core node: measured on the maintainer's indexer, 2026-10-18 |
| Build container | 4 CPUs | 8-10 GB cap | | `cargo build --release --locked -p onyx-node` and the Docker images (docs/audit/2026-10-15-fresh-install.md) |

The reference machine is shared and was loaded while it measured (load averages are given with
each run). Times on a quiet machine of the same kind would be shorter.

## 1. Per role, Bitcoin (built)

"Minimum" is the least that is backed by a tagged number. "Recommended" is a machine Onyx was
measured on: it is known to work, it is not a statement that less would fail. Below the
recommended row nothing is measured.

### Contributor: a Bitcoin full node plus Onyx's recipe runs (miner)

| | CPU (logical) | RAM | Disk size and type | Network | Initial sync | Daily run |
|---|---|---|---|---|---|---|
| Minimum | 2 [D: one for a recipe run, which used at most one CPU (47-75% of one) in every run of section 4 [M7], plus one for bitcoind, whose pages name no CPU count: that one is an assumption, not measured] | 6 GiB [D: 2 GB for Bitcoin Core [L1] = 1,907 MiB, plus one recipe run at the host's memory cap, 2 x 2 GiB + 16 MiB = 4,112 MiB [C1]: 6,020 MiB, rounded up to 6 GiB (6,144 MiB)] | 836 GiB unpruned [M2: blocks 825 GiB + chainstate 11 GiB on the maintainer's indexer, 2026-10-18; the chain grows with every block] + 1 GiB free for one day of fetched blocks [M3: 884 MB]; read/write at least 100 MB/s [L2]; NVMe recommended (next row) | Upload at least 400 kbit/s, about 740 GB download the first time, 20 GB a month after [L2]; 500 MB/day down, 5 GB/day up [L1] | not measured yet (Bitcoin Core's pages give no time) | a day's 144 blocks: fetched in 2 min 41 s, then 9 shards x 2 recipes with preflight and a Python recount in 2 min 2 s [M3] |
| Recommended | 12 [M: reference machine] | 60 GiB [M: reference machine] | NVMe [M: every measured machine]; 6 GiB free for a week of fetched blocks [M3: 5.9 GB] | not measured yet | not measured yet | as above |

### Contributor: the entity indexer and reserve path

Two parts. The Onyx side (a reserve scan with `scantxoutset`, then the reserve recipe rerun over
the blocks) needs the contributor row above; its own run is measured in section 4. The entity
indexer that builds the label sets runs today on the maintainer's machines only (ROADMAP P1); its
numbers below are what that setup uses, measured on the maintainer's indexer, 2026-10-18.

| | CPU (logical) | RAM | Disk size and type | Network | Initial sync | Daily run |
|---|---|---|---|---|---|---|
| Minimum | not measured yet | not measured yet | 2,033 GiB [D: blocks 825 + chainstate 11 + indexes 71 GiB + BRK store 1.1 TiB, read as 1,126 GiB, all M2; `du -h` sizes, which round up, so this is an upper bound for that day] | not measured yet | full replay 327-682 min [M2] (after the node's own sync, which is not measured) | 141-205 min (the indexer's own run notes, carried forward; not re-measured on 2026-10-18) |
| Recommended | 24 [M2: 12 cores / 24 threads] | 91 GiB [M2]; the heavy stages run on a second machine with 251 GB and peak at 95-123 GB (documented in the indexer's own notes, not re-measured) | NVMe [M2] | not measured yet | as above | as above |

Bitcoin Core settings on that node [M2]: `prune=0`, `txindex=1`, `coinstatsindex=1`,
`dbcache=32768`, `par=16`, `rest=1`.

### Validator: rerunning recipes

A validator reruns the same recipes on the same blocks, so it needs the contributor row (its own
node, or the shards' block files). Memory per run is bounded by the host: a run may hold up to
about 2 x the 2 GiB `MAX_MEMORY_BYTES` plus a 16 MiB stack (4,112 MiB), so plan 4 GiB free per
concurrent run, as benchmarks.md rounds it [C1]; a run that cannot allocate abstains instead of
giving a verdict.

| | CPU (logical) | RAM | Disk size and type | Network | Initial sync | Daily run |
|---|---|---|---|---|---|---|
| Minimum | 2 [D, as the contributor] | 6 GiB [D, as the contributor] | as the contributor | as the contributor | not measured yet | a week of shards (66 x 2 recipes) rerun in 19 min 52.76 s at 221.6 MiB peak, on a loaded machine (section 4) [M] |
| Recommended | 12 [M: reference machine] | 60 GiB [M: reference machine] | NVMe [M] | as the contributor | not measured yet | as above |

### Buyer: verifying in the browser or the CLI

| | CPU (logical) | RAM | Disk size and type | Network | Initial sync | Daily run |
|---|---|---|---|---|---|---|
| Minimum | not measured yet | not measured yet | none (the browser cache) | Data page 2,138 KiB in all, loaded once [M4: Lighthouse mobile, 2026-10-11; the page has changed since and was not re-measured] | none | none |
| Recommended | not measured yet | not measured yet | none | as above | none | none |

The browser runs the network's own Rust checks compiled to WebAssembly. Native, the same checks
peak at 10.5 MiB (section 4); the browser's own memory is not measured yet.

### Node operator: serving the API (`onyx-node`, `onyx-node-btc`)

| | CPU (logical) | RAM | Disk size and type | Network | Initial sync | Daily run |
|---|---|---|---|---|---|---|
| Minimum | 1 [D: the tokio runtime starts one worker per CPU and runs on one; not measured on a single CPU] | not measured below the recommended 2 GiB; measured use 25.2 MiB peak RSS for `onyx-node` (run 2), 18.3 MiB for `onyx-node-btc` (run 3) [M5] | 1 GiB free [D: image 53.8 MB [M6] + largest state directory 542,874 bytes [M5] = 54.4 MB; 1 GiB is a margin chosen on top, not a measured need] plus your catalog's row files; any local disk, not NFS [C2] | an HTTPS front and an RPC to Base; traffic not measured yet | none (reads finalized roots from the chain) | always on |
| Recommended | not measured yet | 2 GiB [C2: the shipped systemd unit's `MemoryMax=2G` and `docker run --memory 2g`, the size the Docker smoke serves both nodes in; a cap, not a need] | not measured yet | not measured yet | none | always on |

### Coordinator: settling rounds

Not measured yet on its own: every round so far ran the coordinator in the same process as the
miners and validators (docs/ops/demo-local.md). It needs an RPC to Base and the round's block
files.

### Building Onyx from source

`cargo build --release --locked -p onyx-node`: 4 min 40 s on 4 CPUs in a container capped at
10 GB [M6]; the Docker images build in 282 s (`onyx-node`) and 196 s (`onyx-node-btc`) on 4 CPUs
with 8 GB [M6]. Building the recipe tools (wasmtime) in release: run it under a memory cap, as
CLAUDE.md says.

## 2. Ethereum (planned, ROADMAP P4)

Onyx has no Ethereum adapter yet, so nothing Onyx does on Ethereum is measured. The clients' own
pages [L3, L4]:

| Client, mode | CPU | RAM | Disk size and type | Network |
|---|---|---|---|---|
| reth, full node (mainnet, as of 2025-06-23, block 22,700,000) [L3] | higher clock speed over core count | 8 GB+ | at least 1.2 TB, TLC NVMe recommended | stable 24 Mbps+ |
| reth, archive node (same date) [L3] | higher clock speed over core count | 16 GB+ | at least 2.8 TB, TLC NVMe recommended | stable 24 Mbps+ |
| geth, snap-synced full node (page text of September 2022) [L4] | quad-core (or dual-core hyperthreaded) preferred | at least 16 GB | 2 TB SSD recommended with a consensus client; geth itself more than 650 GB, growing about 14 GB a week; HDDs and cheaper SSDs can struggle | at least 25 Mbps download |

The maintainer runs reth with `--http.api eth,trace,net` [M2]; it is not used for entities yet.
`onyx-node doctor --chain ethereum` for a chain role says it is planned and exits 2.

## 3. Settings that matter

- **Prune or not.** Onyx reads each block twice: `getblock <hash> 0` (raw bytes) and
  `getblock <hash> 3` (each input's prevout) (scripts/fetch-btc-node.sh). Verbosity 3 adds the
  prevout from the block's undo data [L5], and `prune=N` keeps only N MiB of "raw block and undo
  data" [L2]. So a pruned node can only feed the shards still inside its window; to rerun older
  shards, or any history, run unpruned (`prune=0`). Pruning cuts disk to about 7 GB [L1, L2] but is
  incompatible with `-txindex` [L2].
- **txindex.** Not used by Onyx's fetch (it asks blocks by hash). The maintainer's indexer runs
  `txindex=1` for its own stages [M2]; its indexes take 71 GiB together with `coinstatsindex`
  [M2].
- **coinstatsindex.** Optional: the reserve scan records the UTXO set's MuHash at its height
  (`gettxoutsetinfo muhash`) as context, not proof (ADR-0034).
- **dbcache.** Bitcoin Core's cache size in MiB. The maintainer's node sets 32768 [M2]; a large
  value speeds up the initial sync and needs that much RAM on top of the rows above. The effect
  on sync time is not measured here.
- **Concurrent recipe runs.** Each needs up to 4 GiB [C1]; RAM decides how many run at once.

## 4. Onyx's own runs, measured for this page

Reference machine, 2026-10-18, release builds, `/usr/bin/time -v` (peak RSS = "Maximum resident
set size"), each under `systemd-run --user --scope -p MemoryMax=12G`, one test thread
(`--test-threads 1`). The machine was running other jobs: the load average (1 min) is given
before and after each run. Block files read from the local `fixtures-live` copies (fetched
read-only from the maintainer's node, docs/ops/benchmarks.md).

Commands: the test binaries built by `cargo test --release --no-run -p onyx-source-btc` (and
`-p onyx-web-verify`), run from their crate directory with `--test-threads 1` and the block
directories named; `bench-golden` from `cargo build --release --example bench-golden`. All under
`nice -n 10`. Each run's `/usr/bin/time -v` lines, load averages and test result, and the script
that ran them: docs/audit/2026-10-18-hardware/ [M7].

| Run | What it does | Wall | Peak RSS | CPU | Load before / after |
|---|---|---|---|---|---|
| `bench-golden` | the reference recipe on 3 real blocks (11,615 txs), wasmi and wasmtime, 5 warm runs each | 2.11 s | 72,152 kB (70.5 MiB) | 75% | 28.60 / 28.60 |
| `golden` (6 tests) | the golden recipe tests on the committed fixture blocks | 8.08 s | 123,772 kB (120.9 MiB) | 63% | 28.60 / 31.57 |
| `golden_series` with `ONYX_LIVE_DIR` | the series recipe on 24 live blocks (2.39 B fuel) plus the fixture golden | 1 min 0.11 s | 305,160 kB (298.0 MiB) | 58% | 31.57 / 30.45 |
| `golden_pools` with `ONYX_WEEK_DIR` | the pool recipe rerun over a week (1,008 blocks, 7 shards, Python recount) | 50.37 s | 41,424 kB (40.5 MiB) | 52% | 30.45 / 28.26 |
| `golden_history` with `ONYX_HISTORY_DIR` | a validator's week: every shard of both recipes (66 x 2) rerun to the export, headers checked | 19 min 52.76 s | 226,944 kB (221.6 MiB) | 47% | 28.26 / 46.40 |
| `golden_reserve` with `ONYX_RESERVE_DIRS` | the reserve path: every reserve shard rerun over the week and the tail to the scan (blocks 969,839-971,199: 1,361 blocks), chained onto the scan | 11 min 50.66 s | 196,716 kB (192.1 MiB) | 53% | 46.40 / 32.39 |
| `onyx-web-verify` lib tests (25) | the browser verifier's checks, native (proofs, pinned roots, small fixtures; no full recipe rerun) | 0.22 s | 10,796 kB (10.5 MiB) | 61% | 32.39 / 32.39 |
| `onyx-web-verify` `reserves` tests (3) | the client reserve check, native | 0.12 s | 9,124 kB (8.9 MiB) | 50% | 32.39 / 32.39 |

What this says: one recipe run on real blocks peaks between 40 and 300 MiB, far under the 4 GiB
a run may take at the host's cap [C1], so the RAM minimum is set by Bitcoin Core plus that cap,
not by today's recipes. The week reruns were on a CPU for only 47-53% of their wall time (1-minute
load 28-46 before and after, 5-minute load up to 54); the rest was waiting for a CPU or the disk,
not split. Their wall times are upper bounds for this machine.

Earlier runs in docs/ops/benchmarks.md (same machine, 2026-10-08 to 2026-10-11): a day of
history (144 blocks, 9 shards x 2 recipes, preflight and a Python recount) exported in 2 min 2 s;
a week (1,008 blocks, 66 shards) in 12 min 53 s, of which the series runs took 112.0 s; one shard's
series run 0.81-2.09 s on wasmi.

## 5. The preflight: `onyx-node doctor`

```sh
onyx-node doctor --role contributor --data-dir ~/.bitcoin   # the disk that holds the node
onyx-node doctor --role validator --data-dir /srv/onyx
onyx-node doctor --role node                               # ONYX_STATE_DIR by default
```

Roles: `contributor`, `validator`, `indexer`, `node`, `coordinator` (default `contributor`);
chains: `bitcoin` (default), `ethereum` (planned: a chain role exits 2). It reads the logical
CPUs this process may use (Rust's `available_parallelism`: CPU affinity and a cgroup CPU quota
count), the usable RAM (the smaller of `/proc/meminfo` `MemTotal` and any cgroup v2 `memory.max`
or cgroup v1 `memory.limit_in_bytes` above the process, so a container's cap counts on v1, v2 and
hybrid hosts), the size and free space of `--data-dir`'s filesystem (`statvfs`, the space an
unprivileged user may use), and the kind of disk under it from `/sys/dev/block/<major>:<minor>`:
NVMe, another non-rotational disk (a SATA or SAS SSD, eMMC), or rotational. A partition reads
its disk's flag; LVM, dm-crypt and md devices are judged by the disks under them (`slaves/`).
It needs no root, only reads files, and writes nothing; a `/proc` or `/sys` file it cannot read
gives `WARN` (could not be read), never a crash. `MemTotal` is a little under the RAM installed
(the kernel keeps some), so a machine with exactly the minimum installed can read just below it.

Each check prints one line: `PASS` (at the recommended value, or at the minimum where no
recommended value is measured), `WARN` (at the minimum but below the recommended value; a value
it could not read; a disk that is not NVMe where NVMe is recommended), `FAIL` (below the minimum)
or `SKIP` (nothing measured for that check yet; or, off Linux, a value read from `/proc` or
`/sys`). Exit 0 without a `FAIL`, 1 with one, 2 on a usage error. "Disk size" is the whole
filesystem, so point `--data-dir` at the disk that holds (or will hold) the node's blocks; "free
disk" is room for Onyx's own block files. A virtual disk shows as whatever its driver reports,
so a cloud disk that reports itself rotational warns.

On the reference machine, 2026-10-19, inside a session limited to 10 CPUs and a slice's
`memory.max`, `--role contributor --data-dir` on the NVMe disk printed `WARN cpu: 10 logical CPUs
...` and `WARN ram: 55,950 MiB usable (cgroup memory limit) ...` (below the 12 threads and 60 GiB
the machine has), then `PASS` for disk size (3,665 GiB), free disk and `disk type: NVMe`, exit 0:
the doctor reports what the process can use, not what the box has. On a 30 GiB tmpfs it printed
`FAIL disk size` and `WARN disk type: unknown`, exit 1. The SATA SSD, HDD, LVM, dm-crypt and md
cases are tested on a copy of sysfs's layout (`doctor::tests`), not on such disks.

At every start and `--check-config`, `onyx-node` and `onyx-node-btc` run the `node` row on
`ONYX_STATE_DIR` (or, before the node has created it, on its nearest existing parent) and print
every line that is not `PASS` (or one `PASS` line). They refuse to start only when that
filesystem has less free space than the largest state directory the soak measured (542,874 bytes
[M5]): every other shortfall is a warning, because a small machine still serves correct answers
(TRACKER, 2026-10-18). Why that number: no code limit bounds the state files (the journals
compact by entry count, not by bytes), so the soak's growth is the only measure of the room the
node's own writes take. Both soak runs topped out at the same size (542,874 bytes in run 2, 542,667
in run 3, from under 1 KB, with the journals compacting back each time); with less free than
that, a run like the soak's would fill the disk with its own state writes. An unreadable free
space never refuses.

## Doctor thresholds

The doctor's numbers, from the rows above (`crates/onyx-node/src/doctor.rs`; a unit test checks
that this table and the code agree). `-` is "not measured yet".

| Role | CPUs min | CPUs rec | RAM min MiB | RAM rec MiB | Disk size min GiB | Disk size rec GiB | Free min GiB | Free rec GiB | Disk type |
|---|---|---|---|---|---|---|---|---|---|
| `contributor` | 2 | 12 | 6,144 | 61,440 | 836 | - | 1 | 6 | NVMe recommended |
| `validator` | 2 | 12 | 6,144 | 61,440 | 836 | - | 1 | 6 | NVMe recommended |
| `indexer` | - | 24 | - | 93,184 | 2,033 | - | - | - | NVMe recommended |
| `node` | 1 | - | - | 2,048 | - | - | 1 | - | any |
| `coordinator` | - | - | - | - | - | - | - | - | any |

## Sources

- [L1] Bitcoin Core requirements: https://bitcoin.org/en/bitcoin-core/features/requirements
  (read 2026-10-18; Bitcoin Core 31.0). Bare minimum, default settings: 750 GB disk, 512 MB RAM,
  250 MB/day down, 5 GB/day up. Custom settings (pruned): 7 GB disk, 256 MB RAM. Minimum
  recommended: 750 GB disk, 2 GB RAM, 500 MB/day down, 5 GB/day up. Plus a one-time 740 GB
  download. The measured blocks and chainstate (836 GiB [M2]) are already above the page's 750 GB.
- [L2] Running a full node: https://bitcoin.org/en/full-node (read 2026-10-18, re-read
  2026-10-19): 7 GB free disk at 100 MB/s read/write, 2 GB RAM, upload at least 400 kbit/s, about
  20 GB download a month plus about 740 GB the first time, 6 hours a day running; "Reduce
  Storage": `prune=N` is the MiB for raw block and undo data, minimum 550, incompatible with
  `-txindex`. Neither bitcoin.org page asks for an SSD or NVMe: that recommendation here rests
  only on every measured machine using NVMe [M].
- [L3] reth system requirements: https://reth.rs/run/system-requirements (read 2026-10-18; reth
  v2.7.0 docs).
- [L4] geth hardware requirements:
  https://geth.ethereum.org/docs/getting-started/hardware-requirements (read 2026-10-18; page
  last edited 2023-09-14, disk figures "at the time of writing (September 2022)").
- [L5] Bitcoin Core 23.0 release notes: https://bitcoincore.org/en/releases/23.0/ (read
  2026-10-18): `getblock` verbosity 3 adds each input's prevout.
- [C1] docs/ops/benchmarks.md "Validator machine requirements" (`MAX_MEMORY_BYTES`, the value
  stack, `HostError::Abstain`).
- [C2] docs/ops/deployment.md (`MemoryMax=2G`, `--memory 2g`, local disk not NFS);
  scripts/docker-smoke.sh serves both nodes with `--memory 2g` (its refused-start controls run in
  `--memory 1g`, but they never serve).
- [M2] Measured on the maintainer's indexer, 2026-10-18, in the read-only P1 survey (the
  machine's CPU, memory and disks, the node's directory sizes as `du -h` printed them (825G, 11G,
  71G, 1.1T) and its settings, and the indexer's run logs for the full-replay times; the daily
  run times were carried forward from the indexer's notes). The survey's notes are not committed:
  they hold paths and host names, which are not copied here.
- [M3] docs/ops/benchmarks.md, history exports of 2026-10-10 (144 blocks, 884 MB) and 2026-10-11
  (1,008 blocks, 5.9 GB), reference machine.
- [M4] docs/ops/benchmarks.md, "History export: one week of shards (2026-10-11)": Lighthouse on
  `#/data` (mobile), 2,138 KiB in all.
- [M5] docs/audit/2026-10-14-soak.md runs 2 (133.5 min) and 3 (150 min), reference machine, 8
  load clients, sampled each minute.
- [M6] docs/audit/2026-10-15-fresh-install.md and docs/ops/deployment.md.
- [M7] docs/audit/2026-10-18-hardware/: section 4's runs, reference machine, 2026-10-18.
