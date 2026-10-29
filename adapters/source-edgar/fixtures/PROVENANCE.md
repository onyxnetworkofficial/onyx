# Provenance: SEC EDGAR XBRL company concept (`us-gaap:CryptoAssetNumberOfUnits`)

| Field | Value |
|---|---|
| Source | U.S. Securities and Exchange Commission, EDGAR XBRL API, `companyconcept` endpoint (`https://data.sec.gov/api/xbrl/companyconcept/CIK##########/us-gaap/CryptoAssetNumberOfUnits.json`) |
| Concept | `us-gaap:CryptoAssetNumberOfUnits`: the number of crypto-asset units the company tagged in its filing, per period end, in the unit the company chose |
| Reuse terms | <https://www.sec.gov/privacy>: "Information presented on sec.gov is considered public information and may be copied or further distributed by users of the web site without the SEC's permission." Read 2026-10-11 (through a fetch tool: www.sec.gov answered our own client 403). |
| Access policy | <https://www.sec.gov/os/accessing-edgar-data>: "Please declare your user agent in request headers" (example: "Sample Company Name AdminContact@<sample company domain>.com"); "Current max request rate: 10 requests/second." Read 2026-10-11. |
| User-Agent used | `Onyx research project (onyx-network data verification prototype)`. No personal or contact e-mail address is sent (SEC-4). data.sec.gov answered it (HTTP 200); www.sec.gov pages answered 403. |
| Account / key | None. Plain HTTPS GET. |
| Onyx rights | `license = onyx:sec-gov-public-information`, `redistribution = Allowed`, attribution line shown with the data |
| Recorded | 2026-10-11, window starting 21:11:00 UTC (unix 1791753060): three GETs per company about 2.5 s apart, one per simulated observer. Every body of a company was byte-identical. The rehearsal settles period 2026-03-31 (194 days before the window, inside the 110..=730-day rule); the bodies also list 2026-06-30, which the rule does not open until 2026-10-18 (SEC-R1-F4). |
| Strategy Inc (CIK 0001050446) | `mstr-2026-03-31/response.json`, 1,219 bytes, Keccak-256 `0x7f9bfd10b5897c37b75efaabb9278a5f30d97160fbdaa3180ae3e2a2a09e8bad`; rehearsal period 2026-03-31: 762,099 at unit `Bitcoin` (10-Q filed 2026-05-06, accession 0001050446-26-000031) |
| Riot Platforms, Inc. (CIK 0001167419) | `riot-2026-03-31/response.json`, 3,452 bytes, Keccak-256 `0xbac3e181b65746eb1d3c9840f317c7075570317de17cb8c8ed79c92c56cbfd26`; rehearsal period 2026-03-31: 15,679 at unit `item` (10-Q filed 2026-04-30, accession 0001104659-26-053120) |
| Requests made while building this adapter | 12 in total on 2026-10-11: 2 to www.sec.gov policy pages (403), 1 submissions lookup, 3 exploratory `companyconcept` GETs (Strategy, CleanSpark, Riot), 6 recorded. Review SEC-R1 added 1 more `companyconcept` GET (CleanSpark, to check the exclusion reason). |

The recorded bodies are EDGAR's own responses, unmodified. A value is what the company tagged in
its filing; Onyx does not verify that the coins exist (SEC-2). Which companies and why:
`../src/lib.rs` `COMPANIES` (indexer notes: `the entity indexer/docs/unattributed/ITEM-B-treasuries.md`,
read only for the concept name and the per-CIK `companyconcept` approach; every value here was
fetched by Onyx itself).

## What the value is, and why these two companies (review SEC-R1)

EDGAR's `companyconcept` endpoint serves only facts without XBRL dimensions, so each value is the
company-wide total of crypto-asset units the filing tags, not a per-asset bitcoin figure (facts on
`CryptoAssetAxis` are not in this document). That total equals bitcoin holdings only where a
company holds (almost) only bitcoin and tags its whole holding. A company is registered only when
its tagged totals match its own public bitcoin totals; the table lists the periods checked against
a linked public statement. Values not in the table were not cross-checked.

| Company | Period | Tagged (EDGAR) | Public statement | Source |
|---|---|---:|---|---|
| Strategy Inc | 2026-03-31 | 762,099 | 762,099 BTC as of 2026-03-29 ("As of March 29, the company holds approximately 762,099 Bitcoin") | Yahoo Finance, 2026-03-30: <https://finance.yahoo.com/markets/crypto/articles/michael-saylor-skips-bitcoin-purchase-165000053.html> |
| Strategy Inc | 2025-12-31 | 672,500 | 672,497 BTC after its last 2025 purchase (Dec. 22-28) (consistent with: the tagged value is rounded) | Cointelegraph: <https://cointelegraph.com/news/strategy-bitcoin-purchase-active-2025-accumulation> |
| Strategy Inc | 2024-12-31 | 447,470 | ended 2024 holding 447,470 BTC | same Cointelegraph article |
| Riot Platforms | 2026-03-31 | 15,679 | "Bitcoin Held: 15,680" at quarter end (consistent with, off by 1) | Riot, 2026-04-02: <https://www.riotplatforms.com/riot-announces-first-quarter-2026-production-and-operations-updates/> |
| Riot Platforms | 2025-03-31 | 19,223 | prior-year "Bitcoin Held" 19,223 in the same table | same Riot release |
| CleanSpark (not registered) | 2025-09-30 | 2,583 | total holdings 13,011 BTC, of which "2,583 bitcoin of this amount was posted as collateral or as a receivable" | CleanSpark, 2025-10-03: <https://finviz.com/news/183200/cleanspark-releases-september-2025-bitcoin-mining-update> |

CleanSpark's tag is the collateral/receivable subset, not its treasury, so it is excluded (its
2026-06-30 value, 1,719,000, was first misread as a scale error; SEC-R1-F2). Rounding: a filing
may tag a rounded figure (Strategy's 672,500 vs 672,497), and EDGAR's API omits the XBRL
`decimals` attribute, so values are shown "as tagged" (SEC-R1-F3). The 10-Q text itself was not
checked: www.sec.gov answers our client 403. Some links above are press coverage, not the
company's own page, where the company's page refused our fetch tool (strategy.com answered 403).
