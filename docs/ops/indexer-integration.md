# The entity indexer integration plan (ADR-0031)

Phase 1 is done as a bridge, not a new set: the converter (`scripts/build-label-set-v3.py`)
imports only cleared indexer sources, and none beyond v2 is cleared yet. The wBTC reserve list is
excluded (wbtc.network's terms forbid redistribution and automated access) and the cbBTC list is
blocked on a rights review, so the site, the history and the API keep label set v2; every v2
company is queryable at `/v1/metrics` with level 4. This file plans what else in the indexer could
enter Onyx. Rows 2, 4, 5 and 6 are built (status notes below the table); rows 1 and 3 are
not.

Each candidate must enter with a proof level and clean redistribution rights (CLAUDE.md hard
limits). "Rights" below means what the source states, not a legal opinion.

| # | Indexer output | What Onyx would get | Proof level | Rights | Feasibility |
|---|---|---|---|---|---|
| 1 | cbBTC declared reserve totals (the indexer's cbBTC Chainlink PoR history, 1,183 rounds) | A daily "declared reserve" fact per product | Would be 1 for "the aggregator reported answer A at round R" (an oracle attestation, not proof that reserves exist) | **BLOCKED 2026-10-11 (POR-1):** Chainlink Foundation Terms v6.0 (Sections 1-3) define data feeds as Services and forbid distributing them to third parties or commercialising them; the answer is also Coinbase's self-attested figure (rights unreviewed) | Technically good (EVM log or getRoundData reads), but not to be built until a written permission or an openly licensed on-chain attestation exists (RESEARCH.md) |
| 2 | ETF on-chain balances (`out/etf-official-parity*.json`, BITB 117 addresses) | Per-fund on-chain balance series for self-custodied or address-publishing funds | 1 for the balance of the published addresses (rerun over blocks); the attribution itself stays a level-4 claim | Issuer-published lists, no license | Medium: needs a balance recipe (cumulative UTXO state per address set), not a flow recipe; an incremental-state design (base balance + flows, ADR-0013 style) |
| 3 | Entity balance history (`out/entity-balance-history.csv`) over cluster-expanded entities | Long history of exchange balances | Cluster expansion is weak (24.3% unresolved false merges): at most an `Unstaked` claim, and only with a per-cluster review that no member is a person | Indexer's own work; the expansion's privacy risk is the blocker, not the license | Low until per-cluster grading (indexer `docs/PER-CLUSTER-GRADING.md`) proves company-only membership |
| 4 | Treasury and ETF holdings from SEC filings (MSTR, XXI, CLSK; 8 ETFs via XBRL) | "Company says it holds N BTC" facts | Level 2 (re-observed from EDGAR by several observers) for "the filing says N"; never a statement about addresses | SEC EDGAR filings are public records; check EDGAR's access policy (fair-access rate limits) | Good: same shape as the Treasury Debt-to-the-Penny feed (`adapters/source-fiscaldata`), a dated feed template per company |
| 5 | Mining-pool coinbase tags (`mining_pool_payouts.csv`, 37 entities, `chain_attested`) | Pool hashrate share by coinbase tag | 1 for "block N's coinbase says X" (the tag itself is on chain); NOT an address label (LBL-V2-F1) | On-chain data | Good as a block-level metric, not a label set |
| 6 | OKX cluster-level balance (`EVIDENCE.md`: 7/7 PoR snapshots within 0.2%) | Exchange reserve vs declared PoR | 3 for the signed address subset (already in v2); the cluster part stays a claim | OKX publishes its PoR files for verification, no license | Medium: needs the balance recipe of row 2 |

Status of row 4 (merged after reviews SEC-R1 and SEC-R2; on `#/data` and the query API): built as `adapters/source-edgar`, a level-2
period feed per company (ADR-0027 amendment 5). Registered: Strategy Inc (unit `Bitcoin`) and
Riot Platforms (unit `item`), recorded for period 2026-03-31. The value is the company-wide
crypto-asset unit total the filing tags, which for these two matches their own public bitcoin
totals (cross-check in `adapters/source-edgar/fixtures/PROVENANCE.md`). Not registered: XXI (no CIK
checked yet), CleanSpark (its tag is the bitcoin posted as collateral or receivable, a subset of
its holdings), ETFs (not yet probed). Only a value consumes a period (SEC-R1-F1).
Live observers need an operator contact in the User-Agent (THREATS SEC-4).

Status of row 5 (merged after reviews POOL-R1 and POOL-R2): built as the block metric
`btc.pool-blocks` (ADR-0032; shares floored at level 4 by amendment 1, AUD-2-F6), not from `mining_pool_payouts.csv` and not as labels. The recipe
`btc-pool-blocks` counts blocks per pool by the coinbase's largest-value payout script, else coinbase tag, against a
reviewed allowlist of pool companies built from bitcoin-data/mining-pools (MIT; `fixtures/pools/`); other
blocks are `unknown`, and coinbase text is never output. Website: "Who mined the blocks" on
`#/data` (one week, per-day roots checked in the browser). API: `/v1/metrics/btc.pool-blocks`.

Status of rows 2 and 6 (branch `balance-recipe`, ADR-0034; review RES-R1 fixed): built as the recipe
`btc-entity-reserve`, a chain of balance shards over label set v2 (which already holds OKX's signed
addresses, Bitwise BITB's 117 issuer-listed wallets, and Binance's and BitMEX's published lists).
The base comes from a UTXO scan of all 9,850 labels at block 971,199
(`fixtures/reserves/scan-971199.json`) rolled back to block 969,838 by the flows in between
(`fixtures/reserves/base-969838.json`); the chain over the week and on to the scan lands on the
scan exactly (an identity: it checks the arithmetic, not the scan). Balance: level 1 given the
base; the base is one node's scan, cross-checked independently only for OKX (PoR file) and BITB
(issuer list); attribution: level 3 (OKX) or 4 (published lists). Served as `btc.entity-reserve`
(branch `reserve-api`, ADR-0034 Amendment 1) behind a serving check that derives each shard's
params from the committed scan and the finalized rows (RES-7), at level 4; only OKX's and BITB's
rows are served (their bases are cross-checked independently), Binance's and BitMEX's are refused
until a re-scan on a second node reconciles (RES-1).
Website: `#/reserves` (both builds; entity totals only, no label rows).

Live checks, recorded 2026-10-13 with `scripts/reserve-live-check.py` (nothing tuned):

| Published figure | Theirs (BTC) | Onyx (BTC) | Difference |
|---|---|---|---|
| OKX PoR file `por_csv_2026090800_V1.zip` (via [okx.com/proof-of-reserves](https://www.okx.com/proof-of-reserves), list API `https://www.okx.com/v2/asset/audit/list`), snapshot height 965,949, all 5,956 BTC addresses | 146,358.26346743 | 146,358.26346743 | 0; 5,956 of 5,956 addresses exact |
| Same file, the 3,558 addresses also in label set v2 (OKX's signed subset Onyx holds) | 140,974.44061385 | 140,974.44061385 | 0 |
| Bitwise BITB wallet list on [bitbetf.com](https://bitbetf.com/) (`updatedAt` 2026-10-13T06:01:11Z), 66 wallets, vs our scan at block 971,199 (05:45 UTC) | 37,290.75352194 | 37,290.75352194 | 0; 66 of 66 wallets exact |

Onyx's OKX figure is our scan at 971,199 minus each address's flows over the 5,250 blocks after the
snapshot (`scripts/stream-reserve-flows.py`, chain links checked). For context: all 9,443 v2 OKX
addresses held 140,998.89969025 BTC at 965,949 (24.46 BTC on v2 addresses absent from the
September file), and 144,931.85016221 BTC at 971,199. The issuer's "Bitcoin in Trust" is 37,290.75 BTC
(holdings data as of 10/11/2026), the examiner's reserve figure 37,291 BTC (as of 10/09/2026). The
51 BITB v2 addresses no longer on the page hold nothing.

Order of value for the network: 4 (done), 5 (done), 2 and 6 (built, in review), 1 is BLOCKED on
rights (POR-1). Row 3 waits on per-cluster grading.

Rights reviews pending before any reserve list can be cleared: cbBTC (Coinbase's terms for the
proof-of-reserves page). wBTC's list is excluded (IDX-R1-F1).
