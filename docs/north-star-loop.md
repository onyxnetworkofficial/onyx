# North Star work loop (started 2026-10-27)

Owner 2026-10-27: "Do the ranked items setup a Md to track all these changes". Later the same day:
"Setup a loop and Md tracker to get all of the other things done then!" (NL6-NL13 added). The
partners' meeting is Thursday 2026-10-29, so items they can click through go first (NL6, NL5);
the meeting brief is NL7. Work order: NL6, NL5, NL7, NL8, NL3, NL4, NL9, NL12, NL13, NL19, NL21,
NL16, NL17, NL18, NL20, NL10, NL11; NL14 and NL15 were added from the NL6 review and its settle
date; NL21 (agent connect) from the owner on 2026-10-27, ahead of NL16 with NL19.
NL16-NL20 were added for the strongest first launch (owner 2026-10-27: "you are building the best
first launch right now"). Two lanes run at once (owner:
"Is there nothing we can do in parallel?"): lane A builds in this worktree; lane B (branch
`nl-lane-b`) does work that needs no Rust build (scripts, configs, live read-only checks,
research, specs, reviews) and is merged after lane A's next check-all. Deploys wait until every
item is done or parked (owner 2026-10-27), then one deploy of the API and the site.
Coding items after the hosted API went live (NS17). One item at a time: two builds at once would
take the disk below the 150 GB floor. Each item ends with `scripts/check-all.sh` (with
`ONYX_WEB=1` when web files change) passing, a review by hand, THREATS rows for valid findings,
a log line in docs/NORTH-STAR.md or ROADMAP.md, a commit and a push to private `main`. Nothing
here makes the repo public, deploys to mainnet or spends money. A change to the live server
(`scripts/deploy-site.sh`, `scripts/deploy-api.sh`, the firewall) waits for the owner's OK for
that action.

Status words: `todo`, `doing`, `done`, `parked` (with the reason).

| # | Item | Source | Status | Result |
|---|---|---|---|---|
| NL0 | Hosted API node live behind `/v1/` on the site's server | docs/NORTH-STAR.md NS17 | done 2026-10-27 | commit `ce0bb04`; `scripts/deploy-api.sh` run with the owner's OK: `/v1/health` 200, 2 servable shards, payments off, Caddy admin closed, live daily check 11/11 |
| NL1 | Level-2 daily rounds (earthquakes, weather) on Base Sepolia, served by the hosted node | ROADMAP B6 | done 2026-10-27 | commit `fba600b`; live on the hosted API and the site (owner's OK); client check passed against the hosted API and the chain; USGS 2026-07-19 = 5 (round `0x1dba7daf…`, 15 txs) and NCEI 2026-09-27 = 28.3 °C (round `0xbdb43692…`, 14 txs) finalized on Base Sepolia, 0.000024 test ETH; a local node with the export's settings served both and the client check passed off and on chain; `scripts/deploy-api.sh` now ships the stores |
| NL2 | The proof ladder in the product: each value shows its level and evidence; the level history is public | docs/NORTH-STAR.md NS5 | done 2026-10-27 (slice 1; slice 2 is ROADMAP C64) | ADR-0048; `onyx_core::ladder`, `onyx_api::ladder`; `proofLevel` and `ladder` on the value answers; client refuses a forged level, an unchecked rise and a fall; badges and level history on the data page (sealed in a public build); THREATS NS5-1..7; not live until the owner OKs the API and site deploys |
| NL3 | Estimates in their own namespace and level, method and error shown, never in checked totals | docs/NORTH-STAR.md NS14 | done 2026-10-28 (slice 1; slice 2 is ROADMAP C67) | ADR-0050; `onyx_core::estimate`; `GET /v1/estimates/{metric}` (level `E`, `proofLevel` and `ladder` null, method and error always); refused by shards, level-2 feeds, recipe sources, label sets and track resolutions; the data page's estimate card made in the browser from checked days (e2e `ns14.mjs`); THREATS EST-1..7; not deployed |
| NL4 | Track records served: source and forecaster scores on the API and the site | docs/NORTH-STAR.md NS16 | done 2026-10-28 (served live since release `12cb627`; first statement opens after 2026-10-30 00:00 UTC; grades from December) | ADR-0036 amendment 7; `onyx_track::daily` (daily rule, `resolution_of` only from rounds recorded at level 2); a publication under the daily rules is served and checks (`a_live_publication_serves_the_daily_records`); THREATS TRS-1..5; ROADMAP C66 open |
| NL5 | Hosted API front leftovers: connection cap on 443; operator balance in the health watch | ROADMAP L19, THREATS NS17-4 | done 2026-10-28 (cap live since release `2f01280`: `nft` table `inet onyx_conn_cap` loaded on the server) | lane B; `balances.operator_eth` (0.00002 ETH min, skipped while payments are off); block limits fixed for Base Sepolia's 0.2 s blocks (a live watch run alerted `rpc.finalized_lag` before, passes after); `deploy/nftables/onyx-conn-cap.nft` (32 per IPv4 address or IPv6 /64), `deploy/systemd/onyx-conn-cap.service`, `scripts/conn-cap-test.sh` passed; THREATS NL5-1..5 |
| NL6 | Markets live on testnet: stakes, panel and futures on Base Sepolia; the Markets page reads them | docs/NORTH-STAR.md NS11 | done 2026-10-27 (Markets page live since release `2f01280`; market 1 settles in NL15) | ADR-0042 amendment 2; `OnyxObservedFacts`, `DeployMarkets.s.sol`, `onyx_level2::markets`, `scripts/markets-sepolia.sh`; the Markets page's live section (`LiveMarkets.svelte`, `verify/src/markets_view.rs`) reads the chain through the reader's RPC; deployed on Base Sepolia (8 txs, 0.000351 test ETH; addresses in docs/ops/deployment.md); market 1 open with 4 test bets; THREATS NL6-1..9 and NL6-16..19 (NL6-10..15: the parallel review on branch nl-lane-b) |
| NL7 | One-page brief for the Thursday 2026-10-29 meeting with the partners | owner 2026-10-27 | done 2026-10-28 | one-page PDF sent to the owner; kept outside the repo (partner material) |
| NL8 | Validate on first paid query: free proposals; observers run when a buyer or the submitter pays | docs/NORTH-STAR.md NS10 | done 2026-10-27 (slice 1; the rest is ROADMAP S9) | ADR-0049; `onyx_api::observe`; prepaid holds (`PrepaidBook::hold`, `PrepaidState::hold`, `commit_hold`, `release_hold`); `/v1/tickets/{ticket}`; `ONYX_OBSERVE` (`onyx_node::observe`, the operator's round command, killed at the deadline); THREATS NL8-1..5 fixed, NL8-6..12; not deployed |
| NL9 | Bounty board: coarse categories of wanted data, never exact queries | docs/NORTH-STAR.md NS15 | done 2026-10-28 (slice 1; S10 keeps the per-client budget and the deposit floor) | ADR-0051; `onyx_intake::bounty`, `onyx_api::wants`; THREATS NL9-1..7; e2e ns15 |
| NL10 | Storage tiers: hot, warm, cold (root only, recompute on request) | docs/NORTH-STAR.md NS13 | done 2026-10-28 (slice 1; the node's use of it is ROADMAP S11) | ADR-0052; `onyx_coordinator::tiers` (`TieredStore`, `RecomputeQueue`); 9 tests: each move, a bad reference and a missing kept set stay warm, a document never goes cold, a recompute stored only on a chain-root match, unpaid requests start nothing, caps and overdue counts, tier file checked at load; THREATS NL10-1..8 |
| NL11 | Browser TLS prover: a contributor proves a keyed API answer in the browser, the key never leaving the page | docs/NORTH-STAR.md NS6 | parked 2026-10-28: no sound browser slice fits one loop. Upstream (checked 2026-10-28 on GitHub): `tlsn` latest is `v0.1.0-alpha.15` (pre-release, 2026-05-21; no newer tag), `tlsn-js` is archived, and the browser path is `tlsn-extension` (MIT, alpha.15): a Chrome extension the contributor installs plus a Node verifier server; an in-page alpha.15 WASM build is unproven (S2b never built it) and needs cross-origin isolation. Unblock: (1) build alpha.15's WASM and prove the Treasury target from a page against a local verifier, measured; or accept the extension route; (2) a keyed source that passes the rights review (TLS 1.2, account-independent answer, redistribution allowed); (3) an independent notary operator (NS4) before the level can rise above 4. THREATS NL11-1..4 stay TODO S2 | docs/spec/browser-tls-prover.md (draft); RESEARCH.md "NL11 upstream check" |
| NL12 | Launch checklist loop gates against the live node: refresh LC-7/8/9 for the hosted node; LC-17 health watch on it; LC-11 fresh run on the launch commit; LC-16 contract gates; LC-21 fuzz smoke; LC-13 soak | docs/ops/launch-checklist.md | done 2026-10-28 (LC-11 waits for the owner: a fresh live run changes every address and spends test ETH; LC-16 and LC-21 close on the chosen launch commit) | lane B: LC-8 VERIFIED (decided and running); LC-7, LC-9, LC-17 PARTIAL with live evidence (Let's Encrypt certificate, 308 to HTTPS, HSTS, HTTP/1.1 only, TLS 1.0/1.1 refused with alert 70; index health summary, trusted proxy and public RPC; the watch passes against the hosted node); `scripts/check-launch.sh`: 27 gates, 11 verified; runbook for lane A's four gates in docs/ops/launch-checklist.md section 8; LC-16 run by lane B 2026-10-27 (all gates pass, Slither 0 security findings, THREATS LC16-1..2), closes on the launch commit after NL14; lane A 2026-10-28 on main `ccbe553`: LC-21 fuzz smoke passed (19 targets, no crash), LC-16 forge gates, layout, gas snapshot and audit-depth invariants (24, 0 failures) pass and the Slither re-run has 0 security findings, LC-13 VERIFIED (150-min soak, pass, 0 violations) |
| NL13 | Proof ladder slice 2: cross-lane rises, contradictions, a ladder on level-3 answers, `onyx-check` row levels | ROADMAP C64 | done 2026-10-28 (a node route serving the cross-lane form moved to C68: no testnet fact has two lanes) | ADR-0048 amendment 2; `canonical_value`; `v: 2` form with per-lane checks and contradictions; superseded steps no fall; `/v1/signed` ladder checked in the browser; `onyx-check --registry`/`--ladder-state`, `level2-check --ladder-state`; site remembers ladders, badges say stated; THREATS NS5-1, 5, 6, 9..19 |
| NL14 | Markets contract fixes from the lane B review: `mayBet` refuses the coordinator and futures owner (NL6-12), `open` refuses a close after the source publishes (NL6-10), `notBeforeBlock` from `observableAt` (NL6-11), bet checked before any gas grant (NL6-14); drop the dead constant `OnyxFutures.PPM` (LC-16 Slither, docs/audit/2026-10-27-lc16-slither.md); redeploy on Base Sepolia | THREATS NL6-10..14, ROADMAP S6, LC-16 | done 2026-10-28 (Solidity in lane C, Rust in lane A; the live reobserve, panel and stakes still run the pre-LC16 rules until a redeploy) | `OnyxObservedFacts.list` takes `closeBy` and names insiders (`IOnyxFactGuard`); the guarded `OnyxFutures` refuses a later close, an unlisted fact and insider bets; `OnyxFutures.PPM` dropped; second deployment on Base Sepolia (facts `0x8d10…bb76`, futures `0x07e8…4a65`), 0.000027 test ETH; ADR-0042 amendment 3 |
| NL16 | Join kit: one-command onboarding for outside observer/checker operators and suppliers | NORTH-STAR NS4 (pairs with it) | done 2026-10-28 (observers; suppliers use NL21's propose tool; bonds wait for NS12) | docs/join.md; `scripts/join.sh` (key, address, observe), `scripts/join-build.sh`, `scripts/join-rehearsal.sh`; coordinator `--announce`, observer `--announcement`; rehearsal passed in a fresh `ubuntu:26.04` container on a local anvil: the outside observer was drawn, committed and revealed sealed on chain, Agreed (5 of 5); THREATS NL16-1..8 |
| NL17 | More strong datasets: 3-5 more redistributable public sources that fit markets and insurance | NORTH-STAR NS3 follow-up; draft docs/spec/more-datasets.md | done 2026-10-28 | commits `96f68db`, `ac8f2c8`; Denver precipitation, Central Park high and precipitation, M2.5+ near Los Angeles; live rounds on Base Sepolia; served since release `2f01280` (ROADMAP S13 keeps BLS CPI and the revision lags) |
| NL18 | Parametric insurance demo: a test contract pays out when an observed weather or earthquake value crosses a threshold | ADR-0042 amendment 2 pattern; draft docs/spec/parametric-insurance.md | done 2026-10-28: contract, live products, fork payout demo and the Markets page section (read from the chain, checked by the verifier) | `OnyxParametric` `0xfc58…3639` on Base Sepolia; product 1 "Denver high on 2026-10-31 at most 0.0 °C" (100 tONYX per policy, premium 10) and product 2 "at least 6 M5+ earthquakes worldwide on 2026-10-31" (50, premium 15), buying closes 2026-10-31 00:00 UTC; forge tests: one pays and one expires end to end from a recorded registry value; `scripts/insurance-sepolia.sh demo-fork` shows the same on a Base Sepolia fork with the live products' terms, a non-insider underwriter and buyer (docs/ops/demo-testnet.md); invariants at audit depth (512 x 256); THREATS NL18-1..10 |
| NL19 | Buyer quickstart: a developer or an AI agent goes from zero to a paid, verified fact on testnet in 5 minutes | NORTH-STAR NS8 follow-up | done 2026-10-28: `scripts/quickstart.sh` and docs/agents.md section 5; run against the hosted node: VERIFIED in 252 s with the build. The paid path waits for payments on the hosted node | docs/agents.md; `scripts/agent-connect-test.sh` |
| NL21 | Agent connect: an AI agent connects with one line (local or a hosted MCP URL), asks "does Onyx have X?", proposes a missing dataset and follows it | owner 2026-10-27 ("make it really easy for users to connect their ai agents to contribute to the network") | done 2026-10-28 in the repo (lane C tools; lane A: intake and the connector in `scripts/deploy-api.sh`, Caddy `/mcp`); live at the final deploy | `onyx_find_dataset`, `onyx_propose_dataset`, `onyx_proposal_status`, `apps/agent/src/http.mjs`; THREATS NL21-1..6 |
| NL20 | Launch-day review: hosted API load test, monitoring and alert channel, incident drill, a dry run of the launch steps | launch checklist LC-17, LC-18, LC-22 | parked 2026-10-28: done but what needs the server or the owner (installing the watch timer on the server, the alert channel LC-18, rehearsing the API rollback at the next release) | `scripts/load-test.mjs` (release binary, deployed export, dev box: up to 7,090 requests/s, p99 41 ms at 64 concurrent, no error); drill: restart with state (stop 0.06 s, serving again 1.8 s, a recorded proposal kept), stop serving a level-2 series and roll back; `deploy/systemd/onyx-health-watch.{service,timer}`; `deploy-api.sh` keeps the previous release; launch checklist section 9; THREATS NL20-1..6 |
| NL22 | Fund the test insurance pool so the live products have cover for sale (the Markets page said "No cover for sale" with an empty pool, PT28-2) | owner 2026-10-28 ("continue working on everything") | done 2026-10-28 | `underwrite` in the market tool (`onyx_level2::markets`, `underwrite_precheck`: an insider or observer, or a zero amount, is refused before any gas grant or mint; test `an_underwrite_that_cannot_succeed_gets_no_gas`); rehearsed on an anvil fork of Base Sepolia; live: the `customer` test key put 1,000 tONYX in `OnyxParametric` (block 51534119, pool 1,000, reserved 0); level-2 observers on other keys stay the NL18-2 residual |
| NL15 | Settle market 1: the Treasury round for record date 2026-10-27 after 2026-10-29 00:00 UTC, then `scripts/markets-sepolia.sh market ... settle` (docs/ops/demo-testnet.md) | NL6 | done 2026-10-29 | Treasury round `0x4edf5859…` for record date 2026-10-27 on Base Sepolia (started 00:17 UTC; the first try stopped on a public RPC timeout before the window and the rerun on the same state finished it): value $40,582,274,254,843.57, matching the Treasury's published figure, 5 commits and 5 reveals, 3 Agreed and 2 Missed (those two observers' fetches from the Treasury got no answer, so each honestly sent Missed; the round script's one-Missed check stopped it before the pin, written by hand from `level2-coordinator --pin`), 13 txs, 0.000011255 test ETH. `settle --market 1`: resolved, bucket 2 (above $40.50 T) won; validator-0 paid 122.9 tONYX; the 2.1 tONYX fee (2% of the losing 105) stays owed to the treasury |

### Overnight before the meeting (owner 2026-10-29 02:30 UTC: "setup a loop and write down any tasks you need to do in an MD and work on novel things whilst I sleep")

Work order: NL23, NL24, NL26, NL25, then NL27 from 11:00 UTC. The meeting is 2026-10-29 16:00
(owner's time, 20:00 UTC). Rules for the night: no deploy (a site change waits for the owner's OK
in the morning, NL27); nothing that could break the live release `2b7dbc9`; testnet transactions
only with test keys; every push after a passing check-all. Stop new work at 15:00 UTC.

| # | Item | Source | Status | Result |
|---|---|---|---|---|
| NL23 | Market 2: an open Treasury market on the guarded futures (a record date after the meeting), with test bets from non-insider keys, so the meeting shows an open market next to settled market 1; the site's markets file lists it (live after the morning deploy) | owner 2026-10-29 (novel work overnight); NL14 guarded futures | done 2026-10-29 (live on the page after the owner's OK for the morning deploy, NL27) | fork rehearsal, then live on Base Sepolia: guarded futures market 1 (record date 2026-11-02, edges $40.60 T and $40.70 T, close 2026-11-01 23:00 UTC, observable 2026-11-04, refund by 2026-11-11), 4 test bets (120 tONYX), the deployer refused by `mayBet`; the verifier reads the insurance block's `markets` from the guarded futures and facts (`gcount`, `gmarket:N`, `gfact:N`; tests `guarded_markets_are_read_from_the_guarded_futures`, `the_live_guarded_market_decodes` on the live return, `a_missing_or_foreign_guarded_market_is_refused`); the page shows it as market 2 (one card snippet for both futures); e2e ns11 checks its buckets, pool and that nothing is refused |
| NL24 | Round script: a round that records a value always writes the store pin; the Missed check follows the round's own rule instead of "at most one" (NL15 stopped before its pin with 2 Missed) | NL15 | done 2026-10-29 | `scripts/level2-sepolia.sh`: a round that records a level-2 value writes its store pin before any other check of the run; the Missed check follows the coordinator's strict majority (`onyx_coordinator::observe::quorum`) instead of "at most one". Checked on the NL15 round file (3 of 5 agreed: passes) and made-up ones (2 of 5: refused); the pin block run alone replaces the old pin when a value is recorded and writes nothing without one |
| NL26 | Meeting runner: one command reads the live chain and API and prints the demo in plain words (market 1 result, market 2 open, insurance pool, series and levels), rehearsed against the live release | owner 2026-10-29 ("get ready for the meeting") | done 2026-10-29 | `scripts/meeting-demo.sh` (read-only; docs/ops/demo-testnet.md): run against the live release at 04:40 UTC, exit 0: 7 series at level 2 (debt $40,582,274,254,843.57), market 1 settled with "$40.50T or more" won, market 2 open (120 tONYX, closes 2026-11-01 23:00 UTC), insurance pool 1,000 tONYX with both products for sale; a wrong API URL exits 1 |
| NL25 | A new kind of data: a second Treasury dataset (Daily Treasury Statement operating cash balance, public domain, ROADMAP F5) as a level-2 feed, one live round on Base Sepolia | ROADMAP F5, NORTH-STAR "any data" | done 2026-10-29 (served after the morning API deploy, NL27) | `onyx_source_fiscaldata::series` (`TGA_CLOSE`: Daily Treasury Statement table I, row "Treasury General Account (TGA) Closing Balance", millions of USD, the debt feed's terms, rights record and calendar; its own fetcher tag and template; the debt template and pin unchanged); examples pick it with `ONYX_L2_SERIES=dts.tga_close`; fixture recorded 2026-10-29 (fixtures/PROVENANCE.md); THREATS NL25-1..3. Live round `0xc5868671…` on Base Sepolia for record date 2026-10-27: 1,052,530 (the Treasury's figure), 5 of 5 agreed, level 2; the first try failed `accept_claim` (registered as `Fixed {scale: 0}`, the claim is `Int`; fixed, test `the_registry_kind_is_the_claim_kind`) and was finished on the same draw, where the store pin was written first (NL24) and the script's observer count then failed on the resume (observers that had already revealed exit non-zero; fixed after: such an observer counts as revealed in an earlier run, replayed on these logs: 5 resumed, 2 not drawn, 0 failed). The release binary served it locally at level 2 next to the debt value |
| NL27 | Morning: re-check the live site and chain, update the brief, ask the owner for one deploy OK (market 2 and any NL24-NL26 site change) | owner 2026-10-29 | done 2026-10-29 | `scripts/meeting-demo.sh` against the live release at 10:44 UTC: exit 0, every series at level 2, market 1 settled, market 2 open (120 tONYX), insurance pool 1,000 tONYX with both products for sale; brief v3 sent to the owner (outside the repo); one deploy with the owner's "Ok deploy": API with the TGA store and pin, site release `20261029T125215Z-c7ed039` (market 2 on the Markets page, checked live at 390 and 1280 px); the demo script now lists the TGA series (8 series, exit 0 live) |

Not loop work (waits for people): token economics (NS12, the partners); independent observers on
other machines and operators (NS4); the GitHub organization name and the go to publish (NS7,
PP13); label-set rights letters (LC-1); an on-call person and channel (LC-18); pooled private data
(NS18, after paid demand).

## Done when

- **NL1.** The level-2 coordinator and observer run for any `onyx_level2::Source`, not only
  fiscaldata. One USGS day and one NCEI day finalize on Base Sepolia against the registry in
  docs/ops/deployment.md. Their results stores load in the hosted node's catalog; a client
  verifies each served value against the chain. The site links the round.
- **NL2.** Every API answer carries its proof level and the evidence behind it. A level only
  rises, on new proof, and each change is an event anyone can read. The site shows the level as a
  badge with its history. Tests cover a rise, a refused fall and a forged level.
- **NL3.** An estimate lives under its own namespace and level, with its method and error. No
  checked total ever includes it; a test proves the API refuses to mix them. The site labels it.
- **NL4.** The API serves each source's and forecaster's score with how it was computed. The site
  shows them. Scores come only from settled results; a test rejects a score from an open round.
- **NL5.** The health watch alarms when the operator's test ETH falls below a set level, with
  tests. A per-address connection cap on port 443 is written and tested locally; it goes on the
  server only after the owner's OK.
- **NL6.** Stakes, the panel oracle and futures contracts are deployed to Base Sepolia from the
  testnet key, with addresses in docs/ops/deployment.md. One market opens, takes test stakes and
  settles from an observer-checked value. The Markets page reads the chain (no fixture) and a
  browser check at 1280 and 390 px passes. Goes live only with the owner's OK to deploy the site.
- **NL7.** One page in plain words: what is live (links), what changed since 2026-10-26, what
  waits for the partners (NS12) and for the owner. Sent as a PDF; not in the repo.
- **NL8.** A proposal costs nothing; observers run only after a paid query or a submitter fee.
  Tests: an unpaid proposal starts no round; a paid one does; a fee refund rule is stated.
- **NL9.** The API and the site list wanted-data categories at a coarse grain; a test refuses a
  bounty that names an exact query or subject.
- **NL10.** A shard can move hot to warm to cold; a cold shard keeps only its root and recomputes
  rows on request, and the answer still verifies against the chain root. Tests for each move.
- **NL11.** A browser page proves a keyed API answer with TLSNotary (or the best open prover the
  research names), the key never leaving the page; a forged transcript fails. Research first.
- **NL12.** Each named gate is re-checked against the tree and the live testnet node and marked
  VERIFIED, PARTIAL or OPEN with evidence; `scripts/check-launch.sh` passes.
- **NL13.** The C64 items in ROADMAP are each built with tests, or parked with the reason.
- **NL16.** One command sets up an observer/checker (keys made locally, never sent; the bond and
  registration steps printed for the operator to sign) and one sets up a supplier submission. Done
  when an operator on a clean machine joins a testnet round from the docs alone, rehearsed in a
  container with no access to this repo's keys.
- **NL17.** 3-5 more public sources whose licences allow redistribution and that fit markets and
  insurance, each with a rights record and terms review first (Phase F rule). Done when each runs
  a level-2 round on Base Sepolia and the hosted node serves it (after the one final deploy).
- **NL18.** A test contract holds a policy (threshold, payout, period) and pays automatically when
  the observed value crosses the threshold, settled from registry facts like the futures. Done when
  one policy pays and one does not on a fork, then on Base Sepolia.
- **NL19.** A quickstart doc and script take a developer or an agent from nothing to a paid,
  verified fact on testnet with the agent toolkit (`onyx-check`, the MCP server). Done when a fresh
  container follows the doc and gets a verified answer in about 5 minutes.
- **NL21.** One line connects an MCP client: a local stdio server that needs only `apps/agent`
  and Node, and a hosted Streamable HTTP URL (`/mcp` behind Caddy). `onyx_find_dataset` answers
  "does Onyx have X?" with level, date and how to read it, or "not yet" and how to propose;
  `onyx_propose_dataset` checks first and records only on confirm; `onyx_proposal_status` follows
  the receipt. Done when a fresh container with only `apps/agent` runs docs/agents.md's steps
  against a mock node, the read-only steps pass against the hosted node, and (lane A) the hosted
  node serves `/mcp` and takes proposals behind its guard.
- **NL20.** A load test of the hosted API (published method and numbers), monitoring and an alert
  channel on the live node, an incident drill from the runbooks, and a dry run of the launch steps.
  Done when each passes with evidence; the alert channel waits for the owner (LC-18).

## Log

- 2026-10-27: tracker started. NL0 done (API live). NL1 started in the background.
- 2026-10-27: NL1 rounds done on Base Sepolia (USGS and NCEI, 5 of 5 agreed each, receipts and
  values on chain). The generic live coordinator, observer and client check are in
  `onyx_level2::live`. Parked until the owner OKs the API redeploy (ships the stores) and the site
  deploy (links the rounds). check-all with the web gate passed before the commit.
- 2026-10-27: NL1 done. The owner OKed both deploys; the API (rebuilt `onyx-node` from `fba600b`)
  and the site (release `20261027T153436Z-fba600b`) are live. Both values answer 200 and pass the
  client check against the hosted API and the chain; another day answers 404; the live site
  check passed. NL2 started.
- 2026-10-27: NL2 slice 1 done (ADR-0048). Every value answer states `proofLevel`; row, metric
  and observation answers carry a `ladder` (value key, steps with evidence and chain record). The
  client refuses a forged level, a rise no check ran on, and a fall (`check_extends`); a row's
  level is bound to the client's own `verifyRowLeveled`. The data page shows a level badge on every
  dataset and the checked history of the two Base Sepolia values (sealed in a public build). The
  live client check passed against the hosted API and refused a lifted level. `ONYX_WEB=1
  scripts/check-all.sh` passed; the public fallback build and its e2e passed. Left (ROADMAP C64):
  cross-lane rises, contradictions, a ladder on level-3 answers, `onyx-check` row levels. Needs
  the owner's OK to deploy the API and the site.
- 2026-10-27: owner asked for a loop over everything else; NL6-NL13 added, demo items first for
  the Thursday 2026-10-29 meeting.
- 2026-10-27: NL6 non-web part done. Futures settle from level-2 values the registry recorded:
  `OnyxObservedFacts.prove` rebuilds the result hash from the recorded spec, the round id and the
  number (Rust and Solidity vectors). tONYX, the stakes, the drawn panel, the facts and the futures
  are deployed on Base Sepolia; market 1 on the Treasury's debt for record date 2026-10-27 is open
  (closes 2026-10-28 12:00 UTC, before the Treasury publishes) with four test bets, 125 tONYX. Left:
  the Treasury round after 2026-10-29 00:00 UTC and `settle`; the Markets page reading
  `markets-sepolia.sh status` (apps/web, after the NL2 site deploy).
- 2026-10-27: owner: no deploys until every loop item is done. NL6 done: the Markets page has a
  live section above the practice markets. It reads `OnyxFutures` and `OnyxObservedFacts` through
  the reader's own RPC (a public Base Sepolia RPC by default) only when asked; the Rust verifier
  decodes and checks every answer (another fact, another chain, books that do not add up, a wrong
  winner are refused). Checked live in a browser at 1280 and 390 px against Base Sepolia: market 1
  shows 24% / 60% / 16% of 125 tONYX, trading open. Not deployed.
- 2026-10-27: NL5 built in lane B (branch `nl-lane-b`, no Rust build): the operator's gas in the
  health watch (86 watch tests pass), the watch's block limits fixed after a live run against the
  hosted node showed Base Sepolia at 0.2 s blocks, and the 443 connection cap with a container
  test. Parked for the install (owner's OK at the final deploy).
- 2026-10-27: NL12 lane B part done (read-only against the hosted node): LC-8 verified, LC-7, LC-9
  and LC-17 refreshed with live evidence; the TLS 1.1 refusal of ROADMAP L18 shown on the real
  server. Left for lane A: LC-11 fresh run, LC-13 soak, LC-16 contract gates, LC-21 fuzz smoke.
- 2026-10-27: lane B research and draft designs for NL8, NL10 and NL11 (no code):
  docs/spec/paid-observation.md (hold, ticket, charge on a final round, release on failure),
  docs/spec/storage-tiers.md (hot, warm, cold; cold only for inputs fetchable again byte for byte,
  recompute checked against the chain root), docs/spec/browser-tls-prover.md (tlsn alpha.15 is a
  pre-release, TLS 1.2 only, browser work now in tlsn-extension; Onyx's own notary keeps the level
  at 4); RESEARCH.md section "Lane B". Lane A builds them from these drafts.
- 2026-10-27: lane B batch 2: draft specs for NL3 (docs/spec/estimates.md, ROADMAP C65), NL4
  (track-records-served.md, C66), NL9 (bounty-board.md, S10) and NL13 (ladder-slice-2.md, C64),
  each naming the files lane A changes and the tests for its Done-when; ROADMAP S9 and S11 added
  for NL8 and NL10.
- 2026-10-27: lane B hand review of the NL8, NL10 and NL11 drafts (cheating miner, lazy
  validator, griefing challenger, data thief): 13 findings as THREATS NL8-1..5, NL10-1..4,
  NL11-1..4 (TODO S9, S11, S2), and the specs now state each rule. The local Docker image
  `onyx-empty:conn-cap` was removed (scripts/conn-cap-test.sh rebuilds it when it runs).
- 2026-10-27: lane B hand review of the NL2 code (commit `a6d7adf`): 7 findings, THREATS
  NS5-8..14. Six are TODO C64 with the fix and the test; NS5-8 (the live chain's unsalted
  `ObservationRecorded.result` undoes the public seal of the two Base Sepolia values) needs the
  owner's policy choice. No code changed.
- 2026-10-27: lane B hand review of lane A's NL6 commit `67a8920`: THREATS NL6-10..15 (a close
  after the source publishes passes the check; a recording before `observableAt` bricks a market;
  the coordinator, owner and minter may bet and void by withholding; a minter-held panel; gas
  grants before a bet is checked; open `prove`/`resolve` accepted). Deployed addresses and owners
  confirmed read-only on Base Sepolia. No code changed.
- 2026-10-27: lane B merged into main (conflicts only in THREATS sections and this file; both
  sides kept). NL14 (markets contract fixes from THREATS NL6-10..14) and NL15 (settle market 1
  after 2026-10-29 00:00 UTC) added.
- 2026-10-27: lane B batch 5: synced to the merge `037bc1a`; hand review of the Markets page
  (`0317b3a`): THREATS NL6-20..24 (torn reads across blocks, unbounded RPC bodies, shares shown as
  chances, a test that the page sends only reads, what the RPC operator learns); the NL7 brief
  updated outside the repo; a runbook for LC-16, LC-21, LC-13 and LC-11 with time, disk and test
  ETH (docs/ops/launch-checklist.md section 8).
- 2026-10-27: lane B ran LC-16 (Foundry only, no Rust build): forge fmt, sizes, 674 tests, the
  storage layout, the gas snapshot and 22 invariants at audit depth all pass; Slither 0.11.6 over
  the 12 contracts changed since L2: 63 results, 0 security findings, one dead constant; role review
  THREATS LC16-1..2. Report docs/audit/2026-10-27-lc16-slither.md. LC-16 closes on the launch
  commit after NL14.
- 2026-10-27: NL16-NL20 added for the strongest first launch (owner: "you are building the best
  first launch right now"): join kit, more strong datasets, parametric insurance demo, buyer
  quickstart, launch-day review; ordered after NL13 and before NL10 and NL11.
- 2026-10-27: lane B research for NL17 and NL18 (no code): candidate sources probed read-only
  (NCEI PRCP at Denver and New York Central Park, a regional USGS count, BLS CPI v1 keyless, EIA
  needs a key so parked) in docs/spec/more-datasets.md; a parametric policy contract settled from
  registry facts with the NL6 review's rules in docs/spec/parametric-insurance.md.
- 2026-10-27: lane C (branch `nl-lane-b`, Foundry only; the owner: "Whatever you want to do let's
  do. We have 2 days to get test net done"). NL14 Solidity part: listings carry `closeBy` and name
  insiders; the guarded futures refuse a late close, an unlisted fact and insider bets (THREATS
  NL6-10..12). NL18: `OnyxParametric` with the spec's rules, 14 tests and 2 invariants (audit
  depth passed). Rehearsed on an anvil fork, then deployed on Base Sepolia next to the first
  markets (3 contracts, 0.000027 test ETH) and two demo products offered (0.000003 test ETH).
  Left for lane A: the Rust market tool (new `list` argument, `closeBy`, `notBeforeBlock` at
  `observableAt`, NL6-14) and the insurance page; buying needs non-insider keys.
- 2026-10-27: lane C batch 2 (branch `nl-lane-b`, Foundry only). THREATS LC16-1 and LC16-2
  mitigated in code: `OnyxReobserve` observer changes wait `changeDelay` (3 days in
  `DeployMarkets`), and `OnyxPanelOracle` seats are gated on the settlement's staked validators;
  tests fail without the fixes and pass with them; not redeployed live (the final release
  redeploys them). `scripts/insurance-sepolia.sh demo-fork`: on a throwaway fork, products 1 and 2
  are bought by a non-insider, one pays and one expires (DEMO PASSED; docs/ops/demo-testnet.md).
- 2026-10-27: lane A: the lane B merge `037bc1a` passed `ONYX_WEB=1 scripts/check-all.sh` and
  was pushed; 30 `serve-api` processes left over by earlier check-all runs were stopped by PID.
- 2026-10-27: NL8 slice 1 done (ADR-0049). A level-2 subject no round settled yet, which the
  pinned dated template allows now, is a 402 `observe_payment_required` with the offer (price,
  deadline, the value becomes public, the refund rule); a prepaid voucher is held, not taken
  (one hold per buyer, at most half the exit delay), and the request gets a ticket. The worker
  runs one round at a time; a transcript for the held subject that checks against the template
  and arrives by the deadline is stored, served and then charged; anything else releases the hold.
  Tests: 7 API, 3 journal and book, 3 node. Left (ROADMAP S9): the receipt-page button, a node
  rehearsal with a real round command, the health alert, THREATS NL8-6.
- 2026-10-27: THREATS NS5-8 closed by the owner's choice "free" (relayed by the coordinator; ADR-0048
  amendment 1). The public site ships the Treasury, USGS and NCEI rounds and their level history
  unsealed, the verifier pins their result hashes in every build, and the data page says these
  values are free to read; only the SEC holdings stay sealed. `scripts/deploy-api.sh` refuses an
  export that prices a shipped level-2 metric. The public build and its e2e passed.
- 2026-10-27: NL17 (lane A; the tracker row is on branch `nl-lane-b`): GHCN-Daily `PRCP` at Denver
  and `TMAX` and `PRCP` at New York Central Park (`onyx_source_ncei::series`), and M2.5+ within
  300 km of Los Angeles (`onyx_source_usgs::region`), with rights records (S24, S25), recorded
  responses and fetcher tests; the live Denver TMAX and world M5+ templates keep their pins.
  `ONYX_L2_SERIES` picks a series in the live examples and `scripts/level2-sepolia.sh`. Live
  Base Sepolia rounds: Denver `PRCP` and Central Park `TMAX` passed (5 of 5 agreed, 14 txs each,
  about 0.0000118 test ETH each); the other two were still running at the commit. Also: a new
  low npm advisory on a build-time dependency is accepted by id (THREATS DEP-4), and
  `scripts/agent-smoke.sh` no longer leaves its two demo servers running after check-all.
- 2026-10-28: NL17 live rounds done on Base Sepolia: Denver precipitation 2026-09-27 = 0.0 mm,
  Central Park high 17.2 °C and precipitation 40.1 mm (each 5 of 5 agreed), M2.5+ near Los
  Angeles 2026-07-20 = 1 (4 of 5: one observer's fetch failed); 56 transactions, about 0.000047
  test ETH; the deployer keeps 0.0012. `scripts/level2-sepolia.sh` now accepts one drawn
  observer that did not agree (it reports it) instead of failing a settled round.
- 2026-10-27: lane C, NL21 agent connect, Node part (owner: "make it really easy for users to
  connect their ai agents to contribute to the network"). New MCP tools `onyx_find_dataset`,
  `onyx_propose_dataset` (check, then record on confirm; keyed and own-data sources refused; 5 per
  connection) and `onyx_proposal_status`; `apps/agent/src/http.mjs`, the same tools over MCP
  Streamable HTTP (read-only by default, rate-limited per address, sessions, foreign origins
  refused); the toolkit ships its own OpenAPI and dataset copies, so `apps/agent` runs alone;
  `onyx_check_answer` without `onyx-check` says `not_checked`. docs/agents.md;
  `scripts/agent-connect-test.sh` passed in a fresh node:22-alpine container with no network and
  only `apps/agent`, and `--live` passed its read-only steps against the hosted node. 34 toolkit
  tests pass. THREATS NL21-1..6. Left for lane A (ROADMAP S5): serve `/mcp` on the hosted node
  behind Caddy, mount the submit routes behind the guard, the paid step of NL19.
- 2026-10-28: NL14 Rust part (lane A): the market tool lists `closeBy` = `Source::earliest_public`
  (default: the start of the record date, UTC) less one hour and refuses a later close, opens markets
  only on guarded futures (`ONYX_MARKETS_FILE=<export>/insurance.json`), estimates `notBeforeBlock`
  at `observableAt`, and checks `mayBet`, the market's state and close and the bucket before any gas
  grant or mint (THREATS NL6-10, NL6-11 fixed, NL6-14 mitigated). Read-only `status` of market 1
  against Base Sepolia still decodes (open, 125 tONYX, 4 bets).
- 2026-10-28: lane A release prep. NL18 on the Markets page: `OnyxParametric`'s products, pool and
  held-back payouts read from the chain and checked by the verifier (wrong fact, an outcome that
  does not follow from the value, an uncovered pool refused); NL6-20..24 fixed on the same page
  (one block, 64 KiB cap, shares only from 10 bets, read-only e2e, the RPC field never stored).
  S5: `scripts/deploy-api.sh` turns on the node's intake, installs the MCP connector behind Caddy
  `/mcp` and loads the 443 cap; Caddy 2.6.2 validated the file in a container. NL19:
  `scripts/quickstart.sh` passed against the hosted node (VERIFIED, 252 s with the build).
- 2026-10-28: release `2f01280` live with the owner's "Deploy": API (`deploy-api.sh` exit 0, health
  200, the six level-2 series at level 2, intake on, the 443 cap loaded, Caddy admin closed,
  payments off; `/mcp` not running: the host has no Node, its install waits for the owner's OK) and
  site (release `20261028T035536Z-2f01280`, live daily check passed, live Markets page reads market
  1 and both insurance products at 1280 and 390 px). docs/ops/site-hosting.md has the details.
- 2026-10-28: NL3 slice 1 done (ADR-0050). Estimates live under `estimate.` at level `E`, outside
  the ladder, with method, method hash, error band (`unknown` stated) and inputs;
  `GET /v1/estimates/{metric}` serves them and refuses checked metrics, every checked route
  refuses estimate metrics, and shards, level-2 feeds, recipe sources, label sets (plain and
  dated) and track resolutions each refuse an estimate. The data page shows the first estimate in
  its own section: the mean daily fee of the checked window's whole days with a 95% interval,
  made in the reader's browser. THREATS EST-1..7. Left (C67): node loading, a client rerun of a
  pinned method, the agent toolkit, price, a method registry. Not deployed.
- 2026-10-28: NL4 slice 1 built, then parked (ADR-0036 amendment 7). Daily level-2 metrics get a
  close rule (an hour before the UTC day), level 2 resolves them, and `resolution_of` turns a
  round into a resolution only when the client's own node shows it recorded at level 2 after the
  day ended (the "no score from an open round" test). A publication under these rules is served
  by `/v1/track` and checks. The NWS forecast of the Denver high passes the rights review but not
  TR-R2-4: NOAA runs both the forecast and the NCEI record. The owner decides: declare the two
  offices separate operators with the reason on record, or name another forecaster. Until then no
  node publishes daily records and the site keeps the labelled example. THREATS TRS-1..5.
- 2026-10-28: NL20 (lane C, owner: "Yes do all the improvements"). The live server was not
  load-tested: `scripts/load-test.mjs` (refuses any host but this machine) ran against the
  release binary of `2f01280` with a copy of the deployed export on the dev box, indexer and the
  two label shards off: 7,090 requests/s at 64 concurrent from one client, p99 41 ms, no error or
  5xx; only the intake check answers 429 (no per-client limit on reads, THREATS NL20-1). Drill on
  that node: a stop drained in 0.06 s (bound printed: 32 s with payments off, the runbook said 66
  to 75 s; fixed), values again 1.8 s after the start, a recorded proposal survived; stopping a
  level-2 series needs its store and its pin removed (a store alone stops the node, NL20-5) and
  leaves the export too, or the next deploy serves it again (NL20-3); rollback brought it back.
  The reporter rotation was not drilled (payments off: no reporter). One watch pass: an indexer
  that is off alerted twice, now once (NL20-2, 87 watch tests). `deploy-api.sh` now keeps the
  running release in `/var/backups/onyx-prev/` (NL20-4). System units for the watch on the
  server, launch checklist section 9 (the release as a dry run, rollback commands).
- 2026-10-28: lane C's NL20 merged into main (`3486bbc`; conflicts only in THREATS and this log). NL4
  slice 2 (ADR-0036 amendment 8, decision under the owner's delegation): the NWS forecast of the
  Denver high as a labelled testnet forecaster (NOAA grades NOAA, TRS-4; relayed by an Onyx test
  key, TRS-6). `scripts/track-nws-sepolia.sh` stated 2026-10-29 to 2026-11-03 through the intake on
  Base Sepolia (six batch headers, blocks 51,232,516-51,232,594); the publication is in the export
  (`track/publication.borsh`). The data page asks the node and checks the record in the browser,
  "too few to score" below 30 graded days (e2e ns16). Grades come from December (NCEI lag).
- 2026-10-28: NL9 slice 1 done (ADR-0051): `GET`/`POST /v1/wants`, wants from four closed lists
  only, signed for the week by an open prepaid deposit (read on chain, never charged), salted
  account hashes, a category shown from 5 accounts as a bucket and only after the weekly cut; the
  Add data page's "Wanted data" panel asks on request and drops any value outside the lists.
  THREATS NL9-1..7. Not deployed.
- 2026-10-28: NL13 done (ADR-0048 amendment 2, ROADMAP C64 ticked, C68 opened). One value form
  per number in every ladder key; a cross-lane ladder form (`v: 2`) a client asks for, where a
  step stronger than the client's own passes only through its own lane's check, and a stronger
  answer with another value shows as a contradiction that never moves the level; a late, earlier,
  stronger proof is no fall; `/v1/signed` answers carry their one-step ladder and the browser
  takes only exactly that step; `onyx-check --registry` binds `proofLevel` and row ladders to the
  caller's own `levelOf`, and `onyx-check`/`level2-check --ladder-state` and the site remember the
  ladder they accepted. Not deployed; the Denver precipitation `0.0` gets a new value key, so the
  node and the site must ship together (NS5-15). Also: S24 and S25 marked served in
  docs/ops/data-sources.md (live since release `2f01280`).
- 2026-10-28: NL16 done (observer path). The coordinator writes a round announcement
  (`--announce`: chain, registry, metric, template, series, record date, window, pool, reveal
  mode); an outside operator runs `join.sh key` (key made on the observing machine, 0600, only the
  address leaves) and `join.sh observe round.json KEY`; the observer refuses an announcement for
  another template, with spool reveals, with a round flag given twice or naming another chain, and
  signs with its own key on any chain. `scripts/join-rehearsal.sh` ran the guide's commands in a
  fresh `ubuntu:26.04` container (curl, ca-certificates, libudev1 and the kit only) against a local
  anvil with four host observers: the container's observer was drawn, committed, sealed its reveal
  on chain and agreed, the round settled 5 of 5. Rehearsal fixes: 2 s anvil blocks (five pollers
  slowed the coordinator past the 3-block draw lead) and base fee 0 (anvil's 1 gwei tip and the
  signer's 1 gwei cap). Suppliers propose with NL21's tools (docs/join.md "Supply data instead");
  bonds and pay wait for NS12. The rehearsal image was removed by name.
- 2026-10-28: NL10 slice 1 done (ADR-0052). `onyx_coordinator::tiers` moves row batches hot,
  warm and cold: a copy is read back before the old one is dropped; a shard goes cold only over a
  block or block-range input and only after a rebuild matched the chain root (NL10-1); the recipe,
  label set file and attestations must be kept (NL10-2); a recompute is stored only on a match; the
  recompute queue refuses unpaid requests and caps work (NL10-3) and counts overdue ones (NL10-4).
  Left for ROADMAP S11: the node route (`202` ticket), health counts, the move schedule.
- 2026-10-28: NL11 parked after an upstream check: tlsn is still a pre-release (alpha.15), the
  in-page JS package is archived and browser proving now runs through a Chrome extension and a Node
  verifier server. A labelled experiment is the next step, not a product feature; the unblock steps
  are in the NL11 row.
- 2026-10-28: NL12 lane-A gates on main `ccbe553`: LC-21 fuzz smoke exit 0 (19 targets x 30 s, no
  reproducer); LC-16 forge gates pass (697 tests, layout identical, gas snapshot, 24 invariants at
  512 x 256) and Slither 0.11.6 re-run triaged (0 security findings); LC-13 VERIFIED by a 150-minute
  soak (pass, 0 violations, 18 restarts, 9,869 injected faults). LC-11 is not run: it waits for the
  owner and a faucet top-up.
- 2026-10-28: release `12cb627` live with the owner's "deploy": site `20261028T155353Z-12cb627` and the API
  (`scripts/deploy-api.sh`, exit 0; the site went out about 2 minutes before the API because a
  version-check line failed before the API step). Live: health 200, the six level-2 series 200 at
  level 2 with ladders (Denver precipitation 0.0 accepted by the new site, NS5-15), `/v1/track`
  and `/v1/wants` 200, connection cap loaded, live daily check passed, Markets page reads market 1
  and both insurance products at 1280 and 390 px. `/mcp` still 502: the host has no Node.
- 2026-10-28: `/mcp` live after the owner's OK: Node 22 installed on the host and the API
  redeployed (`scripts/deploy-api.sh`, exit 0); `initialize` answers over POST.
- 2026-10-28: pressure test of release `12cb627` (THREATS PT28-1..6), on main `224aa7f`:
  `ONYX_WEB_PUBLIC=1` check-all passed (50 min); the load test on the release binary with the
  deployed export (1.1 M requests at up to 128 at once, 0 errors or 5xx, node peak 21 MiB);
  `scripts/conn-cap-test.sh` passed; fuzz smoke 19 targets x 30 s, no crash; NL15 rehearsed on an
  anvil fork (5 of 5 observers agreed on the 2026-10-26 value, settle and claims paid out, the
  contract ended at 0; early, double, wrong-value, loser and double-claim paths refused). Live
  probe: 6 findings, all fixed for the next release (forecast track withheld as label data,
  insurance shown open with an empty pool, Explorer sideways scroll, 502 for oversize bodies, MCP
  naming a tool it lacks, small hardening). NL15 risk noted: market 1 predates the NL6-11 fix, so
  nothing may be recorded for 2026-10-27 before 00:00 UTC (the round tool refuses it).
- 2026-10-28: NL22 done. The market tool gained `underwrite`; after a fork rehearsal the `customer`
  test key funded the live insurance pool with 1,000 tONYX (block 51534119), so once the next
  release is live the Markets page shows both products for sale instead of "No cover for sale".
- 2026-10-29: NL15 done. Market 1 settled from the live Treasury round for 2026-10-27
  ($40,582,274,254,843.57; 3 of 5 observers agreed, 2 honestly reported no answer from the source);
  bucket 2 won and its one bettor got 122.9 tONYX. The fiscaldata store and its pin are in the
  export for the release deploy. Every NL item is now done or parked.
- 2026-10-29: release `2b7dbc9` deployed with the owner's OK (one deploy after the NL15 payout).
  API (`scripts/deploy-api.sh`; the binary rebuilt with `--remap-path-prefix`, after the script
  refused one holding the home path): Caddy valid, 2 servable shards, connection cap loaded, `/mcp`
  answering with the read-only instructions; live `/v1/track/log` and `/v1/track/registry` 200, an
  oversize POST to `/v1/submit` and `/mcp` 413, OpenAPI names the public server, the Treasury value
  for 2026-10-27 served from round `0x4edf5859…`. Site (`scripts/deploy-site.sh`, release
  `20261029T013922Z-2b7dbc9`): on the live Markets page at 390 and 1280 px market 1 reads Settled
  with the observed value at level 2 and the winning bucket marked, both insurance products read
  Open for cover (pool 1,000 tONYX), no sideways scroll there or on the Explorer after the header
  check. The meeting brief was updated and sent to the owner (kept outside the repo).
- 2026-10-29: NL23 done (market 2 open on the guarded futures; on the page after the morning
  deploy). A process slip: `caa09af` was pushed while its check-all had failed (the background
  wrapper ended in `tail`, so the task said exit 0 while the log said `exit=1`); the failure was the
  new e2e line counting one of two bets in the middle bucket. Fixed in `4559f37` (check-all passed);
  pushes now go through a script that pushes only when the log's own `exit=` is 0 and the tree is
  unchanged. Nothing was deployed.
- 2026-10-29: NL24 done (round script: pin first, majority rule).
- 2026-10-29: NL26 done (`scripts/meeting-demo.sh`, the live demo in one read-only command).
- 2026-10-29: NL25 done: a second Treasury dataset (the TGA closing balance) at level 2, one live round; on the API after the morning deploy.
- 2026-10-29: round script: a rerun on the same draw counts observers that revealed in an earlier run (NL25 follow-up).
- 2026-10-29: NL27 done. Release `c7ed039` deployed with the owner's "Ok deploy" (one deploy for
  NL23 and NL25). API (`scripts/deploy-api.sh`, the remapped release binary): exit 0, 2 servable
  shards, connection cap loaded, `/mcp` 200; the live API serves the TGA closing balance for
  2026-10-27 (1,052,530 million USD, level 2, round `0xc5868671…`) next to the debt value. Site
  (`scripts/deploy-site.sh`, release `20261029T125215Z-c7ed039`): on the live Markets page at 390
  and 1280 px market 2 reads Trading open with 30 / 70 / 20 tONYX in its buckets, pool 120 from 4
  bettors, nothing refused, no sideways scroll. `scripts/meeting-demo.sh` lists the TGA series
  (exit 0 live). Every NL item is now done or parked.
