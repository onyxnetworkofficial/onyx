# Health watch: tests and fault-injection run (ROADMAP L12, LC-17)

Date: 2026-10-15 (UTC run times in the transcripts are 2026-10-16 01:15-01:19Z). Branch
`health-watch` from main `9df2e3d`. Tools: Python 3.14.4 (standard library only), anvil 1.8.5,
`onyx-node` release build of this branch. Thresholds:
[deploy/health-watch/thresholds.json](../../deploy/health-watch/thresholds.json). Runbook:
[docs/ops/runbooks/health-watch.md](../ops/runbooks/health-watch.md).

## Result

| Fault injected | Check(s) that fired | Debounce | Notified | Recovery |
|---|---|---|---|---|
| Site serves the private preview `demo.json` | `site.public` | none (run 1) | once; run 2 printed alert with `notify: null` | `resolved` once |
| onyx-node stopped (SIGTERM) | `node.up` "connection refused" | `warn`, `warn`, alert on run 3 (exit 0, 0, 1) | once (run 3), not run 4 | `resolved` after restart |
| RPC stalled (anvil SIGSTOP: port open, no answer) | `rpc.up` "eth_chainId: timed out"; `node.indexer.failing` (node's own `lastError` "node call timed out") | rpc.up alert on run 2, indexer.failing on run 3 | once each | both `resolved` after SIGCONT |
| Watch RPC on another chain (anvil `--chain-id 1337`) | `rpc.chain` 1337 vs 31337 | none | once | `resolved` on the right RPC |
| Launch config (84532) against the local stack (31337) | `rpc.chain` and `node.chain` | none | once each | (separate state file) |
| none: Base Sepolia, read-only | rpc and both balance checks green; OnyxRevenue holds 6,250 = 7,500 - 1,250 | | | |

The notify hook (`--notify-cmd 'python3 -I hook.py'`, event JSON on stdin) received 10 events
over the local run: alert/resolved for `site.public`, `node.up`, `rpc.up`,
`node.indexer.failing` and `rpc.chain`; no duplicates.

Unit tests: `python3 -I scripts/health-watch-tests.py`, 54 tests at the first run (68 after review L12-R1, below), about 25 s, in check-all: every
threshold at its edge (at the limit ok, one past alert), 19 malformed `/v1/health` shapes and 6
malformed JSON-RPC answers (all alert, none pass), RPC down, stalled (bounded by timeout x tries)
and on the wrong chain, HTTP 429/500, de-duplication, re-alert, recovery, a failing hook retried,
a corrupt state file, strict config, the RPC URL never printed, CLI exit codes 0/1/2.
Mutation check by hand: 6 mutants of the watch (`>` to `>=` on every limit, malformed debounce,
deployer and revenue edges, public flag truthiness, future head) each failed the suite.

## Setup (commands)

```sh
export CARGO_TARGET_DIR=$PWD/target
systemd-run --user --scope -q -p MemoryMax=12G cargo build --release --locked -p onyx-node -p onyx-source-btc --bin onyx-node --example demo-local
ONYX_DEMO_RPC_PORT=18645 ONYX_DEMO_API_PORT=18603 ONYX_DEMO_DRIVER=$PWD/target/release/examples/demo-local \
  systemd-run --user --scope -q -p MemoryMax=8G scripts/demo-local.sh --serve --export-node "$X/export"
cast rpc evm_setIntervalMining 2 --rpc-url http://127.0.0.1:18645   # anvil mines only on txs otherwise; head age needs blocks
(set -a; . "$X/export/node.env"; set +a; exec target/release/onyx-node)  # 127.0.0.1:18604, ONYX_INDEXER=on, payments off
# Site: a copy of the built private preview (apps/web/build) on :18620, and the same copy with
# build-web.sh's public transform applied to data/demo.json only (public: true, runs[].input null) on :18621.
python3 -m http.server 18620 --bind 127.0.0.1 --directory "$X/site-preview"
python3 -m http.server 18621 --bind 127.0.0.1 --directory "$X/site-public"
```

`deploy/health-watch/local-demo.json` overlays chain 31337, anvil's first dev account as the
deployer, and no OnyxRevenue (demo-local deploys none: `balances.revenue` prints `skip`). Every
threshold is the launch one. The :18621 copy is not a real `ONYX_PUBLIC=1` build; it only gives
the watch a `demo.json` with the public shape. All servers were stopped after the run.

## Transcripts

Each run: `scripts/health-watch.py --once --config deploy/health-watch/local-demo.json --node
http://127.0.0.1:18604 --rpc http://127.0.0.1:18645 [--site ...] --state run/state.json
--notify-cmd 'python3 -I hook.py'`, one state file across runs 1-6c. Run 1 in full; later runs
show only lines that are not `ok` or carry a notification.

### 1. Green

```
{"ts": 1792113349, "check": "rpc.up", "status": "ok", "value": null, "limit": null, "detail": "http://127.0.0.1:18645", "notify": null}
{"ts": 1792113349, "check": "rpc.chain", "status": "ok", "value": 31337, "limit": 31337, "detail": "", "notify": null}
{"ts": 1792113349, "check": "rpc.head_age", "status": "ok", "value": 1, "limit": 60, "detail": "", "notify": null}
{"ts": 1792113349, "check": "rpc.finalized_lag", "status": "ok", "value": 64, "limit": 1800, "detail": "", "notify": null}
{"ts": 1792113349, "check": "balances.deployer_eth", "status": "ok", "value": "9999981753738830987088", "limit": "1000000000000000", "detail": "", "notify": null}
{"ts": 1792113349, "check": "balances.revenue", "status": "skip", "value": null, "limit": null, "detail": "addresses.revenue or addresses.usdc not set", "notify": null}
{"ts": 1792113349, "check": "node.up", "status": "ok", "value": null, "limit": null, "detail": "", "notify": null}
{"ts": 1792113349, "check": "node.health", "status": "ok", "value": null, "limit": null, "detail": "", "notify": null}
{"ts": 1792113349, "check": "node.chain", "status": "ok", "value": 31337, "limit": 31337, "detail": "", "notify": null}
{"ts": 1792113349, "check": "node.payments", "status": "ok", "value": null, "limit": null, "detail": "payments off", "notify": null}
{"ts": 1792113349, "check": "node.indexer", "status": "ok", "value": null, "limit": null, "detail": "", "notify": null}
{"ts": 1792113349, "check": "node.indexer.lag", "status": "ok", "value": 11, "limit": 1000, "detail": "", "notify": null}
{"ts": 1792113349, "check": "node.indexer.conflicts", "status": "ok", "value": 0, "limit": 0, "detail": "", "notify": null}
{"ts": 1792113349, "check": "node.indexer.candidates_dropped", "status": "ok", "value": 0, "limit": 0, "detail": "", "notify": null}
{"ts": 1792113349, "check": "node.indexer.candidates_evicted", "status": "ok", "value": 0, "limit": 0, "detail": "", "notify": null}
{"ts": 1792113349, "check": "node.indexer.failing", "status": "ok", "value": false, "limit": null, "detail": "", "notify": null}
{"ts": 1792113349, "check": "node.intents.open", "status": "ok", "value": 0, "limit": 100, "detail": "", "notify": null}
{"ts": 1792113349, "check": "node.intents.open_for", "status": "ok", "value": 0, "limit": 1800, "detail": "", "notify": null}
{"ts": 1792113349, "check": "node.intents.unknown_outcomes", "status": "ok", "value": 0, "limit": 0, "detail": "", "notify": null}
{"ts": 1792113349, "check": "node.receipt_write_errors", "status": "ok", "value": 0, "limit": 0, "detail": "", "notify": null}
{"ts": 1792113349, "check": "node.pending_settle_write_errors", "status": "ok", "value": 0, "limit": 0, "detail": "", "notify": null}
{"ts": 1792113349, "check": "node.intent_write_errors", "status": "ok", "value": 0, "limit": 0, "detail": "", "notify": null}
{"ts": 1792113349, "check": "node.indexer.state_write_errors", "status": "ok", "value": 0, "limit": 0, "detail": "", "notify": null}
{"ts": 1792113349, "check": "node.reporter.write_errors", "status": "ok", "value": null, "limit": null, "detail": "no reporter", "notify": null}
{"ts": 1792113349, "check": "node.receipts_dropped", "status": "ok", "value": 0, "limit": 0, "detail": "", "notify": null}
{"ts": 1792113349, "check": "node.clock_skew", "status": "ok", "value": null, "limit": 240, "detail": "not measured yet", "notify": null}
{"ts": 1792113349, "check": "node.reporter.stuck", "status": "ok", "value": null, "limit": null, "detail": "no reporter", "notify": null}
{"ts": 1792113349, "check": "node.reporter.last_error", "status": "ok", "value": null, "limit": null, "detail": "no reporter", "notify": null}
{"ts": 1792113349, "check": "site.up", "status": "ok", "value": 200, "limit": 200, "detail": "", "notify": null}
{"ts": 1792113349, "check": "site.public", "status": "ok", "value": true, "limit": true, "detail": "", "notify": null}
exit 0
```

```
### 2. fault: the site serves the private preview  (01:15:49Z)
{"ts": 1792113349, "check": "site.public", "status": "alert", "value": null, "limit": true, "detail": "data/demo.json is not marked public: a private preview is served (LC-6)", "notify": "alert"}
stderr: health-watch: alert: site.public: data/demo.json is not marked public: a private preview is served (LC-6)
exit 1
### 2b. preview again (de-duplicated)  (01:15:49Z)
{"ts": 1792113349, "check": "site.public", "status": "alert", "value": null, "limit": true, "detail": "data/demo.json is not marked public: a private preview is served (LC-6)", "notify": null}
exit 1
### 2c. public build back  (01:15:49Z)
{"ts": 1792113349, "check": "site.public", "status": "ok", "value": true, "limit": true, "detail": "", "notify": "resolved"}
stderr: health-watch: resolved: site.public: 
exit 0
```

```
$ kill -TERM <onyx-node pid>
node stopped after 1s   [the loop printed its first index: the node had already exited at the first check, before any 1 s sleep, so run 2c and run 3.1 fall in the same second, 01:15:49Z]
### 3.1 fault: node stopped, run 1  (01:15:49Z)
{"ts": 1792113349, "check": "node.up", "status": "warn", "value": null, "limit": null, "detail": "connection refused", "notify": null}
exit 0
### 3.2 fault: node stopped, run 2  (01:15:49Z)
{"ts": 1792113350, "check": "node.up", "status": "warn", "value": null, "limit": null, "detail": "connection refused", "notify": null}
exit 0
### 3.3 fault: node stopped, run 3  (01:15:50Z)
{"ts": 1792113350, "check": "node.up", "status": "alert", "value": null, "limit": null, "detail": "connection refused", "notify": "alert"}
stderr: health-watch: alert: node.up: connection refused
exit 1
### 3.4 fault: node stopped, run 4  (01:15:50Z)
{"ts": 1792113350, "check": "node.up", "status": "alert", "value": null, "limit": null, "detail": "connection refused", "notify": null}
exit 1
```

```
$ (restart onyx-node from the same export and state dir; wait 20 s)
### 4. node back  (01:16:18Z)
{"ts": 1792113378, "check": "node.up", "status": "ok", "value": null, "limit": null, "detail": "", "notify": "resolved"}
stderr: health-watch: resolved: node.up: 
exit 0
```

```
$ kill -STOP <anvil pid>   # the RPC port accepts connections, nothing answers
### 5.1 fault: RPC stalled, run 1  (01:16:24Z)
{"ts": 1792113384, "check": "rpc.up", "status": "warn", "value": null, "limit": null, "detail": "eth_chainId: timed out", "notify": null}
{"ts": 1792113384, "check": "node.indexer.failing", "status": "warn", "value": true, "limit": false, "detail": "the indexer's last poll failed (node log has the error)", "notify": null}
exit 0
### 5.2 fault: RPC stalled, run 2  (01:16:45Z)
{"ts": 1792113405, "check": "rpc.up", "status": "alert", "value": null, "limit": null, "detail": "eth_chainId: timed out", "notify": "alert"}
{"ts": 1792113405, "check": "node.indexer.failing", "status": "warn", "value": true, "limit": false, "detail": "the indexer's last poll failed (node log has the error)", "notify": null}
stderr: health-watch: alert: rpc.up: eth_chainId: timed out
exit 1
### 5.3 fault: RPC stalled, run 3  (01:17:05Z)
{"ts": 1792113425, "check": "rpc.up", "status": "alert", "value": null, "limit": null, "detail": "eth_chainId: timed out", "notify": null}
{"ts": 1792113425, "check": "node.indexer.failing", "status": "alert", "value": true, "limit": false, "detail": "the indexer's last poll failed (node log has the error)", "notify": "alert"}
stderr: health-watch: alert: node.indexer.failing: the indexer's last poll failed (node log has the error)
exit 1
### 5.4 fault: RPC stalled, run 4  (01:17:26Z)
{"ts": 1792113446, "check": "rpc.up", "status": "alert", "value": null, "limit": null, "detail": "eth_chainId: timed out", "notify": null}
{"ts": 1792113446, "check": "node.indexer.failing", "status": "alert", "value": true, "limit": false, "detail": "the indexer's last poll failed (node log has the error)", "notify": null}
exit 1
node /v1/health during the stall: indexer.lastError = 'node call timed out'
$ kill -CONT <anvil pid>; wait 40 s
### 5.5 RPC answering again  (01:18:27Z)
{"ts": 1792113507, "check": "rpc.up", "status": "ok", "value": null, "limit": null, "detail": "http://127.0.0.1:18645", "notify": "resolved"}
{"ts": 1792113507, "check": "node.indexer.failing", "status": "ok", "value": false, "limit": null, "detail": "", "notify": "resolved"}
stderr: health-watch: resolved: rpc.up: http://127.0.0.1:18645
stderr: health-watch: resolved: node.indexer.failing: 
exit 0
```

```
### 6a. fault: the watch's RPC is a different chain (second anvil, --chain-id 1337, port 18646)
{"ts": 1792113519, "check": "rpc.chain", "status": "alert", "value": 1337, "limit": 31337, "detail": "chain id 1337, expected 31337", "notify": "alert"}
health-watch: alert: rpc.chain: chain id 1337, expected 31337
exit 1
### 6b. fault: launch config (chain 84532) pointed at the local stack (chain 31337)
{"ts": 1792113520, "check": "rpc.chain", "status": "alert", "value": 31337, "limit": 84532, "detail": "chain id 31337, expected 84532", "notify": "alert"}
{"ts": 1792113520, "check": "node.chain", "status": "alert", "value": 31337, "limit": 84532, "detail": "node serves chain 31337, expected 84532", "notify": "alert"}
health-watch: alert: rpc.chain: chain id 31337, expected 84532
health-watch: alert: node.chain: node serves chain 31337, expected 84532
exit 1
### 6c. local RPC again  (01:18:40Z)
{"ts": 1792113520, "check": "rpc.chain", "status": "ok", "value": 31337, "limit": 31337, "detail": "", "notify": "resolved"}
stderr: health-watch: resolved: rpc.chain: 
exit 0
```

### 7. Base Sepolia, read-only (no node; public RPC; launch thresholds and addresses)

`scripts/health-watch.py --once --rpc https://sepolia.base.org --state live/state.json`
(`eth_chainId`, `eth_getBlockByNumber`, `eth_getBalance`, `eth_call` only):

```
{"ts": 1792113525, "check": "rpc.up", "status": "ok", "value": null, "limit": null, "detail": "https://sepolia.base.org", "notify": null}
{"ts": 1792113525, "check": "rpc.chain", "status": "ok", "value": 84532, "limit": 84532, "detail": "", "notify": null}
{"ts": 1792113525, "check": "rpc.head_age", "status": "ok", "value": 1, "limit": 60, "detail": "", "notify": null}
{"ts": 1792113525, "check": "rpc.finalized_lag", "status": "ok", "value": 1004, "limit": 1800, "detail": "", "notify": null}
{"ts": 1792113525, "check": "balances.deployer_eth", "status": "ok", "value": "2734883858681919", "limit": "1000000000000000", "detail": "", "notify": null}
{"ts": 1792113525, "check": "balances.revenue", "status": "ok", "value": "6250", "limit": "6250", "detail": "allocated 7500 - paidOut 1250", "notify": null}
exit 0
```

## What this does not show

- No paid traffic: payments were off, so `node.intents.*`, `node.clock_skew`, `node.payments` and
  `node.reporter.*` fired only in the unit tests (fake `/v1/health`), not against a real node.
- `balances.revenue` was green live (read-only) but its alert paths ran only in the unit tests.
- The watch has not run against a public node (none exists: LC-8, LC-9), and no alert channel is
  wired (LC-18); the default sink is `alerts.jsonl` plus stderr.
- `/v1/health` has no intent ages, so `node.intents.open_for` measures how long the count stayed
  above zero (runbook "Limits").
- Base Sepolia's finalized lag was 1,004 blocks at the live run (500 to 649 at other reads) against
  the 1,800-block limit of that run; review L12-R1 raised it to 3,600 (2 h) in thresholds.json.
- The systemd unit and timer were not installed or run: every run here was the script by hand.
  After review L12-R1 the unit passes `systemd-analyze --user verify`, and the script ran once
  under the unit's sandbox as a transient unit (below); the timer never fired.

## Review L12-R1 fixes (2026-10-16)

Each fix has a test in `scripts/health-watch-tests.py` that failed before it (14 new tests, all
red before the code change, green after; 68 in total).

| Finding | Fix | Evidence |
|---|---|---|
| 1. Valid JSON state with wrong inner types crashed every run before the save | `validate_state` checks every `checks[*]` entry and `mem`; a bad file is moved to `.bad` and `watch.state` alerts; a last-line catch of TypeError/AttributeError/KeyError does the same | 14 bad shapes incl. the reviewer's probes s2-s4 (`StateRobustness`); the probe files replayed through the CLI: `watch.state` alert, exit 1, state saved |
| 2. No RPC: rpc.* and balances.* vanished, exit 0 | `require` in thresholds.json (node and RPC true, site false until LC-6): a required target that is missing alerts, the rest print `skip` with the reason; cron recipe uses `set -a` | `MissingTargets` |
| 3. Worst case ~204 s vs TimeoutStartSec=120 | `deadline_s` (90): every HTTP call is cut to what is left, checks not done alert with `deadline`, the state is saved; notify commands share 2 x 30 s; TimeoutStartSec=180 | `Deadline` (a test fails if the unit's timeout drops below deadline + 60); the reviewer's slow-RPC probe (`rpc.py`, first try of each call times out) with `deadline_s` 5: the run ended in 5.1 s, both balance checks alerted `deadline` |
| 4. Hook saw `ONYX_WATCH_RPC_URL` | the command's environment drops every name matching KEY, SECRET, TOKEN, PASSW, CREDENTIAL, PRIVATE, RPC_URL, MNEMONIC, SEED | `test_hook_does_not_see_secrets` |
| 5. `$ONYX_WATCH_ARGS` word-split | the script reads `ONYX_WATCH_NODE`, `ONYX_WATCH_SITE`, `ONYX_WATCH_NOTIFY_CMD`; ExecStart runs python3 directly, no shell | `test_env_supplies_node_site_and_notify_cmd`, `test_unit_is_hardened_and_has_no_word_split_args` |
| 6. Audit gaps | "not shown" names the unit; the 2c/3.1 timestamps are explained in the transcript | `test_docs_state_their_limits` |
| 7. Finality limit reasoning | measured 500-1,004 blocks; limit 3,600 (2 h, over 3x the largest) with the reason in `rpc._about` | same test |
| 8. Unwatched reads | checklist section 3 says why `rewardPool()`, `coordinatorBond()` and `receiptsPending` are read by hand only | same test |
| 9. Clamped impossible values | cursor more than `indexer_ahead_max_blocks` (100) past the finalized head alerts `node.indexer.lag`; `stuckSince` more than `clock_skew_max_s` in the future alerts `node.reporter.stuck` | `Bounds` (edges) |
| 10. Unit hardening | `ProtectSystem=strict`, `ProtectHome=read-only`, `ReadWritePaths=%S/onyx-health-watch`, `NoNewPrivileges=yes`, `PrivateTmp=yes` | `systemd-analyze --user verify` exit 0; transient run below |

Transient run under the unit's sandbox (read-only, public Base Sepolia RPC, no node given):

```
systemd-run --user --wait --collect -q -p ProtectSystem=strict -p ProtectHome=read-only \
  -p ReadWritePaths=$T -p PrivateTmp=yes -p NoNewPrivileges=yes -E ONYX_WATCH_RPC_URL=https://sepolia.base.org \
  -p StandardOutput=file:$T/unit.out /usr/bin/python3 -I $PWD/scripts/health-watch.py --once --state $T/unit-state.json
exit 1: rpc.up, rpc.chain, rpc.head_age, rpc.finalized_lag (623 of 3,600), balances.deployer_eth,
balances.revenue (6,250 >= 6,250) ok; node.up alert "no node ... configured and require.node is true";
site.up, site.public skip "no site ... configured"
```
