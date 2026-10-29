# Pre-public work loop (started 2026-10-25)

The project owner 2026-10-25: "start a md tracker and work on all this start a loop to get it all done".
Coding items before the repo goes public. One item per loop tick; PP6 was added and done after PP2
(the project owner: "yes do open api standard"). Each item
ends with `scripts/check-all.sh` (with `ONYX_WEB=1` when web files change) passing, a review by
hand (no agents: the project owner 2026-10-24), THREATS rows for valid findings, a commit and a push to
private `main`. Nothing here makes the repo public, deploys to mainnet or spends money.

Status words: `todo`, `doing`, `done`, `parked` (with the reason).

Items were first named W1-W6, which clashed with the website plan's W1-W12 (docs/website-plan.md,
THREATS W1-W6); renamed PP1-PP6 on 2026-10-25. Commits `1aa521d` (PP1) and `86e440a` (PP2) use the
old names.

| # | Item | Source | Status | Result |
|---|---|---|---|---|
| PP1 | Receipt shows the observers' evidence once an approved submission is fed | PRODUCT-PLAN U4 (open part) | done | ADR-0039 amendment 1; THREATS U4-5, U4-6; rehearsal shows round `0xea56…f407` on the receipt |
| PP2 | Drawn-panel fallback oracle instead of one testnet owner key | ROADMAP S3b, ADR-0042 | done | `OnyxPanelOracle`, ADR-0042 amendment 1; THREATS STK-5, STK-15..18 |
| PP3 | Serve uncleared label sets in salted format 3 only | ROADMAP D30, ADR-0037 amendment 2 | done (registry half moved to D31) | ADR-0037 amendment 3; THREATS T1-1 fixed, T1-3..T1-5; `/v1/labels/{set}/{address}` |
| PP4 | Browse data and local end-to-end hand-off | PRODUCT-PLAN U5 | done | dataset cards; buyer check in the rehearsal; docs/ops/local-handoff.md; `scripts/preview-bundle.sh` |
| PP5 | Prepaid buyer balances with batched settlement | partner tracker T7, design proposals | done (HTTP path is PP10) | `OnyxPrepaid`, `onyx_api::prepaid`, ADR-0046; THREATS PRE-1..5 |
| PP6 | OpenAPI 3.1 description of the API, checked against the code; website reference | The project owner 2026-10-25 | done | ADR-0045; THREATS API-DOC-1; `docs/api/openapi.json`, `/v1/openapi.json` |
| PP7 | Organize the repository; a professional README that stays current | The project owner 2026-10-25 | done | `scripts/check-readme.py`; docs/README.md; scripts/README.md; ADR-0007 amendment 1 |
| PP8 | One-page brief for the Monday 2026-10-26 meeting with a partner | The project owner 2026-10-25 ("go") | done | Sent as a PDF (not in the repo); sources: the partner tracker and this file |
| PP9 | Live observers: the Treasury feed for a date without a recording | PRODUCT-PLAN U4 (open part) | done | `scripts/feed-live.sh`, `fed_from_store`, `feed-approved --date / --from-store`; THREATS PP9-1; rehearsed live for 2026-10-22 |
| PP10 | Prepaid balances in the node: a voucher header instead of x402 | partner tracker T7, ROADMAP S7 | done (a node rehearsal with ONYX_PREPAID stays open in ROADMAP S7) | `onyx_api::prepaid_gate`, `onyx_api::prepaid_chain`, `ONYX-PREPAID-VOUCHER`, `scripts/prepaid-anvil.sh`; ADR-0046 amendments 1-3; `ONYX_PREPAID`; THREATS PRE-6..10 |
| PP11 | The observers' value on the receipt; freshness on each dataset card | PP1 and PP4 follow-ups | done | ADR-0039 amendment 2; `scripts/gen-freshness.py`, `data/freshness.json`; e2e PP1, PP4/PP11 |
| PP12 | Public repository dry run: the history-free snapshot and the final secret scan | launch checklist LC-25, LC-26 | merged into PP13 | |
| PP13 | Go public, anonymous: a new repository from today's files, the site on its own server, GitHub-only contacts | The project owner 2026-10-25 (option A; "everything needs to be anonymous") | parked: waits for the project owner (the GitHub organization name, then a yes) | `scripts/public-scrub.py`, `scripts/deploy-site.sh`, docs/ops/site-hosting.md; the snapshot builds and passes check-all; waits for the organization name and the go |

## Done when

- **PP1.** A fed receipt's public status carries the round id, the result hash and the record date,
  read from the feed ledger; nothing private (proposal content, reasons) leaks. The receipt page
  shows "checked by observers" with that evidence and says what it does not prove. Tests for a
  pending, approved-not-fed, fed and failed receipt; the browser rehearsal covers the fed state.
- **PP2.** A fallback decision needs a quorum of a panel drawn from staked validators with a seed
  nobody can pick alone; the one-key oracle stays only as a test double. Foundry tests for the
  draw, quorum, no-show and void paths; THREATS rows; ADR-0042 amendment.
- **PP3.** The API and the site serve an uncleared label set only as format 3 rows (no plain rows);
  the browser checks a salted row against its root. Tests; ROADMAP D30 checked off.
- **PP4.** Dataset-first browse page with details on demand; one runbook takes a contributor
  through submit, review, feed, check and buy on the local stack; a clean build passes all gates.
- **PP6.** Every route, status, schema and error code in one OpenAPI 3.1 file; a check fails when
  the code and the file differ; test answers validated against it; the node serves it; the API
  page renders it.
- **PP7.** README rewritten; its layout and route tables, `scripts/README.md` and `docs/README.md`
  generated from the repository and checked on every run; dead code and stale descriptions gone.
- **PP5.** A buyer deposits once; queries draw down an off-chain balance; settlement to
  `OnyxRevenue` is batched; refunds and withdraw paths tested; the privacy limit is stated.

## Log (newest first)

- 2026-10-26 Loop stopped: PP9, PP10 and PP11 done; PP12 merged into PP13; PP13 parked until the
  project owner names the neutral GitHub organization and says yes to publishing.
- 2026-10-25 PP11 done: a fed receipt's `check` carries `value`, the observers' agreed value
  read from the round (recorded settlement or live transcript); the receipt page shows it grouped
  (40,305,316,210,829.72) and the rehearsal checks it equals the value the buyer's own check
  gives. Each dataset card shows "Data as of" from `data/freshness.json`, which
  `scripts/gen-freshness.py` writes from the shipped data files at every site build (static/data
  is a build folder); the reserves card has no date (its file holds block heights).
- 2026-10-25 PP10 done (part 2b): `ONYX_PREPAID=<address>` in onyx-node (facilitator mode with
  the reporter on; the pass shares the reporter's sender, one nonce sequence per key). The start
  reads the contract's reporter, token, revenue and exit delay and refuses a contract that is not
  the deployment's; the journal is `<state dir>/prepaid.journal`; the pass runs on the
  reporter's timer, at most half the exit delay apart, and stops with the other tasks. Tests: the
  config rules and the start checks. Not done: a node started with `ONYX_PREPAID` against anvil
  (the chain side is rehearsed by `scripts/prepaid-anvil.sh`); kept open in ROADMAP S7.
- 2026-10-25 PP10 part 2a: the chain side. `ChainAccounts` reads `OnyxPrepaid.accounts`
  (finalized for the server, latest for the reporter); `PrepaidReporter::pass` redeems the latest
  vouchers, then allocates the redeemed usage per book, marking the lines before the send so a
  crash never pays twice. `scripts/prepaid-anvil.sh` (in check-all) on anvil: 3 vouchers of 1,000
  on books A, A, B; one pass redeemed 3,000, paid book A 2,000 and book B 1,000, left the pool at
  0; a second pass did nothing; the buyer can still spend 2,000. Review: the pass first re-read
  accounts at `finalized` right after its own redeem (two blocks behind on anvil), so nothing was
  allocated; the reporter now reads `latest` and takes the mined redeem as the new total.
- 2026-10-25 PP10 part 1: paid routes take `ONYX-PREPAID-VOUCHER` instead of x402 on a server
  built with `with_prepaid`. The voucher is charged against an account read at most 60 s old and
  appended to an owner-only usage journal, synced, before the answer leaves; the identical retry
  is free, also after a restart. Review findings fixed before commit: a stale read was charged
  against (now refused), the journal had default permissions (now 0600). 9 tests. Part 2: the
  node's chain reader and config, the redemption schedule, a local-chain rehearsal.
- 2026-10-25 PP9 done: `scripts/feed-live.sh` feeds an approved Treasury submission to a live
  observer round for its own record date (`level2-observers.sh` takes `ONYX_L2_RECORD_DATE` and
  keeps `store.json`). Rehearsed end to end for 2026-10-22 (no recording exists): submit-dev
  intake, review approval, seven observer processes fetched the Treasury API, 5 drawn, 5 agreed on
  40,444,342,709,389.15 USD (equal to the API's value), and the receipt shows level 2 with the
  round id and `eip155:31337`. Review: the receipt's chain came from a command-line flag; it now
  comes from the store's own tag (THREATS PP9-1).
- 2026-10-25 PP13 dry run: `scripts/public-snapshot.sh OUT --without-pending` gives 1419 files with
  no name, home path, secret or e-mail left. check-all inside it: every Rust test passes (174
  label-data tests ignored), the chain rehearsals pass; it found three scrub faults, all fixed:
  the generic test address 192.168.1.1 was replaced (now kept: no home address is in the tree), a
  capital after "(" broke check-launch's owner match, and links to renamed headings and left-out
  files were dead (anchors now follow the renamed headings; links to left-out paths become plain
  text). Disk: the old build cache (138 GB) was deleted; snapshot builds use their own cache on the
  main disk with `CARGO_INCREMENTAL=0`.
- 2026-10-25 PP5 done (contract and library; the node's HTTP path is PP10, ROADMAP S7):
  `contracts/src/OnyxPrepaid.sol` (deposit; EIP-712 vouchers of the running total redeemed in
  batches into a pool; allocation to `OnyxRevenue` books that names no buyer; exit after a delay;
  low s only; testnet chains only), 7 tests and a books fuzz test; `onyx_api::prepaid` (digest,
  signing, recovery, `PrepaidBook::charge` for exactly the last total plus the price);
  `fixtures/prepaid/vectors.json` checked by both sides; spec docs/spec/prepaid.md with the
  privacy limit; ADR-0046. Hand review: the reporter can never take more than a buyer signed
  (PRE-1); a buyer who exits early still pays for what it signed within the delay (PRE-3); the
  chain still shows per-address deposits and totals (PRE-4, stated). PP8 (the brief for a partner) sent.

- 2026-10-25 PP7 done: README rewritten (how it works, proof levels, features, API, quickstart,
  rerun, layout, docs, hardware, licence); its layout (every crate, adapter, app, recipe and
  contract, from their own descriptions) and route table (from the OpenAPI file),
  `scripts/README.md` (every script, from its own header) and `docs/README.md` (start here, the
  seven trackers, every folder) are generated and checked by `scripts/check-readme.py` in
  check-docs. Removed: the empty `onyx-sim` crate, the unused Postgres compose file (ADR-0007
  amendment 1). Fixed: four stale crate descriptions, six undescribed fixture folders, a stale
  runbook name, and a false claim in `OnyxPayout` (no Rust `onyx_markets` exists).

- 2026-10-25 PP4 done: Browse data opens with 9 dataset cards (level, source, unit, rights, API)
  whose button brings the section into view and focus; the local rehearsal now restarts the node
  with the fed store and a buyer checks the served value with its own template
  (`examples/check-observation.rs`: value 40305316210829.72, the receipt's round and result);
  runbook docs/ops/local-handoff.md; `scripts/preview-bundle.sh` built and checked a bundle with
  the node binary. Found on the way: PP3 and PP6 had not run the public web gate
  (`ONYX_WEB_PUBLIC=1`), whose guards refused `openapi.json` and `salted-vectors.json` (not on the
  allowlist; `entity:` fields in a fallback build): both reviewed and allowed, the vectors' expected
  fields renamed; every item from here runs `ONYX_WEB_PUBLIC=1`. Not done: per-card freshness.

- 2026-10-25 PP3 done (serving and browser; registry and payment are ROADMAP D31):
  `SaltedAnswer` and `verify_salted_answer` in `onyx_labels::salted`, a JSON wire form, shared
  vectors (synthetic; 4 rows, 5 refusals), `GET /v1/labels/{set}/{address}` (test sets only, free,
  rate limited, `no-store`, in the OpenAPI file), `ONYX_SALTED_LABELS` in the node, the browser's
  `check_salted_answer` and a Labels page panel (check a row, a changed salt refused, a guessed
  neighbour not confirmed). Hand review: a real uncleared set must never load (name gate, T1-3);
  enumeration over a free route is bounded only by the rate limit until payment exists (T1-4).

- 2026-10-25 PP6 done: `docs/api/openapi.json` (18 operations, 88 error codes), checked by
  `scripts/check-openapi.py` (routes, codes, the delivery error table, Caddy, the site copy; 12
  planted faults caught) and by `openapi_check` on every answer the API, Bitcoin, pools and reserve
  tests receive (all 18 operations at their success status). Found and fixed on the way: 11 codes
  missing from the error table, `snapshotUnix`/`servingCheck` undocumented, nullable
  `attribution`, and POST-only routes answering a wrong method with `Allow: GET, HEAD`. The node
  serves the file at `/v1/openapi.json`; the API page shows every route and error code (checked at
  1280 and 390 px).

- 2026-10-25 PP2 done: `contracts/src/OnyxPanelOracle.sol`: bonded seats, a draw from the request
  block's hash excluding the submitter and the claim's stakers, commit-reveal answers, a strict
  majority or void, no-shows slashed. Hand review found seat grinding (new addresses with small
  keys joined after the beacon is known; or a lapsed beacon re-rolled): fixed (STK-15). 14 tests
  and a tally fuzz test against the real `OnyxStakes`. The demo keeps the owner-key stand-in.

- 2026-10-25 PP1 done: `onyx_api::submit::feed` (ledger at `JOURNAL.feed.json`, owner-only,
  shape-checked, read at node start); `GET /v1/submit/{id}` adds `check` for an approved, fed
  receipt; the receipt page shows "Checked by observers" with record date, round, result hash and
  its limits. The connected rehearsal now feeds the approved receipt and checks the page at 390
  and 1280 px (axe clean). Hand review: a withdrawn approval hides the evidence; failure reasons
  stay private; the record date is the only proposal field shown. Follow-up (not done): show the
  observers' value itself on the receipt.
- 2026-10-25: tracker started. Baseline: `main` at `b0b40a8`, check-all passed; the connected
  browser submission rehearsal (`ONYX_SUBMIT_BROWSER=1 scripts/level3-book.sh local`) passed.
