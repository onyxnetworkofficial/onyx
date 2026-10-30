# Provenance: U.S. Treasury Fiscal Data, Daily Treasury Statement, total TGA deposits

The first level-2 source added as a manifest (contributor loop CL3, ADR-0053): no Rust of its
own; the generic JSON fetcher reads it under the descriptor in `manifest.json`.

| Field | Value |
|---|---|
| Source | U.S. Department of the Treasury, Bureau of the Fiscal Service, Fiscal Data API (public domain, U.S. Government work) |
| Dataset | Daily Treasury Statement, table I "Operating Cash Balance" (`/v1/accounting/dts/operating_cash_balance`), row `account_type` = "Total TGA Deposits (Table II)", field `open_today_bal` (millions of USD; the dataset files the day's amount in this column) |
| Terms | <https://fiscaldata.treasury.gov/about-us/>, "Licensing": "The data on this site is available to copy, adapt, redistribute, or otherwise use for non-commercial and commercial purposes." The API documentation (<https://fiscaldata.treasury.gov/api-documentation/>) says the same of the API. Both cover every Fiscal Data dataset; read 2026-10-11 and again 2026-10-29 (adapters/source-fiscaldata/fixtures/PROVENANCE.md). |
| Terms row | docs/ops/data-sources.md S10 (OK); host cleared in section 6 |
| Account / key | None. Plain HTTPS GET, no contact header. |
| Onyx rights | `license = onyx:us-treasury-fiscal-data-open`, `redistribution = Allowed`, the same attribution line as the Treasury adapter |
| Value | An aggregate (the Treasury's daily deposits), never about a person |
| Recorded | 2026-10-30 01:59 UTC: one GET of the pinned URL for record date 2026-10-27 through `onyx-source draft` (`onyx_fetch`), `recording/2026-10-27.json`, 640 bytes, Keccak-256 `0x172350a78e2b45561c2fe99179d067ee33f59dacac8f2dc3e7af78395f758bc3`. Value 321642. |
| Requests made while building this source | 2 on 2026-10-30 (1 exploratory listing of the day's four rows, 1 recorded), plus the observers' fetches of the local-anvil dry run (docs/contributor-loop.md, CL3). |

The recorded body is the API's own response, unmodified.
