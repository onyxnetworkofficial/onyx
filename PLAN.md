# PLAN

## Contributor-first product loop

The current app work is tracked in [PRODUCT-PLAN.md](PRODUCT-PLAN.md): U1 homepage/navigation,
U2 durable review and receipts, U3 user data input, U4 reviewed source pipeline, U5 browse/rehearsal.
U6A is a concept-only market frontend; U6B remains a separate decision and implementation gate.
These extend Phase S; proof, economics, rights and deployment gates remain in force.

- [x] U1 Contributor-first homepage/navigation and backend blueprint
- [x] U2 Durable review workflow, node submission routes and receipts (ADR-0039; check-all and real-node/browser rehearsal passed 2026-10-22)
- [x] U3 Own-data file/paste input, including datasets without a public source URL (ADR-0040; full check-all, connected node and browser rehearsal passed 2026-10-22)
- [ ] U4 Reviewed public source through the real local verification pipeline
- [ ] U5 Dataset-first browsing and full local contributor/buyer rehearsal
- [x] U6A Future-market concept UI (ADR-0041): explore, example detail, local limit arithmetic, empty positions, resolution/redemption states and term drafting; no market services or mocked activity
- [ ] U6B Decide whether to build a backed future-data market, then design and implement terms registry, collateral, oracle/disputes, positions and settlement as a separate gated milestone

## Milestones
- [x] **M0 Foundation**: workspace, layout, CI (green on GitHub), ADRs, spec skeletons, THREATS, RESEARCH
- [x] **M1 Deterministic recipe runtime** (loops 4-6; acceptance met, see STATUS)
- [x] **M2 Labels** (loops 12-15; acceptance met, waiting for the owner's review)
- [x] **M3 Miners and commit-reveal** (loops 17-21: CAS, signed commit-reveal rounds, miner client, end to end on real blocks; 4 review rounds)
- [x] M4 Validators (loops 22-28: seed, assignment, signed verdict commit-reveal, canaries, settlement by rerun, ledger; real-block end to end). Frozen off-chain per q12 (a); one canonical ledger, call order, beacon and coordinator bond handed to the M6 contract (ADR-0019 loop 28)
  - [x] Loop 25 (q11 a): settle reruns disputed shards itself; rules, ledger and canary deadline fixed at round open
  - [x] Loop 26 (L25 review): recipe checked at open + NoValidRoot verdict; built-in address rules only (`RuleSet`); canary snapshot in the ledger; pending shards voided at `pending_close`; validator deterrence; single slash for unrevealed verdicts
  - [x] Loop 27 (L26 review): committers recorded at commit + canaries re-checked at apply; voided public work burned + `MIN_PENDING_GAP`; `Ledger<R>` bound to one rule set; `min_canary_ppm` deterrence (enforcement PARTIAL)
  - [x] Fresh reviews of loops 27 and 28 (loop 28: 0 merge-blocking); open items handed to M6 (ADR-0019)
- [x] M5 Disputes by recompute (ADR-0021, docs/spec/disputes.md); merged 2d36536
  - [x] Loop 29 (q7 a): bonded challenge, drawn panel reruns and commit-reveals roots, appeals to 2k+1 panels, overturn by compensating entries; 8 real-block tests
  - [x] Fresh adversarial review of loop 29 (3 merge-blocking: replay, shrinking appeals, unfunded fees)
  - [x] Loop 30 (L29 review): consumed challenges, exact panel sizes, bond-funded panels with conservation, undisputable shards rerun at settlement, ledger-wide slash caps, closed shards, lazy-panel ratio, rules guard for panel reruns
  - [x] Fresh adversarial review of loop 30 (2 merge-blocking: Stands griefing, stale slash cap)
  - [x] Loop 31 (L30 review): Stands burns funders' fees, all-silent panels slash nobody, slash caps at apply, exhausted validators never drawn, no-valid-root work held through its challenge window
  - [x] Fresh adversarial review of loop 31 (1 merge-blocking: one sybil reveal slashed honest silence in Stands)
  - [x] Loop 33 (L31 review): Stands judges nobody, open-panel exposure in eligibility, pool counted against all pending slashes plus "rerun required" for undisputable unanimous shards, no-valid-root corrections hold work
  - [x] Fresh adversarial review of loop 33 (third strike on Stands, escalated as q13)
  - [x] Loop 46 (q13 (a), L33-N1..N7): Stands removed, a split or no panel goes to the settler's rerun, which decides the dispute; rerun closes the shard; silence slashed only where the majority root was revealed; pool counts only certain slashes; challenge window outlasts a panel; Solidity round tag v4 (dispute fields)
  - [x] Fresh adversarial review of loop 46 (merge gate: 2 blocking, M5-L46-F1..F4)
  - [x] Loop 49 (M5-L46-F1..F4): held work kept while a rerun is pending; an appeal with no panel sends a split to the rerun; contract `_checkConfig` mirrors `DisputeConfig::is_valid`; `Ledger::finality`/`final_root` (rerun pending is not final) and a rerun deadline (`expire_rerun` -> Unresolved)
  - [x] Fresh adversarial review of loop 49, then merge m5-disputes-wip to main (q13 a)
- [ ] Proof ladder levels 2-5 (re-observed, source-proven, bonded claim, track record): spec first (moved out of M5)
  - [x] Level 2 re-observed (ADR-0027, loops 58-61): observers commit-reveal, one claim intake bound to the settled result; reviews L2R..L2R4, claim backing closed; merged (q15 a)
  - [x] Level 3 lane (a), source-signed data (loops 62-67, ADR-0028): key registry with provenance, EIP-191 reports, one intake, replay/equivocation books, monotonic key life cycle, reviewer-only recovery; example `adapters/source-signed`; merged after review L3R5 (key-misuse family CLOSED)
  - [ ] Level 3 lane (b) zkTLS: design only until a verifier meets ADR-0028 item 8; Ed25519 sources
  - [ ] Level 4 bonded claims: PARKED on branch level4 (q21 b, after 4 reviews each found a new mechanism-design hole); redesign from a proven escalation design. When it returns, `BondedClaim` must take VerificationMethod tag 7 (tag 6 is level 3's `SignedSource`) and regenerate fixtures/claims/vectors.json
  - [x] Level 5 track record core (ROADMAP C1, ADR-0036, branch track-record): `crates/onyx-track` computes per-(source, metric) records against later level 1/3 answers (never peers), binary Brier on stated confidence, equivocation as a confident miss, Merkle-committed entries and records chained by `prev`, client `verify_record`/`verify_history`/`check_extends`; never a level or money (check-boundaries fence); review TR-R1 (3 blockers fixed). Open: intake C5, serving and display C6, on-chain resolutions C7 (PLAN q-TR-1..4)
  - [x] Level 3 books on chain and chain clock (ROADMAP C2, ADR-0028 amendment 6, branch signed-books): `OnyxSignedBook` (key registry, never-pruned sequence and payload books, fact book, lazy withdrawals, `block.timestamp`), Rust vectors and 160 differential traces replayed in Foundry, invariants, monotonic store clock, `signed_chain` cast client, anvil rehearsal; not deployed
  - [x] Track-record intake (ROADMAP C5, ADR-0036 amendment 1, branch track-intake): statements only signed by the source's registered (level-3) key, batches sealed until close with only the header hash posted, `committed_at` = the post's finalized block time, client `verify_log`; `CastRecord` (poster nonce counts posts) and `scripts/track-intake-anvil.sh`; per-metric close review (only the example level-3 metric ruled); THREATS TR-I-1..12
  - [x] Track records served and shown (ROADMAP C6, ADR-0036 amendment 2, branch track-serving): `/v1/track` from an append-only store (node: empty, no metric has a close-time rule), one client check for Rust and the browser (`onyx_track::wire`), Data page empty state plus the labelled example; fence extended to onyx-api and the verifier; review TR-S-R1 fixed
  - [x] Track records on chain (ROADMAP C7, ADR-0036 amendment 3, branch track-onchain): resolutions only from on-chain facts (`OnyxSignedBook` level 3, `OnyxRegistry` level 2; level 1 fail closed), rederived by every client at the commitment's head; commitments in the book's append-only log (latest public, forks refused on chain); reviewer-declared operators so affiliates never judge or shield; the answers usable first judge an entry; `scripts/track-onchain-anvil.sh` in check-all; THREATS TR-C7-*
  - [x] Track-record follow-ups (ROADMAP C9, ADR-0036 amendment 4, branch track-followups): period facts closing before their period starts (`Close::PeriodStart`; the Bitcoin flow rule: UTC days by median time past, close 2 h before the day); skill against persistence beside the counts (q-TR-5 decided); a key counts only while the level-3 book shows it active at the post's block (TR-R4-4); signed intake receipts with a posting deadline (TR-I-6). Open: retired outcome C90, broken receipts published C91, entity self-statements C92, sealed rows encrypted C93
  - [x] C10 wired (2026-10-17, branch signed-wiring): deploy tooling and manifest pin, coordinators post, API serves level 3 only against `settleable` at a finalized block, censorship policy; the live deploy is the lead's (runbook deploy-signed-book.md)
  - [x] Track-record logs served (ROADMAP C20, ADR-0036 amendment 5, branch track-serve): `GET /v1/track/log` (logs and intake; a client reruns `compute` and `verify_log` and checks every commitment since the one it knew extends it), not served free with priced level-1/2 answers (C22, the lead's decision); the coordinator's publication file read by the node (`ONYX_TRACK_PUBLICATIONS`, append-only, checked); paged histories past 1,024 entries; the browser remembers the commitment per record and asks with `since`; THREATS TR-C20-*
  - [x] Paged track logs and the registry served (ROADMAP C21, C23, ADR-0036 amendment 6, branch track-pages): log format 2 (a chained prefix hash per row in publication order; format-1 commitments unchanged and never read as format 2), `GET /v1/track/log` head plus pages of 4,096 rows and whole posts, checked row by row with `check_extends`'s rules in one linear pass on clients and nodes under the C20 caps; `GET /v1/track/registry` (`onyx_core::RegistryData`, rebuilt by each client with its own address rules and used only at `registry_root`; withheld with the logs and, in fallback, when it names an address); the browser reruns the example with the served registry; THREATS TR-C21-*, TR-C23-*
  - [x] C61 client check of `/v1/signed` and the Data page's level-3 card (rehearsal, pinned chain answers), C62 level-3 monitoring (2026-10-18, branch signed-client)
  - [ ] Next: lead deploys OnyxSignedBook (C10 deploy step); C60 level-3 coordinator process, C63 client without one trusted RPC; C22 priced answers in logs (needs the lead); C24 live node URL for the track section (needs LC-8); C40-C41 chain-reader and client follow-ups; C90-C93 track follow-ups
  - [x] Loop 57 (ADR-0027): proof levels as protocol data (`ProofLevel`, `MarketPolicy`, `ReObserved` method); level 2 core (`onyx_coordinator::observe`: commit-reveal, majority/median aggregation, payouts); example fetcher `adapters/source-page`; spec docs/spec/proof-ladder.md
  - [x] Loop 58 (review L2R: 1 blocking): level-2 claims point at the settled result (`ReObserved { spec, result }`, `backs`); money default levels 1 and 3 enforced by the API settlement endpoint; drift no-call band; outages pay nobody; round-bound commits; bounds; pinned claim encodings; ADR renumbered 0027
  - [x] Loop 59 (review L2R2: 1 blocking): one claim intake against the coordinator's own settled results (`ObservationResults::accept_claim`); shard level = weakest input (label sets fail closed); market policy advisory until bound on chain; stale guesses in an outage slashed; far minority judged in a clustered `NoResult`; `backs` reads the feed; one `ProofLevel` enum
  - [ ] Fresh adversarial review of loop 59, then merge proof-ladder
  - [x] Proof level bound on chain: `OnyxRegistry.verifyRowLeveled` returns the attested level, floored by the label set's registered level (ADR-0030; REG-R1, REG-R2)
  - [x] Fresh adversarial reviews of loops 57 (L2R) and 58 (L2R2)
  - [x] A reviewed real source: U.S. Treasury Fiscal Data, Debt to the Penny (adapters/source-fiscaldata; recorded and live rehearsal, loop 62)
  - [x] Level 2 API serving (branch api-level2, ADR-0027 amendment 4): `/v1/observations`, client `verify_observation`, observation revenue books (R-REV-12), onyx-node `ONYX_OBSERVATIONS`, local demo pays and verifies; reviews L2API-R1..R4, price-clock family closed (q19 a), store template pinned to the registration; merged 2c9fc74; fork/testnet book opening TODO (THREATS L2API-4)
  - [x] Random observer draw with an on-chain announcement, draw commitment and beacon seal; at most 3 draws per subject (reviews L2RND-R1..R4, merged 2026-10-12)
  - [x] Observers sign commits and reveals (ATT-R1-F3, ADR-0027 amendment 7; reviews ATT-R2, SIG-R1 0 blocking; merged 2026-10-12): no slash for a missing reveal or commit; public builds ship no result hash
  - [x] Transcript log (ADR-0027 amendment 8): observers post commits on chain, coordinator reveal receipts, clients call `transcriptComplete`; log read once from one finalized block; reviews REVLOG-R1..R4 (R4: 0 merge-blocking); merged 6a9858a (2026-10-12). Review 4's non-blocking fixes on branch `revlog-r4` (R4-1 publish before the book drops a settled round, R4-2 load order, R4-3 reveals checked at arrival, R4-4 docs)
  - [ ] Level 2 on Base Sepolia (ADR-0027 amendment 8 item 8): (c) independent observers with their own keys and a live coordinator on `SystemClock`; (d) a separate hot receipt key per deployment; (e) state REVLOG-1 and REVLOG-5 on the paid API. Done: (a, ROADMAP B1, branch `revenue-books`) the deploy run opens each served fact's revenue book from the reporter key before serving level 2, idempotent and fail closed (L2API-4; rehearsed with `fork --level2`); (g, ROADMAP B5) a restarted coordinator keeps taking reveals while the window is open (`keep_taking_reveals`, REVLOG-R5-1d); (b) unfinished and settled rounds kept in the book; finality wait (1,178 s measured) handled by reveal arrival times; (f, branch `draw-open`) REVLOG-R4-5 (High since review REVLOG-R5): idempotent `fixBeacon`, announced draw and opened round kept in the book from the draw on, failed attempts written before they leave it; live `--rpc` uses `SystemClock`; review R6: the book never forgets a draw the chain may hold (REVLOG-R6-A/B/D, final-block `drawOf`), and a draw the book missed after a crash before its first save is adopted (REVLOG-R5-2)
  - [x] Level 2 sealed reveals (ROADMAP C4, ADR-0027 amendment 10, branch sealed-reveals): reveals posted on chain encrypted to the draw's reveal key (the post is the receipt), `revealLogComplete` refuses a round that drops a post, garbage dismissed with a Chaum-Pedersen proof, `recordAttempt` pins non-value rounds before serving (REVLOG-5 reduced); `onyx_coordinator::sealed`, `OnyxRegistry`, vectors, invariants; not wired, not deployed
  - [x] Level 2 sealed reveals wired (ROADMAP C11, ADR-0027 amendment 11, branch sealed-client): observer posts `postSealedReveal`, the live coordinator reads every drawn slot past reveal close (`open_posted`), opens or dismisses, checks `revealLogComplete`, records every round (`recordAttempt`, `recordObservation`); reveal key file of its own; book version 6 keeps dismissals; served `sealed`; Rust and web clients check posts, dismissals and records with their own drawn set, window and round id; `scripts/level2-sealed.sh` on a local anvil; THREATS C11-*; not deployed (C31)
  - [ ] Level 2 time-lock layer (ROADMAP C12, ADR-0027 amendment 12, branch timelock): dependency review (no acceptable tlock crate) and design done; composition tested with a mock beacon (`onyx_coordinator::timelock`, THREATS C12-*); review C12-R2: own IBE on `blst`'s safe Rust API is the route; real tlock C50 BUILT on branch tlock-ibe (`onyx-tlock`, ADR-0027 amendment 13, THREATS C50-*; Go/tlock-js vectors native and wasm32; fresh cryptography review C50-R1 done, merge recommended), registry and wiring C51 BUILT on branch tlock-wire (ADR-0027 amendment 14, THREATS C51-*, review C51-R1; not deployed, C31), far-date unseal C52 BUILT on branch tlock-far-date (ADR-0027 amendment 15, THREATS C52-*: a second time-lock of each post's body key to the subject's last observable day, checked by the coordinator at intake, registry cap 1,438; not deployed, C31; client unseal C83), beacon outage rule C53, Go cross-check C70
  - [ ] Level 2 next: drand time-lock layer (C12, C50-C53), level-2 disputes and the pool from on-chain bonds (C13), WASM fetchers (C14), EDGAR sealed (C30), registry redeploy (C31), website sealed round (C32), per-round limits text (C33), coordinator RPC cross-check (C34) (THREATS C4-*, C11-*, L2-F2 residuals, F5, F6)
- [x] M6 Settlement contracts on Base Sepolia: code merged (core, loops 44-45, disputes loops 51-53, G4); deployed and exercised by the full run 2026-10-12
- [x] M7 Delivery (query API, x402, final roots only; loops 38-43, 47-51)
- [ ] Money loop (ADR-0025, option A): usage revenue split (`OnyxRevenue`), contributor pass, dataset prices
  - [x] Build: contract + Rust split + API pass, receipts and dataset prices (branch money-loop)
  - [x] Fresh adversarial reviews (REV-L1: 1 blocking fixed; REV-L2: 0 blocking), merged bd523fc
  - [x] Self-settled x402 into OnyxRevenue (ADR-0026, loops 57-58): token proves amount and recipient; receipts only with proof of arrival
  - [x] Reporter loop and persistent receipts (REV-11), client `payTo` pin (REV-4) (branch rev-persist; fork rehearsal: receipt on disk, API restart, credit, miner claim and withdraw); attribution still the operator's (REV-1)
  - [x] Reviews REV-L3 (1 blocking fixed) and REV-L4 (0 blocking; F1-F7 fixed: stuck credits resent, persisted and reported, up to 4 in flight, clamped waits), merged 2899f65
- [x] Per-block series recipe `btc-entity-flow-series`: rows sum to `btc-entity-balance` (R-SERIES-2), Python recount, live check, website "Flows over time"; reviews SER-R1 (1 blocking fixed) and SER-R2 (0 blocking, all fixed); merged. Rounds open only through `Round::open_preflighted` (<= 50% fuel, listed failures only)
- [x] Ethereum P4a (ROADMAP Phase P, ADR-0044, branch eth-adapter): `onyx-source-eth` checks JSON-RPC answers against block hash, transaction hashes and trie roots; recipe `eth-chain-stats` (one row and shard per block, re-checks hashes, roots and the EIP-1559 base fee); 300 finalized blocks; Data page "Ethereum on chain" in every build with a browser rerun. Next: P4b exchanges and issuers on Ethereum, P4c USDT and USDC; a beacon light-client finality check (ETH-2)
- [x] Longer series history: one week (1,008 blocks, 66 preflighted shards sized by inputs + outputs) fetched from the node and merged (HIST-1..7, HIST-R1, HIST-W1). Still open: history roots on chain with the Base Sepolia run (HIST-1); `openRound` itself does not require the registry's preflight attestation (SER-R2-F1 PARTIAL: an unattested shard never verifies through `verifyRowLeveled`)
- [x] On-chain metric registry (ADR-0030): one template per metric, preflight + level attested before the open, `verifyRowLeveled`, one level-2 result per fact (branch onchain-registry, review pending)
- [ ] Phase P production data coverage (ROADMAP, top priority): P1 survey done; P3a first on-chain metrics on the Data page done (ADR-0043: `btc-chain-stats`, 1,360 blocks, eleven charts in every build, browser rerun of one block, `scripts/refresh-onchain.py`); P2, P3 (live refresh, daily active addresses, price-based metrics, full history), P4-P8 open
- [ ] M8 End-to-end demo
  - [x] Local rehearsal on anvil: `scripts/demo-local.sh [--dispute] [--serve]` (loops 54-56, docs/ops/demo-local.md)
  - [x] Base Sepolia driver: `scripts/demo-testnet.sh` (strict flags, `--plan`, `--yes`, `--sweep`), per-actor + operator keys, fork rehearsal passes
  - [x] Service packaging: `onyx-node` binary, Docker image, systemd unit, recoverable paid settles (branch ops-pack, reviews OPS-L1..L5; merged after q16 a)
  - [x] Base Sepolia run itself: REHEARSAL PASSED 2026-10-12 (149 txs, run-20261012T221750Z; website Run page links every tx)
- [ ] Phase P production data coverage (ROADMAP Phase P, top priority since 2026-10-18)
  - [x] P1 entity indexer survey (read only)
  - [x] P5 hardware requirements: docs/ops/hardware.md, `onyx-node doctor` (run at node start), README, Run page
  - [ ] P2a-P2e, P3, P4, P5b, P6-P8

- [ ] Phase S submit data (ROADMAP Phase S, ADR-0038; the project owner + a partner 2026-10-20)
  - [x] S1 onboarding questions, proof-level route, `/v1/submit` API, `#/submit` page with the Treasury and FRED walk-throughs, offline download (branch submit-onboarding, 2026-10-20; fresh review and merge pending)
  - [ ] S2 keyed sources with a real prover (research done), S3 staked submissions, S4 re-observation of accepted level-2 submissions, S5 hosted demo with a reviewer tool
- [ ] Phase P production data coverage (ROADMAP Phase P)
  - [x] P1 entity indexer survey; P7 launch walk
  - [x] P2c ETF holdings and flows from SEC filings: 7 of 14 US spot funds covered as a level-2 source (recorded; observer rounds are P2h), the rest named with reasons (branch etf-filings, 2026-10-19); follow-ups P2g (role contact, then filing XBRL), P2h (observer rounds), P2f (issuer daily files: terms forbid today)

## Current loop
Session 3: M2 done (loops 12-15). Stopped for review. Next: M3 Miners and commit-reveal, after the owner's go.

Phase P (2026-10-18): P2a issuer rosters done on branch `rosters-v3` (label set v3, ADR-0037
Amendment 1). Next for the rosters: D23 (v3 scan, reserve chain over windows, fresh snapshots),
D12 (header check of mapped heights), D24 (full history off git), D25 (dated manifests).

## M0 checklist
- [x] Repo layout as specified; Cargo workspace (11 crates incl. adapter), toolchain pinned 1.98.0
- [x] onyx-core types: Claim, Lane, VerificationMethod, DataRights, Snapshot, Subject, Commitment, Value (+ tests)
- [x] onyx-merkle implemented early (layout locks ADR-0003) + Solidity verifier + shared vectors
- [x] Foundry project, forge-std submodule, settlement interface sketch, fuzz tests
- [x] CI: fmt, clippy -D warnings, tests, boundary check, docs check, forge fmt/build/test
- [x] ADRs 0001-0010 + DECISIONS.md
- [x] 9 spec files with Scope / Interfaces / Assumptions / Open questions
- [x] RESEARCH.md table with reuse vs build
- [x] THREATS.md first draft
- [x] Ops stubs, sector docs, GLOSSARY
- [x] Adversarial review (loop 1): 25 findings logged in THREATS.md
- [x] Loop 2: fixed 12 loop-1 findings (id grammar, rights checks, quorum, evidence order, commitment binding, one commit formula with shared Rust/Solidity vectors, ADR-0011, spec fixes)
- [x] Loop 2 review: 14 more findings logged (THREATS.md, "M0 loop 2" section)
- [x] Loop 3: Registry (ADR-0012), ShardSpec + shard ids, mainnet-only BTC canon, separator grammar, quorum tolerance 0 + most-restrictive rights, commit-reveal challenge interface
- [x] Loop 3 review: 8 findings logged; dispute Draw rule escalated (3rd strike)
- [x] CI green on GitHub (private repo the private repository)

## Session 2 checklist
- [x] Loop 7: shared profile check, bounded LRU cache, stricter reproducibility checks (file set,
      untracked), golden bless guard in CI, discriminating shard asserts, test-rand recipe
- [x] Loop 8: researched engines (RESEARCH.md); wasmi 2.0.0 canonical, wasmtime opt-none advisory
      (ADR-0014); hostile reproducer now passes; benchmarks (docs/ops/benchmarks.md); caps from data
- [x] Loop 8 review: 6 findings (machine OOM changed verdicts, depth off-by-one between engines,
      br_table compile cost, engine-acceptance mismatch, panic guard, cache budget)
- [x] Loop 9: all 6 fixed with regression tests; HOST_ID onyx-host.0.5.0.wasmi.2.0.0; benchmarks updated
- [x] Loop 9 review: 6 findings (stack budget one frame short, fast-path compile blow-up on typed
      blocks, fast-path machine failure as verdict, translation OOM/panic, output allocation, test strictness)
- [x] Loop 10: all 6 fixed with tests; new caps measured; HOST_ID onyx-host.0.5.0.wasmi.2.0.0

## M1 checklist
- [x] Loop 4: registry root pinned in shards, metric registry with value kinds, instrument canon,
      restricted-quorum identity, challenge hash twin; BTC fixture (3 verified blocks, 11,615 txs),
      label set v1 (120 labels), btc-schema crate, fixture adapter, RecipeInput envelope + LabelSetView
- [x] Loop 5: wasmtime 49 host (no floats/SIMD/imports, fuel, instrumented call depth, memory limit,
      canonical failure leaf, sorted unique output, checked input assembly), SDK runtime (bump
      allocator, panic trap, `recipe!`), recipe source written; loop-4 review fixed (range pin,
      hardened fixture with PoW/links/second source, exact Fixed scale, canon vectors in root)
- [x] Loop 6: reproducible recipe builds (clean rebuild + other directory identical; CI job
      rebuilds on a clean runner), golden test on real blocks (repeat runs, serial/parallel
      compile, 4 threads, native = WASM, fuel <= 50%), Rust float and clock recipes rejected;
      loop-5 review fixed (table cap, frame-slot bound, size caps + cache, base coverage,
      base58 spelling, powLimit + txid Merkle + anchor, key order)

## M2 checklist
- [x] Loop 12: research (BIP-322 v2, `bip322` crate review, EIP-191/EIP-2, scoring sources); ADR-0015
      (address proof + registered entity-key signature); `AddressProof` + entity keys in the registry
      root; strict statement; BTC BIP-322 policy layer on all 57 official vector cases (not the 3 tx-hash cases); EIP-191 checked
      against Foundry `cast`; full confidence needs an attestation; serving gate (R-LBL-FIX-1)
- [x] Loop 12 review on 0d2df82: 9 findings (1 High: gate not on validator path), all fixed in loop 13
- [x] Loop 13: review fixes (gate in `assemble_input`, 24 root vectors, consensus limits, linear check
      at ~100 us/label, fixture renamed `test.*`); revocations in the registry (R-ATTEST-7);
      calibration scoring ADR-0016 (exact Brier, clamped log, no lookahead)
- [x] Loop 13 review on 0c1c1fe: 9 findings (scoring farmable, context not bound to shard, revocation
      griefing, DoS cap, root pinned outcomes only); all addressed in loop 14
- [x] Loop 14: `assemble_input(spec, ...)` derives range and domain from the shard and registry;
      failing labels dropped one by one; revocations undoable via key removal; root hashes full
      vectors (27); caps; scoring as a distribution over K entities with anti-farming rules
- [x] Loop 14 review on 5705482: 10 findings (1 High: output depended on the caller's attestation set)
- [x] Loop 15: review fixes (missing cited attestation = abstain; cited-only caps; K from registry;
      resolutions at own start; key retirement keeps revocations; source bytes checked; 16 MiB cap,
      worst case 8.8 s); label-set versions + diff; spread over 2 sets on real blocks (ADR-0017)
- [x] Loop 15 review on 518a854: 0 High, 4 Med, 5 Low; logged in THREATS.md, open. Fixing them is the
      first step of the next session (before M3 work).
- [x] Label set v2 (9,850 labels, 4 companies, first-party sources; fixtures/labels/PROVENANCE.md);
      merged after 2 reviews (pools dropped, per-row confidence, filed-address deny list)
- [x] Indexer bridge (ADR-0031): converter imports only rights-cleared indexer sources (none beyond v2 yet: wBTC excluded by its site's terms, cbBTC blocked on rights review), so no v3 fixture ships; Custodian kind supported; every v2 company queryable at /v1/metrics (level 4); "All labelled exchanges" line and per-company evidence levels on the site. Phase 2 candidates: docs/ops/indexer-integration.md
- [x] Indexer Phase 2 item 4: company-wide crypto-asset units from SEC EDGAR filings as a level-2 period feed (adapters/source-edgar; Strategy Inc and Riot; reviews SEC-R1 2 blocking fixed, SEC-R2 0 blocking; non-value round cap). Live observers need an operator User-Agent contact (THREATS SEC-4). Next level-2 item: publish non-value transcripts per date (SEC-R2-F1 TODO)
  - [ ] cbBTC reserve list: read and record Coinbase's terms (rights review), then add it to the converter's CLEARED list
- [x] Indexer Phase 2 item 5: blocks per mining pool by coinbase as a block metric (level 4: the pool table is a third-party mapping, AUD-2-F6; floored on chain by `OnyxRegistry.recipeMinLevel` (set with `registerMetric`, REG-F), AUD-2R-F3), not labels (ADR-0032; recipe btc-pool-blocks; allowlist of pool companies from bitcoin-data/mining-pools, MIT; "Who mined the blocks" on #/data; /v1/metrics/btc.pool-blocks). Reviews POOL-R1 (2 blocking hard-limit breaches: a real miner handle in a test, raw coinbases with miner handles in a fixture) fixed on fresh branch pool-tags-clean, so main never held them; reviewed allowlist (127 of 149 (one more excluded in review POOL-R2), decisions by id), payout = largest output. Branch pool-tags must never be merged; owner to delete it from the remote
- [x] Indexer Phase 2 rows 2 and 6 (ADR-0034, branch balance-recipe): recipe btc-entity-reserve, a chain of balance shards over label set v2 from a committed UTXO scan (block 971,199) rolled back to the week; the chain lands on the scan exactly; a second scan reconciles; Python recounts agree. Live: OKX PoR 2026-09-08 (5,956 addresses) and BITB wallet list match to the satoshi. Website #/reserves (both builds). Review RES-R1 (strike 1) fixed: base trust reworded (one node's scan; independent checks only OKX and BITB), `check_params_chain`, 21M BTC supply cap (new recipe id, 89 shards re-exported), per-address derivation check. Serving `btc.entity-reserve` needs first: RES-1 second-node re-scan (precondition), RES-7 API params-chain check (TODO M7 delivery); also RES-2 (dated list membership), RES-4 (confirmation depth); API metric not wired yet
  - [x] Served through the paid API (branch reserve-api, ADR-0034 Amendment 1): `btc.entity-reserve` behind a fail-closed serving check (`onyx_api::gate`, `reserve::serving_check`: expected params derived from the committed scan and the finalized rows, RES-7 fixed); rows served only for entities with an independent cross-check (OKX, BITB), others 403 (RES-1 precondition); reserve recipe floored at level 4 in OnyxRegistry; `onyx-node-btc`; demo-local step 8 on anvil; RES-9 fixed; #/reserves "Served through the API"
  - [x] Review RESAPI-R1 fixed (strike 1): gates claim by binding + `KNOWN_GATES` (F1), landing bound to the scan's block (F2), periodic refresh and keep-last-good (F3), `onyx-node-btc` Docker target and systemd drop-in with packaged fixtures (F4), fork gas re-measured (F5)
  - [ ] TODO (RESAPI-R1 F4 follow-up): publish an `onyx-node-btc` image build in CI once Actions billing is fixed (built and smoke-tested locally only: the shipped gate config loads in the image); the systemd drop-in is untested on a real host
  - [x] Client-side reserve check (RES-10, ROADMAP D3, ADR-0034 Amendment 2): `reserve::client_check`, `ClientNode::verify_row`, `/v1/served/{metric}`, `onyx_reserve_serve::client`, browser base derived on the week export; follow-ups D20 (browser on live answers), D21 (anchor on chain), D22 (dated sets after D1, span check from the landing; review D3-R2)
  - [x] Client check after D1 at lower cost (ROADMAP D22, ADR-0034 Amendment 3): `ReservePin::from_dated_scan` (dated set, history, flows file trusted by hash), window hash and window index per link, membership-change deltas, every link's tip and the pin on the client's header chain (6 confirmations), `derive_entity_span` from the landing (or an entity's exit/entry) shared with the browser, `MAX_LINKS` removed for a client budget (`DEFAULT_SPAN_BUDGET` 4,096 rows); THREATS D3-R2-2/3 fixed, D22-*
  - [ ] Second-node scan (or a signed scan from a second operator, RES-11) so Binance and BitMEX can be served. Confirmation depth for serving (RES-4) and date-ranged membership (RES-2): done in ROADMAP D1 (ADR-0037); the client check reads D1's dated sets since D22 (ADR-0034 Amendment 3)
- [ ] Mining-pool labels: only with a method that proves pool ownership (LBL-V2-F1 TODO)

## M2 acceptance
- [x] Attestations: BIP-322 address proof + registered entity key (ADR-0015); all official BIP-322
      vector cases; EIP-191 matches Foundry cast; revocation; validator path keeps only attested labels
- [x] Calibration: overconfident labeler scores worse (`score::tests::overconfident_scores_worse_than_calibrated`);
      Brier strictly proper on a grid (K = 2, 3, 10, 1000)
- [x] Versioning: versions linked by content hash, and diff (`version::tests`); manifest chain: L15-F7
- [x] Spread: OKX net flow over 2 label sets on real blocks, matched by an independent recount
- [x] Test-only fixtures cannot be served (R-LBL-FIX-1; wiring into delivery M7)

## Next (M2 Labels)
1. ~~Label set format; test-only flag for fixtures (R-LBL-FIX-1)~~: serving gate done loop 12; versioning in loop 14.
2. ~~BIP-322 first-party attestation flow with test keys; domain + expiry (R-ATTEST-1)~~: done loop 12 (ADR-0015).
3. ~~Calibration scoring~~: done loop 13 (ADR-0016). Was: Calibration scoring (Brier primary, log reported) in integer fixed point; test: overconfident wrong labeler scores worse.
4. Metric spread across >= 2 label sets, published per metric.
5. ~~Close R-HOST-STACK-2, R-HOST-COMPILE-2 and the small loop-6 items~~: done in loops 7-8 (ADR-0014).
6. Carry-overs: R-FIX-3 residual (raw txs or node check), R-HOST-STACK-1 aarch64 margin, R-SUBJ-3, R-REG-4 tooling.

## Questions for the owner
- q-P2g (2026-10-19, open; blocks ROADMAP P2g, already on the contact-addresses list): the SEC asks for a contact in the User-Agent and www.sec.gov refuses `onyx-dev (contact pending)`. Which role address (a team mailbox, never a person's own) should the SEC recorder send? With it, six more US funds' filings (BITB, HODL, BRRR, BTCO, BTCW, MSBT) can be read.
- q-C4-1 (2026-10-17, DECIDED (a) 2026-10-18 by the loop, ROADMAP C52, ADR-0027 amendment 15: built on branch tlock-far-date; the price, every sealed value public at its far date, is THREATS C52-7; clients unseal with C83; was: open; not blocking C11; C12 2026-10-17 designed option (a) as ADR-0027 amendment 12 item 8, build C52): sealed level-2 rounds stay sealed while their fact is open, so a coordinator that never closes a fact never unseals (THREATS C4-8). (a) C12 adds a second time-lock copy of each reveal to a fixed far date (the subject's last observable day), so every round unseals without the coordinator, publishing the values then (recommended for credibility; the value is old by then); (b) keep sealing at the coordinator's discretion and state it.
- q21 -> (b) park (loop decision 2026-10-11). (2026-10-11): level 4 review 4 (after q18 a) still finds cheap lies: a sybil self-challenge drawn in the same second locks out honest evidence (L4R4-F1), and Void is a weapon against truth (bribe + self-drain: liar -2,000 vs honest -30,000, L4R4-F2); plus a seat-reservation DoS (F3) and a generator that ignores evidence (F4). Each review finds a new mechanism-design hole. (a) another pass: no level-0 draw before challenge close, appellant evidence, pay winning appellants, no unbonding while reserved, queued draws, evidence-aware generator; then review 5; (b) park level 4 and revisit with a redesign (recommended: 4 reviews, new class each time; level 4 is not needed to go live).
- q20 -> (a) (loop decision 2026-10-11). (2026-10-11): level 3 review 4 (after q17 a) found the key-misuse family closed per key id but NOT per signing key (L3R4-F1: one private key registered under two sources stays active under one after compromise under the other). (a) narrow fix: one signer -> one registration (SignerTaken) + replay of equivocation reports refused + independent multi-source model; then review 5 (recommended); (b) park level 3.
- q19 -> (a) chosen 2026-10-11 ("Keep workiny"). (2026-10-11, 3-strikes): paid API level-2 'operator sets the price clock' failed 3 reviews (L2API-R1-F2 unbounded reveal close, R2-F2 up to 24 h, R3-F1 dated feeds: opener picks the window within the 10-day lag, up to ~8 days more paid time). (a) dated feeds age from the earliest window the lag allows for the record date (template + subject only), plus docs, `as_of` for fixed feeds; then review 4 (recommended); (b) accept 'observed late counts as fresh' as policy, document it as an accepted risk in pricing.md, merge.
- q18 -> (a) chosen 2026-10-11 ("Keep workiny"). (2026-10-10, 3-strikes): proof ladder level 4 'lying is cheap' failed 3 reviews (L4R-F1 sybil capture, L4R2-F1 sybil hedging, L4R3-F1 forced Void via bribed abstains / F2 seat exhaustion blocks appeals). Burn-first math holds for decided cases. (a) burden of proof (a challenged case that ends Void burns the asserter's share) + abstainers slashed in Void + full-ladder seat reservation at draw + property test covering Void/NoPanel/cross-case; then review 4 (recommended); (b) park level 4 on its branch.
- q17 -> (a) chosen 2026-10-11 ("Keep workiny"). (2026-10-10, 3-strikes): proof ladder level 3 key misuse failed 3 reviews (L3R-F1 retired-key backdating, L3R2-F1 reinstate revives retired key, L3R3-F1 auto-resolve frees a suspended self-contradicting key; retired/compromised part now CLOSED). (a) simplify: remove auto-resolve/auto-reinstate (reviewer-only recovery), suspend any contradicting joiner, gate register_key on Reviewer, multi-key exhaustive model; then review 4 (recommended); (b) park level 3 on its branch.
- q16 -> (a) chosen 2026-10-10 ("Yea keep working"); RESOLVED: merged after review 5 (receipt loss and intent DoS classes closed). (2026-10-10, 3-strikes): service packaging (branch ops-pack) - 'a paid settle can lose its receipt' failed 3 reviews (OPS-L1-F1, OPS-L2-F1, OPS-L3-F1/F2/F4; each narrower). (a) one more pass: durable-or-release receipts, Transfer at logIndex+1, validBefore cap + per-payer intent cap, block-pinned authorizationState; then review 4 (recommended); (b) merge the packaging with paid serving off (free/decayed data only) until fixed.
- q15 -> (a) chosen 2026-10-10 ("Yea keep working"); RESOLVED: level 2 merged after review 4. (2026-10-10, 3-strikes): proof ladder level 2 claim backing failed 3 reviews in a row (L2R-F3, L2R2-F1, L2R3-F1; each gap narrower). (a) bind a claim to the feed's metric, subject and window, then a 4th review (recommended); (b) park level 2 on its branch and go live with level 1 only.
1. ~~GitHub remote~~: done, private repo the private repository.
2. M1 fixture: which block range and which entities from the entity indexer should the first golden test pin?
   Default: a small recent range and 2-3 exchanges with public addresses.
3. x402: OK to use the community Rust crate `x402-axum` (Apache-2.0, unaudited) for testnet (ADR-0006)?
4. ~~Scoring rule~~: decided with the default in ADR-0016 (Brier primary, log clamped at 0.1% reported). Override if you disagree.
5. Bond token on Base Sepolia: testnet ETH or testnet USDC? (settlement.md)
6. ~~Loop-2 findings~~: closed in M0 loop 3.
7. ~~Dispute resolution~~: decided (a) 2026-10-09 (the owner: 'Go'): a randomly drawn bonded panel
   reruns the disputed shard, with appeals (ADR-0021). (b), an on-chain proof of the rerun, stays the
   mainnet path. Original question: dispute resolution had failed review 3 times (R-DRAW-1/2/3: bribed resolvers can force a "Draw").
   Options: (a) keep k-of-n resolvers and harden in M5 with simulation; (b) skip resolver votes and
   settle objective disputes by an onchain proof (zk or optimistic bisection), slower to build.
9. ~~Native-stack determinism~~: decided 2026-10-08 under the owner's delegation: wasmi canonical,
   wasmtime opt-none advisory (ADR-0014). Original question: native-stack determinism had failed review twice (R-HOST-STACK-1, then R-HOST-STACK-2).
   Options: (a) keep wasmtime/Cranelift but turn optimizations off and set measured, much tighter
   size and depth caps (fast, keeps speed reasonable); (b) compute verdicts on a WASM interpreter
   with an explicit, counted stack (fully deterministic by construction, slower).
   Recommendation: (b) for dispute verdicts, (a) for everyday validation, both must agree.
10. Validator input (third review in a row on this path: L12-F1, L13-F3, L14-F1). Current design:
   the label set pins the hash of every cited attestation; a party without the bytes abstains; M3 must
   make cited attestations available in content-addressed storage before a round opens.
   Options: (a) keep this, enforce availability in M3 (no format change); (b) also pin an attestation
   bundle hash in `ShardSpec` (changes the commitment binding, ADR-0011, and the Solidity vectors).
   Recommendation: (a), it is already fully pinned by the label set hash.
   DECIDED 2026-10-08: (a), under the owner's "just keep going" (his /loop message); M3 enforces availability.
11. DECIDED (a) 2026-10-09 (the owner: 'Yes continue with work' after the recommendation); built in
   loops 25-26 (ADR-0019 items 9-17). Original question: M4 settlement trust (THIRD STRIKE, 2026-10-08: reviews of loops 22, 23, 24 each broke settlement;
   latest: L24-F1..F8 in THREATS.md). The coordinator can still steer who is judged right: it can
   force the recompute path and supply the recompute result, list a known miner as a canary by passing
   an empty ledger, and choose the sampling config and beacon.
   Options: (a) settle computes the truth itself from the pinned inputs (no callback), and config,
   known-miner snapshot and canary deadline are fixed when the round opens; then M5 adds bonded
   challenges as planned. About 1-2 loops. Trust left: the settler's own node data (validators use
   their own nodes; a lying mirror shows up as a content-hash mismatch).
   (b) make every settlement that is not unanimous provisional (escrow) until a bonded challenge window
   ends, and build the M5 dispute game now as part of M4. Closer to the north star ("bonded challenges
   settled by deterministic recompute"), about 3-4 loops, and it needs q7 decided.
   Recommendation: (a) now, then (b)'s challenge window as M5.
8. Fixture labels: OKX PoR addresses and Bitwise BITB addresses carry no stated license. OK to use
   them in test fixtures in this private repo? (They are public addresses published for verification.)
   2026-10-18 (P2a): the same question now covers seven more issuers in label set v3 (Bybit,
   Bitfinex, HTX, CoinEx, MEXC, Crypto.com, Deribit; DATA-LICENSE section 1, data-sources S16-S23):
   their rosters state no licence and their site terms are unread. wBTC (terms forbid) and cbBTC
   (terms unread) stay out.

12. DECIDED (a) 2026-10-09 (the owner: 'Yea continue working' after the recommendation); done in
    loop 28 (ADR-0019 items 22-25 and "Handed to M6"). Original question: M4 off-chain settlement (2026-10-09, after loops 25-27): root selection is sound (reviews of
    loops 26 and 27 agree), but every loop's payout fixes opened new coordinator paths (L27-F1..F4),
    because in an off-chain library the coordinator controls every call, every ledger copy and the
    order of calls. Options: (a) freeze M4 here: apply the two small L27 fixes (ledger id in the
    round tag, count activity only before verdict close), merge, and make "one canonical ledger and
    call order" a requirement of the M6 settlement contract, where it is enforced by the chain;
    continue to M5/M6. (b) keep looping on the off-chain model until a review is clean.

13. M5 "Stands" outcome (2026-10-09, third strike: L30-F1, L31-F1, L33-N1). When the last panel
    has no majority, every rule for who is slashed has opened a hole: slash the silent -> griefers
    slash honest members; slash nobody -> bribed minorities protect a wrong root for free.
    Options: (a) remove Stands as a final answer: a split final panel (or no panel) always sends the
    shard to a protocol rerun by the settler (rerun_shard), which decides; the panel is only the fast
    path; funders' fees burn as now; nobody is slashed for a split. About 1 loop. Trust left: the
    settler's rerun (in M6 the contract cannot run WASM, so the rerun result is posted by the settler
    and is itself challengeable). (b) keep Stands and accept L33-N1 as a documented residual for the
    testnet. Recommendation: (a). DECIDED (a) 2026-10-10 (the owner: 'Just keep doing what we need
    to do' after the recommendation). Done in M5 loop 46 (ADR-0021 loop-46 amendment).


## M6 checklist (settlement contracts, ADR-0022)
- [x] Loop 32 (M6 loop 1): OnyxSettlement canonical ledger (registry with locks and unbonding,
      coordinator bond, reward pool, escrow with pull withdrawals, round lifecycle by block
      number, signed relayable commits, reveals, blockhash beacon with 256-block void, verdict
      commit/reveal, canaries with deadline, unanimous finalization, pending voiding, finalized
      roots + verifyRow); Rust/Solidity vectors; 29 lifecycle + 8 vector tests. Not deployed.
- [x] Loop 34: M6 loop 32 review fixed (M6-F1..F9: seed required to settle, zero commit hash
      refused, one Agree + minimum windows/members/deterrence, earliest commit paid, contract
      snapshots its own set, lazy per-shard void, bounded registry with eviction, chain id in
      the ledger id). Regression tests in contracts/test/OnyxSettlementRegression.t.sol.
- [x] Loop 35: M6 loop 34 review fixed (M6-L34-F1..F6: O(members) settle via verdict tallies,
      settle window ends at pendingClose then void, unbonding after slash-below-minimum, canary
      deadline needs a seed, linear set root + quicksort, real reward-order vector); F7 -> loop 2.
- [x] Fresh review of loop 35 (0 merge-blocking; M6-L35-F1..F6).
- [x] Loop 36: M6 loop 35 review fixed (round span cap, unseeded-void call order, heapsort,
      slash-cascade gas tests, differential tally fuzz); deploy script + runbook
      (`contracts/script/Deploy.s.sol`, docs/ops/deploy-base-sepolia.md). Core contracts done.
- [x] Loop 37: M6 loop 36 review fixed (EIP-170: via_ir, 21,716 B, CI size gate; deploy script
      checks the node's chain id; broadcast logs ignored; runbook subshell; override range checks;
      seed voids from verdictCommitClose).
- [x] Loop 44: unrevealed-verdict slashing matches Rust (round-level, any shard outcome;
      Rust-produced vector) (M6-L34-F7); seed needs a full verdict window (M6-L37-F2).
- [x] Loop 51 (M6 loop 2, ADR-0024): `OnyxDisputes` on chain (rerun cases for Pending shards,
      challenges with committed claims, appeals, coordinator rerun, expiry; judging as Rust
      `settle`); contract split for EIP-170 (`OnyxLedger` base, `OnyxSettlementExt` by
      delegatecall, linked `OnyxRound`); held payouts until final; stake lock through the dispute
      tail. Closes M6-F10, M6-G2, M6-G3, M6-L44-F1, M5-L49-F1, M5-L49-F3, M5-L49-F5; Rust
      vectors for config validity, draws, hashes, verdict rule; IOnyxSettlement sketch removed.
- [x] Fresh adversarial review of loop 51 (1 merge-blocking: M6-L2-F1, fixed in loop 52 with
      F2-F6: panel pool, `MAX_PANEL` 63, appeal floor, settle margin).
- [x] Fresh review of loop 52 (0 merge-blocking); loop 53 fixed M6-L3-F1..F4 (live shards,
      claim grace, silent-panel fees). Merged to main 2026-10-10.
- [x] Rust seed aligned to ADR-0022 (M6-G4): `sample_seed(tag, beacon)` uses `onyx-seed-evm-v1`;
      `Ledger::mirror_of_contract`; `evmSeed` vectors, forge derives the tag. Fresh review 0
      merge-blocking; lows M6-G4R-F1..F3 fixed. Merged 2026-10-10. M6 code complete.
- [x] Deploy to Base Sepolia (done 2026-10-12 by `scripts/demo-testnet.sh testnet`; funded via the CDP faucet).
- [x] Fresh adversarial review of loop 32 (3 merge-blocking, all fixed in loop 34).
- [x] Fresh adversarial review of loop 34 (1 merge-blocking, fixed in loop 35).
- [x] Fresh review of loop 36 (1 merge-blocking for the deploy: EIP-170 size; fixed in loop 37).


## M7 checklist (delivery and x402, ADR-0023)
- [x] Loop 38 (M7 loop 1): `onyx-api` on axum 0.8: `/v1/health`, `proofs`, `metrics`, `export`,
      `settlement`; rows with proofs from the CAS by finalized root; `RootSource` (`LedgerRoots`,
      `ChainRoots` over a mockable `JsonRpc`); rights at serve time (451); stable JSON error codes;
      ETag/304; step time-decay pricing; x402 v2 headers and types, `Facilitator` trait
      (`MockFacilitator`, `HttpFacilitator<HttpPost>`). Real-block end to end on an M4-settled
      round (`adapters/source-btc/tests/api.rs`) and a Rust/Solidity settlement vector
      (`fixtures/api/settlement-vector.json`, `OnyxApiVector.t.sol`). Demo server
      `examples/serve-api.rs`, run book docs/ops/run-api.md.
- [x] Fresh adversarial review of loop 38: THREATS.md "Review findings, M7 loop 38" (M7-F1..F10).
- [x] Loop 39 (M7 loop 2): all ten findings fixed (ADR-0023 amendment). Verification bundle in
      every response and `onyx_api::client::verify_response`; cache policy (`immutable` only for
      root-pinned URLs); price and 402 before any row read, root and tree caches, global
      in-flight limit and deadline, export cap; facilitator deadlines, per-address rate limit,
      local EIP-3009/EIP-712 signature check (server and mock); identical-retry store;
      `ChainRoots::connect` (chain id, code hash, `finalized` tag); JSON rejections, 404, 405;
      canonical URLs (308); metric fallback; price errors are 500. Also: payment bound to the URL,
      export priced per row (R-PAY-3), decay from finalization time (R-PRICE-1), `test.*` label
      sets refused without `--dev` (R-LBL-FIX-1, test-only half), 13-leaf Solidity vector.
- [ ] Fresh adversarial review of loop 39.
- [x] M7 loop 3 (branch `m7-loop3`, 2026-10-13; ADR-0023 Amendment 2, ADR-0033 Amendment 6):
      client checks roots on its own node by default (`client::ClientNode::verify`;
      `verify_response(…, None)` now refused, opt-out `verify_response_unchecked`; IDX-R5-3);
      `authorizationState` + token-log check after an unknown facilitator settle, one re-send of
      the same authorization (M7-F5 residual, `settle_check`, wired in onyx-node facilitator
      mode); review items RESAPI-R2-1/2, IDX-R6-1/2/3, REVLOG-R13-4 fixed; #/api note on the
      client check. Already done before: HTTP transports (`HyperPost`, `HttpJsonRpc`), trusted
      forwarded-for (M7-L48-F2, loop 50), evidence half of `check_servable` (ADR-0033).
      Open: client-side reserve serving check (RES-10, M8; done 2026-10-17 as ROADMAP D3), second independent node for
      `ChainRoots`/`ClientNode` (IDX-R5-3, M7-L3-4); fresh review of this loop.
- [x] Review M7L3-R1 of M7 loop 3 (merged, 0 merge-blocking; branch `m7-loop3-fixes`): lagging-node
      settle (re-check wait, refused-as-used re-send reads the logs, identical retry runs the check
      instead of 409), rescan resume bound to the held set, `NotFinalOnChain` "retry later",
      client read-order test, #/api real state (THREATS M7L3-R1-1..5). Open: fresh review.
- [x] Indexer that fills the catalog from `OnyxSettlement` events (ADR-0033, branch `chain-indexer`,
      2026-10-12): finalized blocks only, capped ranges split on error, overlap re-read, cursor file;
      a shard is served only when `getFinalizedRoot` agrees with an event and the held rows, binding
      and label-set evidence check out (also the evidence half of `check_servable` from M7 loop 3);
      `onyx-node` `ONYX_INDEXER=on`, `held` catalog entries, manifest `deployBlock`; demo-local finds
      its shard by event; live read of the 2026-10-12 Base Sepolia deployment found rounds 1 and 2.
      Reviewed (IDX-R1); its five findings fixed on branch `indexer-fixes` (THREATS IDX-R1-1..5,
      ADR-0033 Amendment 1); the run page shows the read (`scripts/read-indexer-sepolia.py`).
      Follow-ups: CIDX-5 residual (a backend lagging more than `overlap` blocks; JSON-RPC batch per
      range), CIDX-7 residual (state file keeps every event), IDX-R1-2 residual (other operators'
      shards in an oversized block listed only when the overlap reads it whole), run `onyx-node`
      with `ONYX_INDEXER=on` against Base Sepolia (not done yet; only read-only scans).
- [x] Audit AUD-2 fixed and merged (2026-10-11): public website build ships no label rows.
- [ ] Before any public link: publish only an `ONYX_PUBLIC=1` build and a role contact for label removal (docs/ops/deployment.md).

### M7 open questions
- q-M7-1 decided (a) 2026-10-10 (the owner: 'Just keep doing what we need to do' after the
  recommendation): the public x402.org facilitator (`https://x402.org/facilitator`, Base
  Sepolia, no sign-up). Default in `x402::facilitator_url`; tests use the mock only.

14. DECIDED (a) 2026-10-10 (the owner: 'Just keep doing what we need to do' after the
    recommendation); implemented in M7 loop 47 (Expect::Metric.min_as_of, delivery.md
    §Verification). Original question: M7 metric "latest" (2026-10-10, third strike on client
    binding: L39-F2, L40-F1, L41-F1).
    Proof, export and settlement answers are checkable without trusting the server. A metric
    query ("latest value for entity X") is not: one response cannot prove it is the newest shard,
    or that a subject is absent. Options: (a) make it explicit: the client pins a metric
    definition (recipe, schema, params, label set, chain, window) and a min_as_of; recency and
    absence are documented as not proven; anything that settles money pins (round, shard) like
    the settlement endpoint. About 1 loop. (b) add an on-chain index in OnyxSettlement of the
    latest finalized shard per metric, so "latest" becomes provable. About 2 loops plus a
    contract review. Recommendation: (a) now, (b) later if a customer needs provable recency.

### Level 5 track record open questions (ROADMAP C1, ADR-0036)
- q-TR-1: are ADR-0016's thresholds (30 resolved over 5 distinct subjects) right for data
  sources, and should the protocol carry a Wilson lower bound (integer square root, a fixed z)
  instead of the plain count threshold? Default until decided: reuse ADR-0016's numbers, counts
  only below them.
- q-TR-2: who may add a statement under a source (the intake): the source's level-3 key
  (`SignedSource` reports at level 5), a registered contributor key, or both? Decided (C5,
  ADR-0036 amendment 1 item 1): the source's registered key only, the same key id as level 3.
- q-TR-5: should a record carry a skill score against a naive baseline (the previous resolved
  value, persistence) so a source cannot look good on easy facts inside a metric (THREATS
  TR-I-5)? Decided (C9, ADR-0036 amendment 4 item 2): yes, beside the counts, never instead:
  persistence (the last value known at each close, not the source's own or its operator's
  answer), the difference of agreement rates in ppm, shown only when established.
- q-TR-3: should resolutions be read only from on-chain facts (`OnyxRegistry` level-2 facts,
  `verifyRowLeveled` rows, accepted level-3 reports) so a coordinator cannot omit one? Decided
  (C7, ADR-0036 amendment 3): yes, in chain mode (`TrackParams.chain`): every client rederives the
  resolutions from `OnyxSignedBook` settleable reports and `OnyxRegistry` observations at the
  commitment's head (`onyx_track::chain::verify_resolutions`); level 1 is refused (fail closed)
  until a level-1 reader exists (C40).
- q-TR-4: should level 2 join the default resolver levels once level-2 disputes exist (as for
  the money default)? Default: no until then.
