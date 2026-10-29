# Launch checklist: public testnet (ROADMAP L10)

What must be true before the website and the paid API on **Base Sepolia** are shared beyond the
owner, who holds which key, what to watch, what to do when something breaks, and how to roll back.
Testnet only: no real funds, no mainnet key. Mainnet has its own gates (section 7, Phase H).

Sources: the L9 launch-blocker list ([threat sweep](../audit/2026-10-14-threat-sweep.md#what-blocks-a-launch-input-for-docsopslaunch-checklistmd-l10)),
the L1-L4, L6 and L8 audit reports in [docs/audit/](../audit/) (soak:
[2026-10-14-soak.md](../audit/2026-10-14-soak.md)), the ops docs in this folder and
[THREATS.md](../../THREATS.md).

Rules for this file (checked by `scripts/check-launch.sh`, run by `scripts/check-docs.sh`):
- every gate is a checkbox line `LC-n` with an owner (`the project owner`, `loop`, or both), a state and at
  least one evidence link (a `VERIFIED` gate needs one into this repository, not only an outside
  URL); every relative link must resolve to a file, and `#anchor` to a heading;
- a gate is ticked only when its state is `VERIFIED`, and `VERIFIED` names the date and the file or
  command that was actually checked. Everything else is `OPEN` or `PARTIAL`.
- re-run the checks on the commit that is launched; a `VERIFIED` from an older commit is a hint, not
  a gate.

## 1. Go/no-go gates

Last checked 2026-10-15 on branch `launch-checklist` (main with L6 and L9 merged, + this change);
LC-25 2026-10-16 on branch `history-scan` (ROADMAP L13); LC-19 and LC-27 2026-10-16 on branch `runbooks` (ROADMAP L15);
LC-1 re-checked 2026-10-16 on branch `rights-fallback` (ROADMAP L14: fallback prepared, gate still open).
Walked again 2026-10-18 on branch `launch-walk` (main `61408a7`, ROADMAP P7): every gate's evidence
re-read against the tree; re-run there: LC-12 (balances), LC-14 (THREATS rows), LC-15
(`check-secrets.sh`), LC-17 (watch tests), LC-20 (`check-deps.sh`, solc list), LC-27
(`check-source-terms.py`). Changed: LC-3 decided (now VERIFIED), LC-16 back to PARTIAL (two
contracts changed since the L2 audit). Logs of that walk are not committed; the results are below.
LC-7, LC-8, LC-9 and LC-17 re-checked 2026-10-27 on branch `nl-lane-b` (north-star loop NL12)
against the hosted testnet node at https://onyx.cryptagraph.com (read-only: GETs, TLS handshakes,
one health-watch run); LC-8 decided. LC-11, LC-13, LC-16 and LC-21 need builds and runs (lane A
of docs/north-star-loop.md).
Count: 27 gates, 12 verified, 15 open (9 of the open ones are launch blockers, groups A-D).

### A. Rights and privacy (blockers)

- [ ] **LC-1 Label-set rights decided, or the fallback shipped (L8-5).** Owner: the project owner
  (decides and publishes) and loop (fallback). State: PARTIAL: the fallback is built and tested
  on branch `rights-fallback` (2026-10-16: fallback build guard and e2e pass; a local onyx-node
  answers 403 `label_rights_withheld` in fallback, 200 cleared; review L14-R1 the same day: a
  public build or a testnet node now refuses to start without `ONYX_RIGHTS`) but neither path below is done,
  and the project owner has not yet written to OKX or Bitwise, so this is still a launch blocker
  (re-read 2026-10-18: data-sources.md section 4 still holds no written permission; the Phase P
  rosters, ROADMAP P2a, add Bybit, Bitfinex, Deribit and others, each needing its own terms read
  before it is served beyond the fallback). Both website builds by default ship entity totals derived from the OKX, BITB,
  BitMEX and Binance label sets, and the API sells an OKX reserve row; OKX ToS 8.1 and BITB's
  examiner terms restrict distribution. The gate passes by **one** of two paths:
  - **A. Rights cleared.** Written permission from OKX and Bitwise (and the Binance and BitMEX
    terms reviewed) filed in data-sources.md section 4; then an `ONYX_RIGHTS=cleared` public
    build and node (the word is required: since review L14-R1 a public build and a testnet node
    refuse to start without `ONYX_RIGHTS`).
  - **B. Fallback shipped.** The public link serves an `ONYX_RIGHTS=fallback ONYX_PUBLIC=1
    scripts/build-web.sh` build (its guard
    `public_build::a_fallback_build_carries_no_label_derived_totals` passed in that build), and
    the public node runs with `ONYX_RIGHTS=fallback`: `/v1/health` shows `"rights":
    "fallback"` and a reserve or flow request answers 403 `label_rights_withheld` before any
    402. What disappears is listed in [the fallback inventory](../audit/2026-10-16-rights-fallback.md).
  Evidence: [ADR-0035](../adr/0035-rights-fallback.md),
  [data-sources.md section 4](data-sources.md#4-needs-the-project-owner-rights-decisions),
  [data audit open items](../audit/2026-10-13-data-privacy.md#open-items-needs-the-project-owner), THREATS L8-5.
- [ ] **LC-2 Public removal contact published (L8-4).** Owner: loop. State: OPEN, written 2026-10-25: no
  e-mail (the project is anonymous); a public issue, or the private vulnerability reporting form for a
  request that names a person. Live when the public repository turns on private vulnerability
  reporting (PP13). Evidence: [label-removal.md "Contact"](label-removal.md#contact),
  [README](../../README.md#corrections-and-removal), THREATS L8-4.
- [x] **LC-3 Removal proof rule for company-signed addresses (L9A-10).** Owner: loop (decided
  2026-10-18 under the loop-decides policy, TRACKER; the project owner may change it). State: VERIFIED
  2026-10-18: label-removal.md's table now has the rule: a person's removal claim on an address
  the owning company proved it controls (a label row at proof level 3: the company's own
  signature over the address) needs a
  BIP-322 proof by the claimant over a message naming the request id; without one it is handled as
  an entity correction, and the address stays. Every other personal claim still needs no proof.
  Evidence: [label-removal.md "Who may ask"](label-removal.md#who-may-ask-and-what-for), THREATS L9A-10.
- [x] **LC-4 Public guard refuses e-mails and profile links (L9A-8).** Owner: loop. State: VERIFIED
  2026-10-15: `public_build::the_guard_refuses_emails_and_profile_links` passes (and fails with the
  two new checks removed). It covers plain, encoded and obfuscated (`[at]`, `(at)`, `&#64;`)
  e-mails, profile links on any subdomain, `@handles`, and GitHub, Bluesky, YouTube, TikTok and
  Mastodon hosts; `logo@2x.png`, `git@github.com`, share links and listed entity accounts pass.
  The real public build passed the extended guard (LC-5). Evidence:
  [public_build.rs](../../adapters/source-btc/tests/public_build.rs), THREATS L9A-8.
- [x] **LC-5 The public build passes its guard.** Owner: loop. State: VERIFIED 2026-10-15 on this
  branch: `ONYX_PUBLIC=1 scripts/build-web.sh` built and its guard passed
  (`a_public_build_carries_no_label_rows`: allowlist, label rows, unlisted addresses, e-mails and
  profile links; coinbase handles; sealed level-2). Its browser e2e is flaky under load: run 1 had
  3 forged-data FAILs (the page itself refused all forgeries when inspected), run 2 a 30 s timeout,
  run 3 `E2E PASSED` (395 checks), all at load average about 60 on 12 cores; run 4, on the tree
  with main merged and the guard extended after review, exit 0 with `E2E PASSED`. Since ROADMAP E10
  (2026-10-16), `ONYX_WEB_PUBLIC=1 scripts/check-all.sh` builds and tests both public builds
  (`ONYX_RIGHTS=cleared`, then `fallback`); a public build without `ONYX_RIGHTS` is refused (L14-R1).
  Last recorded: exit 0 on 2026-10-18 on branch web-style-14, whose code is main `61408a7`
  (TRACKER decision log; not re-run in the 2026-10-18 walk). Evidence:
  [deployment.md "Website builds"](deployment.md#website-builds-aud-2-f1), [build-web.sh](../../scripts/build-web.sh).
- [ ] **LC-6 The public link serves only an `ONYX_PUBLIC=1` build** (with `ONYX_RIGHTS=fallback`
  while LC-1 goes by path B). Owner: the project owner (publishes).
  State: OPEN: the published site is the private preview (shared privately, docs/website-plan.md).
  Evidence: [website-plan.md](../website-plan.md), [deployment.md "Website builds"](deployment.md#website-builds-aud-2-f1).

### B. Infrastructure (blockers)

- [ ] **LC-7 TLS in front of onyx-node; a public `ONYX_LISTEN` is never plain HTTP (M7-7).**
  Owner: loop (ROADMAP L11), the project owner (domain, LC-8). State: PARTIAL 2026-10-27 on the hosted node
  (`deploy/caddy/onyx-web.Caddyfile`, installed by `scripts/deploy-api.sh` on 2026-10-27): checked
  live with `openssl s_client` and `curl`: a Let's Encrypt certificate for onyx.cryptagraph.com
  (valid to 2027-01-23), `http://` answers 308 to `https://`, HSTS and `nosniff` on `/v1/health`,
  HTTP/1.1 only (a client asking for h2 gets 1.1), TLS 1.0 and 1.1 refused (alert 70,
  protocol_version, with the client's security level lowered so it really offers them), TLS 1.2
  and 1.3 accepted; Caddy's admin endpoint off (the deploy refuses to start the node while
  127.0.0.1:2019 answers). Open: the firewall cap (written and tested, `deploy/nftables/onyx-conn-cap.nft`,
  THREATS NL5-3; installing it waits for the owner's OK) and the paid x402 flow through Caddy
  (payments are off on the hosted node until something sellable is served). The Docker
  trusted-proxy item does not apply to this host (systemd, not Docker). Before 2026-10-27: PARTIAL 2026-10-15: the Caddy config
  ships (`deploy/caddy/Caddyfile`, checked against the server by onyx-node `tests/caddyfile.rs`),
  onyx-node refuses a non-loopback `ONYX_LISTEN` without `ONYX_ALLOW_PLAIN_HTTP_PUBLIC=1`, and a
  local run behind Caddy with `tls internal` showed TLS, the HTTP redirect, the 413 body cap and
  rate limits keyed on the forwarded client. Open: the same file on the real domain with an ACME
  certificate (needs LC-8), a host-firewall cap on connections per client (THREATS TLS-2), the paid
  x402 flow through Caddy (402 with `payment-required`, a settled 200 with `payment-response`, a 503
  `payment_pending` retry; the local run had payments off), a server-side TLS 1.1 refusal test, and
  the Docker trusted-proxy setting (`ONYX_TRUSTED_PROXIES` = the bridge gateway, TLS-7: docs only,
  not run). The three items that need no domain (paid flow, TLS 1.1 refusal, Docker proxy) are
  ROADMAP L18 (loop; Caddy is not installed on the dev box today); the domain and the firewall cap
  wait for LC-8.
  Evidence: [audit 2026-10-15-tls-front.md](../audit/2026-10-15-tls-front.md),
  [deployment.md "TLS in front"](deployment.md#tls-in-front-roadmap-l11-threatsmd-m7-7),
  [Caddyfile](../../deploy/caddy/Caddyfile), THREATS M7-7.
- [x] **LC-8 Where the API runs, and its domain.** Owner: the project owner (hosting may cost money).
  State: VERIFIED 2026-10-27: decided and running: the site's server (Hetzner CX23, Ubuntu) at
  https://onyx.cryptagraph.com, the API behind `/v1/` (owner's OK for the deploy on 2026-10-27);
  checked live: `GET /v1/health` 200, chain 84532, the NS17 settlement contract. Evidence:
  [site-hosting.md](site-hosting.md), [deployment.md open questions](deployment.md#open-questions),
  [ROADMAP "Needs the project owner"](../../ROADMAP.md#needs-the-project-owner-the-loop-never-does-these).
- [ ] **LC-9 Public node configured and checked.** Owner: loop, on the chosen box. State: PARTIAL
  2026-10-27: from the tree and the live node: `ONYX_INDEX_HEALTH=summary` (live `/v1/health` shows
  `indexer.detail` false), `ONYX_TRUSTED_PROXIES=127.0.0.1` and the public `ONYX_RPC_URL`
  https://sepolia.base.org (no API key) are written by `scripts/deploy-api.sh`, the unit has
  `TimeoutStopSec=75s` and its state under `/var/lib/onyx` (local disk); payments off, so the state
  dir holds no receipts yet. Open: `onyx-node --check-config` on the server and a state backup
  before payments go on (a server session, at the final deploy). Needs: `onyx-node --check-config` exit 0; `ONYX_INDEX_HEALTH=summary`;
  `ONYX_TRUSTED_PROXIES` set to the proxy only; `TimeoutStopSec=75s`; state dir on local disk with a
  backup; `ONYX_RPC_URL` a node we trust (or the URL as a 0600 file). Evidence:
  [deployment.md "Run with systemd"](deployment.md#run-with-systemd), [deployment.md "Backups"](deployment.md#backups),
  [config.md](config.md), [systemd unit](../../deploy/systemd/onyx-node.service).

### C. Code and live runs (blockers)

- [x] **LC-10 `ONYX_WEB=1 scripts/check-all.sh` green.** Owner: loop. State: VERIFIED 2026-10-15
  on this branch (exit 0: fmt, clippy, tests, boundaries, docs incl. this checklist, secrets, deps,
  forge, storage layout, gas, website build and e2e). Re-run on the launch commit. Last recorded
  green run: `ONYX_WEB_PUBLIC=1 scripts/check-all.sh` exit 0 on 2026-10-18 (branch web-style-14,
  same code as main `61408a7`; TRACKER). Another full run that day failed once on an onyx-api
  wall-clock test that passes alone (ROADMAP E12), so the launch run may need E12 first. Evidence:
  [check-all.sh](../../scripts/check-all.sh).
- [ ] **LC-11 Fresh Base Sepolia run on the launch commit.** Owner: loop. State: OPEN: the last live
  run (A2, 2026-10-14, 198 txs, REHEARSAL PASSED) predates the L9 merge and every C-phase contract
  change (LC-16). Loop work on the launch commit (ROADMAP P7a), after L17; the deployer holds enough
  for it (LC-12). Evidence:
  [demo-testnet.md](demo-testnet.md), Run page data (not in the public copy).
- [x] **LC-12 Deployer funded for a run.** Owner: the project owner (funds) and loop (checks). State: VERIFIED
  2026-10-18 (03:41 UTC on the 19th; first 2026-10-15) by `cast chain-id` (84532), `cast balance`
  and `cast call <USDC> "balanceOf(address)"` against https://sepolia.base.org: 0.00273 test ETH
  (2,734,883,858,681,919 wei) and 0.9625 test USDC (962,500 smallest units), unchanged since
  2026-10-15; the A2 run used 0.000262 ETH, the watch alerts below 0.001 ETH. Evidence: [deploy-base-sepolia.md](deploy-base-sepolia.md#before-you-start-the-project-owner-about-10-minutes).
- [x] **LC-13 Soak merged and the merged tree soaked (L6, SOAK-7).** Owner: loop. State: VERIFIED
  2026-10-28 (north-star loop NL12, lane A): main `ccbe553` soaked for 150 min with the run-3 shape
  (`scripts/soak.sh --minutes 150 --cycle-minutes 30 --leak-window-minutes 60 --rounds 18
  --round-every 480`, release binaries prebuilt, `ONYX_LIVE_DIR` and `ONYX_RESERVE_DIR` set to the
  block folders): exit 0, `summary.json` `"pass": true`, 0 violations. 30,809 load requests, 6,586
  paid answers served and 6,588 booked (none double-booked, none unproven, none unbooked), 2,995
  replays identical, 10 new rounds finalized and all 12 shards servable on both nodes, 18 restarts
  (8 SIGTERM, 8 SIGKILL plus the final stops; 3 starts failed once in the RPC outage phase and then
  started), 146,307 proxied node calls with 9,869 injected faults; node RSS at most 31.7 MiB, btc
  node 43.2 MiB. The growth gate is "not evaluated" (no restart-free hour by design, as in run 3).
  Logs: `~/.cache/onyx-gates/soak` (outside the repo). Before: PARTIAL:
  L6 is merged (main `28de188`: SOAK-1..4 fixed, run 3 held every invariant for 150 min), but main
  after the merge has not been soaked (ROADMAP E9). Evidence: [soak report](../audit/2026-10-14-soak.md),
  [ROADMAP L6](../../ROADMAP.md#phase-l-launch-readiness-before-any-public-launch-after-a-before-b).
- [x] **LC-14 No open Critical/High threat blocks a testnet launch (L9).** Owner: loop. State:
  VERIFIED 2026-10-18 on branch `launch-walk` (first 2026-10-15) by reading each row
  (`scripts/check-threats.py` checks citations and
  milestones, not severity, so it is no evidence here). Rows rated Critical or High in their
  severity column or text were listed and every one not FIXED, CLOSED, TESTED or ACCEPTED was
  read. No Critical row is open. Three High rows were added after the sweep and are partial, each
  with its testnet decision in its row ("P7 2026-10-18" note): TR-9 and TR-C20-4 (a dishonest
  server or coordinator inflates a track record: the node and the coordinator are operator-run,
  and a record drives no reward, serving or level, `onyx_track::tests::a_perfect_record_never_changes_a_level`);
  B2-4 (colluding level-2 observers: the L2-F2 limit, covered by the conditions below). The
  seven from the sweep are still open, partial or deferred, each with the sweep's per-row testnet decision in its THREATS row
  ("L9 2026-10-14" note) and the [sweep's decisions table](../audit/2026-10-14-threat-sweep.md#decisions-table-medium-and-higher-plus-the-unrated-base-threats):
  R-CANARY-1 (H2) and R-DA-1, R-DA-3 (H7): safe while every validator and miner is operator-run;
  R-DRAW-3 (H3): fine at testnet stakes; R-SCORE-1 (C1): scores drive no reward, serving or level;
  L2-F2 (C4): money markets take levels 1 and 3 only
  (`proof::tests::money_markets_take_levels_one_and_three_only`); L3-F3 (H6): no level-3 fact is
  served or settled. **Launch conditions** that follow: only operator-run miners and validators, no
  level-2 money market, no level-3 fact, no track record that drives money or a level; a change to
  any of them reopens this gate. The sweep's
  remaining launch blockers are LC-1..3, LC-4 and LC-7 here.
- [x] **LC-15 No secret in the tracked tree (T-KEY-1).** Owner: loop. State: VERIFIED 2026-10-18:
  `scripts/check-secrets.sh` exit 0 standalone on branch `launch-walk` (first 2026-10-15 inside
  check-all). History: LC-25. Evidence:
  [check-secrets.sh](../../scripts/check-secrets.sh), [secrets.md](secrets.md).
- [ ] **LC-16 Contracts: audit invariants and gas/layout gates pass (L2).** Owner: loop. State:
  PARTIAL 2026-10-28 (north-star loop NL12, lane A, on main `ccbe553`'s contracts, after the NL14
  fixes and the LC16-1/LC16-2 fixes): `forge fmt --check`, sizes (`OnyxSettlement` 23,232 B, 1,344 B
  under the limit), `forge test` (697 passed, 1 skipped fork test), the Settlement/Ext storage
  layout (identical, 22 slots) and the gas snapshot `--check` all pass; the invariants at audit
  depth (512 x 256, seed `0x20261028`) pass: 24 invariants in 7 suites, 0 failures, 1,500 s.
  Slither 0.11.6 (`--filter-paths "lib/|test/|script/"`): 172 results (High 8, Medium 48, Low 83,
  Informational 30, Optimization 3), 9 more than on 2026-10-27, all on the changed contracts and
  triaged in the report's 2026-10-28 addendum: 0 security findings. Open: the gate closes only on
  the launch commit, which is not chosen yet (a contract change after `ccbe553` reopens it); the
  live reobserve, panel and stakes contracts still run the pre-LC16 code until redeployed. Before:
  PARTIAL 2026-10-27 (north-star loop NL12, lane B, branch `nl-lane-b` at main `037bc1a`'s
  contracts): every gate ran and passed on the current contracts: `forge fmt --check`, sizes and
  `forge test` (674 passed, 1 skipped fork test), the Settlement/Ext storage layout (identical, 22
  slots), the gas snapshot `--check` (628 tests), and the invariants at audit depth (22 invariants,
  512 x 256, seed `0x20261027`, 0 failures, 1,239 s). Slither 0.11.6 over every contract changed
  since L2 (12 files): 63 results on them, 0 security findings after triage, one dead constant
  (`OnyxFutures.PPM`); the role and access review adds THREATS LC16-1 and LC16-2 (Low, TODO S4 and
  S3). Report: [2026-10-27-lc16-slither.md](../audit/2026-10-27-lc16-slither.md). Open: the gate
  closes on the launch commit, after lane A's NL14 contract fixes (NL6-10..14) are in and these
  same commands pass again on it. Before: PARTIAL 2026-10-18: forge tests, storage layout and `.gas-snapshot --check` pass inside check-all
  (last recorded green run 2026-10-18, LC-10), but the 2026-10-15 claim "contracts unchanged since
  the L2 audit" is no longer true: since the L2 merge (`15635c4`) `OnyxRegistry.sol` gained 407
  lines (C4, C11, C51: sealed reveals, time-locks) and `OnyxSignedBook.sol` is new (811 lines; C2,
  C7, C10). Both have forge tests, invariants and fresh per-item reviews; Slither has not run over
  them. A fresh launch run (LC-11) deploys the new registry. Settlement, Ext, Disputes, Revenue and
  Ledger are unchanged since L2. Closes with ROADMAP L17 (Slither and a role/access review of the
  two contracts, about 2 hours). Evidence: [contract audit](../audit/2026-10-13-contracts.md#result),
  [OnyxRegistry.sol](../../contracts/src/OnyxRegistry.sol), [OnyxSignedBook.sol](../../contracts/src/OnyxSignedBook.sol).

### D. Operations (blockers: LC-17 and LC-18)

- [ ] **LC-17 A health watch with alerts runs against the public node.** Owner: loop (ROADMAP
  L12). State: PARTIAL 2026-10-27: `scripts/health-watch.py --once` ran read-only against
  https://onyx.cryptagraph.com and https://sepolia.base.org: every check ok or an explained skip
  (exit 0) after the block limits were fixed for 0.2 s blocks (THREATS NL5-2; the first run alerted
  `rpc.finalized_lag` on normal batching); the operator's gas is watched (NL5-1); 86 unit tests.
  Open: a scheduled run against the node (the timer on a box that is not the server) and an alert
  channel (LC-18). 2026-10-28 (NL20): system units for a run on the server itself
  (`deploy/systemd/onyx-health-watch.{service,timer}`, health-watch.md step 4, not installed yet);
  an indexer that is off now alerts once, not twice (NL20-2); 87 unit tests. Before: PARTIAL 2026-10-15: the watch exists and is tested (`scripts/health-watch.py`,
  thresholds in [thresholds.json](../../deploy/health-watch/thresholds.json), 68 unit tests in
  check-all, a systemd user timer) and every alert fired against a local anvil and demo node and
  read-only against Base Sepolia ([fault-injection run](../audit/2026-10-15-health-watch.md));
  `python3 -I scripts/health-watch-tests.py` re-run 2026-10-18: 68 tests, exit 0. Still open: it does not run against a public node (none exists, LC-8, LC-9) and alerts reach
  only a file and the journal until a channel is picked (LC-18; `--notify-cmd` is the hook).
  Evidence: [runbook](runbooks/health-watch.md), section 3 below.
- [ ] **LC-18 Someone is on call, with a channel.** Owner: the project owner. State: OPEN (incident-response.md
  open question). Evidence: [incident-response.md](incident-response.md#open-questions).
- [x] **LC-19 Incident runbooks for the launch (stop serving a shard, rotate the reporter, redeploy
  and announce).** Owner: loop (ROADMAP L15). State: VERIFIED 2026-10-16 on a local anvil fork of
  Base Sepolia with a local onyx-node, each runbook once, rollback included: a stopped shard
  answered 404 and came back; `setReporter` to a new key, the old key refused by the node at start
  and by the contract, a paid request settled from the new key; a second deployment (new ledger
  id), the node switched to it and back, old roots `verifyRow` true at the old address. Limits:
  the systemd commands (`sudo`, `systemctl`, `/etc/onyx`) ran in their export-directory form;
  the Base Sepolia redeploy and the announcement edits were not run. Walk 2026-10-18: the
  redeploy runbook's website step still said `ONYX_PUBLIC=1 scripts/build-web.sh`, which the build
  refuses since L14-R1 (no `ONYX_RIGHTS`), and its env-file merge did not keep `ONYX_RIGHTS`;
  both fixed in the runbook (text only, not re-rehearsed). Evidence:
  [stop-serving-shard.md](runbooks/stop-serving-shard.md), [rotate-reporter.md](runbooks/rotate-reporter.md),
  [redeploy-new-ledger.md](runbooks/redeploy-new-ledger.md),
  [rehearsal report](../audit/2026-10-16-runbook-rehearsal.md).

### E. Should do (not blockers)

- [x] **LC-20 Dependency gate and solc bug list re-checked (DEP-1, DEP-2).** Owner: loop. State:
  VERIFIED 2026-10-18 on branch `launch-walk`: `scripts/check-deps.sh` exit 0 online (advisories,
  bans, licences, sources ok; `pnpm audit` no known vulnerabilities); bugs_by_version.json fetched
  again lists the same 8 bugs for 0.8.30 (newest release 0.8.37, so SOL-2026-7 is still unfixed),
  and `contracts/src` still has no `transient` variable after the C-phase changes. Review of the
  walk, same day: every other condition the 8 bugs need is still absent over the new code too
  (no `layout at`, no named-argument `require` error, no recursion: a name call-graph over all 240
  functions finds only the `ledgerId` overload, as in the dependency audit). First
  VERIFIED 2026-10-15: `scripts/check-deps.sh` passes online (RustSec advisories, bans,
  licences, sources ok; `pnpm audit`: no known vulnerabilities), standalone and inside check-all;
  solc's [bugs_by_version.json](https://github.com/ethereum/solidity/blob/develop/docs/bugs_by_version.json)
  fetched today lists 8 bugs for 0.8.30, one new since L1: SOL-2026-7
  `YulOptimizerKnowledgeBaseStaleBookkeeping` ([bugs.json](https://github.com/ethereum/solidity/blob/develop/docs/bugs.json):
  severity low, introduced 0.8.19, fixed 0.8.38, which is not released; condition
  `yulOptimizer: true`). **We meet the condition**: `contracts/foundry.toml` sets `optimizer = true`
  and `via_ir = true`, so the Yul optimizer runs on all our code; whether a miscompile happened
  cannot be told by reading the source. New THREATS DEP-3, accepted for testnet, mainnet H5.
  Evidence: [dependency audit](../audit/2026-10-13-dependencies.md#advisories-and-decisions),
  [foundry.toml](../../contracts/foundry.toml), THREATS DEP-1..3.
- [ ] **LC-21 Fuzz smoke on the launch commit (FUZZ-5).** Owner: loop. State: PARTIAL 2026-10-28
  (north-star loop NL12, main `ccbe553`): `FUZZ_SECS=30 scripts/fuzz-smoke.sh` exit 0, all 19 targets
  ran 30 s without a crash (between 7,637 and 3,853,988 executions each), no file under
  `fuzz/artifacts/`; build 7 min 49 s; `fuzz/target` 4.6 GB. `fuzz/Cargo.lock` was stale against
  the workspace's path crates and is refreshed. Open: re-run on the chosen launch commit
  (`scripts/fuzz-smoke.sh`, nightly, about 10 min). Evidence: [fuzzing report](../audit/2026-10-13-fuzzing.md).
- [ ] **LC-22 Benchmarks and an API load test (L5).** Owner: loop. State: PARTIAL 2026-10-28
  (NL20): `scripts/load-test.mjs` ran against the release binary and the deployed export on the
  dev box (no Caddy, no TLS): up to 7,090 requests/s, p99 41 ms at 64 concurrent, no error or
  5xx; the server itself was not load-tested. Open: the rest of L5 (rerun every benchmark).
  Evidence: [benchmarks.md "API load test"](benchmarks.md#api-load-test-2026-10-28-north-star-loop-nl20-lc-22).
- [x] **LC-23 Fresh-machine systemd install (L7).** Owner: loop. State: VERIFIED 2026-10-15 on
  branch fresh-install: the shipped unit on a fresh Ubuntu 26.04 (systemd 259 as PID 1, in a
  container with `CAP_SYS_ADMIN`) by deployment.md alone: health 200, row 200, restart, clean
  stop exit 0, security 1.2 OK; credentials fixed (L7-SD-1, tightened in review to root:root only); BuildKit images of
  bbfa29a (unpinned syntax line) and of 3233ca3 (pinned syntax line, origin/main merged) both
  pass the Docker smoke. Limits: not a real VM, payments off under the unit; re-run on the chosen host (LC host
  decision). Evidence: [fresh-install report](../audit/2026-10-15-fresh-install.md),
  [Docker report](../audit/2026-10-13-docker.md).
- [ ] **LC-24 CI runs again (AUD-1-F1).** Owner: the project owner (GitHub Actions billing). State: OPEN; every
  loop runs check-all locally meanwhile. Evidence: [ROADMAP "Needs the project owner"](../../ROADMAP.md#needs-the-project-owner-the-loop-never-does-these).
- [ ] **LC-27 Every served source's terms re-read before the release (L2-SRC-4).** Owner: loop
  (runs the step), the project owner (reads pages a fetch cannot). State: PARTIAL 2026-10-16: the release
  step exists (`python3 -I scripts/check-source-terms.py`, 90-day threshold, its table in
  data-sources.md section 5, shape and self-test in check-docs). Its run on 2026-10-16 exits 1:
  Binance's site terms (S8, inside label set v2, S4) were never readable by fetch and need a human
  read; every other served source was read 2026-10-14. Re-run 2026-10-18: exit 1, the same single
  row (S8 site terms unread; every read source 5 days old, threshold 90); a plain fetch of
  Binance's terms page again returned 202 with an empty body (a script challenge). Next: the loop
  reads it with the headless Chromium the e2e already drives (`playwright-core`, ROADMAP P7a); a
  human read in a browser only if that is refused too (section 6). Re-run on the launch commit. Evidence:
  [release-source-terms.md](runbooks/release-source-terms.md),
  [data-sources.md section 5](data-sources.md#5-release-step-served-sources-terms-lc-27-threats-l2-src-4).

### F. Only if the repository goes public (not needed for the site or API)

- [ ] **LC-25 History secret scan (T-KEY-1 covers the tree only).** Owner: loop (ROADMAP L13),
  lead or the project owner (the `.env` cross-check). State: PARTIAL 2026-10-16: gitleaks 8.30.1, trufflehog
  3.97.9 and `scripts/check-secrets.sh --history` over all refs, 18 unreachable commits and the
  tree found no real secret (rescanned 2026-10-16 after merging main: gitleaks 0 with the reviewed
  allowlist, 9 reviewed without it), and no 64-hex string in history is the deployer's key (checked
  2026-10-15); `scripts/check-secrets.sh --history` repeats the pattern and gitleaks scans, merge commits
  included, and fails without gitleaks (SCAN-4/5, review 2026-10-16); left: the
  counts-only cross-check of the real `.env` values against history (THREATS SCAN-3; the loop never
  reads `.env`) and a rescan right before any public push. Evidence:
  [scan report](../audit/2026-10-16-history-secret-scan.md), [.gitleaks.toml](../../.gitleaks.toml),
  [check-secrets.sh](../../scripts/check-secrets.sh).
- [ ] **LC-26 History with excluded data: rewrite or keep private (L8-H1..H3, AUD-2-F13, OSS-18).** Owner:
  the project owner. State: OPEN; the repository is private. The same decision covers OSS-18: every commit's
  author and committer e-mail in history is one personal address (counted 2026-10-17, not written
  here), so going public with history as is publishes it; a rewrite to a noreply or role address
  can be done in the same pass as the data rewrite (every hash changes; only with the project owner's
  explicit approval). Evidence:
  [data audit open items](../audit/2026-10-13-data-privacy.md#open-items-needs-the-project-owner),
  [open-source checklist](open-source-checklist.md), THREATS L8-H1..H3.

## 2. Keys and owners

Names and roles only; never a value. Testnet keys only (CLAUDE.md hard limits). Roles read back
2026-10-15 with `cast call` against https://sepolia.base.org (chain id 84532 checked first) for the
A2 deployment (settlement `0x4edc...5640`, OnyxRevenue `0x9041...d7dc`): settlement
`coordinator()`, OnyxRevenue `owner()` and `treasury()` return the deployer; `reporter()` returns the
operator key's address.
Full map: [contract audit key map](../audit/2026-10-13-contracts.md#testnet-key-and-owner-map-base-sepolia).

| Key (name) | Where | Role | Held by | If leaked or lost |
|---|---|---|---|---|
| Deployer `BASE_SEPOLIA_PRIVATE_KEY` (address `0x80c9...7972`, public) | `.env` (git-ignored) | Deploys every contract; settlement **coordinator** (`openRound`, `arbitrate`, registry coordinator powers, `setReceiptSigner`); disputes binder; OnyxRevenue **owner and treasury** (`setReporter`) | The project owner supplies, the loop uses | Immutable roles: redeploy (new ledger id), see section 4 |
| `operator` | `.onyx-testnet/keys/operator.key` (0600); on a server `/etc/onyx/operator.key` via `LoadCredential` | OnyxRevenue **reporter**: self-settle, `credit`, `setLabelPayee`, `registerObservation`; gas only, owns nothing | loop / the node | Owner calls `setReporter` to a new key; credits already made stay |
| `registry-owner` | `.onyx-testnet/keys/registry-owner.key` | OnyxRegistry **owner**: `registerMetric`, `registerLabelSet`, `registerMetricMinLevel`, `reopenDraws` (append-only) | loop | Immutable: redeploy the registry, new manifest |
| `receipt-signer-<registry>` | `.onyx-testnet/keys/receipt-signer-<registry address>.key` (0600; made by the deploy run for a level-2 run, one per OnyxRegistry deployment; `--receipt-key FILE` for the `level2-*` runners) | OnyxRegistry level-2 receipt signer (signs receipts, sends no transaction); never the deployer, reporter, registry owner or an observer (the run refuses to start, ROADMAP B3) | loop | Configure a new key: the next draw is preceded by `setReceiptSigner`; draws announced earlier keep the old key until they end, so finish them with it first (THREATS REVLOG-4). Local rehearsals still use the published test key |
| `miner-*`, `validator-*`, `customer` | `.onyx-testnet/keys/` | Simulated miners, bonded validators, x402 payer | loop | Testnet stake only; make new keys for the next run |
| CDP faucet `CDP_API_KEY_ID`, `CDP_API_KEY_SECRET` | `.env` or environment | Requests Base Sepolia ETH/USDC from the Coinbase Developer Platform faucet (`scripts/testnet-faucet.py`) | The project owner's CDP account | Revoke in the CDP console, issue a new pair |
| RPC URL `ONYX_RPC_URL` / `BASE_SEPOLIA_RPC_URL` | env file 0600 or `ONYX_RPC_URL_FILE` | Node access; a provider URL can carry an API key (today the public sepolia.base.org, no key) | The project owner if paid | Rotate at the provider |
| Accounts: GitHub, website publishing, domain | owner's accounts | Repository, published site, DNS | The project owner | Owner's account recovery |

Mainnet key ceremony (K-1..K-8: one key per role, multisig/KMS addresses before deploy, signed
manifest) is Phase H1 and H4, section 7.

## 3. Monitoring

`scripts/health-watch.py` polls everything below every minute (systemd user timer) and alerts
(LC-17, [runbook](runbooks/health-watch.md)). **Thresholds: [deploy/health-watch/thresholds.json](../../deploy/health-watch/thresholds.json)**,
the only copy; the column below names its keys (`--print-config` prints the values). Gaps left:
no public node runs it yet (LC-8, LC-9), no alert channel (LC-18), no metrics endpoint.

| What | Read by the watch (and by hand) | Alerts when (key) | Check names |
|---|---|---|---|
| Node up | GET `/v1/health` (by hand: `onyx-node healthcheck`, Docker `HEALTHCHECK`, `journalctl -u onyx-node`) | unreachable, timeout, not 200, malformed, `status` not ok, after `after."node.up"` failures in a row; `chainId` not `chain_id` | `node.up`, `node.health`, `node.chain` |
| RPC health | `eth_chainId`, latest and finalized blocks (by hand: `cast chain-id`, `cast block finalized`) | wrong chain; head older than `rpc.head_age_max_s`; finalized lag above `rpc.finalized_lag_max_blocks`; down after `after."rpc.up"` runs | `rpc.up`, `rpc.chain`, `rpc.head_age`, `rpc.finalized_lag` |
| Indexer lag | `/v1/health` `indexer`: `nextBlock` vs `finalizedHead` (and the watch RPC's finalized head), `failing`/`lastError`, `conflicts`, `candidatesDropped`, `candidatesEvicted`, `stateWriteErrors` | lag above `node.indexer_lag_max_blocks`; any conflict, dropped or evicted candidate (change provider); failing `after."node.indexer.failing"` runs; indexer off | `node.indexer*` |
| Payment intents | `/v1/health` `settles`: `intentsOpen`, `unknownOutcomes`, `intentWriteErrors`, `clockSkewSeconds`; `receiptWriteErrors`, `pendingSettleWriteErrors`, `receiptsDropped`, `acceptingPayments`; `reporter.stuckSince`, `reporter.lastError`, `reporter.writeErrors` | open above `node.intents_open_max` or open without a break past `node.intents_open_for_max_s` (validBefore + 10 min + resolver delay); new unknown outcomes; any write error or dropped receipt; skew above `node.clock_skew_max_s` (refused at 300 s); reporter stuck past `node.reporter_stuck_max_s` | `node.intents.*`, `node.*write_errors`, `node.receipts_dropped`, `node.payments`, `node.clock_skew`, `node.reporter.*` |
| Contract balances | `eth_getBalance(deployer)`, `eth_getBalance(operator)`; OnyxRevenue `token()`, `allocated()`, `paidOut()`, USDC `balanceOf(revenue)` (addresses: [deployment.md](deployment.md#deployed-addresses-base-sepolia); on 2026-10-15 6,250 = 7,500 - 1,250, read by the watch) | deployer below `balances.deployer_wei_min` (0.001 ETH, about 4 runs); the API operator below `balances.operator_wei_min` (0.00002 ETH, about 13 sends; only while payments are on); revenue holding less than `allocated - paidOut`; wrong token | `balances.deployer_eth`, `balances.operator_eth`, `balances.revenue` |
| Website | GET `<site>/` and `<site>/data/demo.json` | not 200; not `public: true`; any `runs[].input` | `site.up`, `site.public` |
| Metrics | none: ADR-0008's Prometheus endpoint is not built | | TODO (later): `/metrics` per monitoring.md |

Read by hand, not watched: settlement `rewardPool()` and `coordinatorBond()` (no amount there is
owed to anyone the way OnyxRevenue's `allocated - paidOut` is, so there is no invariant to alert
on; the coordinator reads them before opening a round), and `receiptsPending` (it rises and drains
with normal traffic; a receipt log that cannot drain shows as `receiptsDropped`, a write error or
`acceptingPayments` false, which are watched).

## 4. Incident response

Severity levels and the indexer playbook are in [incident-response.md](incident-response.md).
Step-by-step runbooks (L15, rehearsed on a fork): [stop serving a shard](runbooks/stop-serving-shard.md),
[rotate the reporter](runbooks/rotate-reporter.md), [redeploy and announce a new ledger id](runbooks/redeploy-new-ledger.md).
**No contract has a pause, proxy, sweep or `selfdestruct`** ([admin powers](../audit/2026-10-13-contracts.md#admin-powers-and-upgrade-paths)):
the levers are off-chain (stop the coordinator, stop serving, stop payments) plus the on-chain
dispute game and redeploys.

| Incident | Who | Steps |
|---|---|---|
| Leaked key | The project owner decides, loop executes | 1. Stop every process using it. 2. `operator`: owner `setReporter(new)`. Receipt signer: coordinator `setReceiptSigner(new)`. 3. Deployer: its roles are immutable, so **the attacker keeps them on the old deployment** and nothing we do there stops them: as revenue owner it can `setReporter` to itself and then, as reporter, attribute unallocated USDC (`credit`) and point label shares at itself (`setLabelPayee`); as coordinator it can open rounds and `arbitrate` rerun cases. Our stopping new rounds does not stop theirs. The response is: redeploy everything with new keys, move every user and client to the new manifest, announce the old ledger id as compromised (site, API notice), stop our nodes serving it, set `ONYX_PAYMENTS=off` for it. `registry-owner`: the same, for the registry (it can register metrics and label-set levels). 4. CDP or RPC key: revoke at the provider. 5. If it reached git: rotate first; history rewrite only with the project owner's approval. Post-mortem. |
| Bad root posted | loop (anyone may challenge) | Before finalization: `OnyxDisputes.openRerun(round, shard)` with the recompute evidence; the coordinator may `arbitrate` a case in `RerunRequired`; an unfinalized shard past `pendingClose` is `voidShard`ed. After finalization a root cannot be changed on chain: drop the shard from `ONYX_CATALOG` and restart the node (it stops serving), publish a notice naming the round, tell clients to re-check on their own node. Determinism bug: freeze the recipe (incident-response.md). |
| Data-rights takedown (a source owner) | The project owner decides, loop executes | Same day: remove the source's rows (label-removal.md "data owner"), stop serving affected shards (catalog), rebuild the public site, add the source to the excluded list; record in data-sources.md. |
| Label removal request | The project owner (or named owner) decides, loop executes | [label-removal.md](label-removal.md#steps): log, acknowledge in 2 working days, find every copy, decide, publish a new label set version, rebuild, reply, record. History is not rewritten without the project owner. |
| RPC outage | loop | The node retries start-up reads (about 30 s, SOAK-2) and keeps intents on disk; payments answer 503 while the chain cannot be read. Switch `ONYX_RPC_URL` to another node and restart; check `/v1/health` `indexer` and `settles` recover. A lying node: incident-response.md "Chain indexer". |
| Contract bug | The project owner decides, loop executes | 1. Stop opening rounds (coordinator). 2. `ONYX_PAYMENTS=off` on every node we run: our nodes stop taking payments, but anyone can still send USDC to OnyxRevenue directly (it is an ordinary token transfer), so announce that the contract is retired; funds already in stay withdrawable by pull (`withdraw`, `claim`, `withdrawStake` after `unbondDelay`). 3. Unfinalized shards: let windows run out (`voidShard`, `reclaimBudget`, `refundVoidedShard`). 4. Fix with a test, redeploy, publish the new manifest and a note naming the old ledger id. |

## 5. Rollback

| What | How | Limits |
|---|---|---|
| Website | Rebuild the previous good commit (`git checkout <sha> && ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh` while LC-1 goes by path B, `ONYX_RIGHTS=cleared` after path A) and republish it at the same link. A commit before ROADMAP L14 has no fallback mode and builds the totals whatever the variable says: under path B, roll back only to a commit that has it | Never republish a preview build to a public link (LC-6), nor a `cleared` build while the rights are not cleared (LC-1) |
| Node binaries | [deployment.md "Upgrade and rollback"](deployment.md#upgrade-and-rollback): previous Docker tag or binary with the same state volume; `--check-config` before, `/v1/health` after. On the hosted server `scripts/deploy-api.sh` keeps the running release in `/var/backups/onyx-prev/` (binary, unit, `/etc/onyx` without the key; section 9) | The journal refuses to start on a format it cannot read rather than lose entries; one process per state dir |
| Contracts | No proxies: a fix is a redeploy (`scripts/demo-testnet.sh testnet` or the forge scripts in [deploy-base-sepolia.md](deploy-base-sepolia.md)), which starts a new ledger id; old roots stay readable at the old address; clients pin the new manifest. Steps: [redeploy-new-ledger.md](runbooks/redeploy-new-ledger.md) | Immutable keys (section 2); the Base Sepolia redeploy itself was not rehearsed (only on a fork) |
| Data and label sets | Label sets and recipes are append-only: publish a new label set version (label-removal.md step 5), a new recipe id for a recipe fix; the registry cannot unregister, so stop serving via the catalog; CAS rows are re-creatable from the miners' batches | Old versions stay in restricted storage until dependent challenge windows close (R-REMOVE-1) |

## 6. Needs the project owner (ranked)

Every gate only the project owner can pass, once, ranked by what blocks the launch first (walk of
2026-10-18, ROADMAP P7). Times are his hands-on time, not waiting for replies. Everything else
open in section 1 is loop work: LC-7 rest (L18), LC-9 and LC-17 (on the host), LC-11, LC-13 (E9),
LC-16 (L17), LC-21, LC-22 (L5), LC-27's headless-browser read, all listed in ROADMAP P7a. Choosing path B for LC-1 is no
decision of his: ADR-0035 already says a launch without written clearance waits for nothing and
ships the fallback; his part of path B is the publish (row 6).

| # | Gate | What the project owner does | Time | Unblocks |
|---|---|---|---|---|
| 1 | LC-1 path A (L8-5) | Write to OKX and Bitwise for permission to redistribute totals from their label sets (not a launch blocker under path B, but first because replies take weeks) | 30 min for two e-mails | Path A later turns the label totals and the OKX reserve sale back on |
| 2 | LC-8 | Pick where the API runs (recommended: this dev box behind Caddy, free Let's Encrypt certificate) and a domain (buying one costs money: his call); point its DNS at the box; give the loop a login there | 30-60 min | LC-7 (real certificate, firewall cap), LC-9, LC-17 on a public node, LC-23 re-run on the host, C24 live node URL |
| 3 | LC-2 (L8-4) | Create a role mailbox (on the new domain or any provider; never a personal address) and confirm the response times in label-removal.md | 15 min | LC-2: the loop fills label-removal.md, the README and `ROLE_CONTACTS` in about 15 min |
| 4 | LC-18 | Name who is on call and pick the channel (recommended: e-mail or Telegram, both free); put any bot token or SMTP password in a 0600 file on the host, never in chat or git | 15 min | LC-18; with LC-8, LC-17 (the loop wires `--notify-cmd`, about 30 min) |
| 5 | LC-27 (and LC-1 path A), only if the loop's headless-browser read (P7a) is also refused | Read Binance's site terms in a browser (a plain fetch gets an empty page) and note what they say about redistributing PoR data, with the date | 15-20 min | LC-27 (the release step then exits 0); one of the four label sources for path A |
| 6 | LC-6 (and LC-1 path B) | Publish the `ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh` build at the public link in place of the private preview (after 2-5 and the loop's P7a reruns) | 15 min | LC-6 and LC-1 (path B): the public launch itself |
| 7 | LC-24 | Restore GitHub Actions billing (not a blocker) | 5 min | CI runs again; outside contributors |
| 8 | LC-25, LC-26 (only before the repository goes public) | Run the counts-only `.env` cross-check (THREATS SCAN-3), then decide: keep history private, or approve one rewrite that removes the excluded data (L8-H1..H3) and the personal author e-mail (OSS-18) | 10 min + 5 min to decide (a rewrite is loop work after his yes) | LC-25, LC-26, OSS-3, OSS-18; a public repository |

Decided by the loop on 2026-10-18 (the project owner may change it): LC-3, the removal proof rule for
company-signed addresses (label-removal.md; TRACKER decision log).

## 7. Mainnet gates (Phase H)

Not part of this launch. Mainnet needs everything above plus
[upgrades.md "Mainnet gate"](upgrades.md#mainnet-gate-all-must-be-true-the-project-owner-and-the-second-signer-sign-off)
and every item of [ROADMAP Phase H](../../ROADMAP.md#phase-h-hardening-before-mainnet-after-the-project-owner-decides-to-leave-testnets-l9-sweep):

- [ ] H1 Owner and role keys behind multisig/timelock/KMS; a lost-key plan (K-1..K-3, REG-1, L2C-3, L2C-5).
- [ ] H2 Unbiasable randomness and private assignments.
- [ ] H3 Dispute and round economics (L9S-4..6, M6-D5, M6-D7).
- [ ] H4 Payment attribution and pins, incl. a signed manifest of every code hash (K-5, L2C-2, REV-12).
- [ ] H5 Outside audit of contracts and x402; solc bump first (DEP-1, DEP-3: 0.8.38 or later).
- [ ] H6 Level-3 unwind before any real signed source.
- [ ] H7 Open participation: availability challenges, unlinkable signed canaries.
- [ ] Level 2 sells no money-market data until C4 closes (threat sweep "What blocks a launch").

Key ceremony details: [contract audit K-1..K-8](../audit/2026-10-13-contracts.md#what-a-mainnet-key-ceremony-needs-for-docsopslaunch-checklistmd).

## 8. Runbook: the four loop gates back to back (LC-16, LC-21, LC-13, LC-11)

Draft 2026-10-27 (north-star loop NL12, lane B; nothing here was run by lane B). Run on the launch
commit, in this order: the two cheap static gates first, the soak next (local only, no test ETH),
the live run last (it spends test ETH and changes addresses). Every heavy step runs under
`systemd-run --user --scope -p MemoryMax=16G` with `CARGO_INCREMENTAL=0`, with a disk guard that
stops the scope below 153 GB free on `/` (206 GB free on 2026-10-27, the worktree's `target/`
about 80 GB). Keep logs in `~/.cache/onyx-gates/` (outside the repo). Times are from earlier runs.

| Gate | Time | Disk | Test ETH |
|---|---|---|---|
| LC-16 contract gates | about 2 h with the review (Slither run: minutes) | small (a Python venv) | none |
| LC-21 fuzz smoke | about 10 min of fuzzing (19 targets x 30 s) plus a nightly build | `fuzz/target`, not measured yet: watch it | none |
| LC-13 soak | 150 min run plus a release build (run 3) | the soak dir plus release binaries | none (local anvil fork) |
| LC-11 fresh live run | about 45 min (2,707 s on 2026-10-27; the docs say 60-80 min) | the export dir (under 1 GB) | 0.000211 ETH for the 2026-10-27 run (145 txs) |

### LC-16: contract gates and the delta review (ROADMAP L17)

Contracts changed since the L2 audit (`git diff --stat 15635c4 HEAD -- contracts/src` on
2026-10-27): `OnyxRegistry`, `OnyxSignedBook`, and the new markets and prepaid contracts
(`OnyxFutures`, `OnyxObservedFacts`, `OnyxPanelOracle`, `OnyxPayout`, `OnyxPrepaid`,
`OnyxReobserve`, `OnyxStakes`, `OnyxTestToken`, `OnyxTestnetOwnerOracle`, `IOnyxMarkets`).
Settlement, Ext, Disputes and Revenue are unchanged. The gate's text names only the first two;
review all of them.
```sh
# 1. The gates check-all already runs: forge tests, the Settlement/Ext storage layout, gas snapshot.
cd contracts && forge test && FOUNDRY_PROFILE=gas forge snapshot --check \
  --no-match-test 'testFuzz_|invariant_' --no-match-contract Fork && cd ..
# 2. Slither (free, from PyPI; the L2 audit used 0.11.6), in a venv outside the repo.
python3 -m venv ~/.cache/onyx-gates/slither && ~/.cache/onyx-gates/slither/bin/pip install slither-analyzer==0.11.6
(cd contracts && ~/.cache/onyx-gates/slither/bin/slither . --exclude-dependencies \
  --json ~/.cache/onyx-gates/slither.json) ; echo "slither exit $?"
# 3. Triage every result in the changed contracts against docs/audit/2026-10-13-contracts.md's
#    triage table; a role/access review of each changed contract (owner, coordinator, minter,
#    arbiter: THREATS NL6-12, NL6-13). Write docs/audit/<date>-contracts-delta.md.
```
Slither exits non-zero when it reports findings; the gate is the triage, not the exit code.

### LC-21: fuzz smoke (FUZZ-5)

Needs the nightly toolchain and `cargo-fuzz` (both on the dev box on 2026-10-27).
```sh
systemd-run --user --scope -q -p MemoryMax=16G --unit=onyx-gate-fuzz \
  env CARGO_INCREMENTAL=0 FUZZ_SECS=30 scripts/fuzz-smoke.sh 2>&1 | tee ~/.cache/onyx-gates/fuzz.log
du -sh fuzz/target    # record the disk for this table
```
Pass: exit 0 and no file under `fuzz/artifacts/`. A crash leaves its reproducer there; make it a
stable unit test (as the earlier crashes were) before the gate closes.

### LC-13: soak the merged tree (L6, SOAK-7, ROADMAP E9)

Local only: an anvil fork of Base Sepolia, nothing broadcast. The run-3 shape (docs/audit/2026-10-14-soak.md):
```sh
systemd-run --user --scope -q -p MemoryMax=12G --unit=onyx-gate-soak-build cargo build --release --locked \
  -p onyx-node -p onyx-reserve-serve -p onyx-source-btc --bin onyx-node --bin onyx-node-btc \
  --example demo-local --example soak-load
ONYX_SOAK_DIR=~/.cache/onyx-gates/soak ONYX_SOAK_PREBUILT=1 \
  systemd-run --user --scope -q -p MemoryMax=16G --unit=onyx-gate-soak \
  scripts/soak.sh --minutes 150 --cycle-minutes 30 --leak-window-minutes 60 --rounds 18 --round-every 480
```
Pass: exit 0 (`summary.json` `"pass": true`). Run 4 of 2026-10-14 was lost to a session restart in
its first minutes: start it where a restart does not end it (the background, with the guard).

### LC-11: a fresh Base Sepolia run on the launch commit

What changed since the last live run: `git diff --stat ce0bb04 HEAD -- contracts/src contracts/script`
on 2026-10-27 lists only `OnyxObservedFacts.sol` and `DeployMarkets.s.sol`, which went live on
their own at `67a8920`. Run the check on the launch commit first: if it still lists only the
markets contracts, the 2026-10-27 run (`run-20261027T024839Z`, REHEARSAL PASSED, 145 txs) plus the
markets deploy cover the contracts, and only a markets redeploy is due if NL14 changes them. If
anything else changed, do the full run:
```sh
scripts/demo-testnet.sh testnet --plan                 # budget and balances; sends nothing
scripts/demo-testnet.sh testnet --dispute --yes --export-node ~/.cache/onyx-gates/node \
  2>&1 | tee ~/.cache/onyx-gates/testnet.log
scripts/markets-sepolia.sh deploy ~/.cache/onyx-gates/node   # 8 txs, 0.000351 ETH on 2026-10-27
```
Before you start:
1. **Balance.** The deployer held about 0.00126 test ETH on 2026-10-27 (read by the health watch).
   A full run (0.000211) plus a markets deploy (0.000351) leaves about 0.0007, below the watch's
   `balances.deployer_wei_min` (0.001 ETH): the owner tops it up from a faucet first, or the watch
   alerts after the run. It also needs 0.01 test USDC (the customer's three payments); `--plan`
   checks both.
2. **Addresses change.** A full run deploys new settlement, disputes, revenue and registry
   contracts; markets must be redeployed next to the new registry, and market 1 (on the old
   registry) stays where it is. Export to a new directory and keep the hosted node on the current
   deployment until the one final deploy; then update docs/ops/deployment.md,
   `deploy/health-watch/thresholds.json` (a test compares them) and the site's
   `static/markets/base-sepolia.json` together.
3. **Order with NL15.** Settle market 1 on the current deployment (after 2026-10-29 00:00 UTC)
   before any switch, so its result is not stranded.

## 9. Release dry run (north-star loop NL20, from the 2026-10-28 release)

The steps of the release that went live on 2026-10-28 (`2f01280`: API, then site), in order,
with what each one needs. Times are from that night where they were measured; the rest say
"not timed" and are to be timed on the next release. Every deploy step needs the owner's OK in
chat for that release (a production deploy).

1. **Candidate.** `ONYX_WEB=1 scripts/check-all.sh` green on the commit (about 1 to 1.5 hours
   on the dev box with other jobs running). Only status docs may change after it.
2. **Release binary.** `CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=~/.cache/onyx-ns17/target
   RUSTFLAGS="--remap-path-prefix=$HOME=/build" cargo build --release -p onyx-node` under
   `systemd-run --user --scope -p MemoryMax=16G`; then `grep -caF "$HOME"` on the binary must
   print 0 (`deploy-api.sh` refuses it otherwise). About 1.5 minutes from a warm target.
3. **Freeze.** `git worktree add --detach <path>/release <commit>`, so later work cannot reach the
   build. Copy `apps/demo/site/data/` from a worktree that has it: it is generated and ignored by
   git, and `build-web.sh` stops with "no demo data" without it (this happened on 2026-10-28).
4. **Public build** in the frozen worktree: `ONYX_PUBLIC=1 ONYX_RIGHTS=fallback
   CARGO_TARGET_DIR=~/.cache/onyx-release-target scripts/build-web.sh` must end with
   `E2E PASSED` and `website built (public, rights fallback ...)`. Not timed (tens of minutes).
5. **Export.** The export folder (`~/.cache/onyx-ns17/node` today) holds `deployment.json`,
   `catalog.json`, `cas/`, `node.service.env` and `observations/` with `pins`: every store there
   is installed and served (a withdrawn series must leave it too, THREATS NL20-3).
6. **API.** `scripts/deploy-api.sh <export> <binary>` from the frozen worktree. Expected lines:
   `Valid configuration`, `servable shards: N`, `connection cap: loaded`, the `mcp:` line, then
   `API DEPLOYED: ... (payments off, intake on)`. It keeps the previous release in
   `/var/backups/onyx-prev/` first (from 2026-10-28, NL20-4). On 2026-10-28 `mcp:` said
   "skipped, no Node 20 or later on the host": the connector needs `nodejs` on the server first.
7. **API live checks.** `/v1/health` 200; every series in `observations/pins` answers 200 at
   level 2 for its newest round; `POST /v1/submit/check` with `{}` answers a JSON 400 (not 404);
   `ssh onyx-web sudo nft list table inet onyx_conn_cap`; Caddy's admin port closed
   (`curl http://127.0.0.1:2019/config/` on the server fails); `POST /mcp` with `initialize`
   answers 200 once Node is there.
8. **Site.** `scripts/deploy-site.sh --skip-build` from the frozen worktree (the build of step
   4); it prints `SITE DEPLOYED: https://<domain>/ (<UTC>-<commit>)`. 2026-10-28:
   `20261028T035536Z-2f01280`.
9. **Site live checks.** The live daily check (`apps/web/tests/daily.mjs` against the domain, with
   the frozen worktree's build): `LIVE DAILY PASSED`; the Markets page at 1280 and 390 px reads
   the markets and the insurance products from Base Sepolia with no page error and no sideways
   scroll.
10. **Record.** The release, its checks and anything skipped in the tracker, STATUS.md and
    site-hosting.md.

Rollback, fastest first:

- **Site:** `ssh onyx-web 'ln -sfn /srv/onyx/releases/<previous> /srv/onyx/current.new && mv -T
  /srv/onyx/current.new /srv/onyx/current'` (the script keeps the last 5 releases).
- **API:** on the server, `sudo install -m 0755 /var/backups/onyx-prev/onyx-node
  /usr/local/bin/onyx-node`, `sudo rsync -a /var/backups/onyx-prev/etc-onyx/ /etc/onyx/` (the
  operator key is not in the copy and stays as it is), `sudo install -m 0644
  /var/backups/onyx-prev/onyx-node.service /etc/systemd/system/`, `sudo systemctl daemon-reload
  && sudo systemctl restart onyx-node`, then step 7. Caddy: `deploy-api.sh` keeps
  `/etc/caddy/Caddyfile.bak-<UTC>`; put it back and `sudo systemctl restart caddy`. A release
  deployed before 2026-10-28's NL20 change has no `/var/backups/onyx-prev`: roll back by building
  and deploying the previous commit (steps 2 and 6).
- **Contracts and chain data** cannot roll back (section 5).
