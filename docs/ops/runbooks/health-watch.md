# Runbook: health watch (ROADMAP L12, launch checklist LC-17)

`scripts/health-watch.py` polls the node, the chain RPC, the balances and the public site,
prints one JSON line per check and exits 1 when any check alerts. Read-only: it holds no key and
sends no transaction. Thresholds live in one file,
[deploy/health-watch/thresholds.json](../../../deploy/health-watch/thresholds.json); this page
names them by key and never repeats the numbers. Fault-injection run:
[docs/audit/2026-10-15-health-watch.md](../../audit/2026-10-15-health-watch.md).

## Trigger

Install it next to every node that serves the public (LC-9), and run it by hand after a deploy, a
restart or an RPC change.

## Preconditions

- Python 3.9 or later (standard library only), a checkout of this repository.
- The node's `/v1/health` reachable from the watch (`http://127.0.0.1:8080` on the same box).
- An RPC URL for the chain. It can carry a provider API key: put it in a 0600 file
  (`--rpc-file`) or in `ONYX_WATCH_RPC_URL`, never on the command line. The watch prints only
  `scheme://host[:port]`.

## Steps

1. Run once by hand:

   ```sh
   ONYX_WATCH_RPC_URL=https://sepolia.base.org \
     python3 -I scripts/health-watch.py --once --node http://127.0.0.1:8080 \
     [--site https://<public site>] ; echo "exit $?"
   ```

   Exit 0: every line says `"status": "ok"` (or `skip`, for an address set to `null`). Exit 1:
   at least one `"status": "alert"`. Exit 2: usage, config or another run holds the lock.
   A target left out (no `--node`, RPC or `--site`) alerts when `require.<target>` is true (node
   and RPC in the launch file) and otherwise prints `skip` lines naming why. One run never takes
   longer than `deadline_s`: checks not done by then alert with `deadline` in their detail, and
   the state is still saved.
2. Install the systemd user timer (every minute, `--once`):

   ```sh
   install -D -m 0644 deploy/systemd/user/onyx-health-watch.service ~/.config/systemd/user/onyx-health-watch.service
   install -D -m 0644 deploy/systemd/user/onyx-health-watch.timer ~/.config/systemd/user/onyx-health-watch.timer
   install -D -m 0600 /dev/null ~/.config/onyx/health-watch.env
   ```

   Fill `~/.config/onyx/health-watch.env` (the variables are listed in the service file's header:
   `ONYX_WATCH_REPO`, `ONYX_WATCH_NODE`, `ONYX_WATCH_RPC_URL`, `ONYX_WATCH_SITE`,
   `ONYX_WATCH_NOTIFY_CMD`; the script reads them itself, no shell splits them), then:

   ```sh
   systemctl --user daemon-reload && systemctl --user enable --now onyx-health-watch.timer
   loginctl enable-linger "$USER"   # user timers keep running after logout
   ```

3. Cron instead of systemd (the `set -a` exports the file's variables, which the script reads):

   ```
   * * * * * set -a; . ~/.config/onyx/health-watch.env; set +a; python3 -I /opt/onyx/scripts/health-watch.py --once >>~/onyx-watch.log 2>&1
   ```
4. On the API server (north-star loop NL20): the system units
   [deploy/systemd/onyx-health-watch.service](../../../deploy/systemd/onyx-health-watch.service)
   and `.timer` run the same pass every minute as a dynamic user, next to `onyx-node`. Not
   installed by `scripts/deploy-api.sh` yet; install by hand:

   ```sh
   # on the dev box, from a checkout of the release commit
   rsync -a scripts/health-watch.py onyx-web:onyx-watch-stage/scripts/
   rsync -a deploy/health-watch/ onyx-web:onyx-watch-stage/deploy/health-watch/
   scp deploy/systemd/onyx-health-watch.{service,timer} docs/ops/runbooks/health-watch.md onyx-web:onyx-watch-stage/
   # on the server
   sudo install -d -m 0755 /opt/onyx-watch/scripts /opt/onyx-watch/deploy/health-watch /opt/onyx-watch/docs
   sudo install -m 0644 ~/onyx-watch-stage/scripts/health-watch.py /opt/onyx-watch/scripts/
   sudo install -m 0644 ~/onyx-watch-stage/deploy/health-watch/*.json /opt/onyx-watch/deploy/health-watch/
   sudo install -m 0644 ~/onyx-watch-stage/health-watch.md /opt/onyx-watch/docs/
   sudo install -m 0600 /dev/null /etc/onyx/health-watch.env   # then fill it (the service file's header lists the lines)
   sudo install -m 0644 ~/onyx-watch-stage/onyx-health-watch.{service,timer} /etc/systemd/system/
   sudo systemctl daemon-reload && sudo systemctl start onyx-health-watch.service; systemctl status onyx-health-watch --no-pager
   sudo systemctl enable --now onyx-health-watch.timer && rm -rf ~/onyx-watch-stage
   ```

   The first manual start must exit 0 before the timer goes on. Without a notify command the
   alerts go to the journal and `/var/lib/onyx-health-watch/alerts.jsonl` only, which nobody
   reads: the channel (LC-18) is the owner's choice. A watch on the same box cannot report the
   box itself down (see Limits).
5. Another chain or a rehearsal: overlay a config, e.g. `--config
   deploy/health-watch/local-demo.json` (anvil, chain 31337); overlays may change any key, the
   watch refuses unknown ones. `--print-config` shows the merged thresholds.

## Alerts and the channel (LC-18)

- State: `~/.local/state/onyx-health-watch/state.json` (`--state`). It remembers, per check, the
  consecutive failures, since when it alerts and when it was last sent, so a lasting fault is
  sent once, again after `notify.realert_after_s`, and its recovery once. Every run still prints
  every line and exits 1 while the alert lasts.
- Debounce: `after` in the thresholds sets how many failed runs in a row a check needs before it
  alerts (node up: 3, as section 3 of the checklist asks); earlier runs print `"status": "warn"`
  and exit 0. A malformed answer alerts on the first run.
- Default notifier: each event is appended to `alerts.jsonl` next to the state file (0600) and
  printed on stderr (the journal).
- The hook for a real channel: `--notify-cmd 'CMD ARGS'` (or `ONYX_WATCH_NOTIFY_CMD`) runs `CMD`
  once per event, without a shell, with the event as one JSON line on stdin and an environment
  stripped of every variable whose name suggests a secret (`*KEY*`, `*SECRET*`, `*TOKEN*`,
  `*PASSW*`, `*PRIVATE*`, `*RPC_URL*`, ...; so never `ONYX_WATCH_RPC_URL`):
  `{"event": "alert"|"resolved", "repeat": bool, "check", "status", "value", "limit", "detail",
  "since", "ts", "host"}`. A non-zero exit or a timeout (`notify.command_timeout_s`; all commands
  of one run share twice that) keeps the event unsent; the next run sends it again. No channel is wired: the project owner picks one (LC-18); an
  e-mail or Telegram sender is a 10-line script reading stdin.

## What each alert means

| Check | Fires when (key in thresholds.json) | First action |
|---|---|---|
| `node.up` | `/v1/health` unreachable, timed out or not 200, `after."node.up"` runs in a row | `systemctl status onyx-node`, `journalctl -u onyx-node`; restart; check the state dir disk |
| `node.health` | not JSON, not an object, or `status` not `ok` | Wrong process on the port, or a proxy error page |
| `node.chain` | `chainId` not `chain_id` | Wrong manifest (`ONYX_DEPLOYMENT`): stop serving, fix, restart |
| `node.payments` | payments on but `acceptingPayments` false | A journal write failed or the receipt log is full: free disk, restart (deployment.md "What runs") |
| `node.indexer` | `indexer` null while `node.indexer_required` | Set `ONYX_INDEXER=on` |
| `node.indexer.lag` | the finalized head (the node's, or the watch RPC's if higher) minus the indexer's last read block above `node.indexer_lag_max_blocks`; or the cursor more than `node.indexer_ahead_max_blocks` past that head | RPC slow or rate-limited; see `node.indexer.failing` and the node log; change provider |
| `node.indexer.conflicts`, `candidates_dropped`, `candidates_evicted` | counter above its `_max` | The RPC returned different logs at one position: change provider (deployment.md "Chain indexer"; incident-response.md) |
| `node.indexer.failing` | the last poll failed, `after."node.indexer.failing"` runs in a row | The node log has the error text (summary mode hides it) |
| `node.intents.open` | `settles.intentsOpen` above `node.intents_open_max` | Payments are not settling: the facilitator or RPC; read the unknown-outcome log lines |
| `node.intents.open_for` | `intentsOpen` above 0 without a break for longer than `node.intents_open_for_max_s` | An intent the resolver cannot end: node log, `settle-intents.jsonl`; the RPC must serve `eth_getLogs` |
| `node.intents.unknown_outcomes` | `settles.unknownOutcomes` grew by more than `node.unknown_outcomes_new_max` since the last run | Settles with unknown outcome: the resolver ends them; watch `intents.open_for` |
| `node.*_write_errors`, `node.receipts_dropped` | any write-error counter or `receiptsDropped` above its `_max` | Disk full or read-only state dir: fix, restart (journals recover) |
| `node.clock_skew` | abs(`settles.clockSkewSeconds`) above `node.clock_skew_max_s` (payments refused at 300 s) | Fix NTP (`timedatectl`) |
| `node.reporter.stuck` | `reporter.stuckSince` older than `node.reporter_stuck_max_s`, or more than `node.clock_skew_max_s` in the future | Operator key out of gas or nonce stuck: fund the operator, read the node log |
| `node.reporter.last_error` | `reporter.lastError` set, `after."node.reporter.last_error"` runs in a row | Node log |
| `node.level3` | `level3` not an object; or null or missing while `node.level3_required` (skip line otherwise: no level-3 store yet, or a node from before C62) | Set `ONYX_SIGNED_REPORTS` with the manifest's book pin |
| `node.level3.pending_for` | `level3.oldestPendingSecs` (the node's clock, the one its breaker runs on) above `node.level3_pending_for_max_s` (300 s; the breaker stops level 3 above 600 s, ROADMAP C62, THREATS C10-3) | Before the breaker trips: deploy-signed-book.md "Level 3 stopped" steps 1-2 (the pending entries and their error; fund or switch RPC) |
| `node.level3.stopped` | `level3.stopped` set: `level3_posts_delayed` (the breaker tripped), `level3_clock_skew` (this node's clock over 300 s behind the coordinator's) or `level3_store_unreadable` | deploy-signed-book.md "Level 3 stopped"; for skew, fix NTP on both hosts |
| `node.level3.breaker` | the node's `level3.breakerSecs` leaves less than the longest gap between two watch runs plus one more run (`deadline_s` + 5 + 120 s: 215 s shipped, review C61-R2-1) after `node.level3_pending_for_max_s` | A node built with another breaker: lower the threshold in an overlay (the config refuses more than 385 s with the shipped 90 s `deadline_s`) |
| `node.level3.pending`, `node.level3.refused_new`, `node.level3.retrying` | `postsPending` above `node.level3_pending_max`; `postsRefusedFinal` (refusals that never clear) grew since the last run (each new one alerts once); `postsRefusedRetrying` (a refusal that may clear, which holds its fact) above `node.level3_retrying_max` for `after."node.level3.retrying"` runs | The post log's entries and their `chain.error`; a refusal is the book deciding (Replay, Stale, BadSignature: the source or the coordinator sent something wrong) |
| `rpc.up` | `eth_chainId` or a block read unreachable / timed out / HTTP error, `after."rpc.up"` runs in a row | Provider down: switch `ONYX_RPC_URL` (incident "RPC outage") |
| `rpc.chain` | chain id not `chain_id`, or a malformed JSON-RPC answer | Wrong URL; never point a node at it |
| `rpc.head_age` | latest block older than `rpc.head_age_max_s`, or that far in the future (this box's clock) | Provider stalled; or fix the clock |
| `rpc.finalized_lag` | latest minus finalized above `rpc.finalized_lag_max_blocks` | Chain or provider finality stalled: the indexer waits for finality |
| `balances.deployer_eth` | deployer below `balances.deployer_wei_min` wei | Ask the project owner to fund the deployer (faucet) |
| `balances.operator_eth` | API operator below `balances.operator_wei_min` wei (skipped while the node reports payments off) | Send the operator test ETH from the deployer (`cast send`, testnet key only); paid queries answer 503 once it cannot pay a settle's gas (ROADMAP L19) |
| `balances.revenue` | `USDC.balanceOf(revenue)` below `allocated() - paidOut()`, `paidOut` above `allocated`, `token()` not the configured USDC, or no contract | Contract bug or wrong address: incident "Contract bug" |
| `site.up`, `site.public` | site not 200; `data/demo.json` not `public: true` or a run carries `input` | A preview build is public: republish the `ONYX_PUBLIC=1` build (LC-6) |
| `watch.state` | the state file could not be read or had the wrong shape (moved to `.bad`) | Alerts already sent may be sent again; nothing else to do |
| any check, detail `deadline` | the run's `deadline_s` budget ran out first | A slow RPC or node: see `rpc.up` / `node.up`; raise `deadline_s` and the unit's `TimeoutStartSec` together |

## Verify

`python3 -I scripts/health-watch-tests.py` (in check-all): every threshold at its edge, every
malformed answer, RPC down, stalled and on the wrong chain, de-duplication and the hook. The
fault-injection run in docs/audit/2026-10-15-health-watch.md shows each alert firing against a
real node.

## Roll back

`systemctl --user disable --now onyx-health-watch.timer`. The watch changes nothing it reads.

## Limits

- `/v1/health` has no intent ages: `node.intents.open_for` measures how long the count stayed
  above 0, so steady paid traffic can keep it above 0 and alert falsely; then read
  `settle-intents.jsonl` by hand.
- No metrics endpoint (ADR-0008) and no history beyond the journal and `alerts.jsonl`.
- An indexer that is off alerts once, as `node.indexer` (when required); before 2026-10-28 it
  also raised `node.indexer.state_write_errors` as "malformed" (THREATS NL20-2, fixed).
- The watch runs on the same box as the node: a dead box alerts nobody. An outside probe (a
  second box running `--site` and `--node` against the public URL) is the fix once LC-8 picks a
  host.
