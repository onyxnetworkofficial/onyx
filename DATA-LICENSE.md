# Data in this repository and its terms

The code is Apache-2.0 ([LICENSE](LICENSE)). **Data is not automatically covered.** Each data set
below keeps the terms of its source. Where we have no clear right to pass data on, this file says
so: "not licensed for redistribution; pending". Not legal advice.

Sources, quotes and decisions: [docs/ops/data-sources.md](docs/ops/data-sources.md) (catalogue
S1-S15), [docs/ops/legal-data-rights.md](docs/ops/legal-data-rights.md) and the dataset table in
[docs/audit/2026-10-13-data-privacy.md](docs/audit/2026-10-13-data-privacy.md). Labels name
entities only (exchanges, ETFs, funds, protocols, pools), never individuals. To ask for a label
to be corrected or removed, see [README "Corrections and removal"](README.md#corrections-and-removal).

Last checked 2026-10-17 (branch `open-source` from main `67a1296`); label set v3 rows added 2026-10-18 (branch `rosters-v3`); section 2's ETF row and section 6 added 2026-10-19 (branch `etf-filings`).

## 1. Not licensed for redistribution; pending (label rights, THREATS L8-5)

The entity label sets come from proof-of-reserves address lists that OKX, Bitwise (BITB),
BitMEX and Binance publish for public verification (v3 adds Bybit, Bitfinex, HTX, CoinEx, MEXC,
Crypto.com and Deribit, below). None of these lists states a licence
(Binance's site terms are still unread), and two sets of terms may restrict distribution (OKX
Terms of Service 8.1; the BITB examiner's terms s.11, quoted in data-sources.md S5 and S6);
whether those terms cover the address lists is not stated. No written permission is on file
(launch checklist LC-1). Until a written permission is filed, **everything in this section is
not licensed for redistribution; pending**, including totals computed from the labels.

| Path | What |
|---|---|
| `fixtures/labels/test.onyx.entities.btc.v1.json`, `.v2.json` | Label sets v1 (120 rows) and v2 (9,850 rows): OKX, Bitwise BITB, BitMEX, Binance addresses (S3, S4) |
| `fixtures/golden/btc-entity-balance-*.json`, `btc-entity-flow-series-*.json`, `btc-entity-reserve-*.json` | Expected recipe outputs per entity over those labels |
| `fixtures/reserves/*` (except the header chain, section 3) | UTXO scans and flows over the label-set addresses; `cross-checks.json` holds totals from the OKX and BITB published figures |
| `apps/web/static/history/week/*`, `apps/web/static/history/reserves-week.json` | Per-entity flows and balances derived from label set v2 |
| `apps/web/static/run/live-run.json`, `apps/web/static/run/base-sepolia-*.json` (their OKX figures only) | Run records; the `okx` fields are label-derived ([rights fallback inventory](docs/audit/2026-10-16-rights-fallback.md)) |
| `apps/web/src/lib/reserves.ts` (the OKX and BITB figures in it) | Published-figure comparison values |
| Screenshots under `docs/audit/website-*` that show company totals | Images of the private preview |
| `fixtures/labels/test.onyx.entities.btc.v3.json`, `fixtures/rosters/**` | Label set v3 (dated, 18,444 memberships, 11 companies; ROADMAP P2a) and the issuers' own roster snapshots it is built from, one row per source below |
| `apps/web/static/history/rosters-week.json` | Per-entity flows and balances derived from label set v3 |

Label set v3's sources (each publishes its roster for public verification and states no licence;
terms beyond the roster pages unread unless noted; source sections in
[fixtures/labels/PROVENANCE.md](fixtures/labels/PROVENANCE.md) v3):

| Source id | Issuer and what is held | Terms |
|---|---|---|
| `okx.por` | OKX monthly proof-of-reserves zips: the Bitcoin rows of 7 snapshots (2026), with OKX's signatures | none stated on the files; OKX Terms of Service 8.1 may restrict distribution (S3) |
| `binance.por` | Binance monthly wallet files: the HotCold member of 43 snapshots | none stated; site terms unread (S4) |
| `bitmex.por` | BitMEX proof-of-reserves YAMLs, 61 snapshots (2026) | none stated (S4) |
| `bybit.por` | Bybit monthly proof-of-reserves audit PDFs: their Bitcoin addresses, 17 snapshots | none stated in the reports; site terms unread |
| `bitfinex.wallets` | Bitfinex's `bitfinexcom/pub` wallets.txt (2022 commit and current) | none stated in the repository file; repository licence unread |
| `htx.por` | HTX's tagged reserve CSVs in `huobiapi/Tool-Node.js-VerifyAddress`, 45 snapshots, with HTX's signatures | none stated in the files; repository licence unread |
| `coinex.por` | CoinEx wallet workbooks, 20 snapshots, with CoinEx's signatures | none stated; site terms unread |
| `mexc.por` | MEXC wallet CSVs (por.mocortech.com), 8 snapshots (2026) | none stated; site terms unread |
| `cryptocom.wallets` | Crypto.com's 2022-11-11 wallet disclosure: its 6 Bitcoin addresses | none stated on the page; site terms unread |
| `deribit.por` | Deribit's proof-of-reserves support article: its 9 Bitcoin addresses | none stated on the article; site terms unread |
| `bitwise.bitb` | Bitwise's bitbetf.com wallet list of 2026-08-24: 65 wallets | the BITB examiner's terms s.11 may restrict distribution (S5, S6) |

Not held: wBTC (wbtc.network's terms forbid redistribution and automated access, IDX-R1-F1) and
Coinbase cbBTC (terms unread, IDX-R1-F2).

The rest of each run record (transaction hashes, testnet addresses of our own keys, roots) is
Onyx's own data (section 4).

## 2. Third-party data with clear terms

| Path | Source | Terms (quoted or named) | Duties |
|---|---|---|---|
| `adapters/source-fiscaldata/fixtures/*`, `apps/web/static/level2/debt-to-penny.json` | U.S. Treasury Fiscal Data, Debt to the Penny (S10) | "The data on this site is available to copy, adapt, redistribute, or otherwise use for non-commercial and commercial purposes." (https://fiscaldata.treasury.gov/about-us/) | none stated |
| `adapters/source-edgar/fixtures/*`, `apps/web/static/level2/sec-holdings.json` | SEC EDGAR XBRL `companyconcept` for Strategy and Riot (S11) | "Information presented on sec.gov is considered public information and may be copied or further distributed by users of the web site without the SEC's permission." (https://www.sec.gov/privacy) | SEC fair-access rules when fetching (10 requests/second, declared User-Agent) |
| `adapters/source-edgar/fixtures/etf/*`, `apps/web/static/level2/etf-holdings.json` | SEC EDGAR XBRL `companyconcept` for seven US spot bitcoin funds (IBIT, FBTC, GBTC, Grayscale Bitcoin Mini, ARKB, EZBC, DEFI; S11, ROADMAP P2c), recorded 2026-10-19 from data.sec.gov, SHA-256 per body in `fixtures/etf/manifest.json` | same SEC statement as the row above | SEC fair-access rules when fetching; recorder sends at most 2 requests/second with User-Agent `onyx-dev (contact pending)` (ROADMAP P2g: a role contact replaces "contact pending") |
| `fixtures/pools/pool-allowlist.json`, `apps/web/static/history/pools-week.json` (pool names and links) | bitcoin-data/mining-pools (S9) | MIT | keep `fixtures/pools/LICENSE-mining-pools.txt` with every copy |
| `fixtures/bip322/*.json` | BIP-322 test vectors (bitcoin/bips) | CC0-1.0 | none |
| `fixtures/tlock/go-tlock/*`, `fixtures/tlock/tlock-js/*` | drand/tlock and drand/tlock-js test data | Apache-2.0 OR MIT | keep the source notes in `fixtures/tlock/PROVENANCE.md` |
| `fixtures/tlock/drand/*.json` | drand quicknet and quicknet-t published rounds | none stated; public randomness beacon output | none known |
| `apps/web/static/fonts/*.woff2` | IBM Plex, Instrument Serif | SIL Open Font License 1.1 | keep the `OFL-*.txt` files next to the fonts |

## 3. Public chain data (no licence applies)

| Path | Source |
|---|---|
| `fixtures/btc/blocks-921080-921082.json` | Bitcoin mainnet blocks, fetched from mempool.space (Esplora) and reproducible from any Bitcoin node (S1, S2) |
| `fixtures/btc/coinbase-970019-970029.json` | Coinbase proofs of 11 mainnet blocks from our own node; range chosen so every coinbase carries only a pool tag (POOL-R1-F2; data privacy audit) |
| `fixtures/reserves/headers-969839-971207.json` | 1,369 mainnet block headers (ADR-0037): 969,839-971,199 from our own node's raw blocks, 971,200-971,207 from mempool.space (Esplora) with hashes cross-checked on blockstream.info; every header is checked for proof of work (S1, S2) |
| Base Sepolia transaction hashes, roots and addresses in `apps/web/static/run/*` | Public testnet chain state of our own contracts (S12) |
| `fixtures/eth/blocks-26225268-26225270/*`, `fixtures/eth/window-26225261-26225560.manifest.json`, `apps/web/static/onchain/eth-chain-stats.json`, `apps/web/static/onchain/eth-rerun-input.b64.txt` | Ethereum mainnet blocks and receipts (ADR-0044): the JSON-RPC answers of Onyx's own Ethereum node (reth, on the maintainer's indexer machine, reached over an SSH tunnel on loopback) for three blocks, the SHA-256 of all 601 answers of the 300-block window 26,225,261-26,225,560, and data computed from them; every header, transaction and receipt was checked against the chain's own hashes and trie roots, and block hashes were cross-checked on `rpc.flashbots.net`, so the same bytes come from any Ethereum node. The bytes include transaction calldata and log data as the chain carries them (THREATS ETH-6) |

Consensus data carries no licence (`onyx:public-chain-data`, legal-data-rights.md). Onyx claims no
rights in it. mempool.space's own API terms were not readable by our fetch; the block bytes do
not depend on them because any node returns the same bytes.

Ethereum endpoints (ROADMAP P4a). Since 2026-10-19 19:33 UTC the committed window comes from
Onyx's own reth node (the project owner's decision, THREATS ETH-10): re-fetched block by block, the export's
commitment and every row equal the first fetch's, so no shipped number depends on a third party.
The first fetch (kept here as history) came from keyless public JSON-RPC endpoints, read-only calls only
(`eth_chainId`, `eth_getBlockByNumber`, `eth_getBlockReceipts`), User-Agent `onyx-dev`, at most 4
requests a second, 613 requests in all (603 to PublicNode, 10 to Flashbots), no key, no account:
- PublicNode (Allnodes Inc.), https://www.publicnode.com/terms (page SHA-256
  `b0f2bcab…9423` as fetched): grants "a non-exclusive, limited, non-transferable, freely
  revocable license to access and use the Service" and says it "allows you to connect to
  blockchains through RPC"; it also says "you will not engage in or use any data mining, robots,
  scraping, or similar data gathering or extraction methods" and not to "copy, distribute or
  create derivative works based on the Service or the Service Content". Our reading: a script
  calling the RPC the service offers, at a modest rate, is the use offered, and what Onyx keeps
  and ships is consensus data (checked against the chain's hashes, identical from any node), not
  PublicNode's content. That reading is the loop's, not a lawyer's: THREATS ETH-10 asks the project owner
  to accept it or to run Onyx's own node for production refreshes.
- Flashbots Protect RPC, https://docs.flashbots.net/policies/terms-of-service ("Updated July
  6, 2023", page last edited 2025-12-16, read 2026-10-19): a "single, revocable, non-exclusive, non-transferable, and limited
  license" to use the services; "excessive application program interface ("API") requests may
  result in the temporary or permanent suspension of Your access". Used for 10 calls only (chain id, finalized
  block, 8 block hashes).

## 4. Onyx's own data (Apache-2.0, same as the code)

Generated by Onyx code or from Onyx's own test keys and runs, so covered by [LICENSE](LICENSE):

- Test vectors: `fixtures/merkle`, `commit`, `claims`, `registry`, `revenue`, `settlement`,
  `api`, `signed`, `track`, `eip191`, `ops` (gas from local rehearsals), `fixtures/tlock/onyx-zero-tail.json`,
  `fixtures/tlock/vectors.json`, `fixtures/golden/btc-pool-blocks-*.json` (its pool names come from
  the MIT list in section 2, whose notice travels with them).
- Synthetic examples: `adapters/source-page/fixtures`, `adapters/source-signed/fixtures`,
  `apps/web/tests/fixtures`, `apps/web/static/level5/track-example.json` (invented values,
  marked as examples).
- `recipes/out/*.wasm` (built from this repository's recipes; they contain compiled
  dependencies listed in [THIRD-PARTY.md](THIRD-PARTY.md)).
- `apps/web/static/og.jpg` (rendered from our own home page by `apps/web/tests/og.mjs`; no company figure in it),
  `apps/web/static/run/indexer-*.json`, and run records apart from the fields in section 1.

## 5. Not in this repository

- `fixtures-live/` (our node's raw blocks, and the fetched Ethereum window `fixtures-live/eth`,
  405 MB of answers, 53 MB gzip): gitignored, never committed; the Ethereum window's answer hashes
  are committed (section 3) and `scripts/refresh-eth-onchain.py` fetches it again.
- Excluded on rights grounds: the wBTC reserve list (terms forbid redistribution), cbBTC lists
  (terms unread), Chainlink proof-of-reserve feeds (terms forbid redistribution), the upstream
  mining-pool file (names people). Older commits still hold some excluded rows (THREATS
  L8-H1..H3); that is one reason the repository stays private
  ([docs/ops/open-source-checklist.md](docs/ops/open-source-checklist.md)).

## 6. Issuer daily holdings pages: terms checked, nothing ingested (ROADMAP P2c, P2f)

Daily ETF flows would need each issuer's own daily holdings page. None is fetched or stored by
Onyx. Terms read 2026-10-19 (a fresh research agent, plain fetches, no bot-check bypass); "could
not read" means the page refused the fetch or rendered only by script, so a person must read it.

| Issuer (fund) | Terms page | Result | Clause (short quote) |
|---|---|---|---|
| BlackRock iShares (IBIT) | https://www.blackrock.com/corporate/compliance/terms-and-conditions | blocked | "Such content is solely for your personal, non-commercial use."; robots and automatic copying need BlackRock's permission |
| Fidelity (FBTC) | https://www.fidelity.com/terms-of-use | blocked | "You may use the Fidelity Websites only for personal, non-commercial purposes." |
| Grayscale (GBTC, BTC) | https://www.grayscale.com/terms-of-service | needs the project owner (could not read: HTTP 429 bot check; a search summary says reproduction and distribution are barred) | not read from the page |
| ARK (ARKB) | https://www.ark-funds.com/terms | blocked | bars "systematically, or by any automated means, extracting Content ... to populate databases or other sites" |
| 21Shares (ARKB) | https://www.21shares.com/en-us/terms-of-use | blocked | "You agree not to sell, distribute, publish, broadcast, circulate, or commercially exploit Content in any manner without the express written consent of 21Shares." |
| Bitwise (BITB; bitbetf.com has no terms of its own) | https://bitwiseinvestments.com/terms-of-service | blocked | may not "distribute, or otherwise use Bitwise Content in any way for any public or commercial purpose without Bitwise's prior written consent" |
| VanEck (HODL) | https://www.vaneck.com/us/en/legal/ | blocked | "No information contained on this site may be reproduced, transmitted, displayed, distributed, published or otherwise used for commercial purposes without the prior consent of VanEck." |
| CoinShares (BRRR) | none found (three likely URLs answered 404) | needs the project owner | no US terms text found |
| Franklin Templeton (EZBC) | https://www.franklintempleton.com/help/terms-of-use | needs the project owner (could not read: text loads by script) | not read |
| Invesco (BTCO) | https://www.invesco.com/us/en/resources/terms-of-use.html | blocked | may not "use any data mining, robots, or similar data gathering and extraction tools on the Website" |
| WisdomTree (BTCW) | https://www.wisdomtree.com/us/terms | needs the project owner (could not read: HTTP 403 bot check; a search summary says no copying without written consent) | not read from the page |
| Hashdex (DEFI) | none found (only a legal notice) | not applicable: news reports the fund closed in August 2026 (not checked on EDGAR) | none |
| Morgan Stanley (MSBT) | https://www.morganstanley.com/terms | blocked | "this web site is for your personal and non-commercial use only" |
| Osprey (OBTC) | https://ospreyfunds.io/terms-of-use/ | blocked | "Such content is solely for your personal, non-commercial use." |

Result: no issuer page may be ingested today. Daily flows stay out until an issuer grants
written permission or a licensed source is chosen (ROADMAP P2f, the project owner's call). Robots.txt was
not checked; it cannot loosen terms that already forbid the use.
