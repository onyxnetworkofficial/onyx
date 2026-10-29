<h1 align="center">Onyx</h1>

<p align="center">
  <b>A contributor-run network for any kind of data, where every answer comes with its proof.</b><br>
  Anyone can submit data. Others check it. Buyers pay per row and verify each answer themselves.
</p>

<p align="center">
  <img alt="Status: testnet prototype" src="https://img.shields.io/badge/status-testnet%20prototype-orange">
  <img alt="Chain: Base Sepolia" src="https://img.shields.io/badge/chain-Base%20Sepolia-0052ff">
  <img alt="Licence: Apache-2.0" src="https://img.shields.io/badge/licence-Apache--2.0-blue">
  <img alt="Rust" src="https://img.shields.io/badge/Rust-workspace-b7410e">
  <img alt="Solidity" src="https://img.shields.io/badge/Solidity-Foundry-363636">
</p>

> **Testnet only.** No mainnet deployment and no real funds: tONYX and test USDC have no value, and
> the token economics are not final. A hosted test node and the website run at
> <https://onyx.cryptagraph.com>. The entity label sets are not licensed for redistribution until
> their rights are cleared ([DATA-LICENSE.md](DATA-LICENSE.md)). Current state: [STATUS.md](STATUS.md).

## Try the live testnet

| What | Where |
|---|---|
| Website: browse checked data, run the checks in your browser, add data | <https://onyx.cryptagraph.com> |
| Markets: futures settled from observed data, test insurance (press "Read the chain") | <https://onyx.cryptagraph.com/#/markets> |
| Query API (payments not switched on) | <https://onyx.cryptagraph.com/v1/health>, [OpenAPI](https://onyx.cryptagraph.com/v1/openapi.json) |
| AI connector (MCP): find, read and propose data from ChatGPT, Claude, Cursor, VS Code or Gemini CLI | `https://onyx.cryptagraph.com/mcp`, [docs/agents.md](docs/agents.md) |
| Run an observer of your own | [docs/join.md](docs/join.md) |

## How it works

1. **Contributors** supply data: chain metrics, public records, signed reports, their own data.
2. **Validators** check it, by the strongest method the data allows: rerun an open recipe,
   re-observe the source in a sealed round, or verify the source's signature.
3. **The network commits** the checked rows to a Merkle root on Base Sepolia. Rows stay off chain;
   every row is provable.
4. **Buyers** pay per call with [x402](docs/adr/0006-x402.md) (testnet USDC) and check every answer
   against the root on their own node. They never have to trust the server.

| Level | Name | How it is checked |
|---|---|---|
| 1 | Recomputed | Anyone reruns a pinned WebAssembly recipe on public inputs and gets the same root. |
| 2 | Re-observed | Bonded observers drawn at random fetch the source in one window, seal their answers, then reveal; the majority settles. |
| 3 | Source-proven | The source signed the value with a key registered on chain. |
| 4 | Claim | Someone says it is true; nobody could check it yet. It is labelled unverified. |
| 5 | Track record | A source's statements are scored later against level 1-3 answers. |

Details: [docs/spec/proof-ladder.md](docs/spec/proof-ladder.md).

## Features

| Area | What is built | Read more |
|---|---|---|
| **Data and recipes** | Deterministic WASM recipes (no floats, clock or randomness), content-addressed; Bitcoin and Ethereum chain statistics, company flows and reserves, mining-pool shares | [recipes/](recipes/README.md), [ADR-0043](docs/adr/0043-chain-stats-recipe.md), [ADR-0044](docs/adr/0044-ethereum-chain-stats.md) |
| **Rounds and settlement** | Miners commit and reveal, validators rerun sampled work with canaries, disputes go to a drawn panel; finalized roots and rewards on chain | [docs/spec/settlement.md](docs/spec/settlement.md), [ADR-0021](docs/adr/0021-disputes-by-drawn-panel.md) |
| **Level 2 observations** | Sealed observer rounds over public sources (U.S. Treasury, SEC filings), time-locked reveals, on-chain draws | [ADR-0027](docs/adr/0027-proof-ladder-level-2.md) |
| **Level 3 and 5** | Signed source reports checked against the chain; level-5 track records with checkable logs | [ADR-0028](docs/adr/0028-proof-ladder-level-3-signed-sources.md), [ADR-0036](docs/adr/0036-proof-ladder-level-5-track-record.md) |
| **Labels** | Entity-only label sets (exchanges, ETFs, funds), dated memberships, first-party attestations, salted rows a buyer cannot guess from | [docs/spec/labels.md](docs/spec/labels.md), [ADR-0037](docs/adr/0037-dated-membership-and-confirmation-depth.md) |
| **Query API** | Every row with its Merkle proof and a verify bundle; x402 payments; one OpenAPI 3.1 description, served at `/v1/openapi.json` and checked against the code | [docs/api/openapi.json](docs/api/openapi.json), [docs/spec/delivery.md](docs/spec/delivery.md) |
| **Add data** | A five-question intake that picks the proof level, review receipts, own-data uploads; an approved Treasury proposal runs through the real observer round | [ADR-0038](docs/adr/0038-data-submission-intake.md), [ADR-0039](docs/adr/0039-submission-review.md) |
| **Stakes and futures** (testnet) | Staked claims decided by observation, a stake vote, or a drawn panel; parimutuel futures on level 1-2 facts; prepaid buyer balances with batched settlement | [ADR-0042](docs/adr/0042-staked-submissions-and-data-futures.md), [docs/spec/stakes-and-markets.md](docs/spec/stakes-and-markets.md), [ADR-0046](docs/adr/0046-prepaid-balances.md) |
| **Website** | Runs the network's own Rust checks in the browser (WASM): data, proofs, labels, reserves, markets, API reference | [apps/web/](apps/web/README.md), [ADR-0020](docs/adr/0020-website-stack.md) |
| **Operations** | `onyx-node` service, Caddy TLS front, systemd units, health watch, runbooks, a preview bundle | [docs/ops/](docs/ops/README.md) |

## API

The node serves these routes (OpenAPI 3.1: [docs/api/openapi.json](docs/api/openapi.json)).

<!-- generated:routes (scripts/check-readme.py --write) -->
| Method | Route | What | Paid |
|---|---|---|---|
| `GET` | `/v1/health` | Server and payment health | free |
| `GET` | `/v1/openapi.json` | This description | free |
| `GET` | `/v1/export/{round}/{shard}` | Every row of a shard | x402 |
| `GET` | `/v1/estimates/{metric}` | An estimate, apart from checked values | free |
| `GET` | `/v1/labels/{set}/{address}` | One address in a salted label set | free |
| `GET` | `/v1/metrics/{metric}` | A metric's row for one subject | x402 |
| `GET` | `/v1/observations/{metric}` | A level-2 value with its round | x402 |
| `GET` | `/v1/observations/{metric}/attempts` | Rounds of a fact that settled no value | free |
| `GET` | `/v1/tickets/{ticket}` | A paid observation ticket | free |
| `GET` | `/v1/proofs/{round}/{shard}/{index}` | One row with its proof | x402 |
| `GET` | `/v1/served/{metric}` | Shards a serving check cleared | free |
| `GET` | `/v1/settlement/{market}` | A market's value, ready for a contract | x402 |
| `GET` | `/v1/signed/{fact}` | A level-3 signed fact | free |
| `POST` | `/v1/submit` | Record a data proposal for review | free |
| `POST` | `/v1/submit/check` | Check a data proposal | free |
| `GET` | `/v1/submit/{id}` | A receipt's status | free |
| `GET` | `/v1/track` | Track records: the index | free |
| `GET` | `/v1/track/log` | Track records: the publication logs | free |
| `GET` | `/v1/track/registry` | Track records: the registry | free |
| `GET` | `/v1/track/{source}/{metric}` | One source's track record on a metric | free |
| `GET` | `/v1/track/{source}/{metric}/history` | A page of a record's history | free |
| `GET` | `/v1/wants` | Wanted data categories | free |
| `POST` | `/v1/wants` | Post a wanted category | free |
<!-- /generated:routes -->

## Quickstart

Needs Rust (rustup reads `rust-toolchain.toml`), [Foundry](https://getfoundry.sh) and `jq`; the
website gate also needs `pnpm` and `wasm-bindgen-cli`.

```sh
git clone --recurse-submodules <repo> && cd onyx
scripts/check-all.sh                    # format, lint, every test, docs, boundaries, contracts
ONYX_WEB=1 scripts/check-all.sh         # plus the website build and its browser tests
ONYX_WEB_PUBLIC=1 scripts/check-all.sh  # plus both public builds and their guards
ONYX_DOCKER=1 scripts/check-all.sh      # plus both Docker images, built and smoke-tested
```

The entity label data is not public until its rights are cleared ([DATA-LICENSE.md](DATA-LICENSE.md)).
Without it, the tests that need it are reported as ignored ("needs the label data, not public
yet"), never as passed; the crates' `label-data` feature turns them on.

Try the pieces locally:

```sh
scripts/demo-local.sh                                 # a full round on a local chain, served and checked
ONYX_SUBMIT_BROWSER=1 scripts/level3-book.sh local     # submit, review, observe, serve, buy and check
scripts/demo-markets.sh                               # the staking and futures story with test tokens
cd apps/web && pnpm install && pnpm dev               # the website at http://localhost:5173
```

## Rerun and check it yourself

Every number comes from a recipe whose id is the Keccak-256 of its bytes. Rebuild it, rerun it on
the pinned inputs and compare the Merkle root:

```sh
scripts/build-recipes.sh && git diff --exit-code -- recipes/out   # same bytes, same recipe ids
cargo test -p onyx-source-btc --test golden                       # the root must equal fixtures/golden
cargo test -p onyx-source-btc --test golden_pools                 # the same for the mining-pool recipe
```

## Repository layout

<!-- generated:layout (scripts/check-readme.py --write) -->
| Path | What |
|---|---|
| **Core (sector-agnostic; never imports an adapter)** | |
| `crates/onyx-api` | Query API with per-row proofs, x402 v2 payments, settlement endpoint and time-decay pricing (M7, ADR-0023) |
| `crates/onyx-check` | Command-line check of an Onyx API answer for agents and scripts (NS8, ADR-0047): the network's own client check, with the root read on the caller's own chain node |
| `crates/onyx-coordinator` | Rounds, commit-reveal, sampling and canaries, disputes, level-2 observer rounds, level-3 intake and track-record publication; reward and slash ledger |
| `crates/onyx-core` | Sector-agnostic domain types for Onyx: claims, lanes, verification methods, data rights, label sets, commitments |
| `crates/onyx-intake` | Data submission intake (ROADMAP S1, ADR-0038): onboarding answers, the proof-level route, submission checks, the rights-cleared fetch policy and a preview of a pulled body |
| `crates/onyx-labels` | Label sets (entities only): versions, dated memberships, salted rows, first-party attestations, issuer rosters and calibration scoring |
| `crates/onyx-level2` | Level-2 round runner for any dated feed (NS3): the draw, commits, reveals, receipts, settlement and results store, with each source supplying its template, fetcher and registry |
| `crates/onyx-merkle` | Positional Keccak-256 Merkle commitments and per-row proofs (ADR-0003) |
| `crates/onyx-miner` | Miner: fetch a shard's pinned inputs from the CAS, run the recipe, commit, reveal (ADR-0018) |
| `crates/onyx-node` | The onyx-api service binary (docs/ops/deployment.md): config from env, catalog and rows from disk, roots from the chain, x402 self-settle or facilitator, reporter loop, graceful shutdown |
| `crates/onyx-recipe-host` | Deterministic WASM recipe runner: the canonical wasmi engine (fuel, heap stacks, float and import rejection) and an advisory wasmtime fast path (ADR-0004, ADR-0014) |
| `crates/onyx-recipe-sdk` | What recipe authors import: input decoding, output encoding, the host ABI (no floats, no clock, no randomness) |
| `crates/onyx-tlock` | drand/tlock time-lock encryption (Boneh-Franklin IBE with Fujisaki-Okamoto) for drand quicknet, on blst (ROADMAP C50) |
| `crates/onyx-track` | Proof level 5 track records: per-source history of agreement with later stronger answers, computed and committed deterministically (ADR-0036) |
| `crates/onyx-validator` | Validator: recompute assigned reveals and commit-reveal signed verdicts (ADR-0019) |
| **Sector adapters and schemas** | |
| `adapters/btc-schema` | Borsh schema shared by the Bitcoin source adapter and Bitcoin recipes (no_std) |
| `adapters/chain-schema` | Borsh schema of the Bitcoin chain statistics metric (no_std, ADR-0043) |
| `adapters/eth-schema` | Ethereum block format for Onyx (no_std, ADR-0044): RLP, the ordered Merkle-Patricia trie root, header, transaction and receipt decoding, and the Borsh schema of eth-chain-stats |
| `adapters/pool-schema` | Borsh schema of the Bitcoin mining-pool block metric (no_std) |
| `adapters/reserve-schema` | Borsh schema of the Bitcoin entity reserve metric (no_std, ADR-0034) |
| `adapters/reserve-serve` | Serving check of btc.entity-reserve for onyx-api (ADR-0034 Amendment 1) and the onyx-node-btc binary |
| `adapters/source-btc` | Bitcoin source adapter: pinned block fixtures and blocks read from a node, each checked (proof of work, parent links, Merkle root); entity flows, reserves and pools |
| `adapters/source-edgar` | Level-2 source: crypto-asset holdings a company states in its SEC filings (EDGAR XBRL, public information, no account), read by a pinned fetcher |
| `adapters/source-eth` | Ethereum source adapter (ADR-0044): JSON-RPC block and receipt answers to the canonical eth-chain-stats source, with every hash and trie root checked |
| `adapters/source-fiscaldata` | Level-2 source: one record of the U.S. Treasury Fiscal Data API (open data, no account), read by a pinned fetcher |
| `adapters/source-ncei` | Level-2 source: one station's daily maximum temperature from NOAA NCEI GHCN-Daily (a U.S. government work, no account), read by a pinned fetcher (NS3) |
| `adapters/source-page` | Example level-2 fetcher: reads one value from a JSON page by JSON pointer (no account, no network in tests) |
| `adapters/source-signed` | Example level-3 source: reads a source's published keys document and its signed reports (synthetic fixtures, test key, no network) |
| `adapters/source-usgs` | Level-2 source: the number of magnitude 5+ earthquakes the USGS catalog lists worldwide on one day (U.S. public domain, no account), read by a pinned fetcher (NS3) |
| **Apps** | |
| `apps/agent` | The agent toolkit: an API client and an MCP server built from the OpenAPI file; answers are checked by onyx-check |
| `apps/demo` | The first demo site: one page that walks the pipeline and reruns the real recipe in the browser |
| `apps/web` | The website (SvelteKit): every page runs the network's own Rust checks, compiled to WebAssembly |
| `apps/web/verify` | Browser verifier for the website (ADR-0020): wasm-bindgen exports over the network's own checks |
| **Recipes (WebAssembly, outside the workspace)** | |
| `recipes/btc-chain-stats` | Recipe: per-block Bitcoin chain statistics (transactions, fees, fee rates, size, supply, SegWit and Taproot use, active scripts, coin-blocks destroyed) from blocks alone (ADR-0043) |
| `recipes/btc-entity-balance` | Reference recipe: per-entity BTC flows (and balances when a base is given) over a block range |
| `recipes/btc-entity-flow-series` | Recipe: per-entity BTC flows for every block of a range (a rerunnable time series) |
| `recipes/btc-entity-reserve` | Recipe: per-entity BTC balance at the end of a range, from a committed base plus the range's flows (ADR-0034) |
| `recipes/btc-pool-blocks` | Recipe: blocks mined per pool over a range, by coinbase tag and pool payout script (a rerunnable block metric) |
| `recipes/eth-chain-stats` | Recipe: per-block Ethereum statistics (transactions, gas and fullness, base fee, priority fees, ETH burned, blob gas and fees, contract creations, value sent) from blocks whose hashes and trie roots it checks itself (ADR-0044) |
| `recipes/out` | The built recipe WASM files and their Keccak-256 ids (reproducible, committed) |
| `recipes/test-clock` | Negative test recipe: must be rejected by the host (uses clock) |
| `recipes/test-float` | Negative test recipe: must be rejected by the host (uses float) |
| `recipes/test-rand` | Negative test recipe: must be rejected by the host (uses randomness) |
| **Experiments** | |
| `experiments/tlsn-fiscaldata` | A real TLSNotary proof of a U.S. Treasury API answer with a hidden header, verified offline (its own workspace) |
| **Everything else** | |
| `contracts/` | The Foundry project: settlement, disputes, revenue, registry, level-3 book, stakes and futures (Base Sepolia only) |
| `deploy/` | Caddy TLS front, systemd units, health-watch thresholds, the reserve serving gate |
| `docs/` | Specs, decision records, operations, audits, partner notes, the API description |
| `fixtures/` | Pinned inputs, goldens and shared test vectors (Rust, Solidity and the browser read the same files) |
| `fuzz/` | cargo-fuzz targets over decoders, stores and proofs (opt-in, nightly) |
| `scripts/` | Checks, builds, rehearsals, recounts and data refreshes (scripts/README.md) |

Contracts (`contracts/src`, Base Sepolia and local Anvil only):

| Contract | What |
|---|---|
| [`IStakeToken`](contracts/src/IOnyxMarkets.sol) | The ERC-20 subset the stake and market contracts use (ADR-0042) |
| [`OnyxCommit`](contracts/src/OnyxCommit.sol) | Commitment binding hash and commit hash (ADR-0011) |
| [`OnyxDisputes`](contracts/src/OnyxDisputes.sol) | The on-chain dispute game of one Onyx ledger (ADR-0024; rules from ADR-0021) |
| [`OnyxFutures`](contracts/src/OnyxFutures.sol) | Parimutuel markets on a future value of an Onyx fact (ROADMAP S6, ADR-0042 items 11-14) |
| [`OnyxLedger`](contracts/src/OnyxLedger.sol) | State, types and shared rules of the settlement ledger (ADR-0022, ADR-0024) |
| [`OnyxMerkle`](contracts/src/OnyxMerkle.sol) | Verifier for the positional Keccak-256 Merkle layout in ADR-0003 |
| [`OnyxObservedFacts`](contracts/src/OnyxObservedFacts.sol) | The values of level-2 facts recorded in `OnyxRegistry`, as `OnyxFutures` reads them (NORTH-STAR NS11, ADR-0042 amendment 2) |
| [`OnyxPanelOracle`](contracts/src/OnyxPanelOracle.sol) | The drawn-panel fallback oracle (ROADMAP S3b, ADR-0042 amendment 1) |
| [`OnyxParametric`](contracts/src/OnyxParametric.sol) | Parametric insurance on a future value of an Onyx fact (north-star loop NL18, docs/spec/parametric-insurance.md, ADR-0042 amendment 3): a policy pays a fixed amount when the fact's value crosses a line, with no claim and no loss assessment |
| [`OnyxPayout`](contracts/src/OnyxPayout.sol) | The integer payout math shared by `OnyxStakes` (S3) and `OnyxFutures` (S6), mirrored by the website's `markets.ts` (no Rust client yet, ADR-0042); shared vectors in fixtures/markets/vectors.json (ADR-0042) |
| [`OnyxPrepaid`](contracts/src/OnyxPrepaid.sol) | Prepaid buyer balances with batched settlement (partner tracker T7, a partner's questions 7 and 8; docs/spec/prepaid.md) |
| [`OnyxRegistry`](contracts/src/OnyxRegistry.sol) | The on-chain registry of what each metric may be computed from, and the level a consumer may rely on (ADR-0030) |
| [`OnyxReobserve`](contracts/src/OnyxReobserve.sol) | Re-observation of an accepted auto-pull submission (ROADMAP S4, ADR-0042 item 5): a minimal faithful on-chain port of the level-2 round rules of ADR-0027 for exact values |
| [`OnyxRevenue`](contracts/src/OnyxRevenue.sol) | Usage revenue of finalized shards, split by role (ADR-0025, docs/spec/revenue.md) |
| [`OnyxRound`](contracts/src/OnyxRound.sol) | Round-level hashes and texts |
| [`OnyxSettlement`](contracts/src/OnyxSettlement.sol) | The canonical ledger of an Onyx network (ADR-0022) |
| [`OnyxSettlementExt`](contracts/src/OnyxSettlementExt.sol) | The part of the settlement ledger reached by `delegatecall` from `OnyxSettlement`'s fallback (ADR-0024 item 1): readers for the dispute game and the hooks only `OnyxDisputes` may call |
| [`OnyxSig`](contracts/src/OnyxSig.sol) | Text formatting and EIP-191 signature recovery that match the Rust side (`Hash32` Display, `eip191_digest`, `eip191_recover` in crates/onyx-labels/src/attest.rs) |
| [`OnyxSignedBook`](contracts/src/OnyxSignedBook.sol) | The level-3 signed-source book on chain (ADR-0028 amendment 6, docs/spec/proof-ladder.md "Level 3 on chain") |
| [`OnyxStakes`](contracts/src/OnyxStakes.sol) | Staked data submissions (ROADMAP S3, ADR-0042) |
| [`OnyxTestToken`](contracts/src/OnyxTestToken.sol) | "tONYX test token": a plain ERC-20 with NO value, for the S3/S6 prototype only (ADR-0042 item 2) |
| [`OnyxTestnetOwnerOracle`](contracts/src/OnyxTestnetOwnerOracle.sol) | The fallback oracle of the S3 prototype (ADR-0042 item 7): one owner key answers a contested or low-quorum claim as correct, wrong or void |
<!-- /generated:layout -->

## Documentation

| Read | For |
|---|---|
| [docs/README.md](docs/README.md) | The map of every document and which tracker holds what |
| [STATUS.md](STATUS.md) | What changed last and what is next |
| [ROADMAP.md](ROADMAP.md), [PLAN.md](PLAN.md), [PRODUCT-PLAN.md](PRODUCT-PLAN.md) | Phases, milestones and the product plan |
| [DECISIONS.md](DECISIONS.md) | Every architecture decision record |
| [THREATS.md](THREATS.md) | The threat model: every threat with its test or an open milestone |
| [RESEARCH.md](RESEARCH.md), [GLOSSARY.md](GLOSSARY.md) | Prior art with sources; terms |

## Hardware

Minimum per role on Bitcoin; recommended machines and Ethereum in
[docs/ops/hardware.md](docs/ops/hardware.md). Check a machine with
`onyx-node doctor --role <role> --data-dir <dir>`.

| Role | Minimum |
|---|---|
| Contributor (Bitcoin full node + recipes) | 2 CPUs, 6 GiB RAM, 836 GiB disk unpruned (NVMe recommended) |
| Validator | as a contributor, plus 4 GiB free per concurrent recipe run |
| Entity indexer / reserve path | 2,033 GiB disk; CPU and RAM measured only on a 24-thread, 91 GiB machine |
| Node operator (API) | 1 CPU, 1 GiB free disk; 2 GiB RAM recommended (the shipped cap; 25.2 MiB peak measured) |
| Buyer | a browser (the Data page loads 2,138 KiB) |
| Coordinator | not measured yet |

## Corrections and removal

Labels name companies only (exchanges, ETFs, funds, protocols), never people. To ask for a label
to be corrected or removed, open an issue titled "label correction". A request that names a
person goes through the private form instead (the **Security** tab, **Report a vulnerability**,
title "Removal"; [docs/ops/label-removal.md](docs/ops/label-removal.md)). Requests are logged and resolved by evidence
([docs/spec/rights.md](docs/spec/rights.md), rule 4).

## Security, contributing and licence

- Security reports: [SECURITY.md](SECURITY.md). Contributing: [CONTRIBUTING.md](CONTRIBUTING.md).
- Code and documentation: [Apache-2.0](LICENSE), see [NOTICE](NOTICE). Data keeps each source's
  terms and is **not** covered by the code licence: [DATA-LICENSE.md](DATA-LICENSE.md).
  Third-party code, fonts and vectors: [THIRD-PARTY.md](THIRD-PARTY.md).
