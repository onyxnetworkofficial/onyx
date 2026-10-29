# RESEARCH: prior art

## U6 market frontend boundary (2026-10-22)

The October 20 a partner call distinguishes two proposals: stake/bond incentives for checking submitted
data, and forecasting a future data observation. These are different mechanisms. The call raised
wealthy-attacker/manipulation and duplicate-reward concerns for validity staking, and described a
full futures system as future work rather than a settled launch requirement. See the timestamped
recording transcript at `<local path>`
(21:52–24:15, 26:02–32:20, 33:01–38:49, 44:32–49:20, 56:10–56:39).

For resolution-system prior art, Polymarket's first-party UMA CTF adapter repository documents the
adapter between Conditional Tokens and UMA's optimistic oracle, including proposals and disputes:
[Polymarket UMA CTF adapter](https://github.com/Polymarket/uma-ctf-adapter). The product page must
not suggest that Onyx already has an equivalent oracle, market, collateral, or redemption path.
U6 therefore builds a local interactive concept preview only; it gives no quote and submits no order.

## U2: durable local review (2026-10-21)

- [Tokio blocking tasks](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html): started
  blocking work survives cancellation. Admission, durable append and live index publication must
  stay together, rather than updating the index after awaiting the worker.
- [Rust file locks](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock): retain the
  exclusive journal writer. Stop the node before local review; ADR-0039 records this operational
  constraint. No concurrent edits to the live journal.


Every claim links a source. "Unconfirmed" means no primary source was read; do not
cite those items until checked. Add findings here at the start of each loop
(CLAUDE.md, loop step 1). Decisions that follow from this table live in docs/adr/.

# Contributor-supplied CSV/JSON samples (U3, 2026-10-22)

- [RFC 4180](https://www.rfc-editor.org/rfc/rfc4180) documents the common CSV format, quoting,
  records and MIME type. The intake accepts UTF-8 CSV with quoted fields and consistent row width;
  it treats CSV as plain data and never opens it in a spreadsheet runtime.
- [RFC 8259 §4](https://www.rfc-editor.org/rfc/rfc8259#section-4) says object member names should
  be unique and describes implementation differences when they are repeated. Intake rejects
  duplicate keys after escape decoding so review and client parsing agree.
- [OWASP CSV Injection](https://owasp.org/www-community/attacks/CSV_Injection) describes formula
  execution risks when CSV is opened by spreadsheet software. Intake rejects formula-like CSV
  cells rather than attempting to sanitize a value into a different dataset.

U3 therefore uses small exact-text samples (8 KiB), hashes the original bytes, and refuses obvious
contact, personal-record, markup and credential patterns. The patterns are safeguards only; they
cannot establish rights or prove that a dataset contains no personal information, so reviewer
approval remains mandatory.

## Loop log

- 2026-10-27, NL17 and NL18 (lane B, research only): candidate sources probed read-only (NCEI PRCP and a second station, a regional USGS count, BLS CPI v1, EIA keyed), parametric insurance prior art; section "More datasets and parametric insurance" at end; drafts docs/spec/more-datasets.md and parametric-insurance.md.
- 2026-10-27, dataset backing (S12, NS12 input): The Graph curation, token-curated registries, Numerai staking, EigenLayer deallocation delay; section "Dataset backing (S12)" at end; draft docs/spec/dataset-backing.md.
- 2026-10-27, north-star loop lane B batch 2 (NL3, NL4, NL9, NL13): estimates apart from checked data, a public forecaster for the Denver high (api.weather.gov terms), k-anonymity and Census tabular disclosure methods for the bounty board; section "Lane B batch 2" at end; drafts docs/spec/estimates.md, track-records-served.md, bounty-board.md, ladder-slice-2.md.
- 2026-10-27, north-star loop lane B (NL8, NL10, NL11): pay-per-request oracles (Pyth update fee, Chainlink Functions reservation), history expiry (EIP-4444) and storage classes (Amazon S3), TLSNotary's state today (alpha.15, tlsn-extension, TLS 1.2 only); section "Lane B: paid observation, storage tiers, browser TLS prover" at end; drafts in docs/spec/paid-observation.md, storage-tiers.md, browser-tls-prover.md.

- 2026-10-21, prediction-market follow-up: reread call 29:34–38:18, 44:32–49:20 and 56:10–56:39; checked [Polymarket resolution](https://docs.polymarket.com/concepts/resolution) and [UMA adapter](https://github.com/Polymarket/uma-ctf-adapter). Separate trading from outcome resolution and data-validity staking. Proposed binary testnet market design, without a native-token oracle assumption, is in PRODUCT-PLAN.md; numeric futures require a further margin/liquidation design.

- 2026-10-21, product U1: compared the full October 20 call with the existing [submission spec](docs/spec/submissions.md) and [S1/S2 research](docs/research/2026-10-20-api-keys-zk.md). Reused existing classification, rights and key boundaries; separated contributor entry from verification tools. [PRODUCT-PLAN.md](PRODUCT-PLAN.md) maps requirements to timestamps and distinguishes call evidence from proposed implementation.

- 2026-10-20, ROADMAP S1/S2: data submission and bonded claims (UMA OOv3, Chainlink, Pyth publishers and CO-PIP-99, Dune and Kaggle uploads, Polymarket on UMA), section "Data submission and bonded claims" at end; keyed APIs without sharing the key (TLSNotary, DECO, Reclaim, Opacity, zkPass, Pluto; API terms) in docs/research/2026-10-20-api-keys-zk.md.
- 2026-10-18, ROADMAP C21, C23: paged track logs (hash chains versus Merkle consistency proofs) and serving a registry whose rules are code, section "Paged track logs and the served registry (C21, C23)" at end.
- 2026-10-17, ROADMAP C9: track-record follow-ups (Bitcoin block-time rules, BIP 113, persistence skill scores, receipts), section "Track-record follow-ups (C9)" at end.
- 2026-10-17, ROADMAP C7: track records on chain (CT anchors, EIP-1898 pinned reads, Sybil/affiliate review, hash-bound preimages), section "Track records on chain" at end.
- 2026-10-17, ROADMAP C1: track records for level 5 (Chainlink, Pyth, UMA, API3, Witnet, peer prediction, Sybils, Brier partition, Wilson), section "Proof ladder level 5" at end.
- 2026-10-08, loop 8: deterministic stack options, wasmi 2.0, Cranelift OptLevel::None, NEAR/Polkadot/Stylus/CosmWasm approaches (section at end).
- 2026-10-06, M1 loop 5: wasmtime 49 config, wasmparser feature flags, stack instrumentation crates, Rust wasm codegen and reproducibility (section at end).
- 2026-10-06, M0 loop 1: initial survey (table below), x402 Rust status, wasmtime
  determinism options, Borsh vs deterministic CBOR, BIP-322 crates, EVM hash gas.

Convention: every claim has a URL. "unconfirmed" = I could not verify it from a primary source.

## 1. Table

| Project | What it does | What Onyx reuses | What Onyx builds itself | Links |
|---|---|---|---|---|
| The Graph | Indexers post Proof of Indexing (POI); Fishermen dispute with deposit; an Arbitrator (governance-assigned) reproduces the work and rules. Slash max 10% | Dispute-by-reproduction, fisherman deposit, "draw" outcome for nondeterminism bugs | Arbitrator is replaced by deterministic WASM recompute; no human ruling | [Docs](https://thegraph.com/docs/en/indexing/overview/), [GIP-0009](https://github.com/graphprotocol/graph-improvement-proposals/blob/main/gips/0009-arbitration-charter.md) |
| UMA OOv3 | Assert-then-challenge with bonds and liveness; disputes go to DVM token vote | Bond + challenge window + disputer reward | Final arbiter is recompute, not vote | [How it works](https://docs.uma.xyz/protocol-overview/how-does-umas-oracle-work), [OOv3 quick start](https://docs.uma.xyz/developers/optimistic-oracle-v3/quick-start) |
| Pyth | First-party publishers send price+confidence; on-chain median aggregate; Oracle Integrity Staking (stake, slash) | First-party-publisher framing; confidence field; median of votes | Scoring by calibration instead of stake-slash; BIP-322/EIP-712 self-attestation | [Aggregation](https://docs.pyth.network/price-feeds/pro/understanding-price-data), [OIS](https://docs.pyth.network/oracle-integrity-staking) |
| Space and Time | Proof of SQL: ZK proof a SQL query ran correctly on committed data | Idea: commit data, prove query vs commitment (later option) | WASM recompute by validators (no ZK) in v1 | [Repo](https://github.com/spaceandtimefdn/sxt-proof-of-sql), [Announcement](https://decrypt.co/235003/space-and-time-releases-sub-second-zk-prover-under-open-software-license) |
| Covalent / GoldRush | Block Specimen Producers export canonical block data, publish hash to ProofChain contract; auditors check producers | Canonical-data-plus-hash-onchain pattern; audit of producers | Metric recipes + entity labels (not raw blocks) | [Network pipeline](https://www.covalenthq.com/docs/cqt-network/network-pipeline/), [BSP repo](https://github.com/covalenthq/bsp-geth) |
| ZK coprocessors (Brevis, Axiom, Herodotus, Lagrange) | Prove historical chain state / computation over it in ZK | Concept: verifiable compute over chain data | Does not need ZK; validators rerun WASM | [Herodotus](https://docs.herodotus.dev/), [Axiom](https://docs.axiom.xyz/), [Lagrange](https://docs.lagrange.dev/lpn/zk-coprocessor/overview), [Brevis (secondary)](https://blockeden.xyz/blog/2026/02/07/zk-coprocessor-technology-brevis-verifiable-computation/) |
| Bittensor | Validators score miners; Yuma Consensus aggregates weights; commit-reveal vs weight copying | Commit-reveal of scores/labels | Scoring by objective recompute + calibration | [Taostats](https://taostats.io/docs/concepts/validators/weight-copying), [Commit reveal docs](https://docs.learnbittensor.org/concepts/commit-reveal) |
| Vana | Data DAOs / DLPs; Proof of Contribution validated in TEEs (Satya) | Per-dataset contribution scoring idea | No TEE; recompute + calibration | [PoC](https://docs.vana.org/docs/proof-of-contribution), [DLP](https://docs.vana.org/docs/data-liquidity-pools-dlps) |
| Arkham Intel Exchange | Bounties for wallet identity labels, paid in ARKM | Bounty demand signal for labels | Calibration scoring, entity self-signing, no personal-doxxing bounties | [Cointelegraph](https://cointelegraph.com/news/arkham-ceo-defends-controversial-info-sharing-marketplace), [Decrypt](https://decrypt.co/148410/public-good-panopticon-arkhams-dox-earn-marketplace-cuts-heart-crypto) |
| x402 | HTTP 402 pay-per-call; client, resource server, facilitator | Use directly (Rust x402-axum) | Pricing, metering, Base settlement config | [Repo](https://github.com/x402-foundation/x402), [x402-rs](https://github.com/x402-rs/x402-rs), [CDP docs](https://docs.cdp.coinbase.com/x402/network-support) |
| Chainlink PoR | DON of node operators reach OCR2 consensus on reserve attestations from custodian/auditor | Contrast: attestation-based, not recompute | Recompute-verified balances | [Docs](https://docs.chain.link/data-feeds/proof-of-reserve), [Spark summary (secondary)](https://www.spark.money/research/stablecoin-real-time-attestation-chainlink) |
| Truebit | Solver/verifier game with forced errors and jackpot | Canary tasks = forced errors | Canaries inside miner/validator workloads | [arXiv 1806.11476](https://arxiv.org/pdf/1806.11476), [arXiv 1908.04756](https://arxiv.org/pdf/1908.04756) |
| Arbitrum BOLD | Bisection to one-step proof, all-vs-all, bounded delay (7 days upper bound) | Bisection to single step (dispute by recompute) | Recipe-level dispute, WASM step | [BOLD docs](https://docs.arbitrum.io/bold/concepts/bold-technical-deep-dive), [bisection](https://docs.arbitrum.io/how-arbitrum-works/bold/how-bold-bisection-works) |

## 2. Lessons for Onyx

**The Graph.** POIs are checked by reproduction ([GIP-0009](https://github.com/graphprotocol/graph-improvement-proposals/blob/main/gips/0009-arbitration-charter.md): "The Arbitrator must reproduce work"). Lessons: (a) a dispute needs a deposit (docs say min 10,000 GRT, rejected disputes burn it; [docs](https://thegraph.com/docs/en/indexing/overview/)); (b) the charter has Draw outcomes for software determinism bugs, a statute of limitations, and double-jeopardy rules. Onyx should define these up front: recipe nondeterminism = draw, no double slash for one fault, dispute window cap. Human arbitrator is the part Onyx replaces with recompute.

**UMA.** Assertion + bond + liveness window; disputes escalate to a DVM vote by UMA holders ([docs](https://docs.uma.xyz/protocol-overview/how-does-umas-oracle-work)). Votes are a Schelling game and fit subjective questions. For objective claims (a balance at a block) a vote adds a bribery/capture surface and no accuracy. Onyx keeps bond + window but ends disputes by deterministic recompute only. Note OOv3 default liveness in the quick start is 120 s ([quick start](https://docs.uma.xyz/developers/optimistic-oracle-v3/quick-start)); Onyx must pick a window longer than validator rerun time.

**Pyth.** First-party publishers; aggregate is a median of votes (price, price +/- confidence); confidence = max distance to 25th/75th percentile ([Pyth docs](https://docs.pyth.network/price-feeds/pro/understanding-price-data)). Lesson: first-party sourcing is a quality argument, which maps to BIP-322/EIP-712 self-signed addresses. OIS: stake + slashing remained active but reward rate was set to 0 on 22 April 2026 ([OIS docs](https://docs.pyth.network/oracle-integrity-staking)); Pythnet is being shut down ([docs](https://docs.pyth.network/price-feeds/core/how-pyth-works/price-aggregation)). Lesson: stake-based incentive economics are fragile when token-funded; Onyx revenue should come from x402 fees.

**Space and Time.** Proof of SQL proves a query ran correctly on untampered data ([announcement](https://decrypt.co/235003/space-and-time-releases-sub-second-zk-prover-under-open-software-license)). It proves computation against committed data, not that the committed data is true. Same gap Onyx has: Merkle root on Base proves what was accepted, not that miners were honest. License is "Cryptographic Open Software License 1.0" by Makeinfinite Labs ([LICENSE](https://raw.githubusercontent.com/spaceandtimefdn/sxt-proof-of-sql/main/LICENSE)), GitHub shows NOASSERTION; not MIT/Apache, so check before reuse.

**Covalent.** Producers publish the hash of a Block Specimen plus an IPFS URL to the ProofChain contract; auditors check producers; BSP minimum stake 175,000 CXT ([pipeline](https://www.covalenthq.com/docs/cqt-network/network-pipeline/) via search summary). Lesson: publish hash + retrievable blob so anyone can recompute; availability of the data is part of the design. Exact proof/slash details unconfirmed (docs pages blocked).

**ZK coprocessors.** Herodotus: storage proofs (inclusion + computation proofs) across layers ([docs](https://docs.herodotus.dev/)). Axiom: hosted ZK proving for OpenVM programs ([docs](https://docs.axiom.xyz/)). Lagrange: SQL-style verifiable database over contract storage, indexed per block ([docs](https://docs.lagrange.dev/lpn/zk-coprocessor/overview), via search summary). Brevis: ZK data coprocessor + Pico zkVM ([secondary](https://blockeden.xyz/blog/2026/02/07/zk-coprocessor-technology-brevis-verifiable-computation/)). Lesson: ZK removes need for re-execution but costs proving infra; for exchange/ETF balances, source data is off-chain or custodial, which ZK of chain state cannot cover. Keep WASM recompute; consider ZK later for the on-chain-state subset only.

**Bittensor.** Weight copying: validators copy consensus instead of evaluating, because Vtrust rewards agreeing with others; mitigation is commit-reveal, introduced in Bittensor 7.3.0 per Taostats ([Taostats](https://taostats.io/docs/concepts/validators/weight-copying)); implemented with drand time-lock encryption per search summary of [commit-reveal docs](https://docs.learnbittensor.org/concepts/commit-reveal). Lesson for Onyx: any validator vote or label-score that is visible before finalization will be copied. Use commit-reveal for validator attestations and for miner label submissions, and ground truth in recompute so "agreeing with others" is never the reward.

**Vana.** Each DLP defines its own Proof of Contribution function; recommended validation is Satya network of TEE nodes (Intel TDX) ([docs](https://docs.vana.org/docs/proof-of-contribution), via search summary). Example: r/datadao used karma plus an ownership check via code posted in profile. Lesson: ownership check by posting a code is analogous to our signed-message attestation; but TEE trust is hardware-vendor trust, which Onyx avoids.

**Arkham.** Bounties for identity labels drew "snitch-to-earn/dox-to-earn" criticism in July 2023 and fears of smear campaigns and false accusations; CEO said bounties require approval and focus on large trading firms and hackers ([Cointelegraph](https://cointelegraph.com/news/arkham-ceo-defends-controversial-info-sharing-marketplace)). Also a referral-link email leak ([same](https://cointelegraph.com/news/arkham-ceo-defends-controversial-info-sharing-marketplace)). Lesson: restrict Onyx labels to entities (exchanges, ETFs, funds, protocols), not individuals; let entities self-attest; add a dispute/removal path; score by calibration so false labels cost reputation. Whether Arkham's bounty mechanism has false-label penalties: unconfirmed.

**x402.** See facts below. Lesson: protocol and SDKs now live under x402-foundation; pin versions, expect v2 breaking changes (v1 `x402-express` vs v2 `@x402/*`).

**Chainlink PoR.** Custodian/auditor signs attestation; a DON of independent nodes reaches OCR2 consensus and publishes ([secondary](https://www.spark.money/research/stablecoin-real-time-attestation-chainlink); [official page](https://docs.chain.link/data-feeds/proof-of-reserve) could not be read by my fetch). Lesson: this is attestation-trust; Onyx differs by letting anyone recompute from public chain data, and treating entity signatures as first-party evidence, not as the sole truth.

**Truebit.** Forced errors: a Solver is made to offer a wrong answer occasionally (reported ~1 in 1000 tasks) with jackpot payouts to Verifiers who catch it, so verifiers stay motivated even when solvers are honest ([arXiv 1806.11476](https://arxiv.org/pdf/1806.11476), figures via search summary; verify in paper before citing the 1/1000 number). Lesson: Onyx canary tasks keep validators from rubber-stamping (the same lazy-validator problem as weight copying).

**Arbitrum BOLD.** Disputes bisect until one step, verified by a one-step proof; BOLD bounds delay (7 days extra) and resists delay attacks ([BOLD deep dive](https://docs.arbitrum.io/bold/concepts/bold-technical-deep-dive)). Lesson: bound dispute time and cost regardless of attacker count; for Onyx a recipe is small enough to rerun whole, but include a bisection fallback by trace checkpoints if recipes grow, and an upper bound on dispute duration so a griefing attacker cannot delay finality forever.

## 3. Technical facts

**x402 (item 10).**
- Roles: client, resource server, facilitator. Source: [x402 repo](https://github.com/x402-foundation/x402) (coinbase/x402 redirects here; the old Coinbase repo says it moved under x402 Foundation governance).
- Official repo (x402-foundation/x402): 6681 stars, Apache-2.0, last push 2026-10-06 (GitHub API, checked today). Directories: typescript, python, go, java, specs. No Rust directory seen in the root listing. Rust is community only.
- Official npm packages (all Apache-2.0, v2.28.0 as of today per npm registry): `@x402/core`, `@x402/express`, `@x402/hono`, `@x402/next`, `@x402/fetch`, `@x402/axios`, `@x402/evm` (also `@x402/svm`, per repo README, fastify and paywall dirs exist). Legacy v1: `x402-express` 1.2.0. Sources: [packages dir](https://github.com/x402-foundation/x402/tree/main/typescript/packages), [registry](https://registry.npmjs.org/@x402/express/latest).
- CDP facilitator: mainnets Base, Polygon, Arbitrum, World; testnets Base Sepolia, World Sepolia, Solana Devnet; schemes exact, upto, batch-settlement; first 1,000 settlements/month free then $0.001 each; verification free ([CDP docs](https://docs.cdp.coinbase.com/x402/network-support)). Base Sepolia: confirmed supported. Whether it is USDC specifically on Base: CDP page says ERC-20 tokens on EVM; USDC default asset is in the repo's [DEFAULT_ASSETS.md](https://github.com/x402-foundation/x402/blob/main/DEFAULT_ASSETS.md), which I did not read -> USDC-on-Base = unconfirmed from my own read (x402-rs README lists "Base USDC facilitator" milestone).
- Rust: **x402-rs** ([repo](https://github.com/x402-rs/x402-rs)): Apache-2.0, 293 stars, 176 forks, last push 2026-07-13 (about 12 weeks before today), latest release seen v1.3.0 (2026-02-15) on GitHub releases; crates.io `x402-axum` 2.0.2 (updated 2026-07-13, 42,266 downloads), `x402-types` 2.0.2 (59,536), `x402-reqwest` 2.0.2 (8,467), umbrella `x402-rs` 0.12.5 (21,660) ([crates.io API](https://crates.io/api/v1/crates/x402-axum)). Crates: x402-types, x402-axum (server middleware), x402-reqwest (client), x402-facilitator-local, x402-chain-eip155 (EVM incl. Base), x402-chain-solana, x402-chain-aptos (git-only). Supports protocol v1 and v2; has production facilitator Docker image; OpenTelemetry. Assessment: usable, actively published, not official. Not confirmed: security audit (none found), whether it tracks every upstream v2 feature (e.g. `upto`/batch-settlement; README lists deferred schemes as planned).

**A. wasmtime** ([Config docs](https://docs.rs/wasmtime/latest/wasmtime/struct.Config.html), v49.0.2 page):
- Fuel: `Config::consume_fuel(true)`; out of fuel traps. Docs: fuel "should be used when deterministic yielding or trapping is needed"; epoch interruption is NOT deterministic. Not compatible with Winch compiler.
- NaN: `Config::cranelift_nan_canonicalization(true)` (needs cranelift feature).
- Relaxed SIMD: `Config::relaxed_simd_deterministic(true)` forces deterministic behavior across platforms "at the cost of performance". Docs say default relaxed-SIMD behavior is consistent across x86_64/AArch64 only for same inputs per the description and this option exists for full determinism; simplest: disable relaxed SIMD (`wasm_relaxed_simd(false)`) in recipes. (That setter name from my memory of the API; unconfirmed on this page read.)
- Wasmtime also has a deterministic-execution guidance page in its book: [docs.wasmtime.dev/examples-deterministic-wasm-execution.html](https://docs.wasmtime.dev/examples-deterministic-wasm-execution.html) (URL not fetched by me: unconfirmed content).

**B. Serialization.**
- Borsh: README says it "is designed to serialize any objects to canonical and deterministic set of bytes"; hash maps/sets are sorted lexicographically by key; structs in field order; floats rejected if NaN (`err_if_nan`) ([README](https://github.com/near/borsh)). So one encoding per value, with caveats (decoder strictness on non-canonical input: unconfirmed).
- CBOR: RFC 8949 section 4.2.1 core deterministic encoding = preferred (shortest) serialization, no indefinite-length items, map keys sorted bytewise on their encodings ([RFC 8949](https://www.rfc-editor.org/rfc/rfc8949.html#name-deterministically-encoded-c)).
- Rust crates: `cbor-core` 0.10.x implements CBOR::Core deterministic subset and its decoder rejects non-canonical input ([docs.rs](https://docs.rs/crate/cbor-core/latest)); `cbor2` claims RFC 8949 section 4.2.1 deterministic encoding ([docs.rs](https://docs.rs/crate/cbor2/0.5.0)); `dcbor` implements the dCBOR profile ([docs.rs](https://docs.rs/crate/dcbor/latest)). All three are young (versions 0.x); maturity unconfirmed. `ciborium` (0.2.2, last updated 2024-01-24, [crates.io](https://crates.io/crates/ciborium)): I found no deterministic-mode claim; treat as non-canonical unless you sort keys yourself.

**C. BIP-322.** Status: Complete, Type Specification, version 2.0.0, license CC0-1.0 ([BIP text](https://github.com/bitcoin/bips/blob/master/bip-0322.mediawiki)). Rust crate `bip322` 0.0.12, repo rust-bitcoin/bip322, 30 stars, last push 2026-09-28, crates.io license CC0-1.0 (GitHub shows none), 216,956 downloads ([crates.io](https://crates.io/crates/bip322), [repo](https://github.com/rust-bitcoin/bip322)). Supports simple, full, proof-of-funds, legacy (BIP-137) signatures for P2TR, P2WPKH, P2SH-P2WPKH, P2PKH and multisig P2WSH/P2SH-P2WSH/P2SH; README invites review of "interface design and security", version 0.0.x -> pre-1.0; no audit found. MSRV 1.74. Alternative: `bdk_bip322` 0.1.0 ([repo](https://github.com/bitcoindevkit/bdk-bip322)).

**D. EVM hash gas** (go-ethereum params, [protocol_params.go](https://github.com/ethereum/go-ethereum/blob/master/params/protocol_params.go)): KECCAK256 opcode = 30 + 6 per 32-byte word (also [evm.codes #20](https://www.evm.codes/#20)). SHA256 precompile (address 0x02) = 60 + 12 per word. For one 32-byte word: keccak 36 gas, sha256 72 gas plus CALL overhead (~100 warm access), so keccak is cheaper for Merkle verification. Precompile reference: [evm.codes/precompiled](https://www.evm.codes/precompiled). Ethereum Merkle trees on Base use keccak natively; OpenZeppelin MerkleProof uses keccak (not fetched: unconfirmed).

**E. Proper scoring.** Gneiting and Raftery, "Strictly proper scoring rules, prediction, and estimation", JASA 102(477):359-378, 2007 ([DTIC PDF](https://apps.dtic.mil/sti/pdfs/ADA459827.pdf); I could not open the PDF, 403). Search summary lists the logarithmic and quadratic (Brier) scores among its examples of scoring rules. Definition of strictly proper: expected score maximised only by reporting the true distribution ([Wikipedia: Scoring rule](https://en.wikipedia.org/wiki/Scoring_rule)). I did not read verbatim text stating Brier and log are each strictly proper; the standard result is in the paper (Section 3), pull the exact theorem before citing it.

## 4. Unconfirmed

- Covalent: exact proof/consensus mechanism, slashing rules, and current status of GoldRush vs the Covalent Network after rebrand (docs pages blocked/partial).
- UMA OOv3: exact default bond, burn fraction, current DVM parameters (docs fetch returned only summaries; "65% majority" and 24h commit/reveal come from the generic oracle page, not OOv3-specific).
- Pyth: whether any publisher has actually been slashed; reward economics before the 2026-04-22 pause (only the pause is confirmed).
- Brevis primary docs (fetch returned empty); Axiom's current coprocessor scope (docs show a proving API for OpenVM, not a historical-data coprocessor); Lagrange docs read via search snippet only.
- Vana PoC details beyond the search summary (the dimension list I recalled was not verified; omitted).
- Arkham: whether bounty payout has penalties for false labels; current status of Intel Exchange.
- Truebit "1 in 1000 forced error rate": from search summary of papers, not read in PDF.
- x402: USDC-on-Base default asset (DEFAULT_ASSETS.md unread); x402-rs audit status; x402-rs coverage of every v2 scheme; whether Foundation plans an official Rust SDK (ROADMAP.md unread).
- wasmtime: deterministic-execution book page content; exact relaxed-SIMD disable setter name.
- Bittensor: commit-reveal parameters (reveal period) and the Yuma clipping details (primary docs redirected away).
- CBOR crates: production maturity of cbor-core/cbor2/dcbor.
- Proof of SQL: exact license terms meaning for commercial reuse (Cryptographic Open Software License 1.0; legal review needed).

## M1 research: deterministic WASM runtime (2026-10-06, loop 4-5)

### 1. wasmtime
- crates.io: https://crates.io/crates/wasmtime . Latest STABLE `49.0.2` (published 2026-10-02), MSRV (`rust_version`) **1.96.0**. Pre-release `50.0.0-rc.1` (2026-10-05, MSRV 1.97.0). 48.0.5 also current patch line (MSRV 1.95.0). Pin exact (`=49.0.2`).
- Docs: https://docs.rs/wasmtime/49.0.2/wasmtime/ ; Config: https://docs.rs/wasmtime/49.0.2/wasmtime/struct.Config.html ; Store: https://docs.rs/wasmtime/49.0.2/wasmtime/struct.Store.html
- Method names, all PRESENT in 49.0.2 (checked as method anchors on docs.rs): `Config::consume_fuel`, `cranelift_nan_canonicalization` (cranelift/winch feature only), `wasm_relaxed_simd`, `relaxed_simd_deterministic`, `wasm_threads` (needs `threads` crate feature; default on), `wasm_simd`, `max_wasm_stack`; `Store::set_fuel`, `Store::get_fuel`, `Store::limiter`. None renamed/removed in 49.
- Notes from docs text: store starts with 0 fuel when consume_fuel is on, so call `set_fuel` first; out-of-fuel raises a trap. New `Config::operator_cost(OperatorCost)` exists (configurable per-op fuel cost; not otherwise researched). `cranelift_nan_canonicalization` also covers v128. `max_wasm_stack` is NOT deterministic ("pretty close to this much", native stack, host/arch dependent) - confirms need for instrumented depth limit.
- Memory limits: `StoreLimitsBuilder` methods: `memory_size`, `table_elements`, `instances`, `tables`, `memories`, `trap_on_grow_failure`, `build`. https://docs.rs/wasmtime/49.0.2/wasmtime/struct.StoreLimitsBuilder.html . Install with `store.limiter(|d| &mut d.limits)`. `trap_on_grow_failure(true)` makes failed grow trap instead of returning -1 (still deterministic if limit fixed).
- Deps: wasmtime 49.0.2 depends on wasmparser ^0.258.3 (not 0.261). Use matching 0.258.x in Onyx for validation to avoid two copies (0.261.0 is newest, MSRV 1.88).

### 2. Wasmtime deterministic-execution page
https://docs.wasmtime.dev/examples-deterministic-wasm-execution.html . Lists 5 non-determinism sources and fixes:
| Source | Fix |
|---|---|
| Host imports (clocks, fs) | virtualize (wasi-virt); Onyx: no imports |
| NaN bit patterns | `cranelift_nan_canonicalization` |
| Relaxed SIMD | `relaxed_simd_deterministic`, or disable via `wasm_relaxed_simd(false)` |
| memory.grow/table.grow success depends on resources | limiters, preallocating memory creators, or reject grow instrs at validation |
| Interruption | use fuel, not epoch interruption |
Implication: memory.grow can differ by host; use StoreLimits with fixed caps (or reject memory.grow, but Rust std allocator needs it unless memory is fixed).

### 3. wasmparser
- Latest `0.261.0` (2026-10-02, MSRV 1.88), license Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT. wasm-encoder matches at 0.261.0.
- **`WasmFeatures::FLOATS` / `.floats()` EXISTS, CONFIRMED** (docs: https://docs.rs/wasmparser/0.261.0/wasmparser/struct.WasmFeatures.html ; source src/features.rs: `floats: FLOATS(1<<11) = true`). Doc: "not a proposal... intended for embeddings with stricter requirements"; default true. Is it in 0.258.x too? unconfirmed (check before pinning).
- Source-verified effect (validator.rs / operators.rs / operators/simd.rs): with floats off, value types F32/F64 are rejected ("floating-point support is disabled"), and `check_floats_enabled()` rejects f32/f64 loads/stores/consts/arith, conversions, and the float-lane SIMD ops (via `check_v128_fbinary/funary/fternary_op` etc). So validation covers function types, locals, globals and instructions.
- Not exhaustively verified: every SIMD float-related op (e.g. f32x4.extract_lane, relaxed ops) hits the check; I saw check helpers used, not each op. Safest: ALSO disable SIMD (`WasmFeatures::SIMD`/`RELAXED_SIMD` off) so no v128 type at all. v128 bitwise/int ops are float-free but moot if SIMD is off.
- Detection by operator walk (belt and braces): `Operator` has 627 variants; 146 contain "F32"/"F64": prefixes `F32*`/`F64*` (120) plus conversions `I32TruncF32S`, `I64ReinterpretF64`, `I32TruncSatF64U`, `I32x4TruncSatF32x4S`, `I32x4Relaxed Trunc...`, etc. Non-prefix ones (26) are easy to miss: match on variant NAME is not available; no classification helper (only `Operator::operator_arity`). Hence prefer validator feature flag; if walking operators, generate the match from the variant list (or use `VisitOperator` via `for_each_operator!` macro, unconfirmed whether to use it) - do not hand-write.
- Also float-typed things outside code: block/func types, globals, `f32.const` in const exprs - covered by validator.

### 4. Stack-depth instrumentation
| crate | latest | updated | license | deps | verdict |
|---|---|---|---|---|---|
| wasm-instrument (paritytech) https://crates.io/crates/wasm-instrument | 0.4.0, 2022-11-26 | repo last push 2024-07-31, not archived | MIT OR Apache-2.0 | `parity-wasm ^0.45` | Has `stack_limiter::inject` (counts depth with a global; traps via unreachable). Effectively unmaintained, built on deprecated parity-wasm, NOT wasm-encoder/wasmparser. Does not understand newer features (multivalue, ref types etc. - unconfirmed exactly what fails). Unlikely to accept modern Rust output (sign-ext, bulk-memory, multivalue) - UNCONFIRMED, test before relying. |
| finite-wasm (near) https://crates.io/crates/finite-wasm | 0.6.1, 2026-06-15 (repo pushed 2026-08-01) | MIT OR Apache-2.0 | `wasmparser ^0.228`, wasm-encoder ^0.228 (opt) | Maintained, but pinned to 0.228, three-plus years behind 0.261 (two wasmparser copies in tree; fine but older feature coverage). Does stack-SIZE (not call-count) + gas analysis. Its `instrument` module appends IMPORTS (import_env), which violates "no imports" - would need patching/forking (source seen: instrument.rs maintains an import section; exact imports names unconfirmed). Stack metric is bytes of operand+locals, adjustable by SizeConfig. |
- Neither works out of the box with current wasm-encoder/wasmparser. Recommendation: write own small instrumenter with wasm-encoder/wasmparser 0.258.x: global depth counter (i32 mutable global, internal not imported), +1/-1 around each call and call_indirect, trap (`unreachable`) above limit; reset-safe because traps abort the instance. Subtleties (unconfirmed design notes): returns via `return` / br out of function / tail calls (reject tail-call), trap unwinding not an issue since failure is terminal. Alternatively fork wasm-instrument's algorithm (MIT/Apache) as reference.
- Fuel: use wasmtime consume_fuel natively (deterministic per wasmtime version; fuel cost per op may change across wasmtime versions - pin).

### 5. Rust wasm32-unknown-unknown
- Defaults (https://doc.rust-lang.org/rustc/platform-support/wasm32-unknown-unknown.html): `multivalue`, `mutable-globals`, `reference-types`, `sign-ext` (reference-types + multivalue since Rust 1.82, https://blog.rust-lang.org/2024/09/24/webassembly-targets-change-in-default-target-features/), plus `nontrapping-fptoint` and `bulk-memory` (Rust 1.87.0+, LLVM 20+). Opt-out to MVP needs `-Ctarget-cpu=mvp` AND `-Zbuild-std` (nightly), since precompiled std keeps the features. My test with rustc 1.98 confirmed: output contains `memory.copy` (bulk-memory) and `i32.extend8_s` (sign-ext); `-Ctarget-cpu=mvp` without build-std did not remove them. Wasmtime config must therefore leave bulk_memory, reference_types, multi_value, sign_extension enabled (all default on). Note `nontrapping-fptoint` only emits trunc_sat float ops when floats are used.
- Float-free integer code (experiment, rustc 1.98.0, release, opt-level=s, panic=abort, lto, cgu=1, std cdylib with `format!("{}-{:?}-{:x}")`, Vec, `panic!("boom {}", n)`): converted to WAT, **0 occurrences of f32/f64/v128 and 0 imports**. Same crate with `format!("{}", n as f64/3.0)`: 5 float instrs. So float formatting/arith pulls floats; integer fmt does not. compiler_builtins float routines are only linked if referenced (consistent with result; not exhaustively proven for all code, so keep the validator gate as the actual guarantee).
- SIMD: wasm32 does not enable simd128 by default; no need for `-C target-feature=-simd128`, but harmless to add. Auto-vectorization only with +simd128.
- Recommendations: `panic="abort"`, avoid `{}` on f32/f64, `#![no_std]` + `alloc` optional, enforce with validator (floats off, simd off).

### 6. Reproducible builds (experiment, same toolchain, rustc 1.98.0, cargo, local)
| scenario | result |
|---|---|
| clean rebuild same dir (strip=true) | byte-identical |
| copy of project in different dir, strip=true, debug=false | identical |
| CARGO_BUILD_JOBS=1 vs default | identical |
| debug=1 (DWARF), different project dirs, no remap | DIFFER (abs paths embedded) |
| debug=1, different dirs, with `--remap-path-prefix=<proj>=/src`, `<cargo home>=/cargo`, `<sysroot>=/rustc` | identical, 0 abs-path leaks |
- Advice: release with `strip=true`/`debug=false` removes path leakage here; otherwise remap project, ~/.cargo and sysroot. `codegen-units=1`, `lto=true`, `opt-level` fixed help ordering. Pin toolchain via rust-toolchain.toml and `--locked`; commit Cargo.lock. Cross-toolchain or cross-OS identical output not tested. Also remove any build.rs/timestamp. The 3 runs above are not a proof; run CI repeated builds.
- Also note std precompiled path strings come from rust-src/panic locations (`/rustc/<hash>/...`), which are stable per toolchain version.
- Onyx should not depend on byte-identical wasm for runtime determinism: hash the wasm artifact, and make the runtime output depend only on wasm bytes.

## Deterministic stack: wasmi vs wasmtime (2026-10-08, loop 8)


Method: primary sources only. wasmi 2.0.0, wasmtime 49.0.2 and cranelift-codegen 0.136.2 (the
Cranelift wasmtime 49.0.2 depends on) were downloaded from crates.io and read directly. Claims marked
**(source-read)** come from reading that code, not from docs. Anything not checked is in "Unconfirmed"
at the end.

---

### 1. wasmi

### Facts

| Item | Value | Source |
|---|---|---|
| Latest version | **2.0.0**, released 2026-09-01 (previous: 1.1.0 on 2026-06-12, 0.51.5) | https://crates.io/api/v1/crates/wasmi , https://github.com/wasmi-labs/wasmi/blob/main/CHANGELOG.md |
| MSRV | `rust-version = "1.86"` in the 2.0.0 Cargo.toml | https://crates.io/crates/wasmi/2.0.0 |
| License | MIT/Apache-2.0 | same |
| Maintenance | Active. Last push 2026-09-21, 39 open issues. Mostly one maintainer: Robbepop / Robin Freyler has 1166 commits; the next contributor has 125 (bus-factor risk) | https://github.com/wasmi-labs/wasmi , https://github.com/wasmi-labs/wasmi/graphs/contributors |
| Users | Soroban (Stellar), smoldot, Typst, Zellij, and others (README) | https://github.com/wasmi-labs/wasmi#used-by |
| Wasm parser | wasmparser 0.228.0 | 2.0.0 Cargo.toml |

### Security audits (README table)

| wasmi versions | Auditor | Paid for by | Report |
|---|---|---|---|
| 0.36.0 – 0.38.0 | Runtime Verification Inc. | Stellar Development Foundation | https://github.com/wasmi-labs/wasmi/blob/main/resources/audit-2024-11-27.pdf (dated 2024-11-27) |
| 0.31.0 | SRLabs | Parity Technologies | https://github.com/wasmi-labs/wasmi/blob/main/resources/audit-2023-12-20.pdf (dated 2023-12-20) |

**No audit covers 2.0.** 2.0 has "an entirely new internal IR and executor" ([CHANGELOG 2.0.0](https://github.com/wasmi-labs/wasmi/blob/main/CHANGELOG.md)).
I did not read the PDFs' findings.

### Call stack and value stack: both on the heap, with explicit limits (2.0.0 API)

`wasmi::Config` methods ([docs.rs](https://docs.rs/wasmi/2.0.0/wasmi/struct.Config.html), [source](https://github.com/wasmi-labs/wasmi/blob/v2.0.0/crates/wasmi/src/engine/config.rs)):

- `Config::set_max_recursion_depth(&mut self, value: usize)`: the maximum number of call frames. Default `1000`.
- `Config::set_max_stack_height(&mut self, value: usize)`: the maximum value-stack size **in bytes**. Default `1_000_000`.
- `Config::set_min_stack_height(&mut self, value: usize)`: the initial value-stack size in bytes. Default `1_000`.
- `Config::set_max_cached_stacks(&mut self, value: usize)`: default `2`.
- Defaults are in [`engine/limits/stack.rs`](https://github.com/wasmi-labs/wasmi/blob/v2.0.0/crates/wasmi/src/engine/limits/stack.rs). The internal type is `StackConfig`; there is no `StackLimits` type in 2.0.

How the limits are enforced **(source-read)**, from [`engine/executor/handler/state.rs`](https://github.com/wasmi-labs/wasmi/blob/v2.0.0/crates/wasmi/src/engine/executor/handler/state.rs):

- `ValueStack` is a `Vec<Cell>`. `Cell` is a fixed `u64` (`cell.rs`), and since 2.0 cells are 64-bit regardless of the `simd` feature ([#1755](https://github.com/wasmi-labs/wasmi/pull/1755)).
  - `max_height_bytes / size_of::<Cell>()` gives the cell cap.
  - Growth beyond the cap returns `TrapCode::StackOverflow` (`grow_cold`).
- `CallStack` is a `Vec<Frame>`. When `frames.len() == max_height`, push returns `TrapCode::StackOverflow`.
- **Wasm-to-Wasm calls do not recurse on the native stack.**
  - `call_internal` / `call_imported` push a heap frame (`call_func_entry`) and then continue dispatching ([`exec.rs`](https://github.com/wasmi-labs/wasmi/blob/v2.0.0/crates/wasmi/src/engine/executor/handler/exec.rs)).
  - Native recursion happens only on host-function → Wasm re-entry.
- Frame size is fixed by wasmi's translator, not by an ISA. `len_stack_slots = max_stack_offset + locals.len()` ([`translator/func/mod.rs`](https://github.com/wasmi-labs/wasmi/blob/v2.0.0/crates/wasmi/src/engine/translator/func/mod.rs)).
  - `max_stack_offset` grows by 1 cell per pushed operand (2 for v128) ([`stack/operands.rs`](https://github.com/wasmi-labs/wasmi/blob/v2.0.0/crates/wasmi/src/engine/translator/func/stack/operands.rs)).
  - So a frame is linear in Wasm-visible locals plus operand-stack height. **No LICM or hoisting can inflate it.**
  - A function needing more than `u16::MAX` slots fails translation (`AllocatedTooManySlots` / `len_stack_slots -> Option<u16>`).
- **Determinism verdict:**
  - The trap point does not depend on the host ISA, CPU features or native stack. It depends only on the wasmi version, the `Config` and the crate features (pure Rust integer logic on heap buffers).
  - `set_max_recursion_depth` is a pure frame count, so it is also stable across wasmi versions.
  - The byte limit (`set_max_stack_height`) depends on wasmi's slot allocation, so it can change between wasmi versions. Pin the version, or keep it as a never-reached backstop.

### Native-stack caveat inside wasmi itself (source-read)

- The default `auto-dispatch` uses **tail-call dispatch** on optimizing builds of known targets ([build.rs](https://github.com/wasmi-labs/wasmi/blob/v2.0.0/crates/wasmi/build.rs)).
- On stable Rust this relies on LLVM sibling-call optimization. The guaranteed `become` keyword is used only with the `unstable` feature on nightly ([CHANGELOG](https://github.com/wasmi-labs/wasmi/blob/main/CHANGELOG.md), [v2.0 blog](https://wasmi-labs.github.io/blog/posts/wasmi-v2.0/)).
- The **`portable-dispatch`** feature forces the loop-based dispatcher, which does not rely on TCO.
- For a verdict engine, `portable-dispatch` removes the last native-stack dependency. Cost: the blog says switch-loop dispatch is "significantly slower on Apple Silicon"; no x86 number was given.

### Fuel metering (2.0.0)

- **API:**
  - `Config::consume_fuel(bool)`.
  - `Store::set_fuel(&mut self, fuel: u64) -> Result<(), Error>` and `Store::get_fuel()` ([docs](https://docs.rs/wasmi/2.0.0/wasmi/struct.Store.html)).
  - `Caller::set_fuel` / `Caller::get_fuel` inside host functions.
  - Exhaustion raises `TrapCode::OutOfFuel`.
- **Custom costs:**
  - `Config::operator_cost(OperatorCost)` sets a per-Wasm-operator `u8` cost table.
  - `Config::fuel_cost(CustomFuelCosts)` sets dynamic costs: bulk memory/table ops, plus lazy translation/validation ([config.rs](https://github.com/wasmi-labs/wasmi/blob/v2.0.0/crates/wasmi/src/engine/config.rs), [costs.rs](https://github.com/wasmi-labs/wasmi/blob/v2.0.0/crates/wasmi/src/engine/costs.rs)).
  - Default: 0 for nop, drop, block, loop, unreachable, return, else and end; 1 for everything else.
- **Determinism:**
  - 2.0 changelog: "fuel metering is now stable: it is tied to the input Wasm bytecode instead of Wasmi's internal bytecode" ([#2013](https://github.com/wasmi-labs/wasmi/pull/2013)).
  - Blog footnote: "the metered fuel per unit of execution stays the same across Wasmi versions" ([blog](https://wasmi-labs.github.io/blog/posts/wasmi-v2.0/)).
- **Gotcha (source-read):** with `CompilationMode::LazyTranslation` (the **default**) or `Lazy`, fuel is charged for translating each function on its first call (`compilation_fuel` in [`code_map/mod.rs`](https://github.com/wasmi-labs/wasmi/blob/v2.0.0/crates/wasmi/src/engine/code_map/mod.rs)).
  - Compiled state is cached per `Engine`, so a second run on the same engine skips that charge. Fuel then depends on engine history.
  - **Use `CompilationMode::Eager` for verdicts.**
  - wasmi's own docs say `CompilationMode::Lazy` "must not be used if the result of Wasm execution must be deterministic amongst multiple Wasm implementations" ([enum docs](https://docs.rs/wasmi/2.0.0/wasmi/enum.CompilationMode.html)).

### Floats, features and validation

- `Config::floats(false)` disables f32/f64 instructions **and types** (sets `WasmFeatures::FLOATS` in wasmparser). It is available with the default `validate` feature.
- Other switches:
  - Per-proposal: `wasm_simd`, `wasm_relaxed_simd`, `wasm_multi_memory`, `wasm_memory64`, `wasm_tail_call`, `wasm_reference_types`, `wasm_bulk_memory`, `wasm_multi_value`, `wasm_sign_extension`, `wasm_saturating_float_to_int`, `wasm_mutable_global`, `wasm_extended_const`, `wasm_custom_page_sizes`, `wasm_wide_arithmetic`.
  - Other: `allow_start_fn`, `ignore_custom_sections`.
  - Module-shape caps: `Config::enforced_limits(EnforcedLimits)` (`max_globals`, `max_functions`, `max_tables`, `max_params`, `max_results`, `min_avg_bytes_per_function`, …).
  - Source: [config.rs](https://github.com/wasmi-labs/wasmi/blob/v2.0.0/crates/wasmi/src/engine/config.rs), [limits/engine.rs](https://github.com/wasmi-labs/wasmi/blob/v2.0.0/crates/wasmi/src/engine/limits/engine.rs).
- **NaN:** crate feature **`deterministic`** (new in 2.0, [#1947](https://github.com/wasmi-labs/wasmi/pull/1947)) implements the [Wasm 3.0 deterministic profile](https://webassembly.github.io/spec/core/appendix/profiles.html#deterministic-profile-small-mathrm-det).
  - It canonicalizes NaNs to `0x7FC0_0000` / `0x7FF8_0000_0000_0000` ([wasmi_core value.rs](https://github.com/wasmi-labs/wasmi/blob/v2.0.0/crates/core/src/value.rs)).
  - Without it, NaN bit patterns are whatever the host float ops produce.
  - If Onyx sets `floats(false)`, this is moot.
- `libm` feature: forces libm for some float ops.

### Translation cost / laziness

- `CompilationMode::{Eager, LazyTranslation (default), Lazy}`.
- README claims "Compiler/JIT bomb resisting translation" ([README](https://github.com/wasmi-labs/wasmi#distinct-features)).
- I found **no explicit linear-time statement** for wasmi's translator (see Unconfirmed). Translation does run as a wasmparser visitor that charges fuel proportional to function byte length (`fuel_for_translating_bytes(len_bytes)`), which suggests length-proportional cost by design.

### Performance (official wasmi-benchmarks plots, data committed 2026-08-27)

Source: https://github.com/wasmi-labs/wasmi-benchmarks-plots (`data/amd-epyc-7763/lin-scale/*.svg`). Values were read from the SVG text labels. Lower is better.

| AMD EPYC 7763 | Wasmi v2 (eager) | Wasmtime Cranelift | Wasmtime Winch | Wasmtime Pulley |
|---|---|---|---|---|
| execute geomean (× vs best runtime) | **x12.40** | **x1.15** | x2.35 | x27.92 |
| fibonacci-rec (recursion-heavy) | 41.11 ms | 6.63 ms | 6.89 ms | 54.42 ms |
| startup geomean (× vs best) | x38.88 (lazy-translation: x10.66) | x1634.19 | x298.15 | x1777.72 |

- Execution: wasmi 2.0 is roughly **10.8× slower** than Wasmtime/Cranelift (geomean 12.40 / 1.15) and about 6.2× slower on fib-rec.
- Startup: roughly **42× faster** (eager).
- The blog says 2.0 is about 2.2× faster than 1.0 on an M2 Pro ([blog](https://wasmi-labs.github.io/blog/posts/wasmi-v2.0/)).

---

### 2. Cranelift / wasmtime 49.0.2

- **`Config::cranelift_opt_level(OptLevel::None)` disables the whole egraph pass, which includes GVN and LICM.**
  - In cranelift-codegen 0.136.2 `Context::optimize`: `if opt_level != OptLevel::None { self.egraph_pass(isa, ctrl_plane)?; }` ([context.rs @v49.0.2](https://github.com/bytecodealliance/wasmtime/blob/v49.0.2/cranelift/codegen/src/context.rs)).
  - LICM lives in egraph elaboration (`loop_analysis`, `hoist_block`, `loop_hoist_level` in [egraph/elaborate.rs](https://github.com/bytecodealliance/wasmtime/blob/v49.0.2/cranelift/codegen/src/egraph/elaborate.rs)). GVN is the egraph's hash-consing.
  - Still run at None: NaN canonicalization (if enabled), unreachable-code elimination, `remove_constant_phis`.
  - The wasmtime `OptLevel::None` doc says: "disabling most optimizations" ([config.rs](https://github.com/bytecodealliance/wasmtime/blob/v49.0.2/crates/wasmtime/src/config.rs)).
- **Opt level None does NOT bound frames.** wasmtime issue [#14075](https://github.com/bytecodealliance/wasmtime/issues/14075) (Alex Crichton, open, 2026-08-03):
  - "if optimizations are disabled, then lots of locals are spilled to the stack … a stack frame … proportional to the number of locals".
  - Fuel being "deterministic across targets" would "diverge across compiler options/targets" if frame size were counted.
  - "I wouldn't be confident in saying these are the only variable sources of stack-frame-sizes."
  - Related: [#14042](https://github.com/bytecodealliance/wasmtime/pull/14042) (charge fuel per declared local; closed unmerged).
- **LICM behaviour changes between patch releases.** [#14383](https://github.com/bytecodealliance/wasmtime/pull/14383) "Fix a bug in elaboration that led to missed LICM hoisting" was merged 2026-09-23. So frame sizes are not stable across wasmtime versions either.
- **No wasmtime option bounds per-function native frame size.**
  - `Config::max_wasm_stack(bytes)` (default 512 KiB) caps *native* bytes used by Wasm, so it is ISA- and compiler-dependent.
  - Its doc warns it "does not ensure that this much stack space is available … typically leads to an **abort**" ([config.rs](https://github.com/bytecodealliance/wasmtime/blob/v49.0.2/crates/wasmtime/src/config.rs)).
  - Cranelift's only frame cap is a compile-time `maximum_frame_size() = 1 << 30 // 1 GiB` → `ImplLimitExceeded` ([machinst/abi.rs](https://github.com/bytecodealliance/wasmtime/blob/v49.0.2/cranelift/codegen/src/machinst/abi.rs)).
- **Wasmtime determinism docs do not mention the stack.** The official page covers imports, NaN canonicalization, relaxed SIMD, memory/table growth and fuel. It says nothing about stack overflow ([docs source](https://github.com/bytecodealliance/wasmtime/blob/main/docs/examples-deterministic-wasm-execution.md), [rendered](https://docs.wasmtime.dev/examples-deterministic-wasm-execution.html)).
- GitHub issue search for "deterministic stack overflow" / "stack overflow nondeterminism" in bytecodealliance/wasmtime found nothing beyond #14075.

---

### 3. How other deterministic systems do it

Common pattern: a **logical** stack counter computed from Wasm-level locals plus max operand height. The native limit is set "much larger" and assumed never to be hit. None of them proves that bound against an optimizing compiler.

- **NEAR (nearcore, protocol 84+, Wasmtime).**
  - Method: [finite-wasm](https://github.com/near/finite-wasm) injects stack checks and gas at basic-block boundaries. The max_stack analysis is "a single pass linear-time algorithm" ([max_stack/mod.rs](https://github.com/near/finite-wasm/blob/main/src/max_stack/mod.rs)).
  - Frame cost = 64 bytes + Σ local sizes (+ operand bytes). Code comment: "Rough accounting for rip, rbp and some registers spilled. **Not exact.**" ([prepare_v3.rs](https://github.com/near/nearcore/blob/master/runtime/near-vm-runner/src/prepare/prepare_v3.rs)).
  - `max_stack_height` = 262_144 ([parameters.yaml](https://github.com/near/nearcore/blob/master/core/parameters/res/runtime_configs/parameters.yaml)).
  - Wasmtime runner config ([wasmtime_runner/mod.rs](https://github.com/near/nearcore/blob/master/runtime/near-vm-runner/src/wasmtime_runner/mod.rs)):
    - `max_wasm_stack(1 GiB)` with the comment "we don't want wasmtime to trap before that".
    - Backend: **Winch on x86_64** (production), Cranelift with `OptLevel::None` + `RegallocAlgorithm::SinglePass` elsewhere.
    - `compiler_inlining(No)` and `cranelift_nan_canonicalization(true)`.
  - Their blog explains why they rejected Cranelift: compile-time blowups, and "Cranelift … makes no promise to bound its own compilation cost". Also "operand stack ≤ 8 KB per function" ([Near One blog, 2026-07-01](https://blog.nearone.org/research/2026/07/01/replacing-nears-contract-vm-with-wasmtime.html)).
  - Spec: [Preparation.md](https://github.com/near/nearcore/blob/master/docs/RuntimeSpec/Preparation.md).
- **Substrate / Polkadot PVF.**
  - Method: `sc-executor-wasmtime` `Semantics::deterministic_stack_limit: Option<DeterministicStackLimit { logical_max, native_stack_max }>`.
    - wasm-instrument's stack limiter counts "logical values" (locals + args + max operand height + `ACTIVATION_FRAME_COST = 2`).
    - Its rationale explicitly assumes "a very naive wasm executor" where every value sits on the native stack ([wasm-instrument stack_limiter](https://github.com/paritytech/wasm-instrument/blob/master/src/stack_limiter/mod.rs), [DeterministicStackLimit docs](https://docs.rs/sc-executor-wasmtime/latest/sc_executor_wasmtime/struct.DeterministicStackLimit.html)).
  - PVF defaults: `DEFAULT_LOGICAL_STACK_MAX = 65536`, `DEFAULT_NATIVE_STACK_MAX = 256 MiB` ([executor_params.rs](https://github.com/paritytech/polkadot-sdk/blob/master/polkadot/primitives/src/v9/executor_params.rs)).
  - The execute thread gets a 2 MiB + 256 MiB stack ([execute-worker/src/lib.rs](https://github.com/paritytech/polkadot-sdk/blob/master/polkadot/node/core/pvf/execute-worker/src/lib.rs)).
  - **Uses `OptLevel::SpeedAndSize`**, so the egraph and LICM are on ([runtime.rs](https://github.com/paritytech/polkadot-sdk/blob/master/substrate/client/executor/wasmtime/src/runtime.rs)).
  - [polkadot#5712](https://github.com/paritytech/polkadot/pull/5712) reasoned: "If we assume that each value was spilled … that would be only 512 KiB". **This is exactly the assumption Onyx's LICM case breaks:** about 37k spills from a few logical values.
  - The 256 MiB / 65536 headroom (4 KiB per logical value) is an empirical margin, not a proof.
- **CosmWasm (cosmwasm-vm 3.0.10).**
  - Method: Wasmer **Singlepass** only, `canonicalize_nans(true)`, `Gatekeeper` middleware (operator allowlist incl. `allow_floats`) plus `Metering` gas middleware ([engine.rs](https://github.com/CosmWasm/cosmwasm/blob/main/packages/vm/src/wasm_backend/engine.rs), [gatekeeper.rs](https://github.com/CosmWasm/cosmwasm/blob/main/packages/vm/src/wasm_backend/gatekeeper.rs)).
  - **No logical stack limiter found in the VM source.** It relies on Wasmer's native stack overflow trap.
- **Arbitrum Stylus (nitro).**
  - Method: the `DepthChecker` middleware charges frame cost = worst operand height + locals + 4 words, plus a `max_frame_contention` static cap.
  - Its doc claims the logical space "is strictly smaller than that of the real stack space consumed on any target platform" ([depth.rs](https://github.com/OffchainLabs/nitro/blob/master/crates/prover/src/programs/depth.rs)).
  - **But the claim did not hold in practice.** [nitro#4538](https://github.com/OffchainLabs/nitro/pull/4538) (merged 2026-04-10) added `NativeStackOverflow` "to distinguish retriable native overflows from deterministic OutOfStack (DepthChecker)" ([user.rs](https://github.com/OffchainLabs/nitro/blob/master/crates/arbutil/src/evm/user.rs)).
    - On overflow it retries with Cranelift and a doubled stack (cap 100 MB).
    - If still unresolved, the node **panics**, "a consensus failure" otherwise ([native.go](https://github.com/OffchainLabs/nitro/blob/master/arbos/programs/native.go)).
  - This is the clearest public evidence that compiler-dependent native frames can beat a logical depth counter.

---

### 4. Recommendation

**Pick (b), built on (a): wasmi is the verdict engine. Wasmtime is an optional fast path that can never itself produce a stack-overflow verdict.**

Why not (c), wasmtime with opt level None plus tight caps:

- Cranelift's own maintainers say frame size is unknown until codegen and varies by target and option (#14075).
- None still spills, proportionally to locals.
- `max_wasm_stack` counts native bytes.
- Polkadot and Stylus both rely on headroom, and Stylus had to add retries and panics.

It reduces the problem's size but is not deterministic by construction.

### Concrete design

1. **Define the stack limit at the Wasm level, engine-independently.**
   - Either keep Onyx's call-depth counter and add a per-function logical frame cost (finite-wasm `max_stack`: locals + max operand height), or rely on wasmi's frame count alone.
   - Reject modules at load if any function's logical frame exceeds F (as NEAR caps "operand stack ≤ 8 KB per function").
2. **wasmi verdict config (2.0.0):**
   - `Config::compilation_mode(CompilationMode::Eager)`.
   - `consume_fuel(true)` with a fixed `operator_cost` table, or instrumented gas (see step 4).
   - `floats(false)` if recipes do not need floats; otherwise enable the `deterministic` crate feature.
   - `wasm_simd(false)`, `wasm_relaxed_simd(false)`.
   - `set_max_recursion_depth(D)`, with `set_max_stack_height` sized so it is never the binding limit. The bound: frame cells grow linearly in locals + operand cells and are capped at `u16::MAX`.
   - Crate features `portable-dispatch` + `deterministic`. Pin the exact version (`=2.0.0`).
3. **Fast path (wasmtime/Cranelift, any opt level):**
   - Run the *same instrumented* module.
   - Accept its result only if it finished or trapped on the logical limit or gas.
   - Treat any wasmtime `StackOverflow` trap (or any engine-internal trap) as "no verdict" and fall back to wasmi.
   - Give it a big `max_wasm_stack` on a thread with a large stack, as NEAR and Polkadot do.
4. **Use one gas model for both engines.**
   - wasmi fuel ≠ wasmtime fuel, so out-of-fuel verdicts would disagree.
   - Use Wasm-level instrumented gas (finite-wasm style) as the semantic limit. Keep native fuel only as a loose backstop.
5. **Disputes always re-execute on wasmi.**
   - Everyday validation can use wasmtime and is about 10× faster in execution, per the official plots above.

---

### Unconfirmed

- Whether wasmi's translator is worst-case **linear time**. The README only claims "JIT bomb resisting"; no proof or doc statement was found.
- The exact constant in wasmi's frame-size bound.
  - `len_stack_slots = max_stack_offset + locals.len()`, while `max_offset` appears to start at `local_slots` (debug_assert at operands.rs:373). So locals may count twice.
  - The bound is linear either way. The exact factor was not verified by test.
- Whether any wasmi 2.0.0 cfg (32-bit vs 64-bit host, `indirect-dispatch` → `wasmi_ir/slot16`) changes slot counts or translation success. Pin features identically on all validators.
- Whether LLVM reliably tail-calls wasmi's dispatch on stable Rust. Recommending `portable-dispatch` for verdicts is my inference, not a wasmi recommendation.
- The performance cost of `portable-dispatch` on x86_64: no published number found.
- wasmi MSRV: Cargo.toml says 1.86. A summarised read of the v2.0 blog mentioned "Rust 1.92", possibly about a regression; not reconciled.
- The audit PDFs' contents and findings (not read).
- How NEAR sizes the native thread stack for contract execution (not found). Whether NEAR's 64-byte frame estimate was ever bypassed (no report found).
- CosmWasm's native stack size / Wasmer coroutine stack and whether they consider overflow deterministic (no doc found).
- Whether Wasmtime **Pulley** frame sizes are identical across hosts (same pulley64 target). Plausible, but it is slower than wasmi anyway (x27.92 vs x12.40 geomean).
- No public report was found of a Polkadot `DeterministicStackLimit` bypass through Cranelift spills. The vulnerability is inferred from their own stated assumption (#5712) plus Onyx's observation.

## M2: first-party attestations and calibration (2026-10-08, loop 12)

### BIP-322 (generic signed message format)
- Source: https://github.com/bitcoin/bips/blob/master/bip-0322.mediawiki. Version 2.0.0, status
  Complete, license CC0-1.0.
- Variants and prefixes: legacy (BIP-137, P2PKH only, "SHOULD NOT" be used for other types), simple
  `smp` (witness stack; native segwit P2WPKH, P2WSH, P2TR), full `ful` (whole `to_sign` tx), proof of
  funds `pof` (finalized PSBT with extra UTXO inputs). A verifier "might assume" simple when no
  prefix is present (backward compatibility).
- Verifier output has three states: valid at time T and age S, inconclusive, invalid.
- The BIP itself states the limit Onyx must design around: "no message signing protocol can actually
  prove control of funds, both because a signature is obsolete as soon as it is created, and because
  the possessor of a secret key may be willing to sign messages on others' behalf".
- Proof of funds needs the UTXO set; an offline verifier only checks signatures, and a UTXO list
  never proves there are no other UTXOs. So `pof` does not fit deterministic offline validation.
- Test vectors: `bip-0322/basic-test-vectors.json` and `generated-test-vectors.json` (made with btcd,
  https://github.com/btcsuite/btcd/pull/2521). Copied to `fixtures/bip322/`. Counts: basic 4 simple +
  8 error; generated 4 simple, 10 full, 3 proof-of-funds, 28 error. All 10 generated full vectors use
  time locks (version 2, lock time 2016, sequence 2016).

### `bip322` crate 0.0.12 (https://github.com/rust-bitcoin/bip322, CC0-1.0)
Read `src/verify.rs` and `src/util.rs`. Findings (each has a test in `adapters/source-btc/tests/bip322.rs`):
- Supports the v2 prefixes, but accepts unprefixed signatures as simple (allowed by the BIP).
- `verify_simple_encoded` decodes the witness from a cursor and ignores trailing bytes, so one proof
  has many encodings.
- For P2SH-wrapped P2WPKH it accepts an empty scriptSig, which is not a consensus-valid spend
  (simple proofs are only defined for native segwit).
- Script-path time-lock scripts (P2TR script path, P2WSH time lock) return `Err(InvalidWitness)`,
  not "inconclusive". The other 8 full vector types return "valid at T = 2016".
- Multisig (P2WSH, P2SH, P2SH-P2WSH) is checked by a CHECKMULTISIG emulation (`parse_multisig`), not
  a full script interpreter. Enforces low S, SIGHASH_ALL (or DEFAULT for taproot), strict DER via
  libsecp256k1.
- Contains `.expect(...)` on sighash computation; Onyx wraps calls in `catch_unwind`.

### Consensus limits the crate skips (loop 13, from the loop-12 review)
- BIP-341 (https://github.com/bitcoin/bips/blob/master/bip-0341.mediawiki): a 65-byte key-path
  signature whose last byte is 0x00 is invalid (the hash type byte may not be explicit DEFAULT).
- Stack elements are at most 520 bytes (`MAX_SCRIPT_ELEMENT_SIZE`) and scripts at most 10,000 bytes
  (`MAX_SCRIPT_SIZE`): Bitcoin Core `src/script/script.h`,
  https://github.com/bitcoin/bitcoin/blob/master/src/script/script.h. A P2SH redeem script is pushed
  in scriptSig, so it is limited to 520 bytes.

### EIP-191 and EIP-2
- EIP-191 (https://eips.ethereum.org/EIPS/eip-191, CC0): version 0x45 signs
  `0x19 ‖ "Ethereum Signed Message:\n" ‖ len(message) ‖ message`, length as ASCII decimal.
- EIP-2 (https://eips.ethereum.org/EIPS/eip-2): signatures with `s > secp256k1n/2` are invalid.
- Cross-check: Foundry `cast wallet sign` 1.8.5 vectors in `fixtures/eip191/`; Onyx's signer (libsecp256k1,
  RFC 6979) reproduces them byte for byte.

### Identity: why one signature is not enough
- A signature by an address only shows key control (BIP-322 text above). Binding "address → entity"
  needs a key the entity is known to hold. Onyx uses a registry-listed entity key co-signing the same statement (ADR-0015).

### Calibration scoring (for loop 13)
- Brier score: G. W. Brier, "Verification of forecasts expressed in terms of probability", Monthly
  Weather Review 78(1), 1950, https://doi.org/10.1175/1520-0493(1950)078%3C0001:VOFEIT%3E2.0.CO;2.
- Strictly proper scoring rules (Brier/quadratic, logarithmic, spherical): T. Gneiting and A. E. Raftery,
  "Strictly proper scoring rules, prediction, and estimation", JASA 102(477), 2007, pp. 359-378,
  https://doi.org/10.1198/016214506000001437. Proper rules make honest probability reports optimal.
- The log score is unbounded at p = 0 or 1, so a fixed-point version needs a clamp; Brier is bounded
  in [0, 1]. Forecasting sites (Good Judgment Open, Metaculus) report both (source: search result
  summary of the arXiv survey https://arxiv.org/abs/2504.01781; not independently checked).

## Live data sources (2026-10-08, loop 16)

- Bitcoin Core node on the owner's `indexer` host (mainnet, unpruned, synced): read-only `getblockhash`,
  `getblock 0` (raw bytes) and `getblock 3` (prevouts) via `scripts/fetch-btc-node.sh`. Public chain data,
  so no licence issue. Used for the live demo range 970,543-970,566.
- A commercial data product, per a read-only repo survey: the public API's per-exchange balance
  series (`/api/v1/exchange-etf/exchange-balances`) is scraped from TimeChainIndex with no redistribution
  licence found, so Onyx does not ingest it (hard limit). A vendor's own entity-indexer reserves
  (OKX PoR "exact" method) are a possible later cross-check, as balances, not block-range flows.

## M3: commit-reveal (2026-10-08, loop 17)
- ENS .eth registration uses commit-reveal against mempool front-running: a random 32-byte secret,
  and the commitment must be between `MIN_COMMITMENT_AGE` (60 s) and 24 hours old at registration
  (https://docs.ens.domains/registry/eth). Onyx binds the miner address into the commit instead of
  relying on a minimum age, because reveals open for everyone at the same height (ADR-0018).


## Website stack (2026-10-09)
Versions checked on the registries (npm, crates.io) on 2026-10-09:

| Package | Latest stable |
|---|---|
| leptos | 0.8.22 (pre-1.0) |
| dioxus | 0.7.10 (pre-1.0) |
| axum | 0.8.9 |
| wasm-bindgen | 0.2.129 |
| svelte / @sveltejs/kit | 5.57.2 / 3.0.1 |
| viem / wagmi | 2.57.4 / 3.7.7 |

- Rust UI frameworks: Leptos (fine-grained reactivity, SSR and islands, smallest wasm bundle of
  the three in one benchmark: 86 KB gzip vs Dioxus 210 KB, Yew 180 KB) and Dioxus (also desktop and
  mobile). The same article says their component ecosystems are young: "budget time for writing your
  own UI primitives" (https://www.pistack.xyz/posts/2026-08-29-dioxus-vs-yew-vs-leptos-rust-wasm-frameworks-comparison/,
  numbers are that article's, not checked here).
- The Rust/WASM working group's GitHub org was sunset; wasm-bindgen moved to its own org with new
  maintainers, other repos (incl. wasm-pack) archived or handed to maintainers
  (https://blog.rust-lang.org/inside-rust/2025/07/21/sunsetting-the-rustwasm-github-org). wasm-bindgen
  itself is still released (0.2.129, 2026-09-25).
- Base's own docs use viem for the frontend, with `baseSepolia` as the testnet chain
  (https://docs.base.org/learn/onchain-app-development/frontend-setup/viem). Rust wallet bridges exist
  (`ethers-web` with Leptos support, `ethereum-provider`; https://docs.rs/ethers-web) but are small.
- Rust can still own the trust-critical code in the browser: wasm-bindgen exports compiled from the
  same crates the network runs. `secp256k1-sys` needs clang to build for wasm32 (installed locally,
  free, via apt).

## M5: disputes prior art (2026-10-09)
From a research subagent; every claim linked. Three links were seen only as search results and not opened: the Oxford Law blog (Kleros criticisms), the UMA overview page, and the wasmi lib.rs page. "(Onyx proposal)" marks our own unsourced suggestions.


### 1. Interactive fault proofs (bisection games)

**Truebit verification game.** Solver and challenger binary-search the step trace (solver posts `c` evenly spaced Merkle-rooted states per round) until one step remains, which the chain checks; missing a timeout loses immediately ([Truebit paper, §3](https://arxiv.org/pdf/1908.04756)). Against the *verifier's dilemma* (rational verifiers skip checking), it forces errors "once every thousand tasks" paid from a capped jackpot ([same, §4.1](https://arxiv.org/pdf/1908.04756)). Delay risk: a party with 1000 ETH at stake can post a bogus answer behind a 10 ETH deposit just to delay; deposit sizing is left to the task giver ([same, §4.3](https://arxiv.org/pdf/1908.04756)). Concrete timeout values: not found.

**Arbitrum BoLD.** Bisection runs at three levels:
- **Block level:** up to 2^26 blocks.
- **BigStep level:** up to 2^19 BigSteps of 2^23 WASM steps each.
- **SmallStep level:** up to 2^23 single WASM steps.

At the end, the parent chain runs a one-step WASM proof ([BoLD deep dive](https://docs.arbitrum.io/bold/concepts/bold-technical-deep-dive)). The one-step prover is a set of Solidity contracts covering core ops, math, memory and host I/O ([nitro-contracts/src/osp](https://github.com/OffchainLabs/nitro-contracts/tree/main/src/osp)).

| BoLD parameter | Value | Source |
|---|---|---|
| Assertion bond | 3600 ETH | [AIP](https://forum.arbitrum.foundation/t/aip-bold-permissionless-validation-for-arbitrum/23232) |
| Per-level challenge bonds | 555 / 79 ETH | [AIP](https://forum.arbitrum.foundation/t/aip-bold-permissionless-validation-for-arbitrum/23232) |
| Challenge period | about 6.4 days | [FAQ](https://docs.arbitrum.io/how-arbitrum-works/bold/bold-faq) |
| Worst-case confirmation | 2 challenge periods + 2-day Security Council grace | [AIP](https://forum.arbitrum.foundation/t/aip-bold-permissionless-validation-for-arbitrum/23232) |

How BoLD limits griefing:
- **Delay attacks.** In Arbitrum Classic, N colluding stakers could delay resolution by about 2·N challenge periods. BoLD is all-vs-all and finishes within 2·(C_max+D), however many adversaries join ([BoLD paper](https://arxiv.org/html/2404.10491)).
- **Cost asymmetry.** An attacker pays about 10× the honest party's costs ([FAQ](https://docs.arbitrum.io/how-arbitrum-works/bold/bold-faq)).
- **Self-challenge griefing.** Honest parties get only a small share of confiscated bonds, which removes the profit motive ([deep dive](https://docs.arbitrum.io/bold/concepts/bold-technical-deep-dive)). The FAQ puts that share at 1% ([FAQ](https://docs.arbitrum.io/how-arbitrum-works/bold/bold-faq)).

**Optimism Fault Dispute Game + Cannon.** The game first bisects over output roots, then over one block's MIPS execution trace. Split depth is 30 and maximum depth is 73. At the bottom, `MIPS64.sol` executes one instruction on-chain ([OP reference](https://docs.optimism.io/op-stack/fault-proofs/reference)). Cannon requires the off-chain and on-chain VMs to produce "exactly the same results" for the same instruction and state ([Cannon docs](https://docs.optimism.io/op-stack/fault-proofs/cannon)).

| OP parameter | Value | Source |
|---|---|---|
| Bond per move | grows about 9.5% per depth: 0.08 ETH at depth 0, 60 ETH at depth 73 | [OP reference](https://docs.optimism.io/op-stack/fault-proofs/reference) |
| Total bonds to play to max depth | about 691 ETH | [OP reference](https://docs.optimism.io/op-stack/fault-proofs/reference) |
| Chess clock per side | 3.5 days | [OP reference](https://docs.optimism.io/op-stack/fault-proofs/reference) |
| Clock extension | 3 hours | [OP reference](https://docs.optimism.io/op-stack/fault-proofs/reference) |
| DelayedWETH bond-withdrawal delay | 3.5 days | [OP reference](https://docs.optimism.io/op-stack/fault-proofs/reference) |

Escalating bonds make deep stalling expensive; the clocks cap the game at 7 days.

### 2. Optimistic oracles

**UMA.** A bonded answer stands unless disputed within the liveness window (default two hours; longer advised for high stakes); bond should be "larger than the final fee" ([UMA bond/liveness guide](https://docs.uma.xyz/developers/setting-custom-bond-and-liveness-parameters)). A winning disputer gets half the proposer bond, the Store the other half, so self-disputing never profits ([same](https://docs.uma.xyz/developers/setting-custom-bond-and-liveness-parameters)). Only disputes reach the DVM token-holder vote ([UMA overview](https://docs.uma.xyz/protocol-overview/how-does-umas-oracle-work)).

**Polymarket flow.**
1. A proposer posts about $750 and a 2-hour challenge period starts.
2. If disputed, a new proposal round starts.
3. If the second proposal is also disputed, the question goes to a UMA DVM vote that takes about 48 hours.

The winner gets their bond back plus half the loser's. Undisputed questions resolve in about 2 hours; disputed ones take 4–6 days ([Polymarket docs](https://docs.polymarket.com/developers/resolution/UMA)).

**Known failure.** March 2025: a $7M market resolved "Yes" after a UMA whale allegedly swung the DVM vote; no refunds ([The Block](https://www.theblock.co/post/348171/polymarket-says-governance-attack-by-uma-whale-to-hijack-a-bets-resolution-is-unprecedented)).

### 3. Juries / Schelling courts (Kleros)

- **Draw:** jurors stake PNK in a court and are drawn with probability proportional to their stake. ([Kleros whitepaper](https://kleros.io/assets/whitepaper.pdf); [FAQ](https://docs.kleros.io/welcome/faq)).
- **Appeals:** each appeal uses "twice the previous number of jurors plus one"; fees scale with jury size ([whitepaper §4](https://kleros.io/assets/whitepaper.pdf)). Anyone can crowdfund an appeal ([FAQ](https://docs.kleros.io/welcome/faq)).
- **Incentive:** jurors whose vote is incoherent with the final ruling lose `α · min_stake · weight` PNK to coherent jurors ([whitepaper §4.6](https://kleros.io/assets/whitepaper.pdf)).
- **Criticisms:**
  - Kleros itself names the 51%-of-PNK attack and the p+ε bribe ([whitepaper §4.7](https://kleros.io/assets/whitepaper.pdf)).
  - Stake-weighted selection favours large holders.
  - There may be no reliable focal point in genuinely contested cases.
  - No empirical study of Kleros error rates exists.

  Source for the last three: ([Oxford Law blog](https://blogs.law.ox.ac.uk/node/51631)).

### 4. What fits Onyx

Context: recipes run as deterministic WASM under wasmi, which offers deterministic fuel metering via `Config::consume_fuel` ([wasmi docs](https://docs.rs/crate/wasmi/2.0.0-beta.9/source/src/lib.rs)). Rerunning one shard off-chain takes about 1 s.

| | (a) Off-chain rerun by a drawn bonded resolver panel | (b) Bisection + on-chain one-step WASM proof |
|---|---|---|
| Trust assumption | Honest majority of the drawn panel, backed by appeals | 1 honest party is enough (the chain judges) |
| Build cost | Contracts for bonds, draws, a commit-reveal ruling and slashing. Resolvers reuse the existing validator rerun code. | An on-chain WASM step interpreter that matches wasmi bit-for-bit, plus Merkleised memory/stack and a trace-commit format. Arbitrum needed a separate contract per op class ([osp](https://github.com/OffchainLabs/nitro-contracts/tree/main/src/osp)). |
| Determinism risk | Low: all resolvers run the same wasmi | High: off-chain and on-chain must match exactly ([Cannon](https://docs.optimism.io/op-stack/fault-proofs/cannon)) |
| Latency | One window plus a vote | Many bisection rounds, bounded by chess clocks (OP: up to 7 days) |
| Griefing surface | Panel draw and bribery (Kleros-type risks) | Delay games; needs BoLD/OP-style bond escalation and clocks |
| Fits level-4 bonded claims? | Yes, the same panel machinery can judge evidence | No, only for recomputable results |

Unlike Kleros, (a) has an objective answer (a deterministic root), so dissenting resolvers are slashed objectively; the "no focal point" criticism does not apply.

**Recommendation for the testnet prototype: (a), designed so (b) can be added later.**
1. **Window.** Challenger bonds against a finalized root within the window, naming shard and claimed root.
2. **Draw.** Draw k bonded validators from a future block hash; exclude the original result's validators.
3. **Ruling.** Resolvers rerun and commit-reveal roots; majority wins. Loser's bond split UMA-style (half winner, half burn/treasury).
4. **Appeal.** Any dissent allows appeal to 2k+1 resolvers at a higher bond (Kleros pattern); a labelled team multisig is the testnet backstop.
5. **Commit fuel used and the per-shard trace root now.** Enables later BoLD-style bisection over wasmi steps without changing commitments.

Level-4 bonded claims reuse steps 1–4 with panelists judging evidence, so Kleros/UMA criticisms apply in full.

### 5. Parameters others use

| System | Challenge window | Bond sizing |
|---|---|---|
| Arbitrum BoLD | about 6.4 days; worst case 2 periods + 2 days ([AIP](https://forum.arbitrum.foundation/t/aip-bold-permissionless-validation-for-arbitrum/23232)) | 3600 ETH assertion; 555/79 ETH per level ([AIP](https://forum.arbitrum.foundation/t/aip-bold-permissionless-validation-for-arbitrum/23232)) |
| OP fault proofs | 3.5-day clock per side, +3 h extension ([ref](https://docs.optimism.io/op-stack/fault-proofs/reference)) | 0.08→60 ETH, about 9.5%/depth ([ref](https://docs.optimism.io/op-stack/fault-proofs/reference)) |
| UMA OO | 2 h default ([UMA](https://docs.uma.xyz/developers/setting-custom-bond-and-liveness-parameters)) | bond > final fee; scale with value at risk ([UMA](https://docs.uma.xyz/developers/setting-custom-bond-and-liveness-parameters)) |
| Polymarket | 2 h; DVM vote about 48 h ([PM](https://docs.polymarket.com/developers/resolution/UMA)) | about $750 ([PM](https://docs.polymarket.com/developers/resolution/UMA)) |
| Kleros | per-court periods (values not found) | appeal fees grow with juror count 2n+1 ([whitepaper](https://kleros.io/assets/whitepaper.pdf)) |
| Truebit | timeouts (values not found) | deposits set by the task giver; forced error 1/1000 tasks ([paper](https://arxiv.org/pdf/1908.04756)) |

**(Onyx proposal, unsourced)** For testnet:
- **Challenge window:** short, such as 1 h, so we can iterate quickly. Lengthen it before value is at stake.
- **Challenger bond:** at least the expected cost of a panel rerun plus a margin.
- **Submitter bond:** at least the challenger bond, so challenging a wrong result always profits.

## M6: settlement contract (2026-10-09, loop 32)
- Base Sepolia: chain id 84532, OP Stack L2, 2-second blocks, settles to Ethereum Sepolia; public
  RPC `https://sepolia.base.org` ([Alchemy network page](https://www.alchemy.com/rpc/base-sepolia.md)).
  Two-second blocks set our window sizes: 1 hour is about 1,800 blocks.
- Beacon. `blockhash(n)` only returns the hash for the 256 most recent blocks, else zero (Solidity
  docs, units and global variables: https://docs.soliditylang.org/en/latest/units-and-global-variables.html,
  standard reference, not reopened here). On an L2 with a central sequencer, the sequencer can bias
  both `blockhash` and `prevrandao`; users with mostly private mempools cannot
  ([Arbitrum research thread](https://research.arbitrum.io/t/randomness-please-bridge-block-difficulty-randao-op-code/7897),
  [ethereum-magicians on RANDAO(n)](https://ethereum-magicians.org/t/expanding-eip-4399-prevrandao-with-randao-n/19741)).
  Since Bedrock, OP Stack L2 block hashes are committed to L1 and headers have the EVM layout
  ([Herodotus OP Stack notes](https://docs.herodotus.dev/herodotus-docs/protocol-design/architecture/op-stack)).
  Decision input: use `blockhash(beacon_height)`, accept sequencer trust for the testnet, void the
  round if nobody draws the seed within 256 blocks (about 8.5 minutes on Base).
- Signatures. EIP-191 version 0x45 personal messages (https://eips.ethereum.org/EIPS/eip-191) and
  EIP-2 low-s (https://eips.ethereum.org/EIPS/eip-2); the Rust verifier already accepts only
  v in {27, 28} and s <= n/2, so the contract applies the same rule (standard references).
- Payments. Pull payments plus checks-effects-interactions and a reentrancy guard
  ([OpenZeppelin PullPayment docs](https://docs.openzeppelin.com/contracts/3.x/api/payment),
  [OpenZeppelin security API](https://docs.openzeppelin.com/contracts/4.x/api/security)): credit
  balances in storage, let each payee withdraw, never push tokens inside settlement logic.

### M6 loop 2: on-chain disputes and the contract split (2026-10-10)
- **Size limits.** Deployed runtime code is capped at 24,576 bytes (EIP-170,
  https://eips.ethereum.org/EIPS/eip-170) and initcode at 49,152 bytes (EIP-3860,
  https://eips.ethereum.org/EIPS/eip-3860); standard references, not reopened here.
  `OnyxSettlement` was at 23,989 B, so the dispute game cannot be added in place.
- **Splitting options** (Solidity docs, contracts/libraries:
  https://docs.soliditylang.org/en/latest/contracts.html#libraries, standard reference). A library
  function marked `public`/`external` is deployed once and reached by `DELEGATECALL`, so its code
  leaves the caller's runtime; `internal` library code is copied into the caller. A second contract
  with its own storage and a narrow privileged interface keeps each runtime under the cap without a
  proxy. Measured here: linking `OnyxRound` (round tag, set root, canary commit, commit text) saves
  1,223 B of `OnyxSettlement` runtime (23,989 -> 22,766); not enough on its own.
- **Final arbiter.** UMA escalates a disputed answer to a token vote (DVM) and Polymarket accepts the
  vote's 48 h latency (links in "M5: disputes prior art" section 2). Kleros grows each appeal jury to
  2n+1 and anyone may fund an appeal (section 3). The Onyx M5 model already chose (a) a drawn panel
  with appeals and, for splits, the settler's rerun as final arbiter (q13 (a), ADR-0021 item 22).
  On chain the contract cannot rerun WASM, so the rerun is posted by the bonded coordinator: the
  same party that already runs `Ledger::rerun_shard` off-chain. Mainnet path: q7 (b) proofs.
- **Claw-backs.** A pull-payment escrow cannot take back credits a payee already withdrew (OpenZeppelin
  PullPayment, linked in "M6: settlement contract"). So anything a dispute may reverse (miner reward,
  revealed bond) must stay in escrow until the root is final; validator stakes are locked instead.

## M7: delivery and x402 (2026-10-10, loop 38)
- **x402 v2 HTTP transport** ([specs/transports-v2/http.md](https://github.com/x402-foundation/x402/blob/main/specs/transports-v2/http.md),
  read via the GitHub API at commit e187dda of the core spec, 2026-08-31): the server answers 402 with a
  `PAYMENT-REQUIRED` header (base64 JSON `PaymentRequired`); the client retries with `PAYMENT-SIGNATURE`
  (base64 JSON `PaymentPayload`); the server returns `PAYMENT-RESPONSE` (base64 JSON `SettlementResponse`).
  v1 used `X-PAYMENT` / `X-PAYMENT-RESPONSE` and a JSON body; v2 moves everything into headers and leaves
  the body to the server. Status mapping: 402 payment required or failed, 400 malformed payload, 500
  server error.
- **Types** ([core spec v2](https://github.com/x402-foundation/x402/blob/main/specs/x402-specification-v2.md) §5):
  `PaymentRequired { x402Version: 2, error?, resource { url, description?, mimeType? }, accepts: [PaymentRequirements], extensions? }`;
  `extensions` (§5.1.2, read 2026-10-17): a map of extension id to `{info, schema}` (`schema` a JSON Schema of `info`); clients echo it in `PaymentPayload`; no naming rule for ids; per-extension rules live in `specs/extensions/` ([bazaar](https://github.com/x402-foundation/x402/blob/main/specs/extensions/bazaar.md): JSON Schema draft 2020-12). Used by ROADMAP B4 (`extensions.limits`);
  `PaymentRequirements { scheme, network (CAIP-2), amount (atomic units, string), asset, payTo, maxTimeoutSeconds, extra? }`;
  `PaymentPayload { x402Version, resource?, accepted: PaymentRequirements, payload { signature, authorization {...} } }` (exact/EVM:
  EIP-3009 `transferWithAuthorization` fields); `SettleResponse { success, errorReason?, payer?, transaction, network, amount? }`.
- **Facilitator** (§7): `POST /verify` and `POST /settle`, both with body
  `{ x402Version, paymentPayload, paymentRequirements }`; verify returns `{ isValid, invalidReason?, payer? }`
  and is read-only; settle commits. Default flow for `exact` is verify -> serve -> settle -> respond (§6.1);
  at least one check must run before the resource executes. Error codes in §9 (`insufficient_funds`,
  `invalid_payload`, `settlement_pending` (non-terminal), ...). Replay protection is the EIP-3009 nonce
  enforced on chain (§10.1).
- **Networks/assets** (§11): CAIP-2 ids, `eip155:84532` = Base Sepolia. The HTTP transport spec's example
  uses asset `0x036CbD53842c5426634e7929541eC2318f3dCF7e` with `extra { name: "USDC", version: "2" }` on
  `eip155:84532`; DEFAULT_ASSETS.md points to code for the table (not read). Treat the address as
  configuration and confirm it against Circle's list before the live run.
- **Rust**: ADR-0006 planned `x402-axum` (community x402-rs). Its dependency tree (EVM client stack) is
  large and this box has under 10 GB free; loop 38 implements the v2 wire types directly (about 300 lines,
  serde) behind a `Facilitator` trait, so `x402-axum` or the official TypeScript middleware can replace
  the gate later without touching handlers (ADR-0023).
- **axum 0.8** ([docs.rs/axum 0.8](https://docs.rs/axum/0.8.9/axum/)): path parameters use `{name}`
  (0.7 used `:name`); handlers are async fns with extractors; `Router::with_state`; tests drive the router
  with `tower::ServiceExt::oneshot` without a socket ([axum testing example](https://github.com/tokio-rs/axum/tree/main/examples/testing)).
- **Caching**: `ETag` / `If-None-Match` -> 304 ([RFC 9110 §8.8.3, §13.1.2](https://www.rfc-editor.org/rfc/rfc9110)).
  A finalized row under a fixed root never changes, so `"<root>:<index>"` is a strong validator.
- **Pricing**: docs/spec/pricing.md step function (`base` while fresh, `0` after `fresh_window`). No outside
  source; decay is an Onyx product rule.

## Money loop (option A): fee split, contributor reads, dataset prices (2026-10-10)
- **EIP-3009** ([eips.ethereum.org/EIPS/eip-3009](https://eips.ethereum.org/EIPS/eip-3009), status Draft):
  `transferWithAuthorization(from, to, value, validAfter, validBefore, nonce, v, r, s)`,
  `receiveWithAuthorization(...)` (same, plus a check that the caller is the payee, against
  front-running) and `authorizationState(authorizer, nonce) -> bool` (true once that nonce was used).
  Nothing in it calls the recipient, so a contract that receives a transfer learns nothing about why.
- **x402 `exact` on EVM** ([scheme_exact_evm.md](https://github.com/x402-foundation/x402/blob/main/specs/schemes/exact/scheme_exact_evm.md)):
  the facilitator settles by calling `transferWithAuthorization` with the client's signed
  authorization; `to` is the requirements' `payTo`, which the facilitator cannot change; the client
  picks the nonce (random 32 bytes in the example). The spec does not forbid a contract as `payTo`.
  Consequence for Onyx: with the public facilitator, revenue can land in a contract, but which shard
  a payment bought is known only to the API server; the contract can check that a claimed
  `(payer, nonce)` authorization was really used (`authorizationState`), not what it paid for.
- **Prior art, usage fees to data workers.** The Graph: query fees go to Indexers through a rebate
  function, 10% of claimed query fees go to the subgraph's Curators and 1% is burned
  ([tokenomics docs](https://thegraph.com/docs/en/tokenomics)). Ocean Protocol: one consume
  transaction can pay several parties (publish market, consume market, provider, community wallet)
  ([fees docs](https://docs.oceanprotocol.com/core-concepts/fees)). Both split each usage fee by
  role, which is the shape Onyx takes; both use their own token, which Onyx does not (USDC only).
- **No source found** for a standard "contribute data, read free" rule; it is an Onyx product rule
  (ADR-0025), bounded by bonds and rate limits instead of a token.

## Proof ladder level 2: multi-observer oracles (2026-10-10, loop 57)
- **SchellingCoin** ([Ethereum blog, 2014-03-28](https://blog.ethereum.org/2014/03/28/schellingcoin-a-minimal-trust-universal-data-feed)):
  participants submit a hash of their value with their address, reveal it in the next period with
  a deposit; the median is taken as the answer and answers near it (25th-75th percentile) are
  rewarded. Onyx level 2 takes the shape (address-bound commit, then reveal, reward agreement) but
  replaces the percentile band with a fixed integer tolerance around the median, so the reward rule
  does not depend on how spread the honest answers are. Known failure mode: bribery of a majority
  ([ethresear.ch: Schelling coin bribery bidding wars](https://ethresear.ch/t/schelling-coin-bribery-bidding-wars/7484)).
- **Freeloading** ([A Study of Blockchain Oracles, arXiv 2004.07140](https://arxiv.org/pdf/2004.07140)):
  a node copies another node's published answer instead of querying the source; the standard
  defence is commit-reveal, so answers are exposed only after all commitments are in. Onyx binds
  the observer address into the commit hash, so a copied commit cannot even be revealed by the copier.
- **Chainlink OCR** ([paper v1.2, Breidenbach et al., 2021](https://webcf.waybackmachine.org/web/20240313161048/https://research.chain.link/ocr.pdf);
  [overview](https://mmapped.blog/posts/24-ocr)): n oracles, at most f < n/3 Byzantine, a leader
  collects at least 2f+1 signed observations and the report is their median. Proves: the median is
  bounded by honest observations when f < n/3. Does not prove: that the honest observations were
  right (all nodes may read the same wrong source). A later paper reports a security pitfall in its
  validity rules ([NDSS: "Validity is not enough"](https://www.ndss-symposium.org/ndss-paper/validity-is-not-enough-uncovering-the-security-pitfall-in-chainlinks-off-chain-reporting-protocol/)).
  Onyx level 2 has no leader and no off-chain consensus: commits and reveals are ledger entries.
- **Pyth** ([price aggregation docs](https://docs.pyth.network/price-feeds/how-pyth-works/price-aggregation)):
  first-party publishers submit a price and a confidence interval; the aggregate must stay near the
  majority when one publisher is far off. Pyth publishers are the data owners (closer to Onyx level
  3); Onyx level 2 observers are third parties reading a public page.
- **UMA optimistic oracle** ([how it works](https://docs.uma.xyz/protocol-overview/how-does-umas-oracle-work);
  [bond and liveness](https://docs.uma.xyz/developers/setting-custom-bond-and-liveness-parameters)):
  one bonded proposer, a liveness window, and disputes escalated to a token-holder commit-reveal
  vote (DVM). This is Onyx level 4 (bonded claim), except Onyx resolves disputes by a drawn bonded
  panel (ADR-0021), not a token vote.
- **TLSNotary / zkTLS** ([docs](https://docs.tlsnotary.org); [MPC-TLS](https://tlsnotary.org/docs/protocol/mpc-tls/)):
  a Verifier joins the TLS session through MPC, so the Prover cannot forge the Server's response and
  can selectively disclose parts of it. This is the upgrade path from level 2 to level 3 for the
  same public page: one proven fetch instead of a majority of unproven ones. Not built.
- **Failure modes carried into THREATS** (L2-*): copying (commit-reveal + address binding), majority
  collusion or bribery (bounded by bonds and random assignment, not removed), source spoofing or
  a source that serves different answers to different clients (needs zkTLS to remove), time skew
  (a pinned observation window plus a tolerance; fast-moving exact values do not fit level 2),
  source outages (an explicit `Unavailable` answer).

## Proof ladder level 3: source-signed data and zkTLS (2026-10-10, loop 62)
What first-party signing and zkTLS each prove, and what Onyx level 3 takes from them.
- **Signed JSON and HTTP responses.** JWS ([RFC 7515](https://www.rfc-editor.org/rfc/rfc7515)) signs
  a JSON payload with a key named in its header (`kid`); HTTP Message Signatures
  ([RFC 9421](https://www.rfc-editor.org/rfc/rfc9421)) sign chosen components of an HTTP message
  under strict canonicalization, so a response can be verified after intermediaries touch it. Both
  prove "the key holder signed these bytes"; neither says who the key holder is. That link has to
  come from somewhere the source controls. Onyx level 3 signs a fixed text over a Borsh payload
  digest instead (no JSON or HTTP canonicalization to get wrong), and ties the key to the source
  through its origin.
- **Key discovery under the source's origin.** RFC 8615 reserves `/.well-known/` on an origin for
  site metadata ([RFC 8615](https://www.rfc-editor.org/rfc/rfc8615)); only who controls the origin's
  HTTPS server can publish there. Onyx level 3 records keys from
  `https://<origin>/.well-known/onyx-source-keys.json` (or a DNS TXT record under the origin) with
  the fetched document's hash and time. The record is a review artefact: the intake cannot re-fetch
  the web deterministically.
- **Signed reports from oracle networks.** Chainlink Data Streams hand out a signed report that a
  verifier contract checks against the network's signers before a contract uses the data
  ([onchain report verification](https://docs.chain.link/data-streams/reference/data-streams-api/onchain-verification)).
  Same shape as Onyx level 3 (signed report in, verified value out), but the signers are the
  oracle network, not the data's publisher. Pyth publishers are first-party (see "Proof ladder
  level 2" above). Exchange proof-of-reserves address files signed by the exchange are already
  used by Onyx labels (fixtures/labels/PROVENANCE.md, ADR-0015).
- **zkTLS: TLSNotary.** A Verifier (or a general Notary) joins the TLS session through MPC and signs
  commitments to the data and the server identity, without learning the plaintext; verifiers must
  trust the Notary, and can require several Notaries against Prover-Notary collusion
  ([TLSNotary docs](https://tlsnotary.org/docs/intro)). The docs state it supports TLS 1.2, with
  TLS 1.3 on the roadmap, and that it "does not solve the oracle problem" (same page). The Rust
  implementation is MIT or Apache-2.0 and its README says it is under active development and
  should not be used in production ([tlsnotary/tlsn](https://github.com/tlsnotary/tlsn)).
- **zkTLS: DECO.** Proves that data came from a website over TLS, optionally with zero-knowledge
  statements about it, without trusted hardware or server changes
  ([Zhang et al., arXiv 1909.00938](https://arxiv.org/abs/1909.00938)). A research design; I found
  no open-source, licensed Rust verifier to adopt.
- **Decision for Onyx (ADR-0028).** Build lane (a), source-signed reports, now: EIP-191 over
  secp256k1, the scheme Onyx labels already verify with a pinned crate (`secp256k1 =0.29.1`,
  ADR-0015), and the one the EVM checks natively (`ecrecover`), which the on-chain port needs.
  Ed25519 (RFC 9421 defines an `ed25519` algorithm) would need a new Rust dependency and, for
  the on-chain port, an Ed25519 verifier contract instead of `ecrecover`; it is a TODO for sources
  that cannot sign secp256k1. zkTLS (lane b) stays design-only until a verifier meets the criteria in
  ADR-0028 (not alpha, TLS 1.3, audited, license-clean, a Notary trust model Onyx can bond).

## Level 3 books on chain and the chain clock (2026-10-17, ROADMAP C2)
Prior art for a replay and equivocation book that every reader shares, and for which clock to trust.
- **Unordered nonces.** Permit2's SignatureTransfer does not use incrementing nonces: it keeps a
  `nonceBitmap` per owner, the nonce picks a word and a bit, and "Bits that are dirtied may not be
  used again" ([Uniswap docs, SignatureTransfer](https://developers.uniswap.org/docs/protocols/permit2/concepts/signature-transfer)).
  Signatures can be used in any order. Onyx's chain book takes the same shape: a write-once
  `(key, sequence)` entry, never a per-key high-water mark (THREATS L3R-F5: a high-water mark lets
  whoever holds a later report refuse every earlier one).
- **Monotonic per-feed time.** Pyth's EVM `updatePriceFeedsIfNecessary` applies an update only if
  the stored publish time is before the new one, else `NoFreshUpdate`
  ([Pyth API reference](https://api-reference.pyth.network/price-feeds/evm/updatePriceFeedsIfNecessary)).
  Right for a price feed (only the newest value matters), wrong for Onyx facts, which are dated
  values that may arrive in any order; Onyx keeps one value per dated fact instead.
- **Equivocation as portable evidence.** Ethereum's `process_proposer_slashing` takes two signed
  headers and asserts the same slot, the same proposer, different headers and both signatures
  valid; `is_slashable_attestation_data` flags a double vote (different data, same target epoch)
  ([consensus specs, phase 0](https://ethereum.github.io/consensus-specs/specs/phase0/beacon-chain/)).
  Onyx's two equivocations are the same pattern: one key and sequence with two payloads, or one
  source and fact with two values, both signatures checked on chain, both reports kept as evidence.
- **Whose clock.** OP Stack derivation sets `block.timestamp = prev_l2_timestamp + l2_block_time`
  and requires `l1_origin.timestamp <= block.timestamp <= max_l2_timestamp` with
  `max_l2_timestamp = max(l1_origin.timestamp + max_sequencer_drift, prev_l2_timestamp +
  l2_block_time)`; `max_sequencer_drift` is 1800 s since Fjord (600 s before)
  ([OP Stack derivation spec](https://specs.optimism.io/protocol/derivation.html)). So on Base the
  sequencer does not choose the timestamp of a block; it chooses which transactions go in which
  block, and can delay one. Decision input: use `block.timestamp` for every level-3 time rule;
  treat sequencer delay of a contradicting post as the residual (THREATS L3C-2). Correction
  (review C2-R2): the same bound lets a sequencer that produces blocks faster than every 2 s run
  `block.timestamp` up to `max_sequencer_drift` ahead of its L1 origin, so a time window measured
  in block time can be up to 1800 s shorter in real time (THREATS L3C-12).

## A first real level-2 source (2026-10-11, loop 62)
- **U.S. Treasury Fiscal Data** ([About Us, "Licensing"](https://fiscaldata.treasury.gov/about-us/)):
  "The data on this site is available to copy, adapt, redistribute, or otherwise use for
  non-commercial and commercial purposes." The API needs no account or key
  ([API documentation](https://fiscaldata.treasury.gov/api-documentation/)). Chosen: the Debt to
  the Penny record for one date (`filter=record_date:eq:YYYY-MM-DD`), a decimal string in USD; a
  published historical record does not move, so honest observers agree to the cent.
- **ECB euro reference rates** ([ECB disclaimer and copyright](https://www.ecb.europa.eu/services/disclaimer/html/index.en.html)):
  reuse allowed with the ECB cited as source, modifications stated, and extra duties when sold
  commercially. Usable, but the duties would have to travel with every served row; not chosen for
  the first source.

## Self-settled x402 (2026-10-10, loop 57)
- **What an x402 `exact` client signs** ([scheme_exact_evm.md](https://github.com/x402-foundation/x402/blob/main/specs/schemes/exact/scheme_exact_evm.md),
  read today from the raw file): with the EIP-3009 method, `payload.signature` is "the 65-byte
  signature of the `transferWithAuthorization` operation" and `payload.authorization` its fields;
  the facilitator settles "via the facilitator calling the `transferWithAuthorization` function on
  the EIP-3009 compliant contract". Verify step 5 is "Simulate `token.transferWithAuthorization(...)`".
  The spec also offers Permit2 (any ERC-20) and ERC-7710 methods; Onyx uses EIP-3009 only.
- **Consequence:** `receiveWithAuthorization` ([EIP-3009](https://eips.ethereum.org/EIPS/eip-3009))
  checks a `ReceiveWithAuthorization` type hash, so it cannot redeem an x402 payload; a contract
  can instead submit the client's `transferWithAuthorization` itself (anyone may) with `to` = the
  contract, which the signature already fixes (ADR-0026).
- **Base Sepolia USDC** at `0x036CbD53842c5426634e7929541eC2318f3dCF7e`, read with `cast` today:
  `name()` "USDC", `version()` "2", 6 decimals; a proxy to `0xd74cc5d436923b8ba2c179b4bCA2841D8A52C5B5`
  whose code has both `transferWithAuthorization` forms (v,r,s: `0xe3ee160e`; bytes: `0xcf092995`),
  `receiveWithAuthorization`, `authorizationState` and the `masterMinter`/`configureMinter`/`mint`
  minting surface (used only inside a local fork to mint test USDC).
- **Base Sepolia fees and finality**, read today from https://sepolia.base.org: base fee 5,000,000
  wei (0.005 gwei), `eth_maxPriorityFeePerGas` 1,000,000 wei; the `finalized` block trailed the
  head by about 490 blocks (about 16 minutes at 2 s); the OP Stack `GasPriceOracle` predeploy
  (`0x4200…000F`) reports Fjord and `getL1FeeUpperBound(600)` = 490 wei. Measured gas of a full
  run (fork rehearsal, docs/ops/demo-testnet.md): about 32 million in all, deploy 16.7 million.

## SEC EDGAR for company-filed holdings (indexer Phase 2 item 4, 2026-10-11)
- Reuse: "Information presented on sec.gov is considered public information and may be copied or
  further distributed by users of the web site without the SEC's permission"
  (https://www.sec.gov/privacy, read through a fetch tool on 2026-10-11).
- Access: https://www.sec.gov/os/accessing-edgar-data asks clients to "declare your user agent"
  (example: "Sample Company Name AdminContact@<sample company domain>.com") and states "Current
  max request rate: 10 requests/second". With a project-only User-Agent (no e-mail), data.sec.gov
  answered 200 and www.sec.gov pages answered 403 (2026-10-11).
- `companyconcept` (https://data.sec.gov/api/xbrl/companyconcept/CIK##########/us-gaap/<tag>.json)
  returns every unit a filer used for a concept; the frames API is keyed by unit and misses filers
  who chose another unit (the entity indexer, docs/unattributed/ITEM-B-treasuries.md).
- `companyconcept` serves only facts without dimensions, so a value is the company-wide
  crypto-asset unit total the filing tags, not a per-asset figure (facts on `CryptoAssetAxis`
  are not in it). Units differ by filer: Strategy Inc tags `Bitcoin`, Riot Platforms `item`.
- What the concept tags differs by filer. Strategy's 762,099 at 2026-03-31 equals its own
  disclosure of 762,099 BTC as of 2026-03-29 (Yahoo Finance, 2026-03-30:
  https://finance.yahoo.com/markets/crypto/articles/michael-saylor-skips-bitcoin-purchase-165000053.html).
  Riot's 15,679 at 2026-03-31 is within 1 of its 15,680 "Bitcoin Held" (Riot, 2026-04-02:
  https://www.riotplatforms.com/riot-announces-first-quarter-2026-production-and-operations-updates/).
  CleanSpark's 2,583 at 2025-09-30 equals the BTC it "posted as collateral or as a receivable",
  of total holdings of 13,011 (CleanSpark, 2025-10-03:
  https://finviz.com/news/183200/cleanspark-releases-september-2025-bitcoin-mining-update), so
  there the concept tags a subset, and CleanSpark is not registered (SEC-R1-F2). Its 2026-06-30
  value (1,719,000) was first misread as a scale error; the subset reading explains its whole
  series (1,229; 2,583; 1,648; 1,641).
- Filing deadlines (10-Q 40 or 45 days, 10-K 60, 75 or 90 days after the period by filer status;
  Rule 12b-25 / Form NT adds 15 days for a 10-K and 5 for a 10-Q): SEC Form 10-Q and 10-K
  general instructions; Onyx waits 110 days (SEC-R1-F4).

## On-chain proof-of-reserve feeds (indexer Phase 2 item 1, 2026-10-11): blocked on rights
- Feed the indexer tracks: Ethereum Chainlink cbBTC Proof of Reserve, proxy
  0xcbe87dc0cf9d807848a3e703b01a90b28ecfb2a7 (ENS cbbtc-reserves.data.eth, description
  "cbBTC Reserves", 8 decimals), read by eth_call getRoundData at a pinned block
  (the entity indexer `scripts/fetch-cbbtc-chainlink-history.py`; feed page
  https://data.chain.link/feeds/base/base/cbbtc-por). The indexer itself notes it is a
  "Wallet-Address-Manager, self-attested aggregate" (runs/btcfi/cbbtc-chainlink/README.md).
- Terms: Chainlink Foundation Terms of Service, Version 6.0, effective 2026-08-18
  (https://chain.link/terms, read 2026-10-11 by rendering the page in headless Chrome; the static
  HTML carries no text). They govern the Website and the "Services", and Section 1 defines the
  Services to include data services that node operators provide "via data feeds". Section 3
  (licence) says you may not "resell, lease, lend, share, distribute or otherwise permit any third
  party to use" the Services; Section 2 adds no use of the Services to "commercialize, license,
  or sell" a competing product and no breach of Chainlink Network third-party provider terms.
- Reading: Onyx would redistribute feed answers in a paid API. On the terms' own wording that is
  distributing the data feed service to third parties, so the CLAUDE.md hard limit ("never ingest
  data whose license forbids redistribution") applies. Whether the terms bind someone who only
  reads public Ethereum state is a legal question we do not decide; the rule is to treat a
  restrictive licence as binding. The cbBTC answer is also Coinbase's self-attested number, and
  Coinbase's terms are separately unreviewed (IDX-R1-F2).
- Decision: nothing built; item 1 recorded as BLOCKED. Ways to unblock (owner's call): written
  permission from the Chainlink Foundation (legal@chain.link) and Coinbase for redistribution, or
  a reserve attestation published on chain under an open licence by the issuer itself.


## Chain indexer for `OnyxSettlement` events (2026-10-12, ADR-0033)
- **Block tags** ([ethereum.org JSON-RPC](https://ethereum.org/en/developers/docs/apis/json-rpc/), read
  2026-10-12): the default block parameter takes `"finalized"` ("the latest finalized block") and
  `"safe"`; `eth_getLogs` takes `fromBlock`/`toBlock` (number or tag) or a single `blockHash` (then
  neither range field is allowed). So a scan can be bounded by the number of the `finalized` block,
  and a view read pinned to that number.
- **Base finality** ([docs.base.org transaction finality](https://docs.base.org/base-chain/network-information/transaction-finality),
  read 2026-10-12): L2 block ~2 s, L1 batch inclusion ~2 min, L1 batch finality ~20 min ("older than 2
  epochs, or 64 L1 blocks"), after which the L2 blocks are "in practice impossible to reverse". Our
  Base Sepolia run saw the root reach the `finalized` block after about a quarter hour (STATUS.md).
  Reading only up to `finalized` makes the indexer reorg-safe without its own rollback logic.
- **Range caps, observed** (this loop, 2026-10-12): `https://sepolia.base.org` answers an
  `eth_getLogs` over more than 200 blocks with HTTP 413 and
  `{"code":-32614,"message":"eth_getLogs is limited to a 200 range"}`; a 201-block inclusive range
  (to - from = 200) was accepted. The node is load-balanced and lags (see the read-pinning fixes in
  `crates/onyx-api/src/evm_tx.rs`), so a lagging backend can report an older head or miss recent logs.
- **Prior art: Ponder** ([config reference](https://ponder.sh/docs/api-reference/ponder/config), read
  2026-10-12): `ethGetLogsBlockRange` is the "Maximum block range for `eth_getLogs` requests"; unset, it
  "will attempt to determine the block range automatically based on error messages"; `startBlock` is
  "usually set to the contract deployment block number"; reorgs are handled with a `reorgWindow` of
  retained data (default 180 s). Onyx copies the configured span with a fallback split on error, and the
  deploy block start; it avoids rollback state by reading only finalized blocks.
- **Prior art: The Graph** (table above): indexers run their own event scan and are checked by
  reproduction. Onyx's API indexer is likewise not trusted by clients: every response still carries the
  binding and a client checks `getFinalizedRoot` itself (docs/spec/delivery.md).
- **Live check** (read only, 2026-10-12): `cargo run -p onyx-node --example index-scan` over the
  2026-10-12 deployment (settlement `0x7d2576ee99c98ce8de353eff23708c26742a7116`, deploy block
  48037567, found by bisecting `eth_getCode`) found round 1's `Finalized` at block 48037969 (tx
  `0x3b6a1b1e…ae1eb`, root `0x99789e60…0727e`) and round 2's (the dispute run) at block 48039489 (root
  `0xdca49baf…3f2b7`); `getFinalizedRoot` at the finalized head agreed with both events. 28 calls at
  span 200, 0 splits; at span 1000, 3 splits then 250-block calls.

## Reserve balances of published address sets (indexer Phase 2 rows 2 and 6, 2026-10-13, ADR-0034)
- **UTXO scan at the tip** ([Bitcoin Core 28.0 `scantxoutset`](https://bitcoincore.org/en/doc/28.0.0/rpc/blockchain/scantxoutset/),
  read 2026-10-13): "Scans the unspent transaction output set for entries that match certain output
  descriptors"; the result's `height` is "The block height at which the scan was done". The docs offer
  no past-height scan, so a base before the tip is the scan minus the flows in between. Measured on our
  node (Core 28.1.0, full, txindex and coinstatsindex): 12,248 `addr(...)` descriptors (label set v2 plus
  OKX's 2026-09-08 PoR list) in 45 s, 348,557 UTXOs, passed through `bitcoin-cli -stdin` (one argument
  is limited to 128 KiB on Linux).
- **UTXO set commitment** ([`gettxoutsetinfo`](https://bitcoincore.org/en/doc/28.0.0/rpc/blockchain/gettxoutsetinfo/)):
  `hash_type` `muhash` with `hash_or_height` ("only available with coinstatsindex") gives a hash of the
  whole UTXO set at a past height. It cannot prove that a subset (our addresses' outputs) is complete, so
  the snapshot records it as context only; two scans from the same node are reconciled, which does not prove completeness (review RES-R1: a consistent omission passes); only OKX and BITB are checked against an independent source.
- **Rolling back instead** (`dumptxoutset` with its rollback option, in newer Core releases; not checked on our 28.1 node): rewinds the node's chain
  state with the network off; not for a shared node.
- **OKX proof of reserves** ([okx.com/proof-of-reserves](https://www.okx.com/proof-of-reserves); file
  list from `https://www.okx.com/v2/asset/audit/list`, as the indexer's `scripts/okx_por_history.py`
  reads it): newest file `por_csv_2026090800_V1.zip` (sha256 `fa84690d…364641`), BTC rows carry a
  `Snapshot Height` (965,949), the address, its amount and OKX's signatures. Format changed from the
  July files: a `Type` column (Non Staking / Native Staking) was added.
- **Bitwise BITB** ([bitbetf.com](https://bitbetf.com/), fetched 2026-10-13 06:11 UTC): the page data
  holds a `walletBalances` list (address, balance, active flag; 66 wallets, 65 active, `updatedAt`
  2026-10-13T06:01:11Z) and "Bitcoin in Trust 37,290.75" (holdings data as of 10/11/2026); the proof of
  reserves box (examiner The Network Firm) says "Bitcoin Reserve 37,291 BTC" as of 10/09/2026.
- **Prior art: CEX transparency dashboards.** DefiLlama tracks exchange reserves from wallet lists the
  exchanges give it ([DL News explainer](https://www.dlnews.com/articles/llama-u/how-to-analyse-the-transparency-of-centralised-exchanges/));
  numbers are the dashboard's own reads. Onyx's difference: every balance is a rerunnable shard over a
  pinned label set, chained from a committed scan, and checked in the browser.
- **Prior art: the indexer** (`~/projects/the entity indexer/EVIDENCE.md`): OKX address level "0.0000%
  error against 7 of OKX's own PoR snapshots" (2026-01..07). Onyx repeats the comparison on the newest
  snapshot with its own pipeline (docs/ops/indexer-integration.md, rows 2 and 6).

### Serving reserves through the API (2026-10-13, ADR-0034 Amendment 1)
- **Anchoring derived state on a committed snapshot** ([Bitcoin Core `assumeutxo` design](https://github.com/bitcoin/bitcoin/blob/master/doc/design/assumeutxo.md),
  read 2026-10-13): a node may start from a UTXO snapshot, and once background validation reaches
  the snapshot's base block it hashes its own UTXO set and compares it with "the compiled value"
  in `CMainParams::m_assumeutxo_data`. The snapshot is trusted until the independently built state
  is compared with it. Onyx's serving check takes the same shape: the reserve chain built from
  finalized shards must land on the committed scan, and the start base is derived from that scan
  and the finalized flows, never taken from the shard's own params (which a rerun cannot check).
- **Independence of a cross-check**: the only independent sources Onyx holds are OKX's
  proof-of-reserves file and Bitwise's wallet list (docs/ops/indexer-integration.md, live checks).
  A second scan from the same node only shows the node is consistent (review RES-R1, THREATS
  RES-1), and a server cannot tell nodes apart without a signature (THREATS RES-11).

## Delivery hardening (M7 loop 3, 2026-10-13; ADR-0023 Amendment 2)
- **What `authorizationState` can tell after a lost settle**
  ([EIP-3009](https://eips.ethereum.org/EIPS/eip-3009), read 2026-10-13):
  `authorizationState(address authorizer, bytes32 nonce) returns (bool)`, "True if the nonce is
  used"; the reference implementation marks the nonce used on success and rejects a reuse
  ("authorization is used"). The EIP does not say what `cancelAuthorization` does to that state,
  so "used" alone never proves payment (as M7-F5 already concluded); Onyx reads the token's
  `AuthorizationUsed`-then-`Transfer` pair for that, and treats "unused" as "safe to send the same
  authorization again": its nonce can move money only once.
- **Trusted forwarded-for** ([MDN, X-Forwarded-For](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/X-Forwarded-For),
  read 2026-10-13): with a trusted proxy list, "the `X-Forwarded-For` IP list is searched from the
  rightmost, skipping all addresses that are on the trusted proxy list"; security uses such as
  rate limiting "must only use IP addresses added by a trusted proxy", and multiple headers are
  one list. Onyx's `client_ip` (M7-L48-F2, loop 50) already does exactly this; nothing changed.
- **Client-side chain check by default**: the same reasoning as review IDX-R5-3 (the server's root
  read shares the operator's node). The client library now makes the safe path the default and
  the unchecked one an explicit, loudly labelled call.

## Data-source catalogue (ROADMAP F1, 2026-10-14)
Full table with quotes and decisions: docs/ops/data-sources.md. Facts new to this repo:
- **OKX Terms of Service** (https://www.okx.com/help/terms-of-service, "Last Updated: 17 September
  2026"), 8.1: "You agree that you will not copy, transmit, distribute, sell, license, reverse
  engineer, modify, publish, or participate in the transfer or sale of, create derivative works
  from, or in any other way, exploit any of our products and Services." Whether PoR files are
  "products and Services" is not stated. Input for L8-5.
- **bitbetf.com** ends its Proof of Reserves text with "View Terms of Use", linking
  https://www.thenetworkfirm.com/terms-of-use (the examiner), whose s.11 forbids to "copy,
  distribute, reproduce, publish ... or in any way exploit the Services or Content"; s.1: "The
  software and reporting are collectively referred to in these Terms as the “Services”." The
  wallet list itself sits in Bitwise's page data (`wallets.walletBalances`). Input for L8-5.
- **FRED** (https://fred.stlouisfed.org/legal/) prohibits redistributing third parties' data
  "for commercial use without first obtaining express written permission from the data
  provider"; its API needs a key. Use primary public-domain publishers instead.
- **Federal Reserve Board** (https://www.federalreserve.gov/disclaimer.htm): "Unless otherwise
  indicated, information on Board's website is in the public domain and may be copied and
  distributed without permission."
- **BLS** (https://www.bls.gov/opub/copyright-information.htm): "is in the public domain, except
  for previously copyrighted photographs and illustrations"; API v1.0 "does not require
  registration" (https://www.bls.gov/developers/); API terms require the statement "BLS.gov
  cannot vouch for the data or analyses derived from these data after the data have been
  retrieved from BLS.gov."
- **BEA**: website public domain ("Unless stated otherwise, the information posted on the BEA web
  site is in the public domain", https://www.bea.gov/taxonomy/term/611?page=2); the API needs a
  name, e-mail and agreement to a terms PDF (https://apps.bea.gov/API/signup/), so it is a sign-up.
- **SEC data.sec.gov APIs** (https://www.sec.gov/search-filings/edgar-application-programming-interfaces):
  "These APIs do not require any authentication or API keys to access"; bulk `companyfacts.zip`
  and `submissions.zip` are republished nightly.
- **Stablecoin addresses** from the issuers: USDC on Ethereum and Base
  (https://developers.circle.com/stablecoins/usdc-contract-addresses); USDT on Ethereum, Base not
  listed (https://tether.to/en/supported-protocols/).
- **PoR repositories on GitHub** carry code licences only (okx/proof-of-reserves MIT,
  BitMEX/proof-of-reserves-liabilities MPL-2.0); the address data is not in them.

## TLS in front of onyx-node: Caddy or nginx (ROADMAP L11, 2026-10-15)
Choice: **Caddy** (`deploy/caddy/Caddyfile`, tested with 2.11.7, the GitHub release binary checked
against its `caddy_2.11.7_checksums.txt`; docs/audit/2026-10-15-tls-front.md).
- **Caddy** (https://github.com/caddyserver/caddy, Apache-2.0): "Automatic HTTPS provisions TLS
  certificates for all your sites and keeps them renewed. It also redirects HTTP to HTTPS for
  you" (https://caddyserver.com/docs/automatic-https, read 2026-10-15), from Let's Encrypt or
  ZeroSSL. One static binary; `tls internal` gives a local CA for tests, so the same file is
  tested locally without a domain.
- **nginx**: certificates come from a separate tool (certbot) or the separate `nginx-acme`
  dynamic module (https://github.com/nginx/nginx-acme, Apache-2.0, repository created 2025-07-10);
  the redirect, HSTS and renewal hooks are configured by hand. More parts for the same result.
- Caddy's proxy defaults (https://caddyserver.com/docs/caddyfile/directives/reverse_proxy,
  "Defaults"): it sets `X-Forwarded-For` and, unless `trusted_proxies` is configured, "will
  ignore their values from incoming requests, to prevent spoofing". onyx-node's
  `ONYX_TRUSTED_PROXIES=127.0.0.1` then reads the client from it (M7-L48-F2, `client_ip`).
- Caddy's default `max_header_size` is 16 KiB
  (https://caddyserver.com/docs/caddyfile/options#max-header-size): one full x402
  `PAYMENT-SIGNATURE` header (`MAX_PAYMENT_HEADER`, 16 KiB) would be refused with 431, so the
  file sets 32 KiB. `request_body max_size`: "Reads of more bytes will return an error with HTTP
  status `413`" (https://caddyserver.com/docs/caddyfile/directives/request_body).
- Not in stock Caddy: a per-client connection cap or rate limit (third-party modules only).
  onyx-node's own per-client rate limits work behind it (keyed on the forwarded client); its
  per-client connection cap exempts the proxy (AUD-1R-F1), so that cap is a host firewall's job
  (THREATS TLS-2).

## Proof ladder level 5: track records (ROADMAP C1, 2026-10-17, ADR-0036)
What oracle networks and the reputation and forecasting literature keep as a per-source history,
and what Onyx takes. Pages read on 2026-10-17; "unconfirmed" items were seen only in search
snippets or behind a 403 and are not relied on.
- **Chainlink** ([whitepaper v2, 2021](https://research.chain.link/whitepaper-v2.pdf), section
  3.6.4 "Node Reputation / Performance History"): the network "natively documents the performance
  histories of contributing nodes on chain"; third-party tools (market.link, reputation.link) show
  "average response latency, the deviation of values in their reports from consensus values
  relayed on chain, revenue generated, jobs fulfilled". Section 9 ties future fees to performance
  history (implicit-incentive framework). Deviation from consensus is agreement with peers, which
  Onyx rejects as the scored quantity (below). The current state of those tools is unconfirmed.
- **Pyth** ([publisher quality ranking](https://docs.pyth.network/oracle-integrity-staking/publisher-quality-ranking)):
  uptime 40 %, price deviation from the aggregate (normalized by the aggregate confidence interval)
  40 %, lack of stalled prices 20 %; recalculated monthly so "only the top-performing publishers
  remain permissioned". Again a peer-relative measure. Slashing
  ([rulebook](https://docs.pyth.network/oracle-integrity-staking/slashing-rulebook)) is triggered by
  the aggregate being at least 250 bps from the most liquid venues for at least 60 s, capped at
  5 %, decided by the Pythian Council: a comparison with an outside reference, closer to Onyx's
  "later stronger answer".
- **UMA DVM 2.0** ([docs](https://docs.uma.xyz/protocol-overview/dvm-2.0)): a voter loses "0.1% of
  your staked amount per vote that is incorrect or missed", redistributed to correct voters. A
  history-based penalty for disagreeing with the final outcome; the outcome is itself a vote.
- **API3** ([whitepaper source](https://github.com/api3dao/api3-whitepaper/blob/master/api3-whitepaper.tex)):
  argues first-party oracles give "transparency in terms of the data source" and that providers
  have an off-chain "reputation to maintain"; cites Douceur that a Sybil can "build a sufficient
  track record of honest operation". No on-chain provider track record found.
- **Witnet** ([whitepaper, arXiv 1711.09756](https://arxiv.org/pdf/1711.09756)): witnesses gain
  reputation "when its claims match the claims brought by a majority of its peers" and lose it
  when they contradict the majority; total reputation is fixed and decays ("demurrage").
- **Agreement with peers is gameable.** Peer prediction (Miller, Resnick, Zeckhauser, Management
  Science 51(9), 2005, [author page](https://presnick.people.si.umich.edu/papers/elicit/index.html))
  needs proper scoring rules on implied beliefs about another rater's report to make honesty an
  equilibrium; plain agreement does not. In a lab test, "participants successfully coordinate on
  uninformative equilibria" (Gao, Mao, Chen, Adams, EC 2014,
  [entry](https://lips.cs.princeton.edu/bibliography/gao2014peer/)). Onyx consequence: outcomes
  compare a source with a later level 1-3 answer only; peers appear only as context ("best of N",
  first to state).
- **Cheap names and Sybils.** Friedman and Resnick, "The Social Cost of Cheap Pseudonyms"
  ([author page](https://presnick.people.si.umich.edu/papers/identifiers/index.html)): newcomers
  "pay their dues" when names are free. Douceur, "The Sybil Attack", IPTPS 2002
  ([publication](https://www.microsoft.com/en-us/research/publication/the-sybil-attack/)): "without
  a logically centralized authority, Sybil attacks are always possible". Resnick et al.,
  "Reputation Systems", CACM 2000 ([author page](https://presnick.people.si.umich.edu/papers/cacm00/index.html)).
  Onyx consequence: no record starts above empty; sources are registered, rights-reviewed ids;
  the record shows how many sources stated on the metric.
- **Forecast verification.** The Brier score partitions into reliability, resolution and
  uncertainty (Murphy 1973, J. Appl. Meteor. 12:595-600; summarized by the WMO verification group,
  [page](https://jwgfvr.github.io/forecastverification)). Proper scoring rules: Gneiting and
  Raftery 2007 (section "M2" above). Metaculus's track record page (code read at
  [github.com/Metaculus/metaculus](https://github.com/Metaculus/metaculus), the live page returned
  403) shows a calibration curve, score charts and counts, and puts a per-bin 90 % binomial band
  on each calibration bin (`binom.ppf`), a precedent for showing sample size honestly.
- **Small samples.** The Wilson interval (Wilson 1927, JASA 22(158)) is recommended by the
  [NIST/SEMATECH e-Handbook 7.2.4.1](https://www.itl.nist.gov/div898/handbook/prc/section2/prc241.htm)
  for proportions; [Evan Miller](https://www.evanmiller.org/how-not-to-sort-by-average-rating.html)
  uses its lower bound to rank by rate under uncertainty. Onyx keeps it out of the protocol data
  for now (an integer square root and a chosen z would be new decisions): the record carries
  counts, and a rate is shown only past ADR-0016's minimum sample (PLAN q-TR-1).

## Track-record intake (ROADMAP C5, 2026-10-17, ADR-0036 amendment 1)
What a statement's time must prove, and whose clock may set it.

- **Timestamping proves "existed before".** RFC 3161 section 1: "A time-stamping service supports
  assertions of proof that a datum existed before a particular time"
  ([rfc-editor.org/rfc/rfc3161](https://www.rfc-editor.org/rfc/rfc3161)). OpenTimestamps: "A
  timestamp proves that some data existed prior to some point in time", anchored in the Bitcoin
  blockchain ([opentimestamps.org](https://opentimestamps.org/)). That is the bound a track record
  needs (a statement made before its close); a chain block carrying a hash of the statement gives
  it, and only an upper bound, so nobody can make a statement look earlier.
- **Whose clock.** On an OP Stack chain such as Base the sequencer sets the L2 block time:
  `block.timestamp = prev_l2_timestamp + l2_block_time` (2 s), bounded by
  `l1_origin.timestamp <= block.timestamp <= max_l2_timestamp` with `max_sequencer_drift` 1,800 s
  in the current default (600 s before Fjord)
  ([OP Stack derivation spec](https://specs.optimism.io/protocol/derivation.html)). So chain time
  is trusted within that drift (THREATS TR-I-10), and block times strictly increase.
- **Commit-reveal.** Onyx's own coordinator already commits before it reveals (ADR-0018, ADR-0027
  observer commits); the same shape keeps a statement's value private until its close while its
  existence is fixed on chain. Hiding rests on the leaf including the 65-byte ECDSA signature,
  which nobody without the key can compute, so a short value (a `Bool`) cannot be brute-forced
  from the root.
- **Counting posts without a contract.** A dedicated EOA's nonce at a block
  (`eth_getTransactionCount` at that block) counts every transaction it ever sent, so a hidden
  post shows as a nonce gap; the C2 signed-source book can count its own entries instead.

## Serving track records with freshness (ROADMAP C6, 2026-10-17, ADR-0036 amendment 2)
- Certificate Transparency v2 (RFC 9162, https://www.rfc-editor.org/rfc/rfc9162): a client
  keeps the last signed tree head it saw and asks the log for a consistency proof from it to the
  current head; a log that shows different views to different clients is caught only when views
  are compared (gossip) or the head is anchored. Same shape here: `since` asks for the chain
  from the commitment a client saw last, which proves descent by hash; a full extension check
  needs the logs (`check_extends`, C20) and a public latest needs commitments on chain (C7).
- Taken: remember-and-check-from-last (as CT clients do). Not taken: gossip between clients
  (no client population yet); a signed head per answer (the server's own key adds no trust
  without an anchor).

## Independent observers' reveal transport (ROADMAP B2, 2026-10-17, ADR-0027 amendment 9)

- Need: a private channel from each level-2 observer to the coordinator for the signed reveal
  only (draws are read from `OnyxRegistry`, commits are posted on chain), robust to a
  coordinator restart (REVLOG-R5-1d), with nothing in it able to stop a round.
- Maildir (https://cr.yp.to/proto/maildir.html): deliver by writing a file under a temporary
  name and renaming it into place, so a reader never sees a partial message and nothing needs
  a lock. The spool uses the same rename pattern (`observer_link::send_reveal`).
- OpenSSH `ChrootDirectory` with `ForceCommand internal-sftp` (https://man.openbsd.org/sshd_config)
  confines an account to one folder over SFTP: the cross-host delivery path for one observer's
  spool folder, authenticated and encrypted by SSH, no new service to run.
- Chainlink's Off-Chain Reporting has its oracle nodes talk over a peer-to-peer network
  (https://docs.chain.link/architecture-overview/off-chain-reporting): a fit for many
  independent operators exchanging observations among themselves; Onyx's observers talk only to
  the coordinator, once per round, so a p2p layer is more than this needs today.
- Decision: a spool folder (signed files, one per observer and round) read by the coordinator's
  poll; an HTTP endpoint and an encrypted on-chain reveal (REVLOG-1, ROADMAP C4) were weighed in
  the amendment.

## Sealed reveals: encrypted reveal posts, time-locks, verifiable decryption (ROADMAP C4, 2026-10-17, ADR-0027 amendment 10)

- Need: a level-2 reveal must reach the chain before the coordinator can choose to drop it
  (REVLOG-1), yet its value must stay private until the paid API serves it (ATT-R1-F2,
  SIG-R1-F2); a post that is garbage must be provably garbage without a trusted judge.
- Last-revealer problem of commit-reveal: whoever reveals last sees the others and may withhold.
  Analysed for RANDAO in "RANDAO-based RNG: Last Revealer Attacks in Ethereum 2.0 Randomness"
  (https://arxiv.org/abs/2403.09541); the usual fixes hide the others' contributions until it is
  too late to react (a VDF, or encryption nobody can open early). Onyx's residual after
  amendment 10 is this problem with the coordinator as the only early reader (item 8b).
- tlock (Gailly, Melissaris, Romailler, "tlock: Practical Timelock Encryption from Threshold
  BLS", IACR ePrint 2023/189, https://eprint.iacr.org/2023/189): Boneh-Franklin identity-based
  encryption where the identity is a drand round number and the League of Entropy's threshold
  BLS signature of that round is the decryption key; the drand network needs no change. drand's
  quicknet chain answers without an account (read on 2026-10-17:
  https://api.drand.sh/52db9ba70e0cc0f6eaf7803dd07447a1f5477735fd3f661792ba94600c84e971/info,
  period 3 s, scheme `bls-unchained-g1-rfc9380`). Rust port: crates `tlock`/`tlock_age` 0.0.10
  (https://github.com/thibmeu/tlock-rs), third-party. Fits "nobody opens before reveal close";
  does not fit "values stay private", since everyone can open after the round. Kept as the later
  layer (amendment 10 item 9, ROADMAP C12).
- Threshold encryption by a keyper committee (Shutter; design notes for an encrypted OP-stack
  mempool: https://gov.optimism.io/t/shutterized-optimism-an-encrypted-mempool-for-the-op-stack/6387):
  users encrypt to an epoch key, keypers jointly release it after the ordering is fixed; a
  colluding threshold of keypers can decrypt early. Needs operators Onyx does not have.
- ECIES (SEC 1 v2 §5.1, https://www.secg.org/sec1-v2.pdf): ephemeral Diffie-Hellman to the
  recipient's key, a KDF over the shared secret, symmetric encryption. Onyx uses it with Keccak
  (ADR-0003) as KDF and stream, and no MAC: the plaintext must be the observer's own signed
  reveal opening its commit, and the post is signed, so a forged or altered ciphertext is
  already refused. Keyed Keccak as a PRF: Bertoni, Daemen, Peeters, Van Assche, "On the
  security of the keyed sponge construction" (https://keccak.team/files/SpongeKeyed.pdf).
- Verifiable decryption by a discrete-log-equality proof: the recipient publishes the shared
  point `S = k·R` with a proof that `log_G(K) = log_R(S)`; RFC 9497 §2.2 specifies such a
  non-interactive DLEQ proof and says it resembles Chaum-Pedersen
  (https://www.rfc-editor.org/rfc/rfc9497.html; Chaum and Pedersen, "Wallet Databases with
  Observers", CRYPTO '92, https://doi.org/10.1007/3-540-48071-4_7). Revealing `S` for a
  chosen `R` is a Diffie-Hellman oracle; requiring a proof of knowledge of the ephemeral secret
  bound to the context makes the oracle useless (the poster could compute `S` itself), the same
  idea as the non-malleable threshold scheme TDH2 (Shoup and Gennaro, "Securing Threshold
  Cryptosystems against Chosen Ciphertext Attack", https://www.shoup.net/papers/thresh1.pdf).
  Onyx's proof of knowledge is a Schnorr proof (Schnorr, "Efficient signature generation by smart
  cards", J. Cryptology 1991, https://doi.org/10.1007/BF00196725) checked on chain with one
  `ecrecover`. An ECDSA signature by the ephemeral key is NOT a proof of knowledge: ECDSA public
  key recovery yields a valid signature for a point nobody knows the secret of (SEC 1 v2 §4.1.6,
  public key recovery), which review C4-R1-1 turned into a decryption oracle.
- Decision: encrypt each reveal on chain to a coordinator key (the post is the receipt), dismiss
  garbage with a DLEQ proof anyone checks, pin each non-value round on chain before it is served
  (REVLOG-5). Not chosen now: tlock (publishes values), keyper committees (no operators).

## Track records on chain (ROADMAP C7, 2026-10-17, ADR-0036 amendment 3)
What makes a resolution and "the latest commitment" checkable without trusting the coordinator.

- **An append-only public log closes split views.** Certificate Transparency v2 (RFC 9162,
  [rfc-editor.org/rfc/rfc9162](https://www.rfc-editor.org/rfc/rfc9162)) needs gossip or an
  anchor to catch a log that shows clients different heads. A contract storage slot read at a
  finalized block is such an anchor: every client reads the same `trackHead`, and a contract
  that takes only the successor of its latest entry makes a second branch impossible rather than
  merely detectable. Taken: the contract checks linkage itself (it hashes the Borsh commitment,
  reads `prev` and `as_of`). Not taken: storing the roots separately (the hash binds them, and
  the event carries the bytes).
- **Reading state at a pinned block.** EIP-1898
  ([eips.ethereum.org/EIPS/eip-1898](https://eips.ethereum.org/EIPS/eip-1898)) lets `eth_call`
  name a block by hash, so a read at a block that a reorg replaced fails instead of returning
  another branch's state. `eth_getLogs` takes a number range only for ranges (EIP-234's
  `blockHash` filter is a single block,
  [eips.ethereum.org/EIPS/eip-234](https://eips.ethereum.org/EIPS/eip-234)), so log scans
  re-check the head's hash afterwards. The `finalized` block tag
  ([ethereum.org JSON-RPC](https://ethereum.org/en/developers/docs/apis/json-rpc/#default-block))
  names the head nothing can replace on a healthy chain.
- **Sybils and affiliates.** Douceur, "The Sybil Attack" (IPTPS 2002,
  [microsoft.com/en-us/research/publication/the-sybil-attack](https://www.microsoft.com/en-us/research/publication/the-sybil-attack/)):
  one entity can present many identities unless an authority vouches for them. Onyx's sources are already admitted by a
  reviewer (keys, origins); declaring each source's operator at that same review, before its
  first key, is the reviewer acting as that agency for affiliation. What it cannot see (an
  operator that hides its affiliation) stays a reviewer limit (THREATS TR-C7-5).
- **Hash-bound preimages** (Onyx's own pattern, as for claims since ADR-0011): the chain holds
  the report hash (`settleable`), the level-2 result hash and spec id, and the coordinator
  brings preimages, which open only what the chain recorded. A missing preimage is an omission
  the client sees, not a silent gap.
- **Sybil paper, wording.** Douceur's abstract argues that without a logically centralized
  authority Sybil attacks are always possible except under extreme assumptions; the paraphrase
  above leans on that claim only.

## Time-lock crates for sealed reveals (ROADMAP C12, 2026-10-17, ADR-0027 amendment 12)

- Need: a Rust implementation of drand's tlock (Boneh-Franklin IBE with the Fujisaki-Okamoto
  transform; identity `sha256(round)`; quicknet `bls-unchained-g1-rfc9380`: round signatures on
  G1, the group key on G2) that the coordinator, the Rust client and the web verifier (wasm) can
  run on attacker-chosen ciphertexts without panicking, under `deny.toml`
  (permissive licences, crates.io only). Read from crates.io's API and each crate's own source
  and README on 2026-10-17; nothing below claims more than those pages say.
- Search: crates.io for "tlock", "drand" and "timelock" on 2026-10-17. drand-tlock crates found:
  `tlock`, `tlock_age`, and two tools on top of them (`age-plugin-tlock`, the `dee` CLI); beacon
  crates: `drand`, `drand_core`, `drand-verify`, `drand-client-rs`, `restate-drand`.
- `tlock` 0.0.10 and `tlock_age` 0.0.10 (https://github.com/thibmeu/tlock-rs, MIT, one
  maintainer, released 2026-07-12): the only drand-tlock libraries the search found. Its README says "This software has not
  been audited". Read in its source (`src/ibe.rs`, `src/lib.rs`): `ibe::decrypt` ends with
  `assert_eq!(c.u, r_g)`, so a ciphertext whose `U` decodes but fails the Fujisaki-Okamoto
  check PANICS the caller instead of failing (anyone can post one: a remote crash of every
  coordinator and client that opens chain posts); `xor` also panics on a length mismatch, but
  the 16-byte reads of `lib::decrypt` keep it unreachable from ciphertext bytes (review C12-R2);
  `lib::decrypt` strips trailing zero bytes of the 16-byte message (its own comment: "this does
  not work as expected"), so about 1 in 256 random keys comes back shorter than 16 bytes, which
  for a fixed 16-byte `tau` is undone by padding back to 16 (`ibe::decrypt` itself returns all
  16), so it is a bug, not a bar; messages are 16 bytes only; its API cannot serve Onyx's
  dismissals: `ibe::encrypt` draws `sigma` itself (`rand::rng()`) and `decrypt` returns only the
  message, so neither the opening `(sigma, tau)` nor re-encryption from it is reachable (review
  C12-R2); it pulls `rand` 0.10, `getrandom` 0.4, `anyhow`,
  `itertools`, `serde_with` and `tracing`; curve arithmetic is arkworks 0.6. wasm32: its README
  says it is tested on `wasm32-wasi`. Format and quicknet support match drand/tlock (its tests
  decrypt with quicknet round 1000).
- `drand_core` 0.0.19 (https://github.com/thibmeu/drand-rs, MIT): a drand HTTP client and beacon
  verifier; brings `ureq`, `url` and (on wasm) `ring` into the protocol crate; no tlock.
- `drand` 0.3.0 (https://github.com/sagikazarmark/drand-rs, MIT or Apache-2.0, released
  2026-10-09, under two weeks old): fetches and verifies beacons (arkworks 0.6), no tlock; its
  README says "Not audited".
- `drand-verify` 0.6.2 (https://github.com/noislabs/drand-verify, Apache-2.0, last release
  2023-12-05): beacon verification only (supports quicknet per its README), no IBE; built on
  zkcrypto `bls12_381` 0.8 with its `experimental` hash-to-curve.
- `timelock` 0.3.0 (https://github.com/ideal-lab5/timelock, Apache-2.0): another scheme (IBE
  with AES-GCM, SCALE codec, BLS12-377 and 381; its README lists drand quicknet as a beacon it
  can lock to), on arkworks, not drand/tlock's ciphertext format.
- drand/tlock, the Go reference (https://github.com/drand/tlock): its README says "Our timelock
  scheme and code was reviewed by cryptography and security experts from Kudelski" (report on
  IPFS, CID `QmWQvTdiD3fSwJgasPLppHZKP6SMvsuTUnb1vRP2xM7y4m`; not read here). Go, so not a
  dependency, but the reviewed source of known-answer vectors for C50 (review C12-R2).
- Curve libraries a home-made tlock would sit on:
  arkworks `ark-bls12-381` 0.6 (MIT or Apache-2.0): the repository README warns "This is an
  academic proof-of-concept prototype, and in particular has not received careful code review"
  and "This implementation is NOT ready for production use"
  (https://github.com/arkworks-rs/algebra).
  zkcrypto `bls12_381` 0.9.0 (MIT/Apache-2.0, 2026-09-15): its README says "This implementation
  has not been reviewed or audited. Use at your own risk", and hash-to-curve sits behind
  `experimental` with no compatibility guarantee (https://github.com/zkcrypto/bls12_381).
  `blst` 0.3.17 (Apache-2.0, https://github.com/supranational/blst): its README cites an NCC
  Group audit of January 2021
  (https://research.nccgroup.com/wp-content/uploads/2021/01/NCC_Group_EthereumFoundation_ETHF002_Report_2021-01-20_v1.0.pdf)
  and an ongoing Galois formal verification (https://github.com/GaloisInc/BLST-Verification);
  it is C and assembly behind FFI, built by `cc`. Corrected by review C12-R2 (read in
  `blst` 0.3.17's `src/lib.rs`, `build.rs` and Rust README; not built here): the Rust binding
  wraps every primitive tlock needs in SAFE functions, `min_sig::SecretKey::from_bytes` and
  `sk_to_pk` (`U = r·G2`, 96 bytes compressed), `SecretKey::sign(msg, dst, aug)` (`r·H(id)` on
  G1 through `blst_hash_to_g1` with quicknet's DST), `PublicKey`/`Signature::uncompress` and
  `validate` (subgroup checks), `blst_fp12::miller_loop`, `final_exp` and `to_bendian` (the
  pairing and its bytes), so Onyx code would hold no `unsafe` (the workspace lint
  `unsafe_code = "deny"` covers workspace crates, not dependencies); and its Rust README says
  "To compile for WebAssembly, your clang has to recognize `--target=wasm32`" (or `CC=emcc`),
  with `build.rs` building `wasm32-unknown-unknown` as no_std (the Emscripten folder of the root
  README is a JavaScript example). That is the pattern Onyx already ships for the same sealed
  reveals: `secp256k1` 0.29.1 is libsecp256k1 in C behind `secp256k1-sys`, built by `cc`, and
  `scripts/build-web.sh` builds it for `wasm32-unknown-unknown` with clang.
  `blstrs` 0.7.1 (blst under `ff`/`group` traits; https://github.com/filecoin-project/blstrs):
  its README supports only `x86_64` and `aarch64`, so no wasm.
- Verdict: no crate is acceptable as it is. `tlock` fails on robustness (panics on attacker
  input), has no audit, and exposes no `sigma`; the pure-Rust curve libraries under it say they
  are not reviewed. Review C12-R2: there IS an acceptable path the first verdict missed: our own
  minimal drand/tlock IBE (H2, H3 with drand's scalar derivation, H4, the GT byte order of
  drand/tlock) on `blst`'s safe Rust API, no `unsafe` in Onyx and the wasm build Onyx already
  runs for secp256k1. It still needs that IBE written, known-answer vectors from the reviewed Go
  reference and published quicknet rounds, and a cryptography review (blst's 2021 audit covers
  blst, not our IBE), so C12 still stops at design and composition with a mock beacon
  (`onyx_coordinator::timelock`); this is ROADMAP C50's route. No dependency was added.
- drand quicknet facts used (https://api.drand.sh/52db9ba70e0cc0f6eaf7803dd07447a1f5477735fd3f661792ba94600c84e971/info,
  read 2026-10-17, no account): period 3 s, genesis 1692803367, scheme
  `bls-unchained-g1-rfc9380`; round `r` is published at `genesis + (r - 1)·period` (the latest
  round served at unix 1792260560 was 33152398, matching it). Pinned in code and checked by the
  opt-in test `timelock::tests::c12_live_quicknet_matches_the_pinned_chain`.
- Composition prior art: combining two KEMs so the key needs both shared secrets ("AND" of a
  coordinator key and a time-lock), rather than nesting two encryptions, is the KEM-combiner
  idea of Giacon, Heuer and Poettering, "KEM Combiners" (PKC 2018,
  https://eprint.iacr.org/2018/024; their combiners hash the ciphertexts into the key, so Onyx's
  key hashes both `R` and `keccak256(TL)`, review C12-R1-7); tlock's paper (https://eprint.iacr.org/2023/189) gives the
  IBE and its CCA security via Fujisaki-Okamoto.

## Time-lock interop vectors and the scheme as Go runs it (ROADMAP C50, 2026-10-17, ADR-0027 amendment 13)

- What was read (as data, never run; fetched with no account): drand/tlock v1.2.0 at commit
  `7b54141a` (`tlock.go`, `tlock_test.go`, `testdata/`, https://github.com/drand/tlock), its IBE in
  drand/kyber v1.3.1 `encrypt/ibe/ibe.go` (https://github.com/drand/kyber), the GT encoding of
  drand/kyber-bls12381 v0.3.1 `kyber_gt.go` over kilic/bls12-381 v0.1.0 `fp12.go`, `fp6.go`,
  `fp2.go`, and drand/tlock-js at `17d817ee` (`test/rfc-scheme.test.ts`, `test/crypto/ibe.test.ts`).
- The scheme for quicknet, as Go runs it: `tlock.TimeLock` with `crypto.SigsOnG1ID`
  (`bls-unchained-g1-rfc9380`) calls `ibe.EncryptCCAonG2(bls.NewBLS12381Suite(), pk, id, msg)`
  with `id = sha256(round as 8 bytes big-endian)` (`DigestBeacon`); `Qid` = hash to G1 with the
  suite's G1 tag (RFC 9380 `BLS_SIG_BLS12381G1_XMD:SHA-256_SSWU_RO_NUL_`; the older
  `ShortSigSchemeID` chains used the G2 tag "for retro-compatibility"); `U = r·G2`;
  `V = sigma XOR sha256("IBE-H2" ‖ GT bytes)[..16]` of `e(Qid, pk)^r`; `W = msg XOR
  sha256("IBE-H4" ‖ sigma)[..16]`; `r` = drand's H3: `buffer = sha256("IBE-H3" ‖ sigma ‖ msg)`,
  then for `i = 1, 2, …` `h = sha256(i as 2 bytes little-endian ‖ buffer)`, `h[0] >>= 1`, the
  first `h` (big-endian) below the group order. Decryption recomputes `r` and requires
  `r·G2 = U` (Fujisaki-Okamoto). Ciphertext bytes: `U` compressed (96) ‖ `V` (16) ‖ `W` (16)
  (`CiphertextToBytes`). GT bytes: kilic's `toBytes`, every Fp big-endian with the highest
  coefficient first (`c1` before `c0` at each of the three tower levels); blst's `to_bendian`
  writes another order, so `onyx_tlock::gt_bytes` reorders it, checked against tlock-js's "test
  vectors generated from the go codebase" (`e(G1, G2)` bytes and its `gtToHash`).
- Vectors found for this scheme: Go `testdata/lorem-tle-testnet-quicknet-t-2024-01-17-15-28.tle`
  (quicknet-t round 5423142), Go `TestInteropWithJS` "on testnet with quicknet-t" (made by
  tlock-js, round 1624910), and tlock-js "should pass test vectors from go codebase" (quicknet
  MAINNET round 746511). The drand/tlock repository holds no mainnet quicknet ciphertext of its
  own; its mainnet files are fastnet (`dbd506…`, another scheme, no longer served by api.drand.sh
  since 2024-10-07 per its answer). The quicknet-t chain still answers at pl-us.testnet.drand.sh;
  "testnet-g" (`f3827d77…`) does not. All three decrypt with our code under the published round
  signatures, the opening is age's file key (age header MAC: HMAC-SHA-256 under
  HKDF-SHA-256(file key, "", "header"), https://age-encryption.org/v1), and re-encryption gives
  the same 128 bytes (`onyx-tlock` `tests/interop.rs`, wasm32 `scripts/check-tlock-wasm.sh`).
  None of the three file keys ends in a zero byte, so the zero-tail cases are Onyx-made
  (`fixtures/tlock/onyx-zero-tail.json`) and checked only against real signatures and across
  targets; a Go cross-check of them is ROADMAP C70.

## Dated membership and confirmation depth for reserves (ROADMAP D1, 2026-10-17, ADR-0037)
- **Confirmation depth.** Bitcoin whitepaper section 11 (https://bitcoin.org/bitcoin.pdf): "the
  recipient waits until the transaction has been added to a block and z blocks have been linked
  after it"; its table gives, for an attacker with q = 0.1 of the hash power, P = 0.0009137 at
  z = 5. Bitcoin Core's RPC counts confirmations with the block itself (a block at the tip has 1;
  `getblock` field `confirmations`, https://developer.bitcoin.org/reference/rpc/getblock.html).
  Onyx's 6 is z = 5 in the paper's terms.
- **Header chains as evidence.** A header (80 bytes) commits to its parent and carries its own
  proof of work, so a run of headers above a block is checkable by anyone with no trust in who
  served them; the fixture pipeline already checks headers this way (`node::check_raw_header`,
  R-FIX-2/3). The committed run for the reserve scan mixes our own node's raw blocks and
  971,200-971,207 from mempool.space's Esplora API (`/api/block-height/<h>`,
  `/api/block/<hash>/header`), cross-checked on blockstream.info (the S2 precedent in
  docs/ops/data-sources.md).
- **Address-list rotation in the wild.** The BITB list behind label set v2 holds 52 addresses that
  are "no longer listed (rotated out)" next to 65 live ones (fixtures/labels/PROVENANCE.md; the
  indexer's note per row). Proof-of-reserves programmes publish dated snapshots (same file: OKX
  7 snapshots 2026-01..07; Binance 43 files `wallet_address_YYYYMMDD.zip`), so a
  membership range from the snapshots that list an address is available from the sources
  themselves; the per-row dates are in the indexer's archives, not in this repository.
- **Temporal validity in commitments.** Certificate transparency logs commit validity periods
  inside each leaf (RFC 6962 section 3.4: a leaf holds the certificate, whose TBSCertificate
  carries `notBefore`/`notAfter`, https://www.rfc-editor.org/rfc/rfc6962), so a
  proof of inclusion is also a proof of the dates; the dated label set does the same with
  `(from, to)` in each Borsh leaf.

## drand's HTTP API as a coordinator's beacon source (ROADMAP C51, 2026-10-17, ADR-0027 amendment 14)

- What the coordinator needs: the BLS signature of one quicknet round (the lock round of a window),
  after it is published, from anyone; it is verified against the pinned quicknet key
  (`onyx_tlock::BeaconKey::verify`), so the source is never trusted, only its liveness matters.
- Observed on 2026-10-17 with `curl` (no account; User-Agent `onyx-drand-client`, since changed to
  `curl`, review C51-R1-4): `GET
  https://api.drand.sh/<chain hash>/public/<round>` answers `{"round", "randomness",
  "signature"}` (the same shape as the C50 fixtures, fixtures/tlock/PROVENANCE.md); a round not yet
  published answers HTTP 425 with the body "Requested future beacon". `api2.drand.sh`,
  `api3.drand.sh` and `drand.cloudflare.com` answered the same round with HTTP 200, so
  `timelock::DrandHttp` tries all four in turn (`DRAND_RELAYS`).
- The C51 fixture (`fixtures/tlock/drand/quicknet-c51-rounds.json`) holds 21 published rounds
  around the recorded Treasury window's lock round (32964512) and the anvil rehearsal's two lock
  rounds (33152432, 33152732), verbatim answers of the endpoint above.
- Not used: drand's gossip or gRPC clients (`drand_core` is an HTTP client crate, RESEARCH.md
  "Time-lock crates"); a client library would add a dependency for one GET per round.

## Track-record follow-ups (ROADMAP C9, 2026-10-17, ADR-0036 amendment 4)
What decides when a Bitcoin flow statement must close, which naive forecast a record is compared
with, and what a receipt can and cannot prove.

- **How early a block of a period can be public.** Bitcoin Core refuses a block whose timestamp is
  more than 2 hours ahead of its network-adjusted time (`MAX_FUTURE_BLOCK_TIME = 2 * 60 * 60`,
  [github.com/bitcoin/bitcoin/blob/master/src/chain.h](https://github.com/bitcoin/bitcoin/blob/master/src/chain.h);
  rule summary at [en.bitcoin.it/wiki/Block_timestamp](https://en.bitcoin.it/wiki/Block_timestamp)),
  and a block's timestamp must exceed the median of the previous 11 (median time past). So header
  timestamps are not monotonic, and a block timestamped inside a day can be public up to 2 hours
  before the day starts. Taken: the close is 7,200 s before the period starts.
- **Which blocks belong to a day.** BIP 113
  ([github.com/bitcoin/bips/blob/master/bip-0113.mediawiki](https://github.com/bitcoin/bips/blob/master/bip-0113.mediawiki))
  uses the median time past for lock times because it never decreases. Taken: a day's blocks are
  those whose median time past is in the day, a contiguous height range; such a block has six of
  its eleven predecessors timestamped in the day, each accepted no earlier than `start - 7,200`.
  Not taken: block timestamps (non-monotonic, so a day would not be one height range).
- **Naive baselines and skill scores.** Forecast verification measures skill against a reference
  forecast, usually persistence or climatology (Murphy 1988, "Skill scores based on the mean square
  error and their relationships to the correlation coefficient",
  [doi.org/10.1175/1520-0493(1988)116<2417:SSBOTM>2.0.CO;2](https://doi.org/10.1175/1520-0493(1988)116%3C2417:SSBOTM%3E2.0.CO;2));
  the naive method (every forecast is the last observation) is the standard benchmark in
  Hyndman and Athanasopoulos, *Forecasting: Principles and Practice*, 3rd ed.
  ([otexts.com/fpp3/simple-methods.html](https://otexts.com/fpp3/simple-methods.html)). Taken:
  persistence, the last value known at the close, with the difference of agreement rates as the
  skill (both integer counts), on period metrics only (a grid makes "the last value" defined; for
  point facts it would be whichever earlier fact someone happened to name). Not taken: a Brier skill score (persistence states no confidence)
  and climatology (needs a per-metric window choice; a later per-rule option).
- **Receipts as evidence, not time.** A signed receipt proves the signer took the item by the
  signature's content, like an RFC 3161 time-stamp token proves existence at its time
  ([rfc-editor.org/rfc/rfc3161](https://www.rfc-editor.org/rfc/rfc3161)), but only on the
  signer's word; here the signer is the party that could backdate, so a receipt never sets a
  commit time. It makes a broken promise attributable to the poster key, like Certificate
  Transparency's signed certificate timestamps make a log's missed merge deadline provable
  ([rfc-editor.org/rfc/rfc9162](https://www.rfc-editor.org/rfc/rfc9162): a log's signed
  promise to include an entry within its maximum merge delay).
- **Key status at a block.** `eth_call` at a block hash (EIP-1898,
  [eips.ethereum.org/EIPS/eip-1898](https://eips.ethereum.org/EIPS/eip-1898)) reads
  `OnyxSignedBook.keyState` as of the end of a post's block, so the status a statement is judged by
  is the chain's at its post, the same for every client.


## Client-side reserve serving check (ROADMAP D3, 2026-10-17, ADR-0034 Amendment 2)
- **Check, do not trust the server.** Simplified payment verification (the Bitcoin paper,
  section 8, [bitcoin.org/bitcoin.pdf](https://bitcoin.org/bitcoin.pdf)) has a client check
  block headers and Merkle branches itself instead of a server's word; the reserve client does
  the same with roots read on its own node, bindings recomputed, and the reserve rule re-run.
- **Untrusted hints, checked by proofs.** Certificate Transparency clients and monitors check a
  log's inclusion and consistency proofs rather than its statements
  ([rfc-editor.org/rfc/rfc9162](https://www.rfc-editor.org/rfc/rfc9162)); `/v1/served` is such a
  hint: a list whose omissions or additions fail the client's own check.
- **Proofs of reserves against the chain.** Provisions (Dagher et al., CCS 2015,
  [eprint.iacr.org/2015/1008](https://eprint.iacr.org/2015/1008)) proves an exchange's assets
  from addresses' on-chain balances that a verifier reads from the blockchain itself. Onyx's
  base is a UTXO scan a client without a full node cannot recompute, so the client pins the
  committed scan and the residual (scan completeness, RES-1) is stated, not hidden.
- **Light-client reads of address activity** (compact block filters, BIP 157/158,
  [github.com/bitcoin/bips/blob/master/bip-0157.mediawiki](https://github.com/bitcoin/bips/blob/master/bip-0157.mediawiki))
  could let a client check that a range touched no listed address without a full node; not
  built (the reserve flows are rerun by validators, level 1).

## Far-date unseal: a second time-lock to the subject's last observable day (ROADMAP C52, 2026-10-18, ADR-0027 amendment 15)

- Need: every sealed level-2 round must become recomputable by anyone, without the coordinator,
  once no round of its fact can still run (THREATS REVLOG-5); the coordinator must be able to
  check at intake, before that date, that the copy will open.
- Encrypt-with-hash (Bellare, Boldyreva, O'Neill, "Deterministic and Efficiently Searchable
  Encryption", CRYPTO 2007, https://eprint.iacr.org/2006/186): set an encryption's coins to a
  hash of the message; in the random oracle model this is secure (PRIV) for messages of high
  min-entropy. Onyx's far copy uses it for one fresh uniform 16-byte body key per post, so
  whoever holds the key re-encrypts the far copy exactly, and nobody else learns anything a
  random `sigma` would hide. Chosen over carrying `sigma` in the plaintext (16 bytes more).
- drand/tlock decryption (Go v1.2.0 on kyber v1.3.1's `ibe.DecryptCCAonG2`; Onyx's `onyx-tlock`) recovers `sigma`
  from `V` and checks `U = H3(sigma, msg)·G2`; it never assumes how `sigma` was drawn, so a far
  copy is an ordinary quicknet ciphertext any tlock implementation opens with the round's
  signature (checked here with published signatures, `fixtures/tlock/onyx-far-date.json`).
- drand's chain lifetime: drand documents no end date for quicknet; chains have been retired
  before (fastnet, ADR-0027 amendment 13's note on files api.drand.sh no longer serves). A far
  date therefore has a horizon Onyx sets itself (1,096 days) and a stated fallback: none (the
  round's record stays the coordinator's until it unseals).
- Releasing secrets at a fixed later date without a trusted party is tlock's own purpose (IACR
  ePrint 2023/189, above); the alternative with a committee, Shutter's keypers
  (https://docs.shutter.network/), needs operators Onyx does not have (amendment 10 item 2).

## Paged track logs and the served registry (C21, C23)

ADR-0036 amendment 6. What was weighed, and which claims rest on what:
- **Merkle consistency proofs.** Certificate Transparency's logs prove that a later tree head
  extends an earlier one with a consistency proof of logarithmic size, and any entry with an
  inclusion proof ([rfc-editor.org/rfc/rfc9162](https://www.rfc-editor.org/rfc/rfc9162),
  section 2.1; already cited above for signed timestamps). That fits a client that
  checks a few entries. A track rerun is not that client: `check_extends` judges every row new to
  a commitment against every earlier row (backdating, answers rewriting a judged entry,
  operators for a held source), and `compute` needs the whole latest log, so a rerun reads every
  row anyway and random access buys nothing; a per-page proof only adds bytes and a new tree.
- **Hash chains.** A chain where each link hashes the previous head and one record is the classic
  tamper-evident log (Schneier and Kelsey's secure audit logs, as discussed by Crosby and Wallach,
  "Efficient Data Structures for Tamper-Evident Logging", USENIX Security 2009,
  [usenix.org/legacy/event/sec09/tech/full_papers/crosby.pdf](https://www.usenix.org/legacy/event/sec09/tech/full_papers/crosby.pdf);
  unconfirmed here: not re-read this loop). Its known cost, that proving one entry needs the whole
  prefix, is the cost a rerun pays anyway; its benefit is that every commitment is a checkpoint
  of one stream, checked in one pass.
- **Why not resume a Keccak state.** A sponge's state halfway through a message is not a
  commitment: Keccak-f is a permutation, invertible by design
  ([keccak.team/keccak_specs_summary.html](https://keccak.team/keccak_specs_summary.html)), so a
  server handing a client a 1600-bit "prefix state" could choose one that finishes at any target
  hash. Only heads the client computes itself, or that a commitment names, can be trusted.
- **Rules as code in a registry.** Onyx's registry root already hashes each address rule's id and
  its results on its own vectors (R-REG-3), and settlement accepts only rule types built into the
  binary (L25-F2, `RuleSet`). Serving a registry therefore serves data plus rule ids, and the
  client rebuilds with its own types: the same split as settlement, no new trust.


## Bitcoin chain statistics from blocks alone (ROADMAP P3a, 2026-10-18, ADR-0043)

What a block-only metric needs, and where each definition comes from (sources read for the
definitions only; no vendor's numbers are used, a vendor's series stay a cross-check, P6).
- **Weight and virtual size.** BIP-141 defines block weight as base size x 3 + total size, the
  4,000,000 weight-unit limit, and virtual size as weight / 4 rounded up
  ([BIP-141](https://github.com/bitcoin/bips/blob/master/bip-0141.mediawiki)). A transaction has
  witness data exactly when its total size exceeds its base size; `scripts/recount-chain-stats.py`
  reads the node's own `weight`, `size` and `vsize` instead, so the two agree only if the recipe's
  sums are right.
- **Taproot outputs.** BIP-341: a pay-to-taproot output is a SegWit version 1 program of 32 bytes,
  `OP_1 <32-byte key>` ([BIP-341](https://github.com/bitcoin/bips/blob/master/bip-0341.mediawiki));
  the node's JSON calls it `witness_v1_taproot`, which the recount uses.
- **Unspendable scripts.** Bitcoin Core's `CScript::IsUnspendable` is a script starting with
  `OP_RETURN` or longer than `MAX_SCRIPT_SIZE` (10,000 bytes)
  ([src/script/script.h](https://github.com/bitcoin/bitcoin/blob/master/src/script/script.h)).
  Such outputs are no address, so "active scripts" leaves them out.
- **Coin days destroyed.** The classic measure weights each spent output by the time it sat
  unspent ([Bitcoin Days Destroyed, bitcoin.it](https://en.bitcoin.it/wiki/Bitcoin_Days_Destroyed);
  linked as a definition, its page content is not used). From blocks alone the age is in blocks
  (creation height from the node's prevout), so Onyx commits coin-blocks destroyed; days need the
  creation block's time, which a later recipe can add from headers.
- **Prevouts over REST.** Bitcoin Core's REST interface serves raw blocks (`.bin`), headers and
  chain info unauthenticated, and its block `.json` lists transactions with their prevouts like
  `getblock` verbosity 3
  ([doc/REST-interface.md](https://github.com/bitcoin/bitcoin/blob/master/doc/REST-interface.md)).
  Raw blocks carry no input values, so fees need either the prevouts or a UTXO index of one's own;
  `scripts/refresh-onchain.py` uses the `.json` call. Unconfirmed here: no REST endpoint listens on
  this machine, so it was tested against a fake server only (THREATS CHAIN-6).
- **What needs more than blocks.** Realized cap values each UTXO at the price when it last moved;
  MVRV is market cap over realized cap; SOPR is the ratio of a spent output's value at spend and
  creation prices. All three need a price per block from a source whose terms allow
  redistribution of derived figures; none is in Onyx (docs/ops/data-sources.md), so they are
  planned, not built.

## Issuer rosters as a dated label set (ROADMAP P2a, 2026-10-18, ADR-0037 Amendment 1)

- Prior art read: the local entity indexer's per-issuer adapters (P1 survey; `the entity indexer`
  runs of OKX, Binance, BitMEX, Bybit, HTX, CoinEx, MEXC, Crypto.com, Bitfinex, Deribit, BITB).
  They treat every roster as point-in-time evidence and never forward-fill; Onyx's ADR-0037 fills
  between publications. The amendment takes the middle: a listing supports 62 days (8,928
  blocks), the indexer's own staleness rule for monthly sources is 45 days.
- Signed proofs: OKX publishes, per Bitcoin address, a "I am an OKX address" message signed by the
  keys of the address's multisig script (https://www.okx.com/proof-of-reserves); HTX signs per
  address in https://github.com/huobiapi/Tool-Node.js-VerifyAddress; CoinEx signs hot wallets and
  publishes owner-key signatures for cold multisig wallets without the redeem script. The check is
  the legacy Bitcoin signed-message rule (`bitcoin::sign_message`, recovery per BIP-137 header
  byte), as the indexer's `verify-signed-addresses.py` does it.
- BitMEX publishes redeem scripts in its PoR YAML (https://github.com/BitMEX/proof-of-reserves-liabilities,
  MPL-2.0 code), so a P2SH address can be checked against its script without a signature.
- Measured here: Onyx's verifier agrees with the indexer row for row on the imported snapshots
  (OKX 26,214/26,214, HTX 215, CoinEx 100 verified, 18 CoinEx hot-wallet proofs failing in both). Onyx grades signed only over a message naming the issuer, so 88 HTX rows signed "King will be back!" stay published (review P2a-R1-2).

## Ethereum blocks checked from JSON-RPC (ROADMAP P4a, 2026-10-19, ADR-0044)

What an execution-layer client can check without trusting its endpoint, and where each rule comes
from (sources read for the formats and rules only; no vendor's numbers are used, a vendor's
Ethereum series stay a cross-check, P6).
- **Encodings.** RLP (Yellow Paper appendix B,
  [ethereum.org RLP](https://ethereum.org/en/developers/docs/data-structures-and-encoding/rlp/)),
  hex-prefix and the Merkle-Patricia trie (appendices C and D,
  [ethereum.org trie](https://ethereum.org/en/developers/docs/data-structures-and-encoding/patricia-merkle-trie/));
  typed envelopes `type || rlp(payload)` ([EIP-2718](https://eips.ethereum.org/EIPS/eip-2718));
  transaction types 1-4 ([EIP-2930](https://eips.ethereum.org/EIPS/eip-2930),
  [EIP-1559](https://eips.ethereum.org/EIPS/eip-1559),
  [EIP-4844](https://eips.ethereum.org/EIPS/eip-4844),
  [EIP-7702](https://eips.ethereum.org/EIPS/eip-7702)). The block hash is Keccak-256 of the
  header's RLP, so re-encoding the JSON and comparing hashes turns any encoding mistake into a
  refusal instead of a wrong number: on the first run all 300 window blocks, every transaction
  hash and both trie roots of each matched.
- **Fees.** The base fee follows the parent's gas (EIP-1559: elasticity 2, change denominator 8);
  the effective gas price is `min(maxFee, baseFee + maxPriority)`; the blob base fee is
  `fake_exponential(1, excessBlobGas, fraction)` (EIP-4844) with the fraction of the fork in
  force: 11,684,671 for BPO2 ([EIP-7892](https://eips.ethereum.org/EIPS/eip-7892) blob schedule;
  mainnet BPO2 at timestamp 1,767,747,671, 2026-01-07 01:01:11 UTC, per the Fusaka meta
  [EIP-7607](https://eips.ethereum.org/EIPS/eip-7607)). Checked against data: the window's blob
  receipts all report the `blobGasPrice` the formula gives. Calldata's floor price
  ([EIP-7623](https://eips.ethereum.org/EIPS/eip-7623)) changes gas used, not the fee formula.
- **Reading blocks over JSON-RPC.** `eth_getBlockByNumber` with full transactions and
  `eth_getBlockReceipts` (the execution API spec,
  [ethereum/execution-apis](https://github.com/ethereum/execution-apis)) give everything the
  statistics need in two calls a block; the raw-RLP `debug_*` calls are not offered on the public
  endpoint used. Unconfirmed: other clients' JSON field sets (the adapter refuses an unknown
  header field, so a client that prints extra fields needs a reviewed entry).
- **What execution-layer data cannot prove.** Which chain is canonical and finalized is decided
  by the beacon chain's attestations; a light client verifies it from sync-committee BLS
  signatures (the Altair light-client protocol,
  [consensus-specs sync protocol](https://github.com/ethereum/consensus-specs/blob/master/specs/altair/light-client/sync-protocol.md);
  [Helios](https://github.com/a16z/helios) is one open implementation). Planned for Onyx (THREATS
  ETH-2); until then a second operator's hashes and the `finalized` tag stand in.
- **What needs more than blocks.** Balances at a block need state reads (`eth_getBalance`,
  `eth_call`) proven by `eth_getProof` against the header's `stateRoot`
  ([EIP-1186](https://eips.ethereum.org/EIPS/eip-1186)); token flows need `Transfer` logs, which
  the receipts root already covers. Internal ETH transfers need execution traces. These are P4b
  and P4c.

## Client check of a level-3 answer (ROADMAP C61, 2026-10-18, ADR-0028 amendment 8)
- **Reads pinned to one block.** EIP-1898 lets `eth_call` and `eth_getCode` take
  `{"blockHash": ...}` instead of a tag
  ([eips.ethereum.org/EIPS/eip-1898](https://eips.ethereum.org/EIPS/eip-1898)), so every read of
  one check sees one state; the client asks for the `finalized` block first (the tag from the
  Ethereum JSON-RPC spec, [ethereum.github.io/execution-apis](https://ethereum.github.io/execution-apis/api-documentation/)).
- **Not trusting the RPC's state.** `eth_getProof` returns Merkle proofs of an account's code
  hash and storage under a block's state root
  ([eips.ethereum.org/EIPS/eip-1186](https://eips.ethereum.org/EIPS/eip-1186)); on an OP Stack
  chain that state root is committed in an output root posted to L1
  ([specs.optimism.io/protocol/proposals.html](https://specs.optimism.io/protocol/proposals.html)).
  Checking `settleable`'s storage that way needs the book's storage layout and an L1 read; not
  built (ROADMAP C63). The C61 check pins the code hash at the block and leaves the state to the
  RPC the reader picks.
- **Health before the breaker.** The health watch already alerts on `reporter.stuckSince` before
  money stops flowing (ROADMAP L12); C62 does the same for level 3: the age of the oldest pending
  post on the node's own clock, with the alert at half the breaker.

## Data submission and bonded claims (ROADMAP S1, 2026-10-20, ADR-0038)
From a research subagent; every link opened 2026-10-20. UMA/Polymarket bond splits and the March 2025 vote are in "Proof ladder level 5" above. Keyed APIs and zkTLS: [docs/research/2026-10-20-api-keys-zk.md](docs/research/2026-10-20-api-keys-zk.md).

**(a) Letting outsiders submit data, and what is checked**

- **UMA OOv3.** An assertion "enters a challenge period"; "If no one disputes it during the
  challenge window the statement is taken as true. If disputed, the outcome is arbitrated using
  the UMA DVM" ([quick start](https://docs.uma.xyz/developers/optimistic-oracle-v3/quick-start)).
  "The default liveness window is two hours"; the OOV3 minimum bond comes from
  `getMinimumBond(token)` ([bond and liveness guide](https://docs.uma.xyz/developers/setting-custom-bond-and-liveness-parameters)).
  DVM rounds are a 24-hour commit plus a 24-hour reveal; a dispute resolves at "a minimum 65%
  majority of staked UMA" ([how it works](https://docs.uma.xyz/protocol-overview/how-does-umas-oracle-work)).
- **Chainlink.** Current docs say "node operators each query several sources for the market price
  and aggregate the estimates", and Proof of Reserve uses "security-reviewed and Sybil-resistant
  node operators" ([selecting data feeds](https://docs.chain.link/data-feeds/selecting-data-feeds)).
  I found no public onboarding steps; the docs index marks `/chainlink-nodes/v1/` as legacy
  ([llms.txt](https://docs.chain.link/llms.txt)). Operators are reviewed, not open.
- **Pyth.** Governance proposals "Choose the publishers that are permissioned to provide data for
  each price feed" ([blog, 2023-11-16](https://pyth.network/blog/permissionless-mainnet-token-led-governance-are-live)).
  For the separate Data Marketplace, CO-PIP-99 (passed, 2026-03-20) makes onboarding optimistic:
  "onboardings proceed unless formally objected to and blocked", with a 30-day question window
  and objection by the Pythian Council (6 of 9) or a token vote ([forum](https://forum.pyth.network/t/passed-co-pip-99-establishment-of-the-pyth-data-marketplace-and-assign-douro-labs-as-operator/2417)).
- **Dune.** CSV upload up to 200 MB; "All data uploaded is public and can be accessed by anyone"
  unless on an Enterprise plan; the only stated check is that column names cannot start with a
  special character or digit; the page names no licence field ([upload docs](https://docs.dune.com/web-app/upload-data)).
- **Kaggle.** `dataset-metadata.json` `licenses` "Must have exactly one entry", but `unknown` and
  `other` are allowed values; sources go in an optional free-text `userSpecifiedSources`
  ([kaggle-api docs](https://github.com/Kaggle/kaggle-api/blob/main/docs/datasets_metadata.md)).
  The kaggle.com docs page renders by script and returned no text to the fetch tool.

**(b) Bonded or staked claims**

- **Polymarket on UMA.** "Prediction markets use the UMA Optimistic Oracle for decentralized,
  permissionless resolution"; a "2-hour challenge period"; a proposer bond "(typically $750
  pUSD)" matched by the disputer; a second dispute "escalates to UMA's DVM"
  ([resolution docs](https://docs.polymarket.com/concepts/resolution)).

**What Onyx takes (ADR-0038)**

- A submission form that requires a source URL and a rights answer from a closed list. Unlike
  Kaggle's `unknown`, "not sure" is held for a rights review and never published (CLAUDE.md:
  never ingest data whose licence forbids redistribution).
- Pulls only from rights-cleared hosts; every unverified submission is labelled level 4 until a
  recompute, observers or a source proof lifts it.
- No bond in S1. In S3, a UMA-style bond and window (at least the validator rerun time), ending
  disputes by recompute, not a token vote.
- Later (Onyx proposal, not S1): new pull hosts could join by an objection window (Pyth CO-PIP-99
  style); in S1 the pull allowlist is the reviewed rows of docs/ops/data-sources.md.

## Bonded claims and data markets (partner tracker T10, 2026-10-24, ADR-0042)

Builds on "Data submission and bonded claims" above and the UMA, Bittensor and Kleros notes
earlier in this file; no new external claims beyond these links.
- UMA's optimistic oracle: a bonded answer stands unless disputed in a liveness window; only
  disputes reach a token vote ([how it works](https://docs.uma.xyz/protocol-overview/how-does-umas-oracle-work)).
  This file's earlier note applies: for objective claims a vote adds a bribery surface and no
  accuracy, so ADR-0042 lets observation decide re-readable data and keeps votes for data nobody
  can re-read.
- Polymarket separates trading from resolution and defines each market's source, date and edge
  cases first ([resolution](https://docs.polymarket.com/concepts/resolution),
  [UMA adapter](https://github.com/Polymarket/uma-ctf-adapter); checked 2026-10-21 in
  `PRODUCT-PLAN.md` on branch `product-onboarding`). ADR-0042 items 11-14 follow that split.
- Parimutuel pools (Onyx proposal): winners share the losing pools, so the market never owes more
  than it holds and needs no market maker; the cost is that odds are known only at close.


## Agent toolkit (NS8, 2026-10-26, ADR-0047)
- Model Context Protocol: tools are listed with JSON Schema inputs and called over JSON-RPC 2.0;
  stdio servers read and write newline-delimited messages, and a notification (no `id`) gets no
  reply ([spec, transports](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports),
  [lifecycle](https://modelcontextprotocol.io/specification/2025-06-18/basic/lifecycle)). The
  toolkit implements `initialize`, `ping`, `tools/list` and `tools/call` only.
- x402 v2: a 402 answer carries the offer in `PAYMENT-REQUIRED` (base64 JSON) and the client
  answers with `PAYMENT-SIGNATURE` ([x402](https://github.com/coinbase/x402)); the offer is the
  server's word, so the toolkit checks it against the pinned `OnyxRevenue` (REV-4) before any
  payment.

## Daily public data outside finance (NS3, 2026-10-26)
- USGS ComCat: "USGS-authored or produced data and information are considered to be in the U.S.
  Public Domain"
  ([copyrights and credits](https://www.usgs.gov/information-policies-and-instructions/copyrights-and-credits)).
  The FDSN `count` endpoint answers a bare integer in plain text
  ([service](https://earthquake.usgs.gov/fdsnws/event/1/)). A later product update supersedes an
  earlier one ([products](https://earthquake.usgs.gov/data/comcat/products.php)). Measured by
  the NS3 research pass: of 1,914 M4.5+ events of April-June 2026 most were last updated 70-80
  days after the event and 2.7% after more than 100 days; the M5+ count for 2026-04-02 moved
  21 → 20 → 21 in its first month. So a day is read only 100 to 400 days later
  (`SubjectRule::Daily`). The CDN caches answers for 60 s (`cache-control: public, max-age=60`),
  so observers inside one minute may read one cached copy: the recording spaces its fetches 62 s.
- NWS API (`api.weather.gov`): public domain with three conditions
  ([disclaimer](https://www.weather.gov/disclaimer)), but station observations reach back only
  about 7 days (measured 2026-10-26), too short for a read after revisions settle. NOAA NCEI
  GHCN-Daily through the Access Data Service is dated, needs no token and returns one JSON row
  per station-day ([service](https://www.ncei.noaa.gov/access/services/data/v1)); values changed
  5-25 days later in one sampled diff file, so it needs a 30-day lag. Next NS3 source.

## Lane B: paid observation, storage tiers, browser TLS prover (north-star loop NL8, NL10, NL11, 2026-10-27)

Links opened 2026-10-27, except two read only through a web search's excerpt of the page (the
Chainlink billing page answered 404 to a direct fetch; the Pyth API reference pages): those two
are "unconfirmed" in this file's sense until opened. Designs that follow: docs/spec/paid-observation.md,
docs/spec/storage-tiers.md, docs/spec/browser-tls-prover.md (drafts, not built).

**Paying for an observation when it is asked for (NL8).**
- Pyth: the caller of `updatePriceFeeds` pays the update fee, which `getUpdateFee` computes for
  the given update data ([getUpdateFee](https://docs.pyth.network/price-feeds/api-reference/evm/getUpdateFee),
  [updatePriceFeeds](https://api-reference.pyth.network/price-feeds/evm/updatePriceFeeds)). The
  one who needs a fresh value pays to put it on chain; nobody pays for updates nobody reads. Onyx
  takes the same shape: the first buyer (or the submitter) pays for the round.
- Chainlink Functions: the estimated cost of a request is reserved on the subscription balance
  at request time and deducted at response time
  ([billing](https://docs.chain.link/chainlink-functions/resources/billing)). Onyx's hold, charge
  on a final round, release on failure follows this two-step shape, on a prepaid balance
  (ADR-0046) instead of a subscription.
- UMA OOv3 and Polymarket (already in "Data submission and bonded claims"): bonds price a wrong
  claim; NL8 prices only the work of observing and leaves bonds to S3.

**Keeping less and recomputing (NL10).**
- EIP-4444 (Draft) lets execution clients stop serving headers, bodies and receipts older than
  a fixed window over p2p and expects old history from out-of-band sources (torrents, IPFS, the
  Portal Network) ([EIP-4444](https://eips.ethereum.org/EIPS/eip-4444)). Onyx's cold tier is
  the same idea one level up: the chain keeps the root, a node may drop the rows, and anyone
  can rebuild them from a durable input and check them against the root.
- Amazon S3 storage classes trade access time and retrieval fees for storage price (Standard
  with millisecond access; Standard-IA with a 30-day minimum and per-GB retrieval fees; Glacier
  Flexible Retrieval and Deep Archive archived, "not available for real-time access", restored
  first) ([storage classes](https://docs.aws.amazon.com/AmazonS3/latest/userguide/storage-class-intro.html)).
  Onyx's warm tier is the "seconds" middle; cold differs: it keeps no copy, only the means to
  recompute, so only inputs that can be fetched again byte for byte may go cold.

**Browser TLS prover today (NL11).**
- `tlsnotary/tlsn` releases are all pre-releases; the latest is `v0.1.0-alpha.15` (the release
  page shows "21 May" without a year; the repo's S2b experiment pinned this tag on 2026-10-24).
  Its notes: WASM methods take an I/O channel injected from JavaScript instead of opening
  WebSockets, and MPC threading in the browser runs in Web Workers; alpha.13 called the WASM
  bindings "very experimental" ([releases](https://github.com/tlsnotary/tlsn/releases)). The
  README: "should not be used in production"; Apache-2.0 or MIT
  ([tlsn](https://github.com/tlsnotary/tlsn)).
- `tlsn-js` (the browser NPM modules) is archived; browser work moved to `tlsn-extension`, a
  Chrome MV3 extension with QuickJS plugins, a plugin SDK, a mobile app and a verifier server
  (port 7047) with a WebSocket proxy endpoint, since a browser cannot open TCP to the API; it warns
  that server and client versions must match; Apache-2.0 or MIT
  ([tlsn-js](https://github.com/tlsnotary/tlsn-js), [tlsn-extension](https://github.com/tlsnotary/tlsn-extension)).
- TLSNotary supports TLS 1.2, with TLS 1.3 "on the roadmap"; a verifier accepts signed data only
  if it trusts the notary, and can require several notaries to rule out a colluding prover and
  notary ([intro](https://tlsnotary.org/docs/intro)). Hence the draft's rule: Onyx's own notary
  keeps a keyed proof at level 4 until no single party can forge it.

## Lane B batch 2: estimates, live track records, bounty board, ladder slice 2 (NL3, NL4, NL9, NL13, 2026-10-27)

Links opened 2026-10-27. Designs that follow: docs/spec/estimates.md, track-records-served.md,
bounty-board.md, ladder-slice-2.md (drafts, not built).

**A public forecaster for a level-2 fact (NL4).** The NWS API: "All of the information presented
via the API is intended to be open data, free to use for any purpose"; no key yet, a User-Agent
identifies the caller and "will be replaced with an API key in the future"; rate limits exist and
are not published; a gridpoint `forecast` gives 12-hour periods over seven days and
`forecastGridData` the raw forecast data
([API documentation](https://www.weather.gov/documentation/services-web-api)). NWS web
information is "in the public domain, unless specifically noted otherwise", may not be presented
as NWS-endorsed or altered and presented as official, and the NWS name and identifier need
permission beyond identifying unaltered content ([disclaimer](https://www.weather.gov/disclaimer)).
So a forecast of the Denver high is a candidate statement source against the NCEI observation;
the rights record and terms review still come first (Phase F rule). The NWS asks for contact
details in the User-Agent; Onyx sends `onyx-dev` only and no contact.

**Showing demand without showing a buyer (NL9).** k-anonymity: each record "cannot be
distinguished from at least k − 1 individuals" in the release; introduced by Samarati and Sweeney
(1998); it is weak against homogeneity and background-knowledge attacks
([k-anonymity](https://en.wikipedia.org/wiki/K-anonymity)). Census tabular releases use "data
swapping, rounding, collapsing categories, applying thresholds, table suppression" and synthetic
data ([Census working paper rrs2009-10](https://www.census.gov/library/working-papers/2009/adrm/rrs2009-10.html)).
The bounty board takes thresholds (K accounts), collapsed categories (closed lists) and rounding
(buckets), and avoids the homogeneity problem by publishing nothing per buyer, only per category.

**Estimates (NL3) and ladder slice 2 (NL13)** need no outside prior art beyond what RESEARCH.md
already holds (the proof ladder sections and ADR-0048); the drafts name the Onyx files they change.

## Dataset backing (S12, input for NS12, 2026-10-27)

Comparable designs where people stake behind data or work, earn from it and can lose. Facts as the
linked pages state them on 2026-10-27; the Onyx draft is docs/spec/dataset-backing.md.

- **The Graph, curation.** Curators signal GRT on subgraphs so indexers know what to index and
  earn a share of those subgraphs' query fees: 10%, "subject to governance", pro rata to curation
  shares; signaling pays a 1% tax that is burned; "Curators cannot be slashed for bad behavior";
  risks named include a subgraph failing on a bug (no fees until fixed) and dilution when others
  burn shares at once ([curating](https://thegraph.com/docs/en/resources/roles/curating/)). Onyx
  differs on purpose: backers can lose part of the pool, because a wrong dataset is the risk a
  buyer wants priced.
- **Token-curated registries.** An applicant deposits at least `minDeposit`; a challenger puts down
  a matching deposit; token holders vote in two phases (PLCR); a winning challenger gets its own
  deposit back plus `dispensationPct` of the opponent's stake
  ([TCR owner's manual](https://github.com/skmgoldin/tcr/blob/master/owners_manual.md)). Onyx keeps
  the finder's reward from the loser's stake but never decides by a token vote: loss events come
  from recompute, observation or the drawn panel.
- **Numerai, staking on models.** Users lock NMR on their own model for a scoring period; each
  round's stake is multiplied by a clipped round return, so good scores pay and bad ones burn; in
  the current system a position "can still neither earn nor lose more than 100% of the NMR staked
  in that round"; staked NMR in an active round "cannot be removed early"
  ([staking](https://docs.numer.ai/numerai-tournament/staking)). Onyx takes the lock-while-at-risk
  rule and a cap per event.
- **EigenLayer, deallocation delay.** Deallocations take effect after `DEALLOCATION_DELAY` blocks
  and "the existing allocation _remains slashable_" until then; deregistered operators stay
  slashable for the same delay "to prevent an operator from committing a slashable offence and
  immediately deregistering to avoid penalty"
  ([AllocationManager](https://github.com/Layr-Labs/eigenlayer-contracts/blob/main/docs/core/AllocationManager.md)).
  Onyx's exit delay (R-DB-3) is the same idea, sized to the longest dispute window.

## More datasets and parametric insurance (NL17, NL18, 2026-10-27)

**Sources (NL17).** Read-only probes with User-Agent `onyx-dev`, 2026-10-27:
- NCEI Access Data Service, `dataset=daily-summaries`: `PRCP` and `TMAX` for `USW00003017` (Denver
  International Airport) and `USW00094728` answered for 2026-09-01/02; `includeStationName=true`
  named the second "NY CITY CENTRAL PARK, NY US". Rights as data-sources S25/N20.
- USGS FDSN `count` with `latitude=34.05&longitude=-118.25&maxradiuskm=300&minmagnitude=2.5` for
  2026-07-01 answered `0`. Rights: [USGS copyrights and credits](https://www.usgs.gov/information-policies-and-instructions/copyrights-and-credits) (S24).
- BLS public API v1 (`/publicAPI/v1/timeseries/data/CUUR0000SA0`) answered `REQUEST_SUCCEEDED` with
  2026 M09 = 336.940, no key. [BLS developers](https://www.bls.gov/developers/): "Version 1.0 is a
  more limited API that does not require registration and is open for public use"; version 2.0
  "requires registration".
- EIA API v2 (`/v2/petroleum/pri/gnd/data/`) answered `API_KEY_MISSING` without a key ("Please
  register for one"). [EIA copyrights and reuse](https://www.eia.gov/about/copyrights_reuse.php):
  "U.S. government publications are in the public domain"; EIA asks for an acknowledgment that
  includes the publication date. Keyed, so not a level-2 source as is.

**Parametric insurance (NL18).**
- Definition: "pre-specified payouts based upon a trigger event", also called index-based insurance;
  such policies "often do not cover the full basis risk of the insured"
  ([Parametric insurance](https://en.wikipedia.org/wiki/Parametric_insurance)).
- CCRIF (Caribbean Catastrophe Risk Insurance Facility) pays on modelled hazard and loss rather than
  an adjuster's assessment; payouts rise with modelled loss up to a coverage limit; it states that
  parametric products carry basis risk (summarised from its technical papers, e.g.
  [CCRIF policies technical paper](https://www.ccrif.org/sites/default/files/publications/technical-materials/Technical_Paper_CCRIF_Policies_November_2015.pdf);
  read through a search summary, not opened: "unconfirmed" in this file's sense).
- Etherisc's GIF sandbox demo fire product "pays out an amount of USDC if a fire event is triggered
  by the oracle" ([gif-sandbox](https://github.com/etherisc/gif-sandbox)); who runs the oracle is
  not stated there. Onyx's difference: the trigger is a value observers recorded on chain and anyone
  proves, not one oracle's report.

## NL11 upstream check (2026-10-28)

Checked on GitHub with User-Agent `onyx-dev`, to decide whether a browser TLS prover fits one loop
(docs/north-star-loop.md NL11):

- [tlsnotary/tlsn releases](https://github.com/tlsnotary/tlsn/releases): the latest tag is
  `v0.1.0-alpha.15`, marked pre-release, published 2026-05-21; the two before it are alpha.14
  (2026-01-14) and alpha.13 (2025-10-15).
- [tlsnotary/tlsn-js](https://github.com/tlsnotary/tlsn-js): archived.
- [tlsnotary/tlsn-extension](https://github.com/tlsnotary/tlsn-extension): active (last push
  2026-06-26), `package.json` license MIT, version `0.1.0-alpha.15`; the repository has no LICENSE
  file at its root. Browser proving runs as a Chrome extension with a verifier server
  (`VERIFIER.md`).

Result: NL11 is parked. The Onyx S2b experiment (experiments/tlsn-fiscaldata) proves natively;
an in-page alpha.15 WASM build is not proven, and the extension route asks each contributor to
install an extension.

