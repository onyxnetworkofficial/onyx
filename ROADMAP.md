# ROADMAP

The loop's work queue, in phases. The loop reads this file every tick, takes the top
open item of the current phase, and updates it when something lands. History and
decisions stay in PLAN.md (milestone checklists), docs/TRACKER.md (decision log),
THREATS.md (findings) and STATUS.md (top 15 lines = now).

Rules for every item: worktree + own `CARGO_TARGET_DIR`, heavy builds under
`systemd-run --user --scope -q -p MemoryMax=12G`, `scripts/check-all.sh` green,
fresh adversarial review before merge (paid data always), merge, push, website
update + republish when user-visible. Same problem fails 3 reviews: the lead picks
a narrower design or parks it and logs why in docs/TRACKER.md. Testnets only.

## Current product priority (2026-10-21)

Follow [PRODUCT-PLAN.md](PRODUCT-PLAN.md), U1–U5, for the contributor-first app requested by the project owner.
The active product loop starts at its first unchecked milestone. It does not bypass Phase S
proof/rights gates or the deployment checklist.

## Done (2026-10-12/13)

- [x] Full run live on Base Sepolia (REHEARSAL PASSED, 149 txs, every tx linked on the site)
- [x] Public-RPC hardening: nonce tracking, retries, reads and estimates pinned to own blocks
- [x] Level-2 transcript log + draw lifecycle invariant (property test, 20,000 seeds)
- [x] Chain indexer (ADR-0033): shards found from `Finalized` events, rescan recovery
- [x] Reserve balances (ADR-0034): `btc-entity-reserve`, OKX and BITB exact vs sources
- [x] Reserve API `btc.entity-reserve` behind a non-circular serving gate
- [x] Delivery hardening (M7 loop 3): client checks roots on its own node by default,
      settle check after timeouts, a paying client gets data within the retry window
- [x] CDP faucet script (`scripts/testnet-faucet.py`)

## Phase A: prove it live again (now)

- [x] A1 Fresh Base Sepolia run on current main: REHEARSAL PASSED 2026-10-13 on the first try
      (156 txs, 0.000213 ETH, run-20261013T142726Z; the indexer found round 1 from chain
      events in 4 eth_getLogs calls; finality 873 s)
- [x] A1b Run page: add the 2026-10-13 run (scripts/build-testnet-run.py on its run log and
      console log in .onyx-testnet/runs/), keep the 2026-10-12 run, republish
      (2026-10-13: both runs on the Run page, latest first with a date picker; the parser reads
      step 6a and the floor registrations; home headline names the latest run; both builds
      pass, publishing the rebuilt site is the lead's step)
- [x] A2 Reserve metric live on Base Sepolia: REHEARSAL PASSED 2026-10-14 (198 txs, 0.000262 ETH,
      run-20261014T160422Z): reserve round 3 finalized, indexed at level 4, gate cleared, an OKX
      reserve row sold (2500 micro-USDC booked to the reserve shard), Binance 403, forged base refused;
      per-shard budget contracts (ADR-0022 Am. 1) deployed live for the first time
- [x] A2b Run page: add the 2026-10-14 run (scripts/build-testnet-run.py with its run log and
      console log), republish (2026-10-14, website W7: three runs, latest first, 215 tx links,
      reserve sale and Binance refusal steps; publishing the rebuilt site is the lead's step)
- [x] A3 `onyx-node-btc` Docker image built and smoke-tested in CI-equivalent local run
      (2026-10-13: `scripts/docker-smoke.sh`, opt-in `ONYX_DOCKER=1 scripts/check-all.sh`; both
      images from a clean checkout, run on an anvil fork in a container; docs/audit/2026-10-13-docker.md)

## Phase L: launch readiness (before any public launch; after A, before B)

Free, open-source tools only (installing them is fine; nothing paid). Each item ends
with a committed report under docs/audit/ (date, tool versions, exact commands,
results, what was fixed, what stays open with a THREATS row).

- [x] L1 Dependency audit: `cargo audit`, `cargo deny` (advisories, licenses, bans,
      sources), `npm audit` for apps/web; fix or pin, record exceptions
- [x] L2 Contract audit: Slither static analysis, Foundry invariant tests for
      OnyxSettlement/Ext/Disputes/Revenue/Registry/Ledger (money conservation, no
      stuck funds, roles), fuzz runs at higher counts, gas and size report
      (merged 15635c4: docs/audit/2026-10-13-contracts.md; reviews L2-R1/R2 fixed, L2-R3 0 blocking)
- [x] L3 Rust fuzzing: cargo-fuzz targets for every parser on a trust boundary (x402
      header, JSON-RPC answers, Borsh decoding, Merkle proofs, catalog/book/state files)
      (docs/audit/2026-10-13-fuzzing.md: 18 targets, 706M execs, FUZZ-1 fixed; open FUZZ-2..6)
- [x] L4 Mutation testing (cargo-mutants) on core crates; add tests for surviving mutants
      (docs/audit/2026-10-13-mutation.md; open: THREATS MUT-1..5, coordinator validate.rs/round.rs
      unit tests, dispute.rs and self_settle.rs not yet run)
- [ ] L5 Benchmarks: rerun every benchmark in docs/ops/benchmarks.md; add API load test
      (2026-10-28, NL20: the API load test is added, `scripts/load-test.mjs`, run against the
      release binary and the deployed export on the dev box; left: rerun the other benchmarks)
      (req/s, p50/p99, paid and unpaid paths), indexer catch-up speed, rerun cost per
      shard, website bundle size and first paint; record numbers, never invent them
- [x] L6 Soak and chaos: onyx-node for hours against anvil with injected RPC failures,
      restarts and slow nodes; memory and file-descriptor growth; no stuck intents
      (docs/audit/2026-10-14-soak.md, `scripts/soak.sh`: SOAK-1..4 fixed; open SOAK-5..7)
- [x] L7 Fresh-machine install: build and run the Docker images (`onyx-node`,
      `onyx-node-btc`) in a clean container following only docs/ops; fix every doc gap
      (Docker half 2026-10-13 with A3, 9 doc gaps fixed; systemd unit on a fresh Ubuntu 26.04
      and BuildKit builds 2026-10-15, docs/audit/2026-10-15-fresh-install.md: 8 doc gaps,
      L7-SD-1 credentials refused (fixed), L7-BK-1 syntax frontend pinned; security 1.2 OK;
      not on a real VM, no payment under the unit; L2R2-F2 export TODO moved to L16)
- [x] L8 Data and privacy audit: every shipped dataset's license and source link;
      public build scans; no individual named anywhere; label-removal process documented
      (docs/audit/2026-10-13-data-privacy.md; open for the project owner: THREATS L8-H1..H3, L8-4, L8-5)
- [x] L9 Threat sweep: every open Medium/High row in THREATS.md fixed, accepted with a
      reopen condition, or deferred to a named open item; whole-system adversarial review of main
      (docs/audit/2026-10-14-threat-sweep.md: 90 rows triaged, R-LBL-FIX-1 and T-KEY-1 fixed,
      19 new rows L9S/L9A with 0 Critical/High; mainnet items in Phase H; check-threats.py)
- [x] L10 Launch checklist (docs/ops/launch-checklist.md): keys and owners, monitoring,
      incident response, rollback, what the project owner must decide
      (2026-10-15: 27 gates, 8 verified, 19 open, 12 of them launch blockers; `scripts/check-launch.sh`
      in check-docs checks every evidence link and gate; L9A-8 fixed; new DEP-3; L12-L15 added)
- [x] L11 TLS in front of onyx-node: ship a reverse-proxy config (TLS, request limits) and a
      deployment.md rule that a public `ONYX_LISTEN` is never plain HTTP (THREATS M7-7)
      (2026-10-15: `deploy/caddy/Caddyfile`, onyx-node refuses a non-loopback listen without
      `ONYX_ALLOW_PLAIN_HTTP_PUBLIC=1`, local Caddy run in docs/audit/2026-10-15-tls-front.md;
      the real-domain certificate waits for LC-8; TLS-1..8)
- [x] L12 Health watch and alerts (launch checklist LC-17): a script that polls `/v1/health`
      (indexer lag, conflicts, open and unknown intents, write errors, clock skew, reporter stuck),
      the RPC (chain id, head age), balances (deployer ETH, OnyxRevenue USDC >= allocated - paidOut)
      and the public site (`demo.json` marked public), with the thresholds in the checklist's
      section 3 and the alert channel the project owner picks (LC-18)
      (2026-10-15: `scripts/health-watch.py`, thresholds in deploy/health-watch/thresholds.json,
      systemd user timer, docs/ops/runbooks/health-watch.md, tests in check-all, fault run
      docs/audit/2026-10-15-health-watch.md; open: no public node to run it on (LC-8/LC-9), the
      channel is a `--notify-cmd` hook until LC-18)
- [x] L13 History secret scan before the repository or any mirror goes public (LC-25, T-KEY-1):
      a free scanner (e.g. gitleaks) over all history, redacted output, findings as THREATS rows
      (2026-10-15, rescanned 2026-10-16 after merging main: gitleaks 8.30.1 + trufflehog 3.97.9 over
      all refs (442 commits), 18 unreachable commits and the tree: no real secret; 9 + 3 findings are
      public hashes, test values or placeholders; deployer key never committed (derivation check);
      `.gitleaks.toml` allowlists the reviewed items by path and value; `check-secrets.sh --history`
      runs the patterns and gitleaks in one command; SCAN-1 fixed, SCAN-2 accepted, SCAN-3 needs the
      lead's counts-only `.env` cross-check (LC-25 stays PARTIAL); docs/audit/2026-10-16-history-secret-scan.md)
- [x] L14 Rights fallback, only if LC-1 (L8-5) is not cleared by launch: a public build without
      the label-derived entity totals and an onyx-node catalog without the OKX reserve sale
      (2026-10-16, ADR-0035: `ONYX_RIGHTS=cleared|fallback`, default `cleared`; fallback public
      build guarded by `a_fallback_build_carries_no_label_derived_totals`; onyx-node 403
      `label_rights_withheld`; inventory and screenshots docs/audit/2026-10-16-rights-fallback.md;
      verified 2026-10-16: both builds' guards and e2e pass, a real onyx-node on a local chain
      answers 200 cleared and 403 fallback on metrics, proofs, export and settlement; LC-1 now
      names path A (rights cleared) and path B (fallback shipped), still open; review L14-R1
      2026-10-16: no default for a public build or a testnet node, dead-branch text dropped from
      the bundle, guard rule 4 refuses figures next to company names, 403 bypass attempts tested)
- [x] L15 Launch runbooks (LC-19): stop serving a shard, rotate the reporter, redeploy and
      announce a new ledger id (with G5), each rehearsed once on a fork; a release step that
      re-reads every served source's terms in data-sources.md (LC-27, L2-SRC-4)
      (2026-10-16: docs/ops/runbooks/{stop-serving-shard,rotate-reporter,redeploy-new-ledger,
      release-source-terms}.md; each of a-c rehearsed with its rollback on a local anvil fork
      and onyx-node (docs/audit/2026-10-16-runbook-rehearsal.md); onyx-node refuses an operator
      key that is not `reporter()` and `--check-config` prints shard ids and the key's address;
      `scripts/check-source-terms.py` (90 days) exits 1 today on Binance's unread site terms, so
      LC-27 stays PARTIAL; not run: the Base Sepolia redeploy and the announcement edits)
- [x] L16 Registry pin in the node export (THREATS L2R2-F2, moved from L7): `--export-node`
      writes `registry`/`registryCodeHash`; docker-smoke asserts the shard keeps its attested level
      (2026-10-16: export checks the build and the node's start check; node also requires
      `ledger()` = settlement and keeps the attested level of a labelled shard; L16-1..L16-5;
      `scripts/docker-smoke.sh` on 62652ca: DOCKER SMOKE PASSED, both shards at
      attested level 4 in both images, wrong hash and swapped registry refused; review L16-R1..R7:
      level recomputed from its inputs in any order, attestation bound to the binding's recipe and
      label set, money effect recorded (settles only for a label set registered at 3), health
      counts fallback-withheld shards apart, docker-smoke mounts the reporter key after L15)
- [ ] L17 Contract delta audit (launch checklist LC-16, from P7): Slither over `OnyxRegistry.sol`
      (+407 lines since the L2 merge `15635c4`: C4, C11, C51) and `OnyxSignedBook.sol` (new, 811
      lines: C2, C7, C10), triage every finding into THREATS, and a written role/access table of
      every new external function (who may call it, what it moves); the report under docs/audit/;
      about 2 hours; Slither is not installed on the dev box (free, pip)
- [ ] L19 Hosted API front leftovers (from NS17, review NS17-R1): a per-address connection cap
      on port 443 in the web server's firewall (nftables `ct count`, deployment.md "TLS in front";
      Caddy is a trusted proxy, so the node's own per-address cap does not see its clients), and
      the API operator's balance in the health watch before payments are turned on there
      (2026-10-27, NL5: DONE but the install: `balances.operator_eth` in the health watch, skipped
      while payments are off; the watch's block limits fixed for 0.2 s blocks (NL5-2);
      `deploy/nftables/onyx-conn-cap.nft` and `deploy/systemd/onyx-conn-cap.service`, tested with
      `scripts/conn-cap-test.sh` in a throwaway network namespace. Left: install the cap on the
      web server, with the owner's OK; then tick. 2026-10-28: the cap went live with release
      `2f01280`. NL20 added: a per-client request limit on the read routes (THREATS NL20-1) and
      a rehearsal of the API rollback from `/var/backups/onyx-prev` at the next release (NL20-4))
- [ ] L18 TLS front leftovers that need no domain (LC-7, from P7): with Caddy on `tls internal`
      and a local anvil, the paid x402 flow through the proxy (402 with `payment-required`, a settled
      200 with `payment-response`, a 503 `payment_pending` retry), a server-side TLS 1.1 refusal
      shown with a client that can offer 1.1, and the Docker trusted-proxy setup of TLS-7 run once
      (two clients, separate limits); added to docs/audit/2026-10-15-tls-front.md; Caddy is not
      installed today (free, apt). 2026-10-27 (NL12): the TLS 1.1 refusal is shown on the real
      server instead (onyx.cryptagraph.com answers TLS 1.0 and 1.1 with alert 70 to a client that
      offers them; launch checklist LC-7); the paid flow and the Docker setup stay open

## Phase H: hardening before mainnet (after the project owner decides to leave testnets; L9 sweep)

Every open THREATS row that blocks mainnet but not a testnet launch points at one of these
(docs/ops/upgrades.md "Mainnet gate"; docs/audit/2026-10-14-threat-sweep.md).

- [ ] H1 Owner and role keys: registry owner and revenue owner behind a multisig or timelock,
      coordinator in a KMS, written plan for a lost immutable key (REG-1, L2C-5, contracts audit K-1..K-3)
- [ ] H2 Unbiasable randomness: a beacon the Base sequencer cannot bias (drand, VRF or commit-reveal)
      for the sampling seed and panel draws, and private per-validator assignments (R-M4-6, R-M4-7,
      M6-G1, M6-D6)
- [ ] H3 Dispute and round economics: on-chain proof of the rerun (q7 b), panel canaries, appeal
      panels that cannot copy earlier reveals, a wrong-verdict slash that covers the rerun panel's
      cost, `panelSize >= 3`, a stake check on verdicts, a coordinator bond per open round (L29-R1,
      L29-F7, L24-F6, R-DRAW-3, M6-D5, M6-D7, L9S-4, L9S-5, L9S-6)
- [ ] H4 Payment attribution and pins: the client commits the shard in the x402 payment, a signed
      deployment manifest and an on-chain `reporter()` check in the client pin, OnyxDisputes code
      pinned, canaries never registered as miners, a case-closed shard's validator cut not passed to
      the miners (REV-1, REV-12, L2C-2, L9S-1, L9S-3)
- [ ] H5 Outside security audit of the contracts and the x402 code, solc bump first (M7-6, DEP-1,
      contracts audit K-8; Needs the project owner: paid)
- [ ] H6 Level-3 settlement unwind after a key-compromise re-review, before any real signed source
      is served (L3-F3, with C2)
- [ ] H7 Open participation: availability challenges (serve row i with its proof or lose the bond)
      and canaries that cannot be told apart, before any miner or validator the operator does not
      run joins, canary entries signed by the canary address (R-DA-1, R-DA-3, R-CANARY-1,
      R-CANARY-3, L9S-2)

## Phase B: level 2 on Base Sepolia (ADR-0027 amendment 8 item 8)

- [x] B1 Open each fact's revenue book (L2API-4) (2026-10-16, branch revenue-books: the deploy
      run opens every served fact's book from the reporter key before serving level 2, idempotent
      and fail closed, `onyx_api::observation_book`; rehearsed with `fork --dispute --level2`;
      `--level2` is fork-only until B6)
- [x] B2 Independent observers with their own keys; live coordinator on `SystemClock`
      (2026-10-17, branch observer-keys, ADR-0027 amendment 9: `examples/level2-observer.rs`, a
      process per observer with its own 0600 key, reads the draw from the registry, fetches,
      posts its own commit on chain and sends its signed reveal through a spool folder
      (`observer_link::DirInbox`, B5's inbox); `examples/level2-coordinator.rs` holds no observer
      key and runs on the system clock; rehearsed by `scripts/level2-observers.sh`: 7 processes,
      distinct anvil dev keys, ONE machine. Real independence (own hosts, networks, operators,
      THREATS L2-SRC-1) needs the project owner; EDGAR observers stay rehearsal-only until an operator
      contact exists, SEC-4)
- [x] B3 A separate hot receipt key per deployment
      (2026-10-17, branch observer-keys: `keyfile::ReceiptKey` from a 0600 file, one per
      registry deployment in the deploy run, refused when it is the reporter, coordinator,
      deployer, owner or an observer; the public test key only on a replay or local anvil;
      rotated with `setReceiptSigner`, an earlier draw's round never receipted with the new key;
      THREATS REVLOG-4)
- [x] B4 State REVLOG-1 and REVLOG-5 on the paid API (2026-10-17, branch revlog-api: `limits`
      `[{id, applies, text, docs}]` on every level-2 answer, its 402 body and x402 offer
      `extensions.limits.info.limits`; REVLOG-5 `applies` only to sealed rounds; client refuses a misstated
      list; delivery.md §Level-2 limits)
- [x] B5 Keep taking reveals after a restart while the window is open (REVLOG-R5-1d)
      (2026-10-16, branch revenue-books: `observe::keep_taking_reveals` with a `RevealInbox`; the
      window comes back from the book; observers resend through B2's transport)
- [x] B6 Level-2 round on Base Sepolia, linked on the site (2026-10-27, commit `fba600b`; LIVE
      on the hosted API and the site the same day with the owner's OK; rounds: one USGS
      day (2026-07-19: 5) and one NCEI day (2026-09-27: 28.3 °C) finalized on Base Sepolia against
      registry `0x4973…2b`, 15 and 14 transactions, 0.000024 test ETH in all
      (`scripts/level2-sepolia.sh`, the shared `onyx_level2::live` coordinator, observer and
      client check, in-process signing); both stores served by a local node with the export's
      settings and checked by a client off and on chain; the data page links the rounds; after the
      deploys the client check passed against https://onyx.cryptagraph.com and the live site
      check passed with the five B6 checks)

## Phase C: proof ladder

- [x] C1 Level 5 track record: per-source history of agreement, spec + ADR first (ADR-0036,
      `crates/onyx-track`: computation, commitment, client checks; serving split into C5-C7)
- [x] C2 Signed-source books on chain (L3-F7), chain-anchored clock (L3R-F8) (2026-10-17, branch
      signed-books: `OnyxSignedBook` holds the key registry, never-pruned `(key, sequence)` and
      payload books and the fact book on `block.timestamp`, permissionless `postReport` over the
      Borsh bytes sources sign, lazy withdrawals matching the store on 160 differential traces;
      store clock monotonic (`ClockWentBack`); `signed_chain::SignedBookCast` posts and checks
      `settleable`; ADR-0028 amendment 6; wired in C10, deploy lead-pending)
- [ ] C3 Level 4 bonded claims redesign (parked on branch `level4`; start from a proven
      escalation design, VerificationMethod tag 7)
- [x] C4 Level-2 next: encrypted reveal posts (REVLOG-1), sealed-round binding (REVLOG-5),
      level-2 disputes (2026-10-17, branch sealed-reveals, ADR-0027 amendment 10: observers post
      their signed reveal on chain encrypted to the draw's reveal key (`postSealedReveal`, two
      signatures), the post is the receipt, `revealLogComplete` refuses a round leaving one out,
      garbage is dismissed with a Chaum-Pedersen proof anyone checks; `recordAttempt` pins a
      round's entries and result before it is served (REVLOG-5 reduced, not closed); disputes
      designed only; protocol in `onyx_coordinator::sealed` and `OnyxRegistry`, not wired, not
      deployed: C11-C13)
- [x] C5 Track-record intake: statements only under the source's registered key, `committed_at`
      from a finalized record, close-time rules reviewed per metric (PLAN q-TR-2; THREATS TR-6, TR-7, TR-13)
      (2026-10-17, branch track-intake, ADR-0036 amendment 1: `onyx_track::intake` (EIP-191 by the
      source's level-3 key, batches sealed until close, header hash posted, `committed_at` = the
      post's finalized block time, client `verify_log`), `onyx_coordinator::track_chain::CastRecord`
      (calldata from a dedicated poster to a sink, counted by the poster's nonce; C2's book can
      replace it behind `FinalizedRecord`), `scripts/track-intake-anvil.sh` in check-all; close-time
      review: only the example level-3 metric ruled; THREATS TR-I-1..12; fresh review TR-R3: 2
      blockers fixed before commit; review TR-R4: relaxed R-TR-8 only with an intake pinned, the
      level fence closed against a re-export in onyx-coordinator, the anvil binds its own port)
- [x] C6 Track records served and shown: `/v1/track/{source}/{metric}` with record and history
      proofs, browser `verify_history`, website display per spec (proof-ladder.md "Level 5")
      (2026-10-17, branch track-serving, ADR-0036 amendment 2: `onyx_track::wire` (Borsh rows and
      proofs; one client check for Rust and the browser: record, summary, whole history recounted,
      floor `min_as_of`, `since` chain back to a known commitment), `onyx_api::track_serve`
      (append-only store, `GET /v1/track` and `/v1/track/{source}/{metric}`, 404
      `no_track_record` with a reason, 403 `label_rights_withheld` in fallback, free with a
      per-client budget), node mounts an empty store (no metric has a close-time rule); Data page
      "Track records": plain empty state plus the labelled example (`onyx_track::example`) checked
      in wasm against a pinned commitment; fence extended to onyx-api and the verifier; THREATS
      TR-S-1..13; TR-R2-5 partial until C7)
- [x] C7 Resolutions from on-chain facts only (level-1 roots, `OnyxRegistry` level-2 facts,
      level-3 books from C2) and commitments on chain (PLAN q-TR-3; THREATS TR-5)
      (2026-10-17, branch track-onchain, ADR-0036 amendment 3: `onyx_track::chain` derives a
      commitment's resolutions from one finalized head (`OnyxSignedBook` settleable reports,
      `OnyxRegistry` observations opened by hash-bound preimages; level 1 refused, fail closed),
      every client reruns it (`verify_resolutions`); `OnyxSignedBook.postTrackCommitment` keeps
      an append-only, one-chain log per (poster, track), `trackHead` is the public latest
      (`check_latest`, `wire::Expect.latest`); `declareOperator` before a source's first key and
      `TrackParams.operators`: affiliates never judge or shield; the answers usable first judge
      an entry (self-shield closed); `track_chain::{CastFacts, CommitmentLogCast}` with pinned
      reads by block hash; `scripts/track-onchain-anvil.sh` in check-all; THREATS TR-5, TR-R2-4,
      TR-R2-5, TR-R1-11 closed, TR-S-14 narrowed, TR-C7-*; client `verify_chain` walks the log
      from its first entry; contest window of 3,600 s; fresh review TR-C7-R1: 2 merge blockers
      fixed before commit; not deployed)
- [x] C9 Track-record follow-ups from C5 (2026-10-17, branch `track-followups`, ADR-0036
      amendment 4): a close at the period's start (`Close::PeriodStart`; the fact carries its
      period through its aligned `as_of`), the Bitcoin flow rule (UTC days, a day's blocks by
      median time past, close 7,200 s before the day; the first production rule, no record yet:
      no source states and level 1 has no chain reader, C40); skill against persistence beside the
      counts (PLAN q-TR-5 decided; frozen once judged; own and affiliate answers left out); a
      statement counts only while its key reads active in the level-3 book at its post's block
      (`KeyLives`, `CastFacts::key_lives_at`, on anvil; TR-R4-4 closed); signed intake receipts
      with a posting deadline (`check_receipt`: kept, posted late, broken; TR-I-6 narrowed).
      Not built: the retired outcome (C90), sealed rows encrypted (C93) (THREATS TR-13, TR-I-5,
      TR-I-6, TR-I-11, TR-R1-4, TR-R4-4, TR-C9-*)
- [ ] C90 Track retired outcome (from C9, TR-R1-4): before any registry removal path, retirements
      pinned in the params (`extends` keeps them, a new one only with `from > prev.as_of`),
      registry checks skipped for a retired item, statements from `from` on `Retired` (not
      counted), answers public from `from` on refused; the same exemption in key books and
      `verify_log`
- [ ] C91 Broken intake receipts published (from C9, TR-I-6): a source hands a `Broken` or
      `PostedLate` receipt to the API, which serves it beside the record (checked by
      `check_receipt` in the browser too); the coordinator signs receipts with the poster key
      outside anvil
- [ ] C92 An entity stating on itself (from C9, TR-C9-2): the reviewer declares which subjects a
      source controls (as operators), and an entry whose subject the source controls is not
      counted (or shown apart) on metrics whose truth the subject decides (Bitcoin flows)
- [ ] C93 Sealed track rows encrypted until their close (from C9, TR-I-11; after C51): each
      batch leaf a drand/tlock ciphertext to the quicknet round at the batch's close, so neither
      the coordinator nor anyone else reads a row before its close; `verify_log` opens each
      revealed leaf with the round's signature (`onyx_tlock`, through `BeaconKey::open`)
- [x] C20 Track-record serving follow-ups from C6: serve the logs (params, statements,
      resolutions) and the intake view so a client reruns `compute` and `verify_log`; a
      publication path from the coordinator's intake to the node's store once a served metric has
      a close-time rule; page a history past 1,024 entries; the browser remembers the last
      commitment it saw per record and asks with `since` (THREATS TR-S-13, TR-R2-5)
      (2026-10-18, branch `track-serve`, ADR-0036 amendment 5: `onyx_track::publication` (every
      commitment with its logs in one Borsh value, logs kept once in publication order, each step
      a prefix; `check_steps`); `GET /v1/track/log[?since=]` and `check_log_body` (each served
      commitment's log hashes to it and extends the one before, `compute` rerun with the client's
      registry, `verify_log` on the published intake with its reviewed keys); not served free when
      a resolution is at level 1 or 2 (403 `track_log_priced`, C22) or, in fallback, any ruled
      metric is label-derived; the coordinator's `write_publication` and the node's
      `ONYX_TRACK_PUBLICATIONS` (checked, append-only, re-read on change, 503 on a bad or
      rewritten file; no file exists yet: no source states on the Bitcoin flow rule and C40 is
      open); `GET .../history?at=&from=` pages of 1,024 entries recounted by
      `complete_history`; the index paged; the browser remembers the commitment per record and asks
      with `since` (the example's since file), and reruns the example's logs; THREATS TR-C20-*;
      review C20-R2 the same day: cached-key canonical sorts and a client budget of 64 links over
      16,384 rows (a body inside the old budget cost 37 s native), `MAX_INSTALL_CHECK` and a kept
      head naming the served bytes (restart checks only what is new), requests answered from the
      last finished read, history pages built after the budget, 503 for a node that served before
      and names no file; C22 kept, release rule named below)
- [x] C21 Paged track logs (from C20, TR-C20-5): `GET /v1/track/log` past 16,384 rows in pages a
      client hashes incrementally (a prefix hash per page, or a commitment format with a Merkle
      root over the logs) (2026-10-18, branch `track-pages`, ADR-0036 amendment 6: log format 2,
      a chained prefix hash per row in publication order, each commitment a checkpoint; the
      commitment's layout and domain unchanged, its two log hashes domain-separated per format, so
      format-1 commitments keep their hashes and never check as format 2; a second publication
      layout with a format byte; a head on `GET /v1/track/log` and pages of 4,096 rows
      (`?at=&from=&since=`) and of whole posts (`?at=&posts=`) with their own budget of 60 a
      minute; `PagedLog` checks each row as it arrives with `check_extends`'s rules
      (`ChainedCheck`, same accept and refuse on 3,000 generated scenarios) and each link's hashes
      at its last row; nodes and clients check a chain in one linear pass, rows plus steps, under
      the C20 caps; THREATS TR-C21-*; not built: `verify_chain` over chained logs, C41)
- [ ] C22 Priced answers in track logs (from C20, TR-C20-1, TR-S-13; the lead's decision): a log
      holding a level-1 row or a level-2 result is not served free (403 `track_log_priced`), so
      those records cannot be rerun by a client. Options: release after a delay, serve to buyers
      of the value, or a commitment format that hashes each resolution so values can be redacted.
      Review C20-R2: the 403 is stricter than the paid API (a level-1 row or level-2 result is free
      once past its metric's fresh window, docs/spec/pricing.md; a metric without a policy is free)
      and blocks every real rerun, since the only ruled real metrics (Bitcoin entity flows) resolve
      at level 1. The consistent rule: serve the log free once, for every level-1/2 answer in it,
      `public_from` plus its metric's fresh window has passed (and the metric is not `Restricted`),
      with the node passing the per-metric windows to the track store as plain data (a fence
      amendment: the track routes still name no pricing code). With chained logs (C21), the
      redaction option needs a format 3 whose chain absorbs each row's hash (THREATS TR-C21-R2-6)
- [x] C23 Registry for track reruns (from C20, TR-C20-4): serve the reviewed registry a
      commitment names (`registry_root`) so any client, the browser included, reruns `compute` on
      a real record (2026-10-18, branch `track-pages`, ADR-0036 amendment 6 item 6:
      `onyx_core::RegistryData`, the registry as data with its address rules named by id, rebuilt
      by a client with the rule types it trusts (`RuleBuilder`, `BtcRules`) and used only when the
      root is `registry_root`; carried by the second publication layout, served at
      `GET /v1/track/registry`, withheld with the logs and, in every rights mode, whenever it
      names an address (review C23-R2-2); the browser reruns the example with the served registry
      (`level5/track-example-registry.json`) and checks a node's logs the same way
      (`check_track_log`, `check_track_log_paged`); THREATS TR-C23-*. The live node URL is split
      out as C24)
- [ ] C25 Intake batches larger than one log page (from C21, review C21-R1-2): a batch of more than
      about 27,000 revealed statements with proofs does not fit a 48 MiB posts page and is 413;
      page inside a batch (rows of one batch across pages, checked against its header) or cap
      batches for paged logs (THREATS TR-C21-R1-2)
- [ ] C24 A live node URL for the website's track section (from C23): the section checks the
      example only, because no node is deployed and no domain exists (lead item LC-8,
      launch-checklist.md section 6). Once both exist, the page asks the node's
      `/v1/track/...` routes, `/v1/track/log` and `/v1/track/registry`, with the checks C20-C23
      built (TR-C23-5)
- [ ] C40 Track chain-reader follow-ups from C7: a level-1 reader (rows under final shards via
      `verifyRowLeveled` behind a reviewed fence exception, a per-recipe row decoder, a per-fact
      row index so completeness is checkable), a per-fact index of settled level-3 reports
      instead of the per-source scan of the book, `eth_getLogs` scans paged under RPC range caps;
      a stronger later answer judging entries a weak self answer left self-resolved; a dispute
      path for a resolution the book withdraws after inclusion (a compromised key's report); a
      rule version in the params with the first real record; the coordinator recovers a relayed
      level-3 report's bytes from call traces (`debug_traceTransaction`) so a source relaying its
      own report through a contract cannot halt the record (THREATS TR-C7-4, TR-C7-7, TR-C7-8,
      TR-C7-R1-5, TR-C7-R1-6, TR-C7-R1-10, TR-C7-R2-2)
- [ ] C41 Track client follow-ups from C7: the browser reads `trackHead` from a public RPC and
      passes `Expect.latest`; a commitment log's poster rotation (a new log whose first entry
      continues the old latest); the coordinator posts each published commitment and the node's
      store serves only posted ones (with C20); `chain::verify_chain` over chained (format-2) logs,
      each entry's log a publication-order prefix (from C21) (THREATS TR-R2-5, TR-C7-9, TR-C21-8)
- [ ] C8 Label scores (ADR-0016) before any score counts: external-publication time for
      resolutions (R-SCORE-1), canaries and coverage (R-SCORE-2), a stake or identity cost per
      scored key (L14-F4), finalized rounds from the settlement contract (L18-F8)
- [x] C11 Wire sealed reveals (ADR-0027 amendment 10 item 12) (2026-10-17, branch
      sealed-client, ADR-0027 amendment 11: `level2-observer` seals its signed reveal to the
      draw's reveal key with a fresh ephemeral key and posts it (`postSealedReveal`, only once the
      chain is past commit close); `level2-coordinator --reveal-key FILE` (a key of its own, B3
      checks, `keyfile::RevealKey`) sets it before the draw, reads every drawn slot from one
      finalized block past reveal close (`sealedRevealOf` with `sealedKeyProofOf`, through
      `open_posted`), opens or dismisses each, checks `revealLogComplete` before keeping the read,
      and records every round before writing it (`recordAttempt`, `recordObservation`); book
      version 6 keeps the dismissals; served rounds carry `sealed` (posts, dismissals, record);
      the Rust client asks `drawRevealKey`, `revealLogComplete`, every served post and
      `attemptMatches` with its own drawn set, window and round id and requires a record for every
      sealed round; the web page mirrors the checks (`verify/src/sealed.rs`); rehearsed on a local
      anvil (`scripts/level2-sealed.sh`, two draws: five garbage posts dismissed and recorded,
      then five sealed reveals settled); adversarial pass THREATS C11-*; fresh review C11-R1
      (3 Medium: 2 fixed, keyed draws sealed only and `clearRevealKey`; 1 to C31/C34); not
      deployed: C31)
- [ ] C12 Time-lock layer for sealed reveals (amendment 10 item 9): encrypt to the coordinator
      key AND a drand quicknet round at `reveal_close` (tlock), so nobody opens a post early
      (C4-7, REVLOG-1 residual b); optional second copy to a fixed far date as the
      coordinator-independent unseal (REVLOG-5 residual); dependency review of a tlock crate first
      (2026-10-17, branch timelock, ADR-0027 amendment 12: dependency review done, NO crate
      acceptable (`tlock` 0.0.10 unaudited, panics on a malformed ciphertext, lossy decrypt;
      pure-Rust BLS12-381 libraries self-declared unreviewed; audited `blst` needs `unsafe` FFI
      and offers wasm only as an example); design and composition built and tested with a MOCK beacon only:
      `onyx_coordinator::timelock` (key = ECDH AND a 16-byte secret locked to the first quicknet
      round 60 s past reveal close; no opening without that round's signature, no fallback;
      dismissals carry the time-lock opening, checked by re-encryption); adversarial pass THREATS
      C12-*; fresh review C12-R1 (0 High; 2 Medium: non-canonical time-lock veto fixed with
      `open_canonical`, unbounded block-time lag under a sequencer outage to C53; Lows fixed: seal
      freshness, time-lock in the key); gas measured +90,910 per post (282 bytes, 466,316
      execution gas with the registry cold; 375,406 for today's 154 bytes, not C4's all-zero
      271,992). Second fresh review C12-R2 (cryptography): crate claims re-read in source and
      corrected (`blst`'s Rust binding needs no Onyx `unsafe` and builds for wasm32 with clang,
      as `secp256k1` does: own IBE on it is C50's route); composition holds; block time ahead
      cannot open early, lag is C53; cross-registry replay Low to C31. Remaining work is C50-C53;
      C4-7 stays open)
- [ ] C13 Level-2 disputes (amendment 10 item 10): garbage and bad-aggregation proofs slash
      bonds; needs the pool from an on-chain bond book (L2-F2 residual c) and a coordinator
      settlement bond; on-chain DLEQ check or a panel vote; a receipt-key compromise costs a bond
      (REVLOG-4)
- [ ] C14 Level-2 fetchers compiled to WASM and run under the recipe host, pinned by content
      hash instead of a version tag (THREATS L2-F5; moved here from C4's old notes)
- [ ] C30 Sealed reveals for the EDGAR runner (from C11): its rounds through
      `finish_reveals_sealed` and `record_round`, a reveal key for its draws (EDGAR stays
      rehearsal-only, THREATS SEC-4, C11-13)
- [ ] C31 Redeploy `OnyxRegistry` with sealed reveals (lead only; from C4 and C11): Base Sepolia,
      a reveal key and a receipt key of the deployment's own, then a `level2-sealed`-shaped round
      against it; consider `announceDraw` taking the receipt signer and reveal key as arguments
      (THREATS B2-R1-L1); bind the ciphertext into the key proof and the status-2 entries
      (THREATS C11-R1-1); refuse a draw without a reveal key once observers post sealed, or a
      client cutover that refuses keyless draws (THREATS C11-R2-1); bind the EVM chain id and the
      registry address into the sealed-reveal message and the key proof (THREATS C12-R2-3)
- [ ] C32 The website shows a sealed round (from C11): a recorded sealed rehearsal exported with
      its `sealed_reveals` section and recorded chain answers, the level-2 card showing posts and
      dismissals (THREATS C11-14; after the web restyle)
- [ ] C33 The buyer-facing level-2 limits per round (from C11): REVLOG-1's text and `applies` for a
      draw with a reveal key (the drop is closed there; the coordinator's early view, C4-7, is
      not), stated without breaking B4 clients
- [ ] C34 The coordinator's chain reads for sealed rounds (from review C11-R1): cross-check the
      read slots against a second RPC before keeping them, and read again at the same block when
      the chain refuses the record (THREATS C11-R1-1, C11-R1-4)
- [ ] C50 A tlock implementation Onyx can ship (from C12): drand/tlock's Boneh-Franklin IBE with
      Fujisaki-Okamoto for quicknet (G1 signatures, G2 key, RFC 9380 hash to G1) behind the
      `timelock::TimeLock` trait, on a reviewed BLS12-381 library (an audited crate when one
      exists, or, the route review C12-R2 found, our own IBE on `blst`'s safe Rust API: no
      `unsafe` in Onyx, wasm32 with clang as for `secp256k1`; the dependency added through
      `deny.toml` and `scripts/check-deps.sh` with its own review), never panicking on attacker
      bytes, `lock` deterministic and exposing `sigma`, interop vectors from the Kudelski-reviewed
      Go drand/tlock and published quicknet rounds (decrypt, then re-encrypt to identical bytes,
      on native and wasm32, with `tau` values ending in zero bytes), a fresh cryptography review; then the mock leaves the tests only
      (THREATS C12-1..C12-5, C12-R2-1, C12-R2-2)
      (2026-10-17, branch tlock-ibe, ADR-0027 amendment 13: BUILT, review pending. `crates/onyx-tlock`
      on `blst` 0.3.17 (pinned, reviewed docs/audit/2026-10-17-blst.md, only it may depend on blst),
      no Onyx `unsafe`; Go and tlock-js vectors (quicknet and quicknet-t) decrypt under published
      drand signatures, prove to be age's file key and re-encrypt to identical bytes, native and
      wasm32 (`scripts/check-tlock-wasm.sh`); `tau` ending in zero bytes Onyx-made only (C70);
      adversarial pass THREATS C50-*; fuzz target `tlock_open`; `timelock::QuicknetTimeLock` behind
      the trait, mock in tests only. Fresh cryptography review C50-R1 done (THREATS C50-R1-*:
      0 High, 0 Medium; `decrypt_unverified` fenced, constant-time H3 range test); C50 closes at
      the merge. Not wired: C4-7 stays open until C51)
- [ ] C51 Time-locked sealed posts wired (from C12, after C50): `OnyxRegistry` cap 1,166 -> 1,294
      bytes and a registry or per-draw mode where every sealed post is time-locked (no downgrade,
      C11-R2-1 class; lead redeploys, with C31); observer seals with it; the coordinator waits for
      the lock round's signature; book and served dismissals carry openings; Rust and web clients
      check them (THREATS C12-7, C12-9)
      (2026-10-17, branch tlock-wire, ADR-0027 amendment 14: BUILT, not deployed (the lead
      redeploys with C31). `requireTimeLock()` one way (owner or coordinator), snapshotted per draw
      as `drawTimeLocked`; with it on `announceDraw` refuses a draw without the term's reveal key
      and draws announced before are never recorded (`NotTimeLocked`); posts of a time-locked
      draw longer than 128 bytes; cap 1,294. Observer seals with `QuicknetTimeLock` to the lock
      round of the on-chain draw's window; coordinator waits past reveal close for the lock
      round's signature (`--beacon drand`: four public relays over HTTPS, no account, generic
      User-Agent; `--beacon fixture:FILE`: anvil only), verified against the pinned key, before
      reading or opening anything (missing or forged: the round waits); book v7 and served
      `sealed.timeLock` carry the lock round, signature and openings; Rust and web clients
      re-encrypt each opening (`open_canonical`) and ask `drawTimeLocked`. Rehearsed on a local
      anvil started in the past with a fixture beacon (`scripts/level2-timelocked.sh`); the live
      relays only in the opt-in `c51_live` test. THREATS C51-*; C4-7 PARTIAL (closed for
      time-locked draws; clock lag C53, not deployed C31). C51 closes at the merge)
- [x] C52 Far-date unseal copy (from C12, ADR-0027 amendment 12 item 8): a second time-lock of the
      body key to the subject's last observable day so every round unseals without the
      coordinator (REVLOG-5 residual), checked by the coordinator at intake; 144 more bytes a post
      (2026-10-18, branch tlock-far-date, ADR-0027 amendment 15: BUILT, not deployed (C31). Post
      `TL ‖ TF ‖ w ‖ body`: a fresh body key `kappa`, wrapped for the coordinator with `S` and
      `tau` (16 bytes) and time-locked to the far round with `sigma = far_sigma(kappa)`
      (encrypt-with-hash, 128 bytes). Far round = first quicknet round at or after the fact's
      latest possible reveal close (`FeedTemplate::last_observable_day`: record date +
      `max_calendar_days`, period end + `max_days`, a fixed subject's own window; plus window,
      600 s grace, 86,400 s reveal phase) + 1 day; anyone recomputes it from the registered
      template; bounded at 1,096 days (`FarBeyondHorizon`), never within a day of the round
      (`FarTooSoon`); beacon stop: no unseal, no fallback (C53). Coordinator re-encrypts the far
      copy at intake (before the far round exists) and dismisses a mismatch; clients check
      dismissals with the far rule and `farRound` against their own template; `unseal_far` /
      `unseal_posts` open every post with the far signature alone. Registry cap 1,294 -> 1,438,
      `FAR_COPY_BYTES` 144, time-locked posts > 272 bytes; book v8; served `timeLock.farRound`;
      web mirror; vectors `fixtures/tlock/onyx-far-date.json` native and wasm32. THREATS C52-*.
      Client unseal at the far date: C83)
- [ ] C83 Clients unseal at the far date (from C52): the Rust client and the website take the far
      round's signature (served with the round, verified against the pinned key, or read from a
      drand relay), unseal every in-phase post of a sealed attempt (`timelock::unseal_posts`),
      recompute the revealed set, the outcome and the attempt record, and refuse a round that
      disagrees: a revealed post that does not unseal, or an unsealed post neither revealed nor
      dismissed with a proof that checks (a rightly dismissed post may unseal, review C52-R1-1);
      the API
      serves a fact's attempts unsealed once its far round is out; a rehearsal with a published
      far round (THREATS C52-2, C52-8)
- [ ] C53 Beacon outage and clock margin (from C12): a checkable rule for abandoning and re-drawing
      a sealed draw whose lock round never arrives (no silent fallback), and a checkable rule for
      block time lagging the wall clock past the 60 s margin; the sequencer sets that lag (outage
      catch-up blocks at past timestamps, review C12-R1-1), so a measured Base lag alone
      does not close it (e.g. a round counts only if the L1 batch holding its last in-phase block
      landed before the lock round's time). Block time AHEAD (L3C-12) only delays opening
      (THREATS C12-4, C12-6, C12-R2-4). From C52: the same rules for the far round (a far round
      that never comes leaves the round sealed as before C52; a lag of more than a day at the far
      date, THREATS C52-4, C52-5)
- [ ] C70 Go cross-check of the time-lock (from C50): drand/tlock (Go, pinned v1.2.0) decrypts
      `fixtures/tlock/onyx-zero-tail.json` under the same published signatures, and Go-made
      quicknet ciphertexts whose file key ends in zero bytes (about 1 in 256 `tle` runs) join the
      interop vectors; run where executing drand's code is allowed (C50 executed no downloaded
      code); also Go decrypting `fixtures/tlock/onyx-far-date.json` (C52's far copies, made by
      Onyx) (THREATS C50-9, C50-12, C12-R2-6, C52-10)
- [ ] C80 The website shows a time-locked sealed round (from C51): a recorded
      `scripts/level2-timelocked.sh` export with `sealed_reveals.time_lock` and the recorded
      `drawTimeLocked` answer, the level-2 card showing the lock round and openings (with C32;
      THREATS C51-10)
- [ ] C81 Client cutover to time-locked draws (from C51, with C31 and C33): on a registry that
      requires time-locks, clients refuse rounds of draws announced before the mode, and the
      buyer-facing limits say per round whether the coordinator's early view (C4-7) is closed
      (THREATS C51-2, C11-R2-1)
- [ ] C82 Time-locked rounds for the EDGAR runner (from C51, with C30): its sealed rounds through
      `absorb_sealed_locked` and a beacon source (THREATS C11-13)
- [ ] C10 Wire the level-3 chain book: deploy `OnyxSignedBook` (lead only, a new contract) with a
      reviewer key that is not the deployer, coordinators post every report they take and pass the
      latest block's timestamp as `now`, the API serves level 3 only against `settleable` at a
      finalized block, sequencer-censorship policy for the dispute window (L3C-2), reviewer
      multisig with H1
      (2026-10-17, branch signed-wiring, ADR-0028 amendment 7, spec R-SIG-ON-7, THREATS C10-1..8:
      wired and rehearsed, everything but the live deploy. Done: `DeploySignedBook.s.sol` and
      `scripts/deploy-signed-book.sh` (reviewer == deployer refused by the dry run, the
      constructor and the node), manifest pin `signedBook`/`signedBookCodeHash`/
      `signedBookReviewer` after a build check (`pin-signed-book`), node start check;
      `signed_intake::BookIntake` posts every report it takes, store on the latest block time;
      `GET /v1/signed/{fact}` only against `settleable` at the finalized block, 1,800 s margin over
      the sequencer drift, facts held while a post is pending, breaker at 600 s; money unchanged;
      `scripts/level3-book.sh local` in check-all, `fork` rehearsed. **Deploy step: LEAD-PENDING**,
      docs/ops/runbooks/deploy-signed-book.md. Reviewer multisig: hook only, the manifest names
      one address, H1. Follow-ups C60-C62. Review C10-R2: oversized payloads refused before posting,
      API clock-skew stop, `--plan` rehearsed, failed-deploy broadcast kept; THREATS C10-R2-1..5)
- [ ] C60 A level-3 coordinator process (from C10): reports from a source's endpoint into
      `BookIntake`, a keyed poster for Base Sepolia (`CastPoster` sends from unlocked anvil
      accounts only), `retry_pending` on a timer, the post log at `ONYX_SIGNED_REPORTS`; needs a
      real source with reviewed rights first (THREATS L3-F8)
- [x] C61 Client and website side of `/v1/signed` (from C10): a verify-module check of the body
      against the client's own `settleable` read at a finalized block, and the Data page's level-3
      card (2026-10-18, branch signed-client, ADR-0028 amendment 8, spec R-SIG-CL-1, THREATS
      C61-1..9: `signed_view::check` in the browser verifier (also a Rust client), the payload's
      key bound to its signer, the RPC's finalized block fresh, code hash pinned, margin on block
      and client clock; `scripts/level3-book.sh` step 7 checks the real node's answer on anvil;
      the Data page shows the local-anvil rehearsal with its recorded, pinned chain answers,
      labelled rehearsal, and reads the chain through an RPC the reader picks; shown in public
      builds, TRACKER 2026-10-18)
- [x] C62 Level-3 monitoring (from C10): `/v1/health` counts of pending and refused posts, a
      health-watch alert before the 600 s breaker trips (THREATS C10-3) (2026-10-18, branch
      signed-client: `level3` on `/v1/health` from the stop function the route uses; watch alerts
      at 300 s, on a stop, new and retrying refusals; config refuses a threshold past 385 s, review C61-R2-1: slow runs counted;
      THREATS C62-1..4)
- [ ] C63 Level-3 client without trusting one RPC (from C61, THREATS C61-4): read `settleable`
      through two independent RPCs, or prove it with `eth_getProof` against an output root
      anchored on L1; and pin a live book once C10's deploy is done (the site then checks a live
      answer instead of the rehearsal)
- [x] C64 Ladder slice 2 (NORTH-STAR NS5, ADR-0048; THREATS NS5-1, NS5-5, NS5-6): a node that
      holds a level-3 and a level-2 answer of one fact serves both steps, with `at` lined up across
      lanes; the client checks each stronger step through its own lane's check; a weaker value
      names a stronger answer with another value; level-3 answers carry `ladder` once the
      browser's level-3 client takes it; `onyx-check` binds a row's level to the caller's own
      `verifyRowLeveled`. (Slice 1 done 2026-10-27: every value answer states its level and a
      one-step ladder; forged levels and falls refused; badges and history on the site.)
      Draft spec: docs/spec/ladder-slice-2.md (north-star loop NL13). Also fix the lane B code
      review of slice 1 (THREATS NS5-9..14): clients that remember the last ladder, append-only
      ladders so a late stronger proof is no false fall, a versioned cross-lane field, one canonical
      value form, badges marked stated unless checked, export and settlement levels bound.
      (2026-10-28, ADR-0048 amendment 2, north-star loop NL13: done, except a node route serving
      the cross-lane form, which moved to C68: `canonical_value`; the `v: 2` form with per-lane
      checks and contradictions; `check_extends` accepts a superseded step; `/v1/signed` carries
      its ladder and the browser checks it; `onyx-check --registry` and `--ladder-state`,
      `level2-check --ladder-state`; the site remembers ladders and marks unchecked badges stated.)
- [ ] C68 Serve cross-lane ladders (from C64): when a testnet fact has two lanes (a source that
      both signs and is observed), the node looks up the other lane by `fact_key` and serves
      `?ladder=2` (`onyx_api::ladder::compose`, `to_json_v2`) with contradictions; a contradiction
      is then checked through its own lane before it is shown (THREATS NS5-1, NS5-16); the row
      step's `at` becomes the attestation block's time (R-LAD2-2 for rows).
- [x] C65 Estimate layer (NORTH-STAR NS14, north-star loop NL3): estimates under `estimate.` with
      level `E` outside the ladder, method and error on every answer, refused as an input of any
      checked value; draft spec docs/spec/estimates.md (2026-10-28, ADR-0050: `onyx_core::estimate`,
      `GET /v1/estimates/{metric}`, refusals in shards, level-2 feeds, recipe sources, label sets and
      track resolutions; the data page's estimate card, made in the browser from the checked
      window's whole days. Left: C67)
- [ ] C67 Estimate layer slice 2 (from C65): estimates loaded into `onyx-node` from a file, a
      client rerun of a pinned method program over the named inputs (THREATS EST-4), estimates
      listed apart by the agent toolkit's `onyx_find_dataset` (EST-3), a price policy, an on-chain
      registry entry for method ids (docs/spec/estimates.md open questions 2-3)
- [ ] C66 Track records served for real (NORTH-STAR NS16, north-star loop NL4): daily close-time
      rules for the two Base Sepolia level-2 metrics, a public forecaster's statements, resolutions
      from final rounds only, live records on the site; draft spec docs/spec/track-records-served.md
      (2026-10-28 slice 1, ADR-0036 amendment 7: `onyx_track::daily` daily rule and fact,
      `resolution_of` only from a round the client's node shows recorded at level 2; a publication
      under these rules is served and checks (`a_live_publication_serves_the_daily_records`).
      2026-10-28 slice 2, ADR-0036 amendment 8: the NWS forecast of the Denver high as a labelled
      testnet forecaster (NOAA grades NOAA, TRS-4; relayed by an Onyx test key, TRS-6), statements
      through the intake on Base Sepolia (`scripts/track-nws-sepolia.sh`; six days stated
      2026-10-28), the publication in the export, the data page's live panel checked in the browser
      with "too few to score" (e2e ns16). Left: grades from December (NCEI lag), the deploy that
      serves the publication, the live panel pinning its last commitment (TRS-8), a fixed fetch time
      (TRS-7), a forecaster with another operator before any record leaves testnet (TRS-4))

## Phase P: production data coverage (TOP PRIORITY; the project owner 2026-10-18)

The project owner, 2026-10-18: Onyx must carry every exchange and ETF issuer a vendor tracks, with
their inflows and outflows, plus the on-chain data, for Bitcoin and Ethereum first, and be ready
for a production launch. Today Onyx labels only Binance, OKX, BitMEX and Bitwise BITB, and the
Data page shows only a 24-block demo. Every number is still rebuilt from public chain data by an
open recipe anyone can rerun; a vendor's own series are a cross-check, never copied as data.
The loop works this phase before C, D and X.

Scope (from a vendor's API, 2026-10-18): `exchange-balances` names 30 exchanges plus an
all-exchanges total; `etf-balances` has 31 ETF fields (a 21Shares total plus products of 21Shares, 3iq, ARK, Bitwise,
BlackRock, Bosera/HashKey, CoinShares, ETC Group, Fidelity, Franklin Templeton, Grayscale,
Invesco, Osprey, Purpose, Valkyrie, VanEck, WisdomTree); 480 on-chain metrics over 73 assets.
The entity indexer is not in production yet: it runs locally (this PC's
`~/projects/the entity indexer*` and the SSH host `indexer`).

- [x] P1 Survey the entity indexer (2026-10-18, read only): the live indexer is
      `the entity indexer` (Bitcoin only, daily timer, heavy stages on a 251 GB build machine, a
      1.1 TB BRK store and an unpruned bitcoind on the `indexer` host). Main finding:
      a vendor's 30-exchange and 31-field ETF API series come from TimeChainIndex (offline)
      and CoinGlass (commercial), not from the indexer, so no address sets sit behind them and
      Onyx rebuilds coverage venue by venue. Shareable address evidence exists for about 13
      venues (issuer-published or signed rosters); ETF holdings exist as SEC XBRL filings and
      issuer pages; on-chain ETF addresses only for BITB. Keep out: CoinGlass, TimeChainIndex,
      Dune/Arkham, WalletExplorer, Blockpath, an unsourced IBIT/GBTC file, a private customer
      export, personal names in sanctions files; GraphSense and bitcoin.it until their licenses
      are checked
- [x] P2a Issuer rosters on Bitcoin (absorbs D10) (2026-10-18, branch `rosters-v3`, ADR-0037
      Amendment 1): label set v3 of `test.onyx.entities.btc`, dated (format 2), 18,444
      memberships of 16,555 addresses, 11 issuers (OKX, Binance, BitMEX, Bybit, Bitfinex, HTX,
      CoinEx, MEXC, Crypto.com, Deribit, Bitwise BITB) from 206 of their own dated snapshots
      (`fixtures/rosters/`, sha256 and keccak of each raw snapshot, URL, issuer and fetch dates,
      height and its basis); each membership cites its first and last snapshot in its Merkle
      leaf; a listing counts 8,928 blocks; signatures verified in Onyx over messages naming the issuer (OKX 26,214, HTX 127,
      CoinEx 100 rows); byte-for-byte rebuild (`build-rosters.py listings` +
      `build-label-set-v3`, test `labels_v3`); daily flows per entity over v3 windows on the
      fixture blocks with the balance recipe (`export-rosters`, Python recount), balances where
      the committed scan holds the addresses; Reserves page lists all 11 (private build), fallback
      build refuses the export. Left out: wBTC (terms forbid), cbBTC (terms unread), OKX/BitMEX/
      MEXC before 2026 (size, D24). Open: v3 scan and reserve chain over windows, fresh
      snapshots (D23); header check of mapped heights (D12, ROS-3); rights (L8-5)
- [ ] P2b Mining pools from coinbase tags (chain-derived): the 46 pools' payout attribution
      rebuilt by an Onyx recipe from block coinbase tags, dated, with ambiguity refused
- [x] P2c ETF holdings and flows from SEC filings (2026-10-19, branch `etf-filings`, split: the
      rest is P2g, P2h, P2f): 14 US spot bitcoin funds found on EDGAR itself, each with its
      CIK; 7 covered (IBIT, FBTC, GBTC, Grayscale Bitcoin Mini, ARKB, EZBC, DEFI) by
      `onyx_source_edgar::etf` (pinned fetcher `edgar-etf-btc-holdings:v1`, one Period feed per
      fund, BTC as satoshis converted exactly from the filed decimal, accession, form, filing
      date and `us-gaap` fact kept per period; THREATS ETF-1..8); flows only between two filed
      period ends with both dates; recorded fixtures with SHA-256 (a level-2 source; no observer round
      has run on it yet, P2h) (`fixtures/etf/`); Data page
      section recomputed in the browser, shown in every build (filings are public data);
      `cargo run -p onyx-source-edgar --example etf-holdings` reruns it. Not covered, with the
      reason: BITB, HODL, BRRR, BTCO, BTCW, MSBT (quantity only in issuer or dimensional XBRL,
      needs www.sec.gov), OBTC (cross-check fails), six products outside the US. Issuer daily
      pages: terms read for 13 issuers, none allows it (DATA-LICENSE.md section 6)
- [ ] P2g Filing XBRL reader (Needs the project owner first: a role contact for the SEC User-Agent;
      www.sec.gov refuses `onyx-dev (contact pending)`, THREATS ETF-5): read each filing's own
      XBRL instance for BITB, HODL, BRRR, BTCO, BTCW, MSBT, OBTC and FBTC's 10-Qs (issuer
      concepts and dimensions per fund, learned from the local indexer's notes), plus an
      automated per-period fair-value cross-check in the fetcher (ETF-6)
- [ ] P2h Observer rounds on the ETF feeds (as the company feeds have: draws, commit-reveal,
      results stores) and a re-record schedule after each filing season (ETF-7)
- [ ] P2f Daily ETF flows from issuer daily holdings pages: only with an issuer's written
      permission or a licensed source (the project owner's call). Terms read 2026-10-19: BlackRock,
      Fidelity, ARK, 21Shares, Bitwise, VanEck, Invesco, Morgan Stanley and Osprey forbid it;
      Grayscale, WisdomTree, Franklin Templeton (pages not readable by the loop) and CoinShares
      (no terms found) need a person to read them; Hashdex DEFI reported closed
- [ ] P2d Totals-only proofs (Bitget, KuCoin, Upbit, Gate and the rest) as level-2 check
      values, never as address sets
- [ ] P2e Estimated layer, later: clustering from shareable seeds only, graded "estimated"
      and never mixed into published totals, with its checks declared before the run; names the
      exchanges with no shareable list (Coinbase, Kraken, Gemini, Robinhood, Upbit and others)
      as not covered until then
- [x] P3a First on-chain metrics on the Data page (2026-10-18, branch `onchain-data`,
      ADR-0043): recipe `btc-chain-stats`, one row and one shard per block, level 1 (no label,
      no price): transactions, inputs and outputs, size, weight and fullness, block interval,
      subsidy and new supply, fees and the median and mean fee rate, value moved, SegWit and
      Taproot share, active scripts per block, coin-blocks destroyed. Window 969,840-971,199
      (1,360 blocks, 4-13 Oct 2026) from the fetched block folders; golden test, independent
      Python recount of every field, browser rerun of one block, eleven charts in every build
      (public and fallback included), each naming the recipe, its blocks and the rerun command;
      `scripts/refresh-onchain.py` refreshes from a Bitcoin Core REST endpoint or block folders
      and rewrites the pins. THREATS CHAIN-1..10
- [ ] P3 The rest of the on-chain data: refresh from live blocks on a schedule (needs a reachable
      node: none listens here, and the fetched folders end at 971,199); daily active addresses
      (a day-wide distinct count: a day-sized shard is about 105 MB of source, past the 256 MiB
      memory limit decoded, so a two-stage recipe); realized cap, MVRV and SOPR (a licensed price
      per block; none in the repository); exchange and ETF netflows on the page (P2a, P2c); a
      longer window, then the full history (971,200 blocks with prevouts from an unpruned node,
      at most about 8.4 days of one core at today's density; P5 measures it); a fresh
      adversarial review of P3a
- [ ] P4 Ethereum (absorbs X1): an EVM adapter, then the same exchanges and issuers on
      Ethereum (ETH, USDT, USDC balances and flows), then the Data page section
- [x] P4a Ethereum adapter and on-chain data (absorbs X1's adapter; 2026-10-19, branch
      `eth-adapter`, ADR-0044): `onyx-eth-schema` (RLP, ordered trie root, header, transaction
      types 0-4 and receipt decoding, EIP-1559 and EIP-4844 fee rules; no new crate),
      `onyx-source-eth` (JSON-RPC answers re-encoded and checked against the block hash, every
      transaction hash and both trie roots), recipe `eth-chain-stats` (one row per block: txs,
      reverted, contract creations, gas used and limit in ppm, base fee, ETH burned, priority fees,
      blob gas, blob base fee and fees, value sent; it re-checks hashes, parents, roots and the base
      fee itself), 300 finalized blocks 26,225,261-26,225,560 from Onyx's own reth node cross-checked on
      Flashbots (hashes committed, 3 blocks in full), `scripts/refresh-eth-onchain.py`, an
      independent Python recount (19 of 19 fields, 300 of 300 blocks), the Data page "Ethereum on
      chain" section in every build with a browser rerun. Open in P4d: a beacon light-client check of
      finality (ETH-2), a worst-case fuel measurement (ETH-11), per-block browser reruns (ETH-12)
- [ ] P4b Exchanges and issuers on Ethereum (next): balances and flows of the venues whose
      proof-of-reserve snapshots already name Ethereum addresses (read-only survey of the
      indexer's run folders, 2026-10-19; counts are distinct 0x addresses on Ethereum): OKX (about
      14,900 in the 2026-07-07 file, ETH, USDT, USDC and other ERC-20, every row signed "I am an OKX
      address"), CoinEx (35 wallets plus 12 multisig owner keys and 311 validators, signed),
      HTX (55, mostly signed), Binance (47, unsigned monthly files), MEXC (18, unsigned),
      Bybit (105 in the latest auditor report, no asset column), Deribit (about 13), Bitfinex
      (4), Crypto.com (4, a 2022 blog post), Bitget (10 of a list the API pages). None of BitMEX,
      Gate, KuCoin, Upbit or Luno publish any. The indexer's pipelines keep
      only Bitcoin rows, so the Ethereum rows exist only in the raw snapshots, and no folder records
      a licence: same rights gate as P2a (L8-5, LC-1) before any total is served. Needs: dated
      snapshots as label set rows (EVM address canon, EIP-191 signature checks where signed, as
      for OKX's Bitcoin rows), ETH balances at a block from `eth_getBalance` proven by
      `eth_getProof` against the header's `stateRoot` (EIP-1186; historical blocks need an archive
      node or a recorded proof), flows from the blocks' value transfers plus internal transfers
      (execution traces, which a plain RPC does not prove), and validator balances for staked ETH
      (beacon chain, not the execution layer)
- [ ] P4c USDT and USDC on Ethereum: supply from `Transfer` logs to and from the zero address
      plus each issuer's own mint and burn events, balances per labelled address from
      `Transfer` logs (covered by the receipts root `eth-chain-stats` already checks) rolled from a
      base read with `eth_call balanceOf` proven by `eth_getProof` against the token contract's
      storage slot (to be pinned per contract and tested against the node), freeze and blacklist
      events, and the token contracts' upgrade events (both are proxies), each as its own recipe
- [ ] P4d Ethereum trust follow-ups from P4a: a beacon light-client check of finality in the
      adapter and the browser (sync-committee BLS signatures over the finalized header; ETH-2),
      a worst-case fuel measurement for a dense block (ETH-11), per-block browser reruns on
      request (ETH-12). Named as its own item when main's P-item rule (P4a, P4b whole) met the
      branch's `TODO P4` rows (review P4A-R2)
- [ ] P5 Hardware requirements: per role (contributor/indexer, validator, buyer) and per
      chain (Bitcoin, Ethereum), measured on the indexer machines plus official client
      requirements with links (no invented numbers); docs/ops/hardware.md, a README table,
      the website, and a preflight check that tells a contributor before an indexer starts if
      the machine is too small (CPU, RAM, free disk, disk type)
- [x] P5 Hardware requirements (2026-10-18, branch `hardware`): docs/ops/hardware.md per role
      (contributor, entity indexer and reserve path, validator, buyer, node operator,
      coordinator) and chain (Bitcoin; Ethereum planned, linked to reth and geth), every number
      measured (machine, date, method), linked (URL, date read), a code limit, or "not measured
      yet"; Onyx's own release runs measured with `/usr/bin/time -v`; `onyx-node doctor`
      (CPU, usable RAM with cgroup v1 and v2 limits, disk size and free space, disk kind NVMe,
      other SSD or rotational; PASS/WARN/FAIL/SKIP; thresholds tested against the doc's table)
      run at every node start (warns; refuses only below the soak's measured state size); README
      table; Run page section. Review P5-R1 (2026-10-19): 2 Medium, 3 Low fixed (THREATS P5-R1-*)
- [ ] P5b Hardware cells still "not measured yet": Bitcoin Core initial sync time and dbcache's
      effect, the coordinator alone, the node under load (CPU), the browser's memory while it
      checks, the entity indexer below the maintainer's machine; Ethereum after P4
- [ ] P6 Parity check: each Onyx entity balance and flow against a vendor's series for
      the same day, differences reported per entity with their cause; cross-check only
- [x] P7 Production launch readiness: walk docs/ops/launch-checklist.md to green; every gate
      only the project owner can pass (domain, host, rights, contacts, keys) listed once, ranked
      (2026-10-18, branch launch-walk: all 27 gates re-read with evidence; re-run LC-12, LC-14,
      LC-15, LC-17 tests, LC-20, LC-27; LC-3 decided by the loop (label-removal.md), LC-16 back to
      PARTIAL (two contracts changed since L2, new L17), LC-7 leftovers split to L18, redeploy
      runbook fixed for `ONYX_RIGHTS`, OSS-18 folded into LC-26; 10 verified, 17 open, 11
      blockers; ranked the project owner table in the checklist's section 6. Not green: 8 gates need
      the project owner, the rest is P7a)
- [ ] P7a Launch-commit reruns (loop, after the project owner's LC-1, LC-8 and L17): on the commit that is
      launched, `ONYX_WEB_PUBLIC=1 scripts/check-all.sh` (LC-10, LC-15, LC-16; E12 first if its
      timing test still flakes), a fresh Base Sepolia run (LC-11), `scripts/fuzz-smoke.sh` (LC-21),
      `python3 -I scripts/check-source-terms.py` (LC-27; first read Binance's site terms with the
      e2e's headless Chromium, `playwright-core`, since a plain fetch gets a script challenge), the history rescan if the repository goes
      public (LC-25); on the chosen host LC-9, LC-17 and the LC-23 install; plus E9 (LC-13) and L5
      (LC-22)
- [ ] P8 Then the remaining a vendor chains (X2-X5) and the rest of the 480 metrics, in
      batches, each with its recipe, review and website section

## Phase D: data coverage (entity indexer everywhere)

- [x] D1 Reserve: date-ranged list membership (RES-2), confirmation depth (RES-4) (2026-10-17,
      branch `reserve-ranges`, ADR-0037): dated label sets (format 2, ranges in Merkle leaves,
      windows, no back-dating), membership changes rebased in the serving check, 6
      confirmations on a proof-of-work header chain for every scan, flows file and shard;
      the served v2 chain unchanged; no dated fixture yet (D10)
- [ ] D2 Reserve: second-node re-scan tooling, so Binance/BitMEX can be served once a
      second node exists (RES-1, RES-11)
- [x] D3 Client-side reserve serving check (RES-10): `reserve::client_check` derives the base
      from the client's own scan pin and the links' flows and refuses any row on another base;
      `onyx_api::client::verify_row`, free hint `/v1/served/{metric}` (withheld in fallback),
      `onyx_reserve_serve::client`; the browser runs it on the week export (ADR-0034 Amendment 2,
      THREATS RES-C1..C8)
- [ ] D4 cbBTC reserve list: rights review first (no build until cleared)
- [ ] D5 Mining-pool labels only with a method that proves pool ownership (LBL-V2-F1)
- [ ] D6 Longer history (beyond one week) with roots on chain (HIST-1)
- [x] D10 (split 2026-10-18: the set itself shipped with P2a; the manifest part is D25) Dated label set v3 of `test.onyx.entities.btc` from the sources' own dated snapshots
      (OKX monthly PoR files, Binance wallet files, BITB page captures): per-row first and last
      listing dates mapped to heights by header timestamps; `VersionLog` manifest format field
      and `append_dated`; needs the indexer archives read and L8-5 (no new source)
- [ ] D11 Membership-change balances checked by reruns, not the flows file's word (RES-13):
      rerun the changing addresses' out-of-range flows as their own shards, and a Python recount
      of the dated derivation (`recount-entity-reserve.py`)
- [ ] D12 Header chain upkeep (RES-4): the gate's headers file is static (to 971,207), so a later
      scan or shard is refused until it is extended; an operator tool to refresh it from its own
      node (comparing its best chain's work with the file's, RES-20), and the depth check in
      `export-reserves` and the website's reserve check
- [ ] D23 Roster set follow-ups (from P2a): a UTXO scan of every v3 address at a fixture height
      and the `btc-entity-reserve` chain over v3 windows (ADR-0037 items 3 and 5), so every
      roster entity gets balances, not only those the v2 scan holds; snapshots newer than
      2026-08 fetched from the issuers; a scheduled re-fetch of every raw snapshot that checks
      its sha256 and re-extracts and re-runs `build-rosters.py deny` (ROS-5, ROS-6, P2a-R2-1); an
      opt-in golden that reruns `export-rosters` over `ONYX_RESERVE_DIRS` (P2a-R2-5)
- [ ] D24 Full-history roster set kept outside git: OKX 2023-2025, BitMEX 2022-2025, MEXC 2025
      snapshots (recorded by hash in `fixtures/rosters/*/snapshots.json`) in a production set name,
      content-addressed storage instead of fixtures (ROS-11)
- [ ] D25 (from D10) `VersionLog` manifest format field and `append_dated`, so a dated version
      is logged like an undated one (today the dated history is checked by `check_history` only);
      and the successor rule for roster sets (review P2a-R2-4): today a v4 derived with one more
      snapshot that relists an address open at v3's publication changes that
      membership's cited last snapshot below 971,207 and is refused as back-dated, so
      `append_dated` must close open ranges at the old publication or carry their evidence
      (`roster::tests::p2a_r2_4_a_relisting_successor_is_refused_until_d25` flips then)
- [ ] D20 Browser check of live reserve API answers (from D3, RES-C7): a chain read and the
      binding check of an answer in the browser verifier, then `client_check` on the links; build
      the `/v1/served` list once per gate run and serve it from memory (D3-R1-5)
- [ ] D21 Reserve anchor on chain (from D3, RES-10, RES-C2): the scan hash, label set and start
      height registered with the reserve recipe, so a client reads its pin from the chain instead
      of the repository
- [x] D30 Serve label sets whose rights are not cleared in salted format 3 only
      (`onyx_labels::salted`, partner tracker T1, ADR-0037 amendment 2): the serving path, the
      browser verifier and the registry read format 3; a buyer gets its row's salt, never a sibling's.
      Done 2026-10-25 for the serving path and the browser (ADR-0037 amendment 3, pre-public loop
      PP3): `GET /v1/labels/{set}/{address}`, `check_salted_answer`, shared vectors
      `fixtures/dated/salted-vectors.json`; only `test.*` sets load. The registry half moved to D31.
- [ ] D31 Paid salted label rows for real sets: the registry records a set's format-3 content hash
      (so a client reads its pin from the chain), x402 pricing per row, an operator tool that writes
      the set file, and the rights review of the set; until then real sets are refused at startup

## Phase X: more chains (after Phase D; the project owner 2026-10-17: "What about all the other blockchains a vendor offers?")

Onyx rebuilds every number from public chain data with its own node or RPC reads and an open
recipe; it never copies a vendor's (or any vendor's) numbers. Company labels on a new chain
need the same rights review as Bitcoin's (L8-5, LC-1) before any label-derived total is served.
One chain at a time, each with its own adapter, recipe, indexer, threats pass and website section.

- [x] D22 Client check after D1 and at lower cost (from D3 review D3-R2-2, D3-R2-3) (2026-10-18,
      branch `reserve-dated-client`, ADR-0034 Amendment 3): `ReservePin::from_dated_scan` (dated
      set and history, flows file trusted by hash, `totals_at`), each link's window hash and window
      leaf index, membership-change deltas (`change_at`), every link's tip and the pin's scan and
      flows on the client's header chain (6 confirmations); `derive_entity_span` from the landing
      (or an entity's exit or entry), shared with the browser, so a newer reconciled scan can be
      pinned and a check pays `|answer - landing| + 1` rows; `MAX_LINKS` removed for a client
      budget (`DEFAULT_SPAN_BUDGET` 4,096 rows, 10.24 USDC); browser tips against headers stay D12
- [x] X1 Ethereum: absorbed by Phase P (2026-10-19): the adapter and block metrics are P4a
      (ADR-0044), entities on Ethereum are P4b, stablecoins (F3 on Ethereum) are P4c; F9 staking
      stays with P4b (validator balances)
- [ ] X2 Solana: an adapter on public RPC with pinned slots; supply, fees, stablecoin supply
      (USDT, USDC on Solana)
- [ ] X3 Tron: an adapter for blocks and TRC-20 transfers; USDT and USDC on Tron supply first
- [ ] X4 BNB Chain, then the other EVM chains, reusing X1's adapter where the chain allows
- [ ] X5 XRP Ledger, Cardano, Dogecoin, Litecoin, Bitcoin Cash: adapters in order of measured
      demand; UTXO chains reuse the Bitcoin recipes where the format allows

## Phase S: submit data (the project owner + a partner 2026-10-20)

Partner questions and the work they create (T1-T14; T, not M, so they never read as PLAN milestones): the private partner notes (not published).

Lowest-friction way for anyone to submit data: a form (and API) where a person gives a
description, the source and an endpoint; an onboarding questionnaire decides the data type and
its proof level; anything else becomes an unverified level-4 claim. Demo story: "I'm a random
public-data user, walk me through submitting" (U.S. Treasury Fiscal Data; FRED is blocked, N10).
ADR-0038, docs/spec/submissions.md, THREATS SUB-*.

- [x] S1 Submit data (2026-10-20, branch submit-onboarding, not merged): `crates/onyx-intake`
      (five questions, route to levels 1-5, refusals and holds, byte caps, no personal contact,
      rights-cleared pull policy, shared vectors), `onyx_api::submit` (`/v1/submit/check`,
      `/v1/submit`, `/v1/submit/{id}`, journal, budgets, SSRF-safe `HttpsFetch`, `submit-dev`
      example), website `#/submit` "Add data" (four steps, Treasury example, offline download;
      the project owner's addition: a keyed-source walk-through for a FRED user, key never leaves the
      browser, stub prover labelled not real, held for rights review), e2e `tests/s1.mjs`
- [ ] S2 Keyed sources without sharing the key: research done; S2b experiment done 2026-10-24
      (experiments/tlsn-fiscaldata: a real TLSNotary presentation of the Treasury API with a
      hidden header, verified offline; partner tracker T11); research
      (docs/research/2026-10-20-api-keys-zk.md: TLSNotary first, level 3 only when no single party
      can forge the proof); next: the smallest experiment there (local TLSNotary notary against
      the Treasury API with a dummy key header redacted), then a browser prover behind the stub's
      data shape; encrypted keys on Onyx servers are rejected; a FRED run needs the project owner's own key
      (Needs the project owner) and still publishes nothing. 2026-10-28: the browser prover (NORTH-STAR NS6,
      north-star loop NL11) is parked after an upstream check (RESEARCH.md "NL11 upstream check")
- [ ] S3 Staked submissions (Polymarket-style): a submitter bonds money on a value being right.
      PARTIAL 2026-10-24 (ADR-0042, branch stake-markets, partner tracker T10): contracts and
      tests in a local EVM, demo `scripts/demo-markets.sh`. The partners chose to try a stake vote
      (call 56:26); re-observation still decides re-readable data, and a vote-decided value is
      level 4 and never settles a market. S3b drawn-panel fallback done 2026-10-25 (`OnyxPanelOracle`,
      ADR-0042 amendment 1). Open: cap squatting (STK-6), panel pay and seat sharing (STK-16),
      validator pay (STK-10), Rust client, website step, deployment, partners' parameters
- [ ] S4 Validator re-observation of accepted level-2 submissions (PARTIAL 2026-10-24: `OnyxReobserve`, a minimal on-chain port; S4b: draws, bonds, tolerance): an accepted `auto_pull`
      submission becomes a reviewed feed template and observer rounds (ADR-0027); move the
      JSON-pointer fetcher into a core crate if the intake and observers must share it
- [ ] S6 Data futures: PARTIAL 2026-10-24 (ADR-0042): parimutuel markets on a future value of a
      fact, resolved only from level 1-2 facts. 2026-10-27 (NS11, NL6, ADR-0042 amendment 2):
      DEPLOYED on Base Sepolia with the stakes and the drawn panel (`scripts/markets-sepolia.sh`,
      addresses in docs/ops/deployment.md); futures settle from level-2 values the registry
      recorded (`OnyxObservedFacts.prove`); market 1 (Treasury debt, record date 2026-10-27) open
      with four test bets, settles after the 2026-10-29 round; the Markets page reads the markets
      from the chain in the reader's browser (`LiveMarkets.svelte`, `markets_view.rs`). Open: a
      challenge window before `resolve` (THREATS NL6-2), refusing the
      level-2 observer registry's members as bettors (NL6-4), legal review.
      2026-10-27 (NL14, NL18, ADR-0042 amendment 3): a second deployment with guarded listings
      (`closeBy`, insiders) and guarded futures, and `OnyxParametric` test policies
      (`scripts/insurance-sepolia.sh`, two products offered).
- [ ] S7 Prepaid balances over HTTP (ADR-0046, partner tracker T7): a request header carrying a
      voucher instead of an x402 payment, `onyx_api::prepaid::PrepaidBook` per buyer from chain
      reads of `OnyxPrepaid.accounts`, a redemption schedule inside the exit delay, receipts for
      the books; the contract and the voucher library are built (pre-public loop PP5). Part 1
      done 2026-10-25 (PP10, ADR-0046 amendment 1): the header, account reads behind
      `AccountSource`, the owner-only usage journal written before the answer leaves, retries;
      part 2a done 2026-10-25 (ADR-0046 amendment 2): chain reads, the redeem-then-allocate pass
      with marks before the send, `scripts/prepaid-anvil.sh`; part 2b done 2026-10-25 (ADR-0046
      amendment 3): `ONYX_PREPAID` in onyx-node, start checks, the pass on the reporter's timer.
      Open: a node started with `ONYX_PREPAID` against anvil (rehearsal), a Base Sepolia deploy,
      an alert on a pool that stays above zero
- [ ] S8 Agent toolkit trust anchors (NS8 follow-up, ADR-0047, THREATS AGT-10): tools that read a
      market's round, shard and subject from the market contract and a metric's definition from the
      registry root, both on the agent's own node, so an agent never copies what it checks from
      the server it checks; `onyx-check` kinds for level-2 observations, signed facts and track
      records (their Rust checks exist)
- [ ] S5 Hosted demo on the VPS: mount the submit routes in onyx-node behind the guard (TLS front,
      trusted proxy), a reviewer tool for the journal (clear, hold, drop), the reviewer checklist
      (terms re-read, personal data, entities only: SUB-3..SUB-6), an alert before the recorded
      cap, a website build with `VITE_ONYX_SUBMIT_API=1` on the same origin; needs the host (LC-8)
      (2026-10-27, north-star loop NL21: the agent toolkit's `onyx_propose_dataset` and the remote
      MCP server `apps/agent/src/http.mjs` are built in lane C. Left for lane A at the final
      deploy: run `http.mjs` as a systemd unit on 127.0.0.1:8090 with `ONYX_API_URL` the local
      node, `ONYX_MCP_TRUSTED_PROXY=127.0.0.1` and proposals off, and route `/mcp` (POST, DELETE)
      to it in deploy/caddy/onyx-web.Caddyfile; mount the submit routes behind the guard with the
      intake limits named in THREATS NL21-2 before `ONYX_MCP_ALLOW_PROPOSE=1`)
      (2026-10-28, lane A: done in the repo, on the server at the next `scripts/deploy-api.sh`:
      the node's intake (`ONYX_SUBMISSIONS`) with its limits, `/v1/submit` routes in
      deploy/caddy/onyx-web.Caddyfile, the connector as `deploy/systemd/onyx-mcp.service` behind
      `/mcp` (read-only); left: the reviewer alert before the recorded cap, the website's Add data
      page against this host, the connector's client address and freshness (THREATS REL-4, REL-5))
- [ ] S9 Observe on the first paid query (NORTH-STAR NS10, north-star loop NL8): free proposals,
      a hold on a prepaid voucher, a ticket, charge on a final round, release on failure; spec
      docs/spec/paid-observation.md (2026-10-27: slice 1 DONE, ADR-0049: `onyx_api::observe`,
      prepaid holds, `/v1/tickets/{ticket}`, `ONYX_OBSERVE` with the operator's round command;
      7 API tests, journal and node tests. Left: the receipt-page "observe now" button, a node
      rehearsal against an anvil `OnyxPrepaid` with a real round command, the health watch alert
      on an old queue, THREATS NL8-6 (failed rounds are free to their buyer))
- [ ] S10 Bounty board (NORTH-STAR NS15, north-star loop NL9): coarse wanted-data categories from
      closed lists, bucketed and shown only above K accounts; draft spec docs/spec/bounty-board.md
      (2026-10-28 slice 1, ADR-0051: `onyx_intake::bounty`, `GET`/`POST /v1/wants` signed for the
      week by an open prepaid deposit, salted account hashes, 5 accounts, buckets, a weekly cut;
      the Add data page's "Wanted data" panel. Left: a per-client budget (THREATS NL9-6), a minimum
      deposit before money pledges (NL9-1), pledges themselves (after NL8's hold))
- [ ] S11 Storage tiers (NORTH-STAR NS13, north-star loop NL10): hot, warm, cold; cold only for
      inputs fetchable again byte for byte, a recompute served only if it hashes to the chain root;
      draft spec docs/spec/storage-tiers.md. 2026-10-28 slice 1 DONE (ADR-0052,
      `onyx_coordinator::tiers`: `TieredStore` moves hot, warm, cold with write and read back first;
      cold only after a rebuild matched the chain root; kept inputs checked; `RecomputeQueue` paid,
      capped, overdue counted; tier file checked at load). Left: the node uses it (`ONYX_TIERS`,
      move schedule), the `202` cold answer with an NL8 ticket, `/v1/health` tier counts, the
      health-watch alarm on overdue requests (NL10-4), warm compression, a mover lock (NL10-8)
- [ ] S12 Dataset backing (input for NORTH-STAR NS12; waits for the partners' numbers): backers stake
      behind a dataset, earn from its paid fees only, lose part of the pool on a checked error;
      draft spec docs/spec/dataset-backing.md, THREATS DB-1..8
- [ ] S13 More datasets (north-star loop NL17, spec docs/spec/more-datasets.md on branch
      `nl-lane-b`): 2026-10-27 DONE for rows 1 to 3: GHCN-Daily `PRCP` at Denver and `TMAX` and
      `PRCP` at New York Central Park (`onyx_source_ncei::series`), M2.5+ within 300 km of Los
      Angeles (`onyx_source_usgs::region`), rights records S24 and S25, live rounds with
      `ONYX_L2_SERIES` (docs/ops/demo-testnet.md). Left: measure the revisions of `PRCP` and of
      small regional events (the lags are the TMAX and world-feed ones, THREATS NL17-2), BLS CPI
      (a monthly subject rule), the new values on the website

## Phase W: website (own lane, runs alongside the other phases)

Detailed checklist and numbers: docs/website-plan.md (W1 baseline audit, W2 phone layout,
W3 accessibility, W4 speed, W5 home page clarity, W6 chart consistency, W7 Run page with the
2026-10-14 run, W8 copy pass, W9 final audit). One website item at a time; republish after each.

## Phase F: new data sources (rights review first, every time)

Rule for every source: before any code, record in docs/ops/indexer-integration.md (or a new
docs/ops/data-sources.md) the source, link, licence/terms quote, and decision (OK / blocked /
needs the project owner). Only public-domain, openly licensed, or self-published-for-verification data;
on-chain state is public. Entities only, never individuals. No build until the row says OK.

- [x] F1 Data-source catalogue: docs/ops/data-sources.md listing every current and candidate
      source with rights status, proof level reachable, and value; rank the candidates
      (2026-10-14: 15 current, 18 candidates; ranking below; rights questions in "Needs the project owner")
- [ ] F2 Bitcoin network metrics without labels (data-sources N1): fees, subsidy, miner revenue,
      tx count, size/weight per block; pure chain data, level 1. First: recipe `btc-block-stats`
      over `fixtures-live/week`, golden test, `#/data` panel
- [ ] F3 Stablecoin supply on Ethereum and Base (N2): USDC and USDT first (`totalSupply`,
      mint/burn events; contract state is public), DAI and PYUSD once their addresses are
      confirmed on the issuers' own pages; level 1 with pinned logs or storage proofs. First:
      spec + ADR for EVM inputs (receipt proofs vs `eth_getProof`); node access is the project owner's
      call if it costs money
- [ ] F4 SEC EDGAR more companies and spot ETF trusts (N4) as level-2 feeds, no-email
      User-Agent. First: probe `companyfacts` for the trusts and more treasuries; register only
      whole-holdings tags (SEC-R1-F2). Spot ETF trusts done in P2c (2026-10-19); more treasuries open
- [ ] F5 US Treasury Fiscal Data beyond Debt to the Penny (N3, public domain) as level-2 feeds.
      First: Daily Treasury Statement operating cash balance (2026-10-29, north-star loop NL25:
      the TGA closing balance, `onyx_source_fiscaldata::series`, one live round; THREATS NL25-1..3)
- [ ] F6 Federal Reserve Board and BLS public-domain series (N7, N8) as level-2 feeds, with BLS's
      required citation and disclaimer (FRED is blocked for redistribution, N10). First: one
      H.15 rate series and CPI
- [ ] F7 More entity reserves where the entity self-publishes its addresses for verification
      (N13 Bybit, N14 Deribit, N15 Bitfinex, N16 Bitwise ETHW): every one waits on the project owner's
      rights decision; add only cleared ones to the reserve recipe with cross-checks
- [ ] F8 Bitcoin UTXO age bands (N6), level 1 from a pinned UTXO-set snapshot; heavy inputs,
      memory-capped run plan first
- [ ] F9 Ethereum staking (N11): deposit-contract deposits (after F3's EVM inputs), then
      validator totals from a pinned beacon state
- [ ] F10 Base / OP-stack L2 metrics (N12): tx count, gas, fees, L1 data fee; after F3
- [ ] F11 Longer Bitcoin history (months), with roots on chain, sized by measured cost
- [ ] F12 SEC Form 13F data sets (N5): per-security aggregates only, never per-filer rows
- [ ] F13 Lightning graph aggregates (N18): parked until a Lightning node exists; never aliases

## Phase G: long-term health (one item whenever A-F are blocked or waiting)

- [ ] G1 Client SDKs: a small TypeScript and Python client that pays (x402) and verifies on
      its own node, with examples and tests against the local rehearsal
- [ ] G2 API docs: OpenAPI description of every endpoint, generated or checked against the
      router in tests
- [ ] G3 Observability: Prometheus-style metrics endpoint on onyx-node (requests, payments,
      indexer lag, gate state), docs for alerting
- [ ] G4 Independent operators: a runbook for a second operator to run miner/validator and
      onyx-node from docs alone; test it with a fresh container as "another operator"
- [ ] G5 Upgrade path: versioned contracts and data formats, migration notes, a
      testnet-redeploy playbook that keeps old roots verifiable (playbook done in L15:
      docs/ops/runbooks/redeploy-new-ledger.md, rehearsed on a fork; versioning and migration
      notes still open)
- [ ] G6 Performance budget: track check-all time, build time, recipe fuel, gas per round;
      fail when a budget regresses beyond a stated tolerance

## Progress log (newest first; one line per landing)

- 2026-10-27 B6 level 2 on Base Sepolia: the coordinator, observer and a client check are shared by every dated source (`onyx_level2::live`; Treasury examples now call it; USGS and NCEI got theirs); live senders sign in process (`TxSigner`, `KeySigner`); the draw lead is 50 blocks off anvil (0.2 s blocks); `scripts/level2-sepolia.sh` ran one USGS day (5 earthquakes, 15 txs) and one NCEI day (28.3 °C, 14 txs) on registry `0x4973…2b`, 5 of 5 agreed, all receipts on chain; `scripts/deploy-api.sh` ships the stores with their pins; THREATS B6-1..8. Hosted serving and the site link wait for the owner's OK to redeploy
- 2026-10-20 S1 review S1-R1 (fresh reviewer, 0 Critical/High, 2 Medium, 5 Low, 1 Info) fixed on submit-onboarding, each test shown failing first: page key guard trimmed and case-blind, no IPv6 prefix multiplier plus daily quotas, all of stlouisfed.org blocked, one blank/format-character rule and a strict JSON reader shared by Rust and the browser, percent-decoded secret parameter names, honest level-2 and stub texts, wallet field removed, no duplicate-content oracle
- 2026-10-20 S1 submit data (ADR-0038, branch submit-onboarding, not merged): `onyx-intake` crate (answers -> level route, refusals, holds, caps, no contact fields, secret-in-URL refusal, pull allowlist `api.fiscaldata.treasury.gov`, blocked FRED), `/v1/submit` check/record/status with journal, budgets and an SSRF-safe pull (public addresses only, no redirects, 256 KiB, 10 s), website `#/submit` four-step walk-through with the Treasury example and a keyed FRED walk-through (key stays in the browser, stub proof labelled not real, held), offline JSON download in every shipped build, shared vectors run by Rust and the browser; S2 research note; THREATS SUB-1..12
- 2026-10-20 deps: RUSTSEC-2026-0378/0379 (wasmtime 49.0.3 -> 49.0.4), RUSTSEC-2026-0381 (webpki-roots 1.0.9 -> 1.0.10), RUSTSEC-2026-0382 (rustls-webpki 0.103.15 -> 0.103.16) fixed by bump, no ignore; `recipes/out/` byte-identical, golden tests pass unblessed, check-all green (branch advisories-1020)
- 2026-10-19 P4a Ethereum adapter and on-chain data (ADR-0044, branch eth-adapter): JSON-RPC answers re-encoded and checked against block hash, transaction hashes and both trie roots; recipe `eth-chain-stats` re-checks them and the EIP-1559 base fee; 300 finalized blocks (publicnode, cross-checked on Flashbots), Python recount agrees on every field; Data page "Ethereum on chain" in every build with a browser rerun; P4 split into P4a done, P4b entities, P4c stablecoins; X1 absorbed
- 2026-10-18 P7 launch walk (branch launch-walk, docs only): 27 gates re-read with evidence, 6 re-run; LC-3 decided by the loop, LC-16 to PARTIAL (L17), LC-7 leftovers to L18, redeploy runbook `ONYX_RIGHTS` fix, ranked Needs the project owner table (checklist section 6); 10 verified, 17 open, 11 blockers; follow-up P7a
- 2026-10-18 D22 client check on dated chains at span cost (ADR-0034 Amendment 3): `ReservePin::from_dated_scan` (dated set, history, flows file by trusted hash), window hash and index per link, membership-change deltas, every link's tip and the pin on the client's headers (6 confirmations), `derive_entity_span` from the landing or an entity's exit/entry (shared with the browser), `MAX_LINKS` removed for client `Limits` (budget 4,096 rows = 10.24 USDC; a landing answer costs 1 row); THREATS D3-R2-2/3 and RES-C6 (headers) fixed, D22-1..5; fresh review D22-R1: no wrong balance, 1 Medium + 2 Low fixed, 1 Low documented; break-the-code 7 mutations, 6 caught, 1 equivalent; fresh review D22-R2: 1 Medium (trusted flows file not checked against the finalized flows: a wrong balance accepted) + 2 Low fixed, 8 mutations caught
- 2026-10-17 D1 reserve ranges and depth (ADR-0037): `onyx_labels::dated` (format 2: `from`/`to` per membership in Merkle leaves, windows, `check_successor`/`check_history`, shared vector), serving check with membership epochs, rebase at each change from the scan and a flows file that must match the finalized flows, `MIN_CONFIRMATIONS` 6 on a proof-of-work header chain (own node to 971,199, 971,200-971,207 from two explorers) for the scan, every shard tip (stale branches before the landing refused), flows files and second scans; gate `headers`, `datedLabels`, `membershipFlows`, `datedHistory`; served v2 chain unchanged; THREATS RES-2/RES-4 closed for the mechanism, RES-13..RES-19; fresh review D1-R1 (2 Medium, 5 Low, 3 Info) fixed or pinned; follow-ups D10-D12
- 2026-10-17 D3 client-side reserve serving check (ADR-0034 Amendment 2): a client re-runs the check with no operator input trusted: its own scan pin (`ReservePin::from_scan`), the entity's row in every link fetched through `/v1/proofs` and verified on its own node (`ClientNode::verify_row`), `reserve::client_check` derives the start base from the scan and the flows and refuses any row on another base, wrong label set, recipe or landing block; free hint `/v1/served/{metric}` (no row; 403 in fallback); the answer must equal a proven link; the browser derives the base on the week export; THREATS RES-C1..C8 (operator skips the check, forged base, swapped scan, wrong label set version tested); fresh review D3-R1: no base forgery; 2 Medium (same work in two rounds broke every client check, a padded list cost paid fetches) and 3 Low (stale answer, untrusted scan hash, free-endpoint budget) fixed; break-the-code 9 mutations, each caught; follow-ups D20, D21
- 2026-10-17 C51 time-locked sealed posts wired (ADR-0027 amendment 14, branch tlock-wire, not deployed): OnyxRegistry cap 1,294, one-way `requireTimeLock`, `drawTimeLocked` snapshot (keyed draws only once on; pre-mode draws never recorded); observer seals with `QuicknetTimeLock`; coordinator opens only after the lock round's signature verifies against the pinned key (waits otherwise); book v7, served `sealed.timeLock`; Rust and web clients re-encrypt openings and ask `drawTimeLocked`; anvil rehearsal with a fixture beacon (`scripts/level2-timelocked.sh`); THREATS C51-*, review C51-R1; follow-ups C80-C82
- 2026-10-17 C11 sealed reveals wired (ADR-0027 amendment 11): observer posts `postSealedReveal`, live coordinator reads every drawn slot past reveal close (`open_posted`), opens or dismisses, checks `revealLogComplete` before keeping the read, records every round (`recordAttempt`, `recordObservation`); `RevealKey` of its own; book v6 keeps dismissals; served `sealed`; Rust client asks `drawRevealKey`, `revealLogComplete`, each post and `attemptMatches` with its own drawn set, window and round id (sealed returns a required argument); web mirror; anvil rehearsal `scripts/level2-sealed.sh`; THREATS C11-1..14; fresh review C11-R1: keyed draws sealed only (ex-post inclusion), `clearRevealKey`, value-round post check tested; the node-trust finding to C31/C34; follow-ups C30-C34; not deployed
- 2026-10-17 C4 sealed reveals (ADR-0027 amendment 10): reveals posted on chain encrypted to the draw's reveal key (ECIES on secp256k1 with Keccak, ephemeral-key proof of knowledge, observer signature), the post is the receipt; `revealLogComplete` refuses a round that drops a post (REVLOG-1 closed for sealing observers), garbage dismissed with a DLEQ proof anyone checks, `recordAttempt` pins non-value rounds before serving (REVLOG-5 reduced); disputes and the drand time-lock designed (C12, C13); wiring C11; fresh review C4-R1: 1 High fixed (ECDSA key signature was no proof of knowledge, a decryption oracle; now a Schnorr proof checked with one `ecrecover`), C4-R1-2/3 fixed or moved to C11; second review C4-R2: Schnorr fix holds, 1 Medium fixed (ciphertext length showed the answer's length; padded to one length per feed); registry 13.6 -> 19.4 KB, not deployed
- 2026-10-17 C1 level 5 track record (ADR-0036, `crates/onyx-track`): per (source, metric), each statement judged only against a later level 1/3 answer for the same fact (never other sources); outcomes agreed, disagreed, equivocated (a confident miss), late, void, contested, unresolved; binary Brier on the stated confidence; "first to agree" and "one of N sources" against copiers and Sybils; entries and records in Merkle trees, commitments chained, client recount of a record's whole history and an append-only check; a check-boundaries fence keeps records away from level and market-policy code; serving and intake split into C5-C7, label-score TODOs moved to C8; fresh review TR-R1: 3 merge blockers fixed (appended resolution erased a miss, params rewrote outcomes, junk at 0% confidence scored a perfect Brier), resolver identity and strongest-level-only resolutions, committed metric summaries
- 2026-10-17 B4 level-2 limits on the paid API: every `/v1/observations` answer, its 402 body and x402 offer (`extensions.limits.info.limits`, x402 v2 §5.1.2 `{info, schema}`), and every `/attempts` answer carry `limits` (REVLOG-1 always applies; REVLOG-5 applies only to sealed rounds); shard rows carry none; the client accepts an older server's answer without it and refuses a misstated list; 5 tests (shown failing on code without the offer limits or with REVLOG-5 never applying); review: offer extension reshaped to the spec's `{info, schema}`, echoed payment accepted, REVLOG-1 text names the owner reopen, `limits` stated as unauthenticated, client break announced in docs/ops/upgrades.md
- 2026-10-16 L14 rights fallback prepared (ADR-0035): `ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh` ships no label-derived total (no company flows, reserves, per-entity history, home reserves card, OKX run figures; banner and held-back panels say why; 5 in-browser checks with a pool check) and a guard refuses the build on any of them in any encoding; onyx-node `ONYX_RIGHTS=fallback` answers every label-derived shard 403 `label_rights_withheld` before any 402 (no OKX reserve sale); default `cleared`, nothing changed today; verified by building both public variants and running onyx-node both ways on a local chain (docs/audit/2026-10-16-rights-fallback.md); LC-1 lists both launch paths
- 2026-10-16 L15 launch runbooks: stop serving a shard, rotate the reporter, redeploy with a new ledger id (G5 playbook), each rehearsed with its rollback on a local anvil fork + onyx-node (shard 404 then back; old reporter key refused at start and on chain, paid request settled by the new key; old roots `verifyRow` true at the old address); release step `scripts/check-source-terms.py` (90 days; exits 1 on Binance's unread terms); LC-19 VERIFIED, LC-27 PARTIAL; review: rotation re-run step by step on a fresh fork, RB-1..6 fixed (redeploy verifyRow args and old-key rollback, catalog edit, health-watch calls, a `via` row covering itself)
- 2026-10-16 L13 history secret scan: gitleaks + trufflehog + repo patterns + key derivation over all refs, unreachable commits and the tree, rescanned after merging main: 0 real secrets (gitleaks 9 generic-api-key: 8 public hashes, 1 comment; trufflehog 3 unverified (USDC contract address, a test URL twice); 1 public test key by derivation; 1 mistyped anvil key); deployer key never committed; review: merge commits now scanned and gitleaks required (SCAN-4/5); anvil allowlist typo fixed (SCAN-1); `check-secrets.sh --history` (patterns + gitleaks); `.gitleaks.toml`; lead step: `.env` values vs history, counts only (SCAN-3, LC-25 PARTIAL)
- 2026-10-15 W4 website speed: home first visit 2,090 -> 217 KiB (public) and 3,236 -> 219 KiB (private) by loading the verifier and recipe input only when a checked value is on screen or Verify is pressed (API 2,029 -> 192 KiB); slab shader in a worker, one shared number formatter, week charts drawn once: Data Lighthouse TBT median 1,961 -> 1,312 ms (public), 1,865 -> 622 ms (private); wasm-opt measured and not kept (gzip +1.3 %)
- 2026-10-15 L12 health watch: `scripts/health-watch.py` (node `/v1/health`, RPC chain/head/finality, deployer ETH, OnyxRevenue USDC >= allocated - paidOut, public site), thresholds in deploy/health-watch/thresholds.json, de-duplicated alerts with a `--notify-cmd` hook, systemd user timer; 68 tests in check-all (review L12-R1: state validation, required targets, a 90 s run deadline, secret-free hook env, hardened unit); node down, RPC stall, wrong chain and preview site each alerted on a local anvil + node (docs/audit/2026-10-15-health-watch.md); LC-17 PARTIAL (no public node, no channel)
- 2026-10-15 L7 fresh-machine install done: shipped systemd unit on a fresh Ubuntu 26.04 (systemd 259 PID 1) by deployment.md alone: start, health, row 200, restart, clean stop exit 0, `systemd-analyze security` 1.2 OK; 8 doc gaps fixed; L7-SD-1 server refused systemd credentials (0440), fixed; BuildKit builds 282 s / 196 s (49.8 / 53.8 MB), docker-smoke passes on them; `# syntax=` pinned (L7-BK-1)
- 2026-10-15 L11 TLS front: Caddy config (auto ACME, redirect, HSTS, API routes only, 1 KiB body / 32 KiB header caps) proven locally with `tls internal` (TLS 1.3, 308, 413, rate limit per forwarded client); onyx-node refuses a public plain-HTTP listen without an explicit opt-in; 3 proxy gaps fixed (TLS-1, -3, -4) and 3 review findings (TLS-7 Docker trusted proxy, TLS-8 error headers + HTTP/1.1 only); docker-smoke passes; launch gate LC-7 now PARTIAL (needs the domain)
- 2026-10-15 W10 website visual polish: one token set (type scale, 4 px spacing grid, radii, shadows, colours) used by every page (distinct computed font sizes 24 -> 15, spacing 30 -> 12, radii 14 -> 6, colours off-token 10 -> 0), hover and press states on every control, one fold marker and placeholder style; W11: tooltips kept inside charts, reserve down week ▼ in --btc, public-build scan finds packed addresses
- 2026-10-15 E8 (part) mutation follow-ups: 42 coordinator unit tests on real rounds; kill rate validate.rs 30.4% -> 97.6%, round.rs 2.0% -> 91.8%, draw invariant 60.0% -> 92.7%; no bug found
- 2026-10-15 W6 website chart consistency: one chart frame, tooltip, tick format, legend and colour rule on Data, Reserves and Run (tooltip styles 4 -> 1, pools keyboard chart, Run step tables); W8 first fix: Treasury/SEC values tagged "real value · test round", not "demo data"
- 2026-10-15 L10 launch checklist (docs/ops/launch-checklist.md): 27 go/no-go gates (8 verified, 19 open, 12 of them blockers), keys, monitoring, incident response, rollback, 7 the project owner decisions; check-launch.sh in check-docs; public guard refuses e-mails and profile links (L9A-8); solc SOL-2026-7 recorded (DEP-3); L12-L15 added
- 2026-10-15 L6 soak and chaos: 133.5 + 150 min runs, 4 bugs fixed (SOAK-1..4), money invariants held
- 2026-10-15 L9 threat sweep: every Medium+ row fixed, accepted with a reopen condition, or deferred to a named open item (7 High / 36 Medium still open, partial or deferred); 19 new rows (0 Critical/High); Phase H for mainnet; check-threats (verified test citations, legacy baseline 0) and check-secrets in check-all
- 2026-10-15 W5 website home page: one-sentence what-it-is, 3 proof cards (Run, Reserves, in-browser checks), roots named as Base Sepolia (testnet), testnet note above the fold; run proof moved from 4,434 px to 758 px down the page; +1.9 KiB first load
- 2026-10-14 W3 website accessibility (WCAG 2.2 AA): axe 0 violations on every page (was 18 public / 48 private nodes), charts keyboard + data tables, pool fill 2.78 -> 3.81:1, motion pause; dark only by decision
- 2026-10-14 W2 website phone layout: menu button, skip link, 44 px targets, Run tx lists folded (Tab stops 247 -> 46)
- 2026-10-14 W1 website baseline audit (docs/audit/website-baseline.md); W7/A2b Run page shows the 2026-10-14 run
- 2026-10-14 Phase W website plan created (docs/website-plan.md)

- 2026-10-14 A2 live: reserve round + paid reserve sale on Base Sepolia, new budget contracts (198 txs)

- 2026-10-14 F1 data-source catalogue (docs/ops/data-sources.md: 15 current, 18 candidates, top 5 ranked)
- 2026-10-14 rustls 0.23.46 (4 advisories published that day; the L1 gate blocked them)
- 2026-10-14 L3 fuzzing merged (18 targets, 706M execs, FUZZ-1 fixed)
- 2026-10-14 L2 contract audit merged (per-shard budgets, bond covers fees, invariants)
- 2026-10-13 A3 Docker images smoke-tested; A2 reserve round in fork/testnet driver
- 2026-10-13 L8 data/privacy audit; L1 dependency audit; A1b Run page shows both runs
- 2026-10-13 A1 fresh Base Sepolia run passed first try (156 txs)
- 2026-10-12/13 see "Done" above

## Phase E: operations and quality (every tick may take one)

- [x] E13 Remote build host (2026-10-19): `scripts/remote-check.sh <worktree> [ONYX_WEB_PUBLIC=1]`
      runs check-all on the build machine (approved 2026-10-18) so the gate leaves the dev box: secret
      refusals and the exact `git ls-files` list here, then one capped systemd scope there (48 GB,
      no swap, 48 CPUs, nice 19, idle I/O) inside a Landlock file-system sandbox, load and disk
      guards, a disk watchdog, `--clean <branch>`; tools
      and fixture blocks copied from the dev box, no downloads; the Docker smoke stays local
      (docs/ops/remote-check.md, THREATS REMOTE-1..10)
- [x] E12 Load-proof timing tests (2026-10-18, done 2026-10-19, branch `load-proof-tests`): `onyx-api` test
      `a_settle_slower_than_the_request_is_kept_for_the_retry` failed once (504 instead of 200)
      inside a full check-all at load 60 and passed 3 of 3 alone in about 0.5 s; it and any other
      wall-clock test get a controlled clock or a margin tied to the measured cost, never a
      looser assertion; same audit for the web tests' fixed waits. Done: tokio paused time for
      onyx-api's router, reporter, self-settle, sender, intent and book tests; signals instead of
      sleeps in onyx-node's serve, shutdown and settle-path tests; CPU time for two cost tests; a
      virtual clock and transport in health-watch-tests.py; condition waits with wait.mjs caps
      (`capWaits`, `WAIT_CAP`) in the browser tests; rule in CLAUDE.md Conventions (TRACKER
      2026-10-19)
- [ ] E1 Open Low/Info rows in THREATS.md: fix the cheapest open one per tick
- [ ] E2 Dependency audit (`cargo audit`/`cargo deny` if installed; no paid tools)
- [ ] E3 Performance: re-run benchmarks in docs/ops/benchmarks.md, record drift
- [ ] E4 Website: accessibility and phone-width pass, browser test count kept growing
- [ ] E5 Docs: keep README/STATUS/PLAN consistent with what main actually does
- [ ] E6 Fuzzing follow-ups (from L3 via L9): one AddressSanitizer run of the signature targets
      (FUZZ-2), targets for the recipe host and BIP-322 proofs (FUZZ-3), a router fixture with a
      level-2 feed, gate, contributors and journals (FUZZ-4)
- [ ] E7 Cross-language vector for a rerun with 2+ revealed roots: Rust ledger vs contract payouts
      (L2-R2-4)
- [ ] E8 Mutation follow-ups (from L4 via L9): unit tests for the `validate.rs` survivors (MUT-1), a
      direct `release_draw(Published)` test per branch (MUT-2), one cargo-mutants run each of
      `dispute.rs` and `self_settle.rs` first, then the rest of MUT-5 (memory-capped, off-peak)
      2026-10-15: MUT-1 unit tests done (`validate.rs` 30.4% -> 97.6%, `round.rs` 2.0% -> 91.8%) and
      MUT-2 direct tests done (60.0% -> 92.7%). Open: the 20,000-case `revlog_r8` pass (MUT-2) and
      the `dispute.rs`, `self_settle.rs` and other MUT-5 runs
- [ ] E9 Soak follow-ups (from L6): re-soak main after the L6 merge (SOAK-7), a slow-node chaos run
      with a settle fee bump (M8-L57-N3); both wait for a quieter machine. 2026-10-28: main
      `ccbe553` re-soaked for 150 min, pass (launch checklist LC-13); left: the slow-node run.
      2026-10-16 done: SOAK-5 (`404 no_shard_at_as_of`, naming the oldest `as_of`, before any 402;
      test `an_as_of_older_than_every_shard_has_its_own_code`); the `a11y charts` flake (1 of 10
      charts counted): pages are now measured once the route has drawn, nothing is loading or
      checking and the DOM has been still 500 ms (`apps/web/tests/wait.mjs`), not "no skeleton
      yet" plus 800 ms, which passed while the previous route was still on screen
- [ ] E10 Public-build gate (from L10). 2026-10-16 done: `ONYX_WEB_PUBLIC=1 scripts/check-all.sh`
      builds and tests the public site with `ONYX_RIGHTS=cleared`, then `fallback`, then the preview
      last; the e2e waits wait on page state (the tally wait matched the idle "0 passed · 0 failed"
      tally, so overview, Home and forged-data checks read before the checks ran; clicks waited for
      the page to stop moving and are confirmed by the state they set; held data instead of 5-6 s
      delays); `the_permit_lasts_until_the_body_is_written_or_the_send_timeout` and
      `settles_never_queue_for_a_facilitator_slot` (found failing 2 of 10 suite runs) run on a
      paused tokio clock. Repeat runs: docs/ops/benchmarks.md "Test robustness under load".
      Open: one public e2e run of 8 found a fresh page whose route had not drawn 120 s after load
      (not reproduced in 75 fresh-page probes or 7 more runs; the wait now reports what the page showed,
      and since the review its errors, console errors and failed or open requests).
      Review 2026-10-17: `settles_never_queue_for_a_facilitator_slot` passed with the slot dropped
      before the settle (on main too: busy payers spent their nonces at once); payers now retry, a
      sampler counts settle tasks, all 48 payments must settle. SOAK-5 admits the oldest shard first
      (a fallback server keeps its 403 at `as_of=99`). Home's focus must land while the checks run.
      `build-web.sh` refuses a second build in the same worktree. `store_load_is_not_quadratic_in_rounds`
      (flaked under load; also passed with the old O(n^2) scan put back: 41.0 s against a 42.6 s bar)
      now adds 2,000 rounds to a 40k store and to an empty one in alternating chunks, on CPU time:
      1.00x with the index, 5.2x with the scan (bar 1.5x)

## Needs the project owner (the loop never does these)

- A separate Unix user (or a container) on the build machine for Onyx builds, so branch code cannot reach
  the production work in the shared user's home (THREATS REMOTE-4, docs/ops/remote-check.md)
- Before any public push or mirror: run the counts-only `.env` cross-check in
  docs/audit/2026-10-16-history-secret-scan.md (THREATS SCAN-3; the loop never reads `.env`) and
  decide rewrite or keep private (LC-26, L8-H1..H3)
- GitHub Actions billing (CI is off; the loop runs every check locally instead)
- Public launch, ranked with time and what each unblocks in docs/ops/launch-checklist.md
  section 6 "Needs the project owner" (P7, 2026-10-18; P7 review: path B needs no choice, ADR-0035): 1. LC-1
  path A: write to OKX/Bitwise (replies take weeks; not a blocker); 2. LC-8 host and domain; 3. LC-2 removal mailbox; 4. LC-18 on-call and alert
  channel; 5. LC-27 a browser read of Binance's terms, only if the loop's headless read fails; 6. LC-6
  publish the fallback public build (also passes LC-1 by path B);
  7. LC-24 CI billing; 8. before going public, LC-25/LC-26 (`.env` cross-check, history rewrite or
  private, incl. the OSS-18 author e-mail). The removal proof rule (L9A-10, LC-3) was decided by
  the loop on 2026-10-18; he may change it
- A second Bitcoin node, if one exists, for the reserve cross-check (D2)
- An outside security audit and any move off testnets
- S2 FRED experiment: a FRED API key of his own (FRED needs an account), used only on his machine,
  never committed or sent to Onyx; the run still publishes nothing (FRED is blocked, N10)
- Data-source rights (docs/ops/data-sources.md section 4): label-set sources OKX, BITB, BitMEX,
  Binance (L8-5; new: OKX ToS 8.1 and BITB's examiner terms both restrict distribution); cbBTC
  (D4); Chainlink PoR permission (POR-1); new PoR lists Bybit, Deribit, Bitfinex and Bitwise ETHW
  (F7); BEA API signup (name and e-mail); ECB rates' "free at the ECB" notice on sold rows;
  Ethereum/Base mainnet node or RPC for F3 if paid
