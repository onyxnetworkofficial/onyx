# Data-source catalogue (ROADMAP F1)

Every source Onyx uses today, every candidate researched on 2026-10-14, and the order the loop
builds them in. Rights first: no code for a source until its row here says OK. "Rights" means what
the source states, quoted and linked; it is not a legal opinion. Hard limits (CLAUDE.md): never
ingest data whose licence forbids redistribution; entities only, never individuals; no invented
numbers.

Decision words: **OK** (public domain, open licence, consensus chain data, or published for
public verification with nothing restrictive found), **OK with duties** (open, but conditions
travel with every served row), **The project owner** (needs his rights decision; listed in ROADMAP "Needs
The project owner"), **blocked** (terms forbid what Onyx does), **excluded** (decided, not in the tree).

Proof levels (docs/spec/proof-ladder.md): 1 recomputed from pinned public inputs; 2 re-observed by
bonded observers; 3 source-proven (signed); 4 claim anyone can check; 5 track record. A value is
only as strong as its weakest input: a balance can be level 1 while its attribution is level 3 or 4.

Value and effort are estimates for ranking, not measurements. Effort is in loops (one loop =
research, design, build, test, review, as in CLAUDE.md).

## 1. Current sources

| # | Source | Link | Licence / terms (quoted) | Decision | What Onyx derives | Proof level | Status |
|---|---|---|---|---|---|---|---|
| S1 | Bitcoin mainnet chain data, our own Bitcoin Core node (unpruned) | `scripts/fetch-btc-node.sh` (RPC `getblock`), RESEARCH.md "Live data sources" | Consensus data; no licence applies (`onyx:public-chain-data`) | OK | Entity flows (`btc-entity-flow-series`), balances, reserve shards, pool block counts; `fixtures-live/` (local only) | 1 for every chain-only value | Live; one node only (RES-1, D2 wants a second) |
| S2 | Bitcoin blocks 921,080-921,082 (first fixture) and block headers 971,200-971,207 (the reserve header chain, ADR-0037) via mempool.space Esplora, cross-checked on blockstream.info | fixtures/btc/README.md, `scripts/build-reserve-headers.py` | Consensus data. The API's own terms are not recorded: https://mempool.space/terms-of-service renders only by script and our fetch got no text (2026-10-14) | OK (the bytes are reproducible from S1) | Golden-test inputs | 1 | Frozen fixture |
| S3 | Label set v1 (`test.onyx.entities.btc.v1`, 120 rows: OKX 100, Bitwise BITB 20) | fixtures/labels/PROVENANCE.md | Sources publish the addresses for public verification and state no licence | **The project owner** (PLAN question 8, THREATS L8-5) | Entity attribution for v1 recipes and the demo | 3 (OKX, signed PoR) / 4 (BITB, issuer list) | Test fixture (`test.*`, never served as labels) |
| S4 | Label set v2 (9,850 rows: OKX 9,443, BitMEX 209, BITB 117, Binance 81), built from the entity indexer files | fixtures/labels/PROVENANCE.md | As S3, per source (S5-S8) | **The project owner** (L8-5) | Attribution for flows, history, reserves | 3 (OKX) / 4 (others) | Test fixture; preview ships rows, both builds ship entity totals only |
| S5 | OKX proof-of-reserves files (signed address lists) | https://www.okx.com/proof-of-reserves, list API `https://www.okx.com/v2/asset/audit/list` | PoR page: no licence. OKX Terms of Service (https://www.okx.com/help/terms-of-service, "Last Updated: 17 September 2026"), 8.1: "You agree that you will not copy, transmit, distribute, sell, license, reverse engineer, modify, publish, or participate in the transfer or sale of, create derivative works from, or in any other way, exploit any of our products and Services." Whether a PoR file published for verification is one of the "products and Services" is not stated | **The project owner** (L8-5; the 8.1 clause is new evidence for that decision) | v2's OKX rows (signature-checked); cross-check of the reserve base: 5,956/5,956 addresses exact at height 965,949 | 3 for attribution; 1 for balances given the base | In v2; reserve row served at level 4 |
| S6 | Bitwise BITB issuer wallet list | https://bitbetf.com/ (wallets in the page's own data, `wallets.walletBalances`) | No licence on the page. New 2026-10-14: the page's Proof of Reserves text ends "View Terms of Use", linking https://www.thenetworkfirm.com/terms-of-use (the examiner's terms), whose s.11 says "you may not modify, edit, copy, distribute, reproduce, publish, display, perform, license, sell, rent, lease, loan, create derivative works of, create an index, reverse engineer, alter, enhance, provide access to or in any way exploit the Services or Content in any manner", and s.1 defines "The software and reporting are collectively referred to in these Terms as the “Services”." Whether the wallet list is the examiner's "reporting" or Bitwise's own page data is not stated | **The project owner** (L8-5; the examiner-terms link is new evidence) | v2's BITB rows; cross-check 66/66 wallets exact | 4 attribution; 1 balances | In v2; reserve row served at level 4 |
| S7 | BitMEX PoRL bucket (address lists) | https://s3-eu-west-1.amazonaws.com/public.bitmex.com/data/porl | No licence on the bucket. The verifier code (https://github.com/BitMEX/proof-of-reserves-liabilities) is MPL-2.0; that licence covers the code, not the address data | **The project owner** (L8-5) | v2's BitMEX rows | 4 | In v2; reserve row refused until a second-node re-scan (RES-1, D2) |
| S8 | Binance proof-of-reserve wallet files | https://public.bnbstatic.com/static/proof-of-reserve/ | No licence in the files. Binance's site terms were not readable by fetch (2026-10-14) | **The project owner** (L8-5) | v2's Binance rows | 4 | In v2; reserve row refused (RES-1, D2) |
| S9 | Mining-pool list | https://github.com/bitcoin-data/mining-pools (branch `generated`, `pool-list.json`) | MIT: "The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software." | OK (notice ships, L8-F2; 22 ids excluded) | `btc.pool-blocks` allowlist (127 pools) | 1 for "block N's coinbase pays/tags X"; shares served at 4 (ADR-0032 Am. 1) | Live on `#/data` |
| S10 | US Treasury Fiscal Data, Debt to the Penny | https://fiscaldata.treasury.gov/ (`v2/accounting/od/debt_to_penny`) | "The data on this site is available to copy, adapt, redistribute, or otherwise use for non-commercial and commercial purposes." (https://fiscaldata.treasury.gov/about-us/) | OK | Level-2 feed (`adapters/source-fiscaldata`) | 2 | Live (preview values; public build sealed) |
| S11 | SEC EDGAR XBRL `companyconcept` (Strategy, Riot; 7 spot bitcoin ETFs since 2026-10-19, ROADMAP P2c) | https://data.sec.gov/api/xbrl/companyconcept/ | "Information presented on sec.gov is considered public information and may be copied or further distributed by users of the web site without the SEC's permission." (https://www.sec.gov/privacy). Fair access: "Current max request rate: 10 requests/second." and "Please declare your user agent in request headers" (https://www.sec.gov/search-filings/edgar-search-assistance/accessing-edgar-data); Onyx's User-Agent carries no e-mail (SEC-4) | OK | Level-2 "filing says N BTC" per company (`adapters/source-edgar`) | 2 (never a statement about addresses) | Live (2 companies, 7 ETFs) |
| S12 | Base Sepolia chain state (our contracts, testnet USDC `0x036C…CF7e`) | https://sepolia.base.org | Public testnet chain data; our own transactions | OK | Run pages, indexer (`Finalized` events), settlement proofs | 1 (any node re-reads the events) | Live (runs 2026-10-12, 2026-10-13) |
| S13 | Chainlink cbBTC Proof of Reserve feed | https://data.chain.link/feeds/base/base/cbbtc-por | Chainlink Foundation Terms v6.0 s.3: you may not "resell, lease, lend, share, distribute or otherwise permit any third party to use" the Services (https://chain.link/terms; RESEARCH.md) | **blocked** (POR-1) | Nothing | Would be 1 for "feed answered A", not proof of reserves | Not built |
| S14 | wBTC reserve address list | wbtc.network | Terms forbid reproducing or distributing its materials and automated access (IDX-R1-F1) | **excluded** | Nothing | n/a | Not in the tree (history: L8-H2) |
| S15 | cbBTC reserve address list (Coinbase) | Coinbase proof-of-reserves page | Unread: the page refuses our fetches (IDX-6) | **The project owner** (IDX-R1-F2, D4) | Nothing | Would be 4 attribution, 1 balances | Held back |
| S16 | Label set v3 (`test.onyx.entities.btc.v3`, dated, 18,444 memberships of 11 companies), built from the issuers' own roster snapshots (S5-S8, S17-S23) by `scripts/build-rosters.py` (ROADMAP P2a, ADR-0037 Amendment 1) | fixtures/labels/PROVENANCE.md v3, `fixtures/rosters/` | As S3, per source | **The project owner** (L8-5) | Attribution for the roster flows and balances (`history/rosters-week.json`) | 3 (OKX, HTX, CoinEx signed rows) / 4 (the rest) | Test fixture; private preview only; refused in a fallback build |
| S17 | Bybit monthly proof-of-reserves audit PDFs (`bybit.por`) | https://www.bybit.com/common-static/cht-static/por/ | None stated in the reports; site terms unread (N13) | **The project owner** (L8-5) | v3's Bybit rows (17 snapshots) | 4 | In v3 |
| S18 | Bitfinex wallet file (`bitfinex.wallets`) | https://github.com/bitfinexcom/pub (wallets.txt) | None stated in the file; repository licence unread (N15) | **The project owner** (L8-5) | v3's Bitfinex rows | 4 | In v3 |
| S19 | HTX tagged reserve CSVs (`htx.por`), signed | https://github.com/huobiapi/Tool-Node.js-VerifyAddress | None stated in the files; repository licence unread | **The project owner** (L8-5) | v3's HTX rows (127 signed over "huobi") | 3 signed / 4 | In v3 |
| S20 | CoinEx wallet workbooks (`coinex.por`), partly signed | https://www.coinex.com/en/reserve-proof (files on file.coinexstatic.com) | None stated; site terms unread | **The project owner** (L8-5) | v3's CoinEx rows (100 signature-verified) | 3 signed / 4 | In v3 |
| S21 | MEXC wallet CSVs (`mexc.por`) | https://por.mocortech.com/ (linked from MEXC's proof-of-reserves page) | None stated; site terms unread | **The project owner** (L8-5) | v3's MEXC rows (2026 snapshots) | 4 | In v3 |
| S22 | Crypto.com 2022 wallet disclosure (`cryptocom.wallets`) | https://crypto.com/en/company-news/transparency-first | None stated on the page; site terms unread | **The project owner** (L8-5) | v3's 6 Crypto.com rows (support 62 days in 2022 only) | 4 | In v3 |
| S23 | Deribit proof-of-reserves article (`deribit.por`) | https://support.deribit.com/hc/en-us/articles/31211285163165-Proof-Of-Reserves | None stated on the article; site terms unread (N14) | **The project owner** (L8-5) | v3's Deribit rows | 4 | In v3 |
| S24 | USGS ANSS Comprehensive Earthquake Catalog (ComCat), FDSN event `count` (NS3) NL17 (2026-10-27): also a regional count, M2.5+ within 300 km of Los Angeles (`usgs.earthquakes.la300km_m2_5_count`), the same service and terms. | https://earthquake.usgs.gov/fdsnws/event/1/ | "USGS-authored or produced data and information are considered to be in the U.S. Public Domain." and "we ask that proper credit be given" (https://www.usgs.gov/information-policies-and-instructions/copyrights-and-credits, read 2026-10-26; the page refuses curl, read in a browser fetch). ComCat merges events from contributing networks; Onyx keeps only the day's count, a fact, never their event records | OK (credit line shown) | Level-2 daily count of M5.0+ earthquakes (event type `earthquake`) worldwide (`adapters/source-usgs`), read 100 to 120 days after the day | 2 | Built and rehearsed on a recording (2026-10-26); served by the hosted API and the site since release `2f01280` (2026-10-28): world M5+ and the Los Angeles M2.5+ count, both level 2 from Base Sepolia rounds |
| S25 | NOAA NCEI GHCN-Daily via the Access Data Service, station `USW00003017` (Denver International Airport), TMAX (NS3) NL17 (2026-10-27): also `PRCP` at `USW00003017` and `TMAX` and `PRCP` at `USW00094728` (New York Central Park), both NWS stations under the same terms. | https://www.ncei.noaa.gov/access/services/data/v1 | No NCEI licence; a U.S. Government work ("Copyright protection under this title is not available for any work of the United States Government", 17 U.S.C. 105(a)); NWS values "are in the public domain, unless specifically noted otherwise" with three conditions (https://www.weather.gov/disclaimer, read 2026-10-26); NCEI asks a citation (GHCN-Daily "Cite as") | OK with duties (cite GHCN-Daily; no endorsement; not presented as official) | Level-2 daily maximum temperature (`adapters/source-ncei`), read 30 to 60 days after the day | 2 | Built and rehearsed on a recording and live (2026-10-26); served by the hosted API and the site since release `2f01280` (2026-10-28): Denver TMAX and PRCP, Central Park TMAX and PRCP, all level 2 from Base Sepolia rounds |
| S26 | U.S. National Weather Service API, gridpoint forecast `BOU/75,66` (`maxTemperature`, the cell of station `USW00003017`), north-star loop NL4 | https://api.weather.gov/gridpoints/BOU/75,66 | NWS information "is in the public domain, unless specifically noted otherwise" with the same three conditions as S25 (https://www.weather.gov/disclaimer, read 2026-10-28); the API needs no key and asks for a User-Agent (https://www.weather.gov/documentation/services-web-api, read 2026-10-28; Onyx sends `onyx-dev`, no contact details) | OK with duties (the NWS named; no endorsement; not presented as official), testnet only | Track-record statements: each coming day's forecast high, relayed by an Onyx test key (docs/ops/nws-relay.md), graded by S25 | 5 (a track record; never a proof level) | Live on Base Sepolia through the intake (2026-10-28); served after the next deploy |

## 2. Candidate sources

| # | Source | Link | Licence / terms (quoted) | Decision | What Onyx would derive | Proof level | Value | Effort |
|---|---|---|---|---|---|---|---|---|
| N1 | Bitcoin network metrics without labels: fees per block, subsidy, miner revenue, tx count, size and weight, difficulty | Our node (S1) | Consensus data | OK | Per-block and per-day series | 1 | High: widely used, no attribution risk, reuses the block inputs and recipe pattern of `btc-pool-blocks` | 1-2 loops |
| N2 | Stablecoin supply on Ethereum and Base: USDC, USDT, DAI, PYUSD (`totalSupply`, mint/burn `Transfer` events from/to `0x0`) | USDC Ethereum `0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48`, Base `0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913` (https://developers.circle.com/stablecoins/usdc-contract-addresses); USDT Ethereum `0xdac17f958d2ee523a2206206994597c13d831ec7` (https://tether.to/en/supported-protocols/; Base is not on Tether's list, so no native USDT there); DAI and PYUSD: addresses to be confirmed from the issuers' own pages before build | Consensus chain state; no licence applies | OK | Supply per token per chain per day; net mint/burn | 1 if inputs are pinned logs with receipt proofs or `eth_getProof` storage proofs against block hashes; 2 if only observed through RPC | High (estimate): supply is the headline stablecoin figure; first non-Bitcoin chain | 3-4 loops (EVM input format, ADR, recipe); needs Ethereum and Base mainnet access (own node or an RPC; a paid RPC is the project owner's call) |
| N3 | US Treasury Fiscal Data beyond Debt to the Penny: Daily Treasury Statement operating cash (`v1/accounting/dts/...`), Average Interest Rates (`v2/accounting/od/avg_interest_rates`), Rates of Exchange (`v1/accounting/od/rates_of_exchange`), Monthly Treasury Statement (`v1/accounting/mts/mts_table_1`) | https://fiscaldata.treasury.gov/api-documentation/ ("Our API is open, meaning that it does not require a user account or registration for a token.") | As S10: "available to copy, adapt, redistribute, or otherwise use for non-commercial and commercial purposes" | OK | More level-2 feeds on the existing adapter | 2 | Medium: macro context buyers know; low risk | 1 loop |
| N4 | SEC EDGAR beyond 2 companies: `companyfacts`, `frames`, `submissions` for more bitcoin treasury companies and spot ETF trusts | https://www.sec.gov/search-filings/edgar-application-programming-interfaces ("These APIs do not require any authentication or API keys to access.") | As S11 | OK (no e-mail in the User-Agent) | Level-2 "filing says N" per company; register only where the tag is a whole-holdings total (SEC-R1-F2) | 2 | High: holdings of treasuries and ETFs next to the reserve metric | 1-2 loops |
| N5 | SEC Form 13F data sets (quarterly institutional holdings) | https://www.sec.gov/data-research/sec-markets-data/form-13f-data-sets ("The data sets will be updated quarterly") | As S11 | OK, aggregates only: some 13F filers may be natural persons, so serve per-security totals (e.g. all reported holders of a spot BTC ETF), never per-filer rows | Quarterly reported institutional holdings per ETF | 2 for "the data set says N" | Medium | 2 loops (bulk ZIP parse, aggregation recipe) |
| N6 | Bitcoin UTXO age bands (supply by coin age) | Our node: a pinned UTXO-set snapshot (`dumptxoutset`) whose hash anyone can compare (`gettxoutsetinfo`) | Consensus data | OK | Supply per age band per day | 1 given the pinned snapshot | High (a standard on-chain metric) | 3 loops; heavy inputs (full UTXO set), needs a memory-capped run plan |
| N7 | Federal Reserve Board statistical releases (e.g. H.15 rates, H.10 exchange rates) | https://www.federalreserve.gov/datadownload/ | "Unless otherwise indicated, information on Board's website is in the public domain and may be copied and distributed without permission. Please cite to the Board as the source of the information." (https://www.federalreserve.gov/disclaimer.htm); third-party material excepted | OK (Board-owned series only) | Level-2 rate feeds | 2 | Medium | 1 loop (template of S10) |
| N8 | BLS public data (e.g. CPI) | https://www.bls.gov/developers/ (v1.0 "does not require registration and is open for public use") | "is in the public domain, except for previously copyrighted photographs and illustrations"; "You are free to use our public domain material without specific permission"; "we do ask that you cite the Bureau of Labor Statistics as the source." (https://www.bls.gov/opub/copyright-information.htm). API terms: applications must state "BLS.gov cannot vouch for the data or analyses derived from these data after the data have been retrieved from BLS.gov." (https://www.bls.gov/developers/termsOfService.htm) | OK with duties (cite BLS, access date, the disclaimer) | Level-2 monthly feeds | 2 | Medium | 1 loop |
| N9 | BEA (GDP, PCE) | https://apps.bea.gov/API/signup/ | Website: "Unless stated otherwise, the information posted on the BEA web site is in the public domain and may be used or reproduced without specific permission." (https://www.bea.gov/taxonomy/term/611?page=2). The API needs "your name (or organization name), a valid email address, and agree to the published terms of service"; the terms are a PDF we did not read | **The project owner** (signing up for a service is his call; API terms unread) | Level-2 quarterly feeds | 2 | Low-medium | 1 loop after signup |
| N10 | FRED | https://fred.stlouisfed.org/legal/ | Prohibited: "Redistribute any third party's proprietary content, including any graphs, maps, images, logos, data, or datasets, ... for commercial use without first obtaining express written permission from the data provider." The API terms (same page) also forbid storing, caching or archiving FRED content and need a registered key (our summary, not a quote) | **blocked** for redistribution; use the primary public-domain sources instead (N7, N8, N3) | Nothing | n/a | n/a | n/a |
| N11 | Ethereum staking: deposit-contract deposits; validator count and total effective balance | Deposit contract `0x00000000219ab540356cBB839Cbe05303d7705Fa` (https://ethereum.org/en/staking/deposit-contract/); beacon state via the standard beacon API | Consensus data | OK (aggregates; never per-validator owner labels) | ETH deposited per day (execution logs); active validators and stake (beacon state) | 1 (deposits from pinned logs; validator totals from a pinned beacon state) | Medium | Deposits 2 loops after N2's EVM inputs; beacon totals 3+ loops (needs a beacon node) |
| N12 | Base / OP-stack L2 metrics: tx count, gas used, base fee, L1 data fee, batch posting to L1 | https://mainnet.base.org (listed in https://docs.base.org/base-chain/quickstart/connecting-to-base) | Consensus chain data | OK | Per-block and per-day series for Base | 1 with pinned blocks | Medium (Onyx already settles on Base) | 2 loops after N2's EVM inputs |
| N13 | More exchange PoR address lists: Bybit | https://www.bybit.com/en/help-center/article/How-to-Verify-Bybit-Ownership-of-Wallet-Addresses-and-Their-Balances (addresses under "Download Proof of Funds" on the PoR page; a "Send to Self" ownership method, as the help article reads in search results; the page timed out for our fetch) | Bybit's terms not read (PDF and pages not readable by fetch, 2026-10-14) | **The project owner** | Reserve row (balance recipe exists) | 4 attribution (3 if the send-to-self transactions are pinned as proofs), 1 balances | Medium | 1 loop after clearance |
| N14 | More exchange PoR address lists: Deribit | https://support.deribit.com/hc/en-us/articles/31211285163165-Proof-Of-Reserves (answered 403 to our fetch) | Terms not read | **The project owner** | Reserve row | 4 / 1 | Low-medium | 1 loop after clearance |
| N15 | More exchange PoR address lists: Bitfinex | Lists were published in 2017 and 2022 per press (https://bitcoinmagazine.com/business/bitfinex-challenges-insolvency-rumors-provides-proof-wallet-reserves); no current official list found | Not found | **The project owner** (and a current first-party list must be found first) | Reserve row | 4 / 1 | Low until a current list exists | 1 loop after clearance |
| N16 | ETF issuer wallet lists beyond BITB: Bitwise ETHW (Ether) | https://ethwetf.com/ shows balances only today; ENS subnames were announced in 2024 (https://www.theblock.co/post/306931/bitwise-discloses-ens-addresses-for-spot-ethereum-etf-holdings) | Same examiner "Terms of Use" link as S6 | **The project owner** (same question as S6); other spot BTC ETFs publish no addresses (indexer `etf_custody.csv` header; sleuth-found addresses are third-party attribution, excluded) | ETH reserve row | 4 / 1 | Medium (first non-BTC reserve) | 2 loops after N2 |
| N17 | ECB euro reference rates | https://www.ecb.europa.eu/services/disclaimer/html/index.en.html | Free use "subject to the following conditions": cite the ECB, state modifications, and when included in documents that are sold, tell buyers the information is free on the ECB site (conditions 1-3, paraphrased by our fetch tool; RESEARCH.md loop 62) | **The project owner** (a sold row must carry the "free at the ECB" notice) | Level-2 FX feed | 2 | Low (N3 and N7 cover USD FX) | 1 loop |
| N18 | Lightning public graph, aggregates only (channel count, announced capacity) | BOLT 7 (https://github.com/lightning/bolts/blob/master/07-routing-gossip.md): a channel announcement's proof starts with "a node finding the output given by the `short_channel_id`" | Protocol gossip, no licence; node aliases are free text that can name people ("customize its appearance in maps and graphs") | OK for aggregates only; never aliases or per-node rows | Announced capacity whose funding outputs are unspent on chain | 1 for "these announced outputs hold N" given a pinned gossip snapshot; completeness of the snapshot is a level-4 claim | Low-medium | 3 loops; needs a Lightning node (parked) |
| N19 | USGS earthquake counts (FDSN event `count`) | https://earthquake.usgs.gov/fdsnws/event/1/ | As S24 | OK | Daily counts by magnitude band | 2 after a 100-day lag (revisions measured, NS3 research 2026-10-26) | Medium: the first non-finance dataset | Built as S24 |
| N20 | NOAA NCEI GHCN-Daily via the Access Data Service (station daily summaries, e.g. TMAX at Denver International Airport `USW00003017`) | https://www.ncei.noaa.gov/access/services/data/v1 | NWS station values: "in the public domain, unless specifically noted otherwise" (https://www.weather.gov/disclaimer); the GHCN-Daily record asks a citation ("Cite as") and states no licence (https://www.ncei.noaa.gov/access/metadata/landing-page/bin/iso?id=gov.noaa.ncdc:C00861) | OK with duties (cite NCEI and the station) | Level-2 daily station values | 2 after a 30-day lag (values changed 5-25 days later in one diff file) | Medium | Built as S25 |

## 3. Ranked shortlist (what the loop builds next)

| Rank | ROADMAP | Source | Why | Proof level | First concrete item |
|---|---|---|---|---|---|
| 1 | F2 | N1 Bitcoin network metrics | No rights question, own node, the block inputs and recipe pattern already exist; level 1 end to end | 1 | Recipe `btc-block-stats` (fees, subsidy, tx count, weight per block) over `fixtures-live/week`, golden test, `#/data` panel |
| 2 | F3 | N2 Stablecoin supply (USDC, USDT first) | Highest buyer value and Onyx's first EVM source; clean rights | 1 (pinned logs or storage proofs), 2 if observed only | Spec + ADR for EVM inputs (logs with receipt proofs vs `eth_getProof`), then USDC `totalSupply` on Base for one day |
| 3 | F4 | N4 SEC EDGAR more companies and ETF trusts | Adapter exists; holdings sit next to the reserve metric | 2 | Probe `companyfacts` for spot BTC ETF trusts and more treasuries; register those whose tag is a whole-holdings total |
| 4 | F5 | N3 Treasury Fiscal Data more datasets | Licence explicit, adapter exists, one loop | 2 | Daily Treasury Statement operating cash balance as a second fiscaldata feed |
| 5 | F6 | N7 + N8 Fed Board and BLS public-domain series | Public domain, simple feeds; replaces FRED (blocked) | 2 | One H.15 rate series and CPI as level-2 feeds, with BLS's required citation and disclaimer |

Next after the five: F7 more reserve lists (N13-N16, rights first, all the project owner today), F8 UTXO
age bands (N6), F9 Ethereum staking (N11), F10 Base L2 metrics (N12), F11 longer Bitcoin history,
F12 13F aggregates (N5), F13 Lightning aggregates (N18, parked). N9 (BEA) and N17 (ECB) wait on
The project owner; N10 (FRED) is blocked.

## 4. Needs the project owner (rights decisions)

| Source | Question |
|---|---|
| S3-S8 label-set sources (OKX, BITB, BitMEX, Binance) | PLAN question 8 / L8-5, with two new facts: OKX ToS 8.1 forbids distributing "any of our products and Services"; bitbetf.com links its examiner's terms that forbid redistributing "the Services or Content" |
| S15 cbBTC | Coinbase's PoR terms (D4) |
| S13 Chainlink PoR | Written permission or keep blocked (POR-1) |
| N13 Bybit, N14 Deribit, N15 Bitfinex | Read and decide each exchange's terms before any row enters |
| N16 Bitwise ETHW | Same question as BITB |
| N9 BEA | Sign up for an API key (name, e-mail, terms PDF) or skip |
| N17 ECB | Accept the "free at the ECB" notice on sold rows, or skip |
| N2 and N11/N12 node access | Own Ethereum/Base nodes or an RPC provider; anything paid is his call |

## 5. Release step: served sources' terms (LC-27, THREATS L2-SRC-4)

Before every release (and before LC-27 is ticked), run:

```sh
python3 -I scripts/check-source-terms.py
```

It prints every served source below with its terms link, last-read date and age, and exits 1
when one is unread, not recorded, or older than the threshold. Re-read threshold: 90 days (no
earlier document set one; decision log in docs/TRACKER.md, 2026-10-16). `--lint` checks only the
table's shape and runs in `scripts/check-docs.sh`, with `--self-test`; the dated run is a release
step, not part of check-all, so an old read never turns an unrelated commit red.

To clear a failing row: open the link in a browser, compare the text with the quote in section 1,
update the quote and the decision there if anything changed, and set **Last read** to today. If the
new terms forbid what Onyx does with the source, stop serving its shards first
([runbooks/stop-serving-shard.md](runbooks/stop-serving-shard.md)) and add the source to section 4
for the project owner. A page our fetch cannot read needs a human read; record what was read, never a guess.

Every section 1 id has a row (the script checks it). **Served** is `yes` when rows or totals derived
from the source reach the API or either website build. **Terms read at** is a URL, `none: consensus
data` (nothing to re-read), `via S5 S6` (covered by those rows, which must be read directly: a
`via` row never covers another), or `not recorded` (always fails).

| # | Served | Terms read at | Last read | Covers |
|---|---|---|---|---|
| S1 | yes | none: consensus data | n/a | Our own Bitcoin node's blocks |
| S2 | yes | none: consensus data | n/a | Fixture blocks on the Run pages and 8 headers of the reserve header chain; the bytes are reproducible from S1, so mempool.space's API terms (unread, section 1) do not govern them |
| S3 | yes | via S5 S6 | n/a | Label set v1 behind the demo shard's entity totals |
| S4 | yes | via S5 S6 S7 S8 | n/a | Label set v2 behind both builds' entity totals |
| S5 | yes | `https://www.okx.com/help/terms-of-service` | 2026-10-14 | OKX ToS 8.1 (quoted in section 1) |
| S6 | yes | `https://www.thenetworkfirm.com/terms-of-use` | 2026-10-14 | The examiner terms bitbetf.com links (s.1, s.11) |
| S7 | yes | `https://s3-eu-west-1.amazonaws.com/public.bitmex.com/data/porl` | 2026-10-14 | No licence on the bucket |
| S8 | yes | `https://public.bnbstatic.com/static/proof-of-reserve/` | 2026-10-14 | No licence in the files |
| S8 | yes | not recorded | unread | Binance's site terms: not readable by fetch on 2026-10-14; a human read is needed |
| S9 | yes | `https://github.com/bitcoin-data/mining-pools` | 2026-10-14 | MIT notice (ships with the allowlist) |
| S10 | yes | `https://fiscaldata.treasury.gov/about-us/` | 2026-10-14 | Free reuse statement |
| S11 | yes | `https://www.sec.gov/privacy` | 2026-10-14 | Public information statement |
| S11 | yes | `https://www.sec.gov/search-filings/edgar-search-assistance/accessing-edgar-data` | 2026-10-14 | Fair access: 10 requests/second, declared User-Agent |
| S12 | yes | none: consensus data | n/a | Base Sepolia chain state |
| S13 | no | `https://chain.link/terms` | 2026-10-14 | Blocked (POR-1) |
| S14 | no | not recorded | unread | Excluded |
| S15 | no | not recorded | unread | Held back (page refuses our fetch) |
| S16 | yes | via S5 S6 S7 S8 | n/a | Label set v3 behind the preview's roster totals (also via S17-S23, below) |
| S17 | yes | not recorded | unread | Bybit's site terms: a human read is needed |
| S18 | yes | not recorded | unread | The bitfinexcom/pub repository licence |
| S19 | yes | not recorded | unread | The huobiapi repository licence |
| S20 | yes | not recorded | unread | CoinEx's site terms |
| S21 | yes | not recorded | unread | MEXC's site terms |
| S22 | yes | not recorded | unread | Crypto.com's site terms |
| S23 | yes | not recorded | unread | Deribit's site terms |
| S24 | yes | `https://www.usgs.gov/information-policies-and-instructions/copyrights-and-credits` | 2026-10-26 | Public-domain statement and credit request (NS3, NL17; served since release `2f01280`, 2026-10-28) |
| S25 | yes | `https://www.weather.gov/disclaimer` | 2026-10-26 | NWS public-domain statement and its three conditions (NS3, NL17; served since release `2f01280`, 2026-10-28) |
| S26 | no | `https://www.weather.gov/disclaimer` | 2026-10-28 | NWS public-domain statement and the API's User-Agent request (NL4; served after the next deploy) |

Dates are the catalogue's F1 read (2026-10-14, launch checklist LC-27); none was re-read for this
table.

## Scope, assumptions, open questions

- Scope: sources only. Each build still needs its own spec or ADR and review.
- Submissions (ADR-0038): the hosts a server may pull to check a submission
  (`onyx_intake::CLEARED_HOSTS`, today `api.fiscaldata.treasury.gov`, S10/N3) and the hosts that
  hold any submission naming them (`onyx_intake::BLOCKED_HOSTS`: the whole of stlouisfed.org, FRED
  and ALFRED among it, N10) come from this file's
  rows; a new pull host needs an OK row first.
- Assumption: chain state (Bitcoin, Ethereum, Base) carries no licence; this is the repository's
  existing `onyx:public-chain-data` rule (docs/ops/legal-data-rights.md).
- Open: the pages that refused our fetch (mempool.space terms, Bybit, Deribit, Binance terms,
  Coinbase) need a human read or a headless browser before their rows can move.
