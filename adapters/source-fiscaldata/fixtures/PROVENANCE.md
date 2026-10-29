# Provenance: U.S. Treasury Fiscal Data, Debt to the Penny

| Field | Value |
|---|---|
| Source | U.S. Department of the Treasury, Bureau of the Fiscal Service, Fiscal Data API |
| Dataset | Debt to the Penny (`/v2/accounting/od/debt_to_penny`), field `tot_pub_debt_out_amt` (Total Public Debt Outstanding, USD) |
| Terms | <https://fiscaldata.treasury.gov/about-us/>, "Licensing": "The data on this site is available to copy, adapt, redistribute, or otherwise use for non-commercial and commercial purposes." Read 2026-10-11. |
| API terms | <https://fiscaldata.treasury.gov/api-documentation/>: "The data is offered free, without restriction, and available to copy, adapt, redistribute, or otherwise use for non-commercial or commercial purposes." This line covers the API itself (L2SRC-R1-F5). Read 2026-10-11. |
| Account / key | None. Plain HTTPS GET. |
| Onyx rights | `license = onyx:us-treasury-fiscal-data-open`, `redistribution = Allowed`, attribution line shown with the data (a courtesy; the terms ask for none) |
| Recorded | 2026-10-11 05:26 UTC: five GETs of the pinned URL for record date 2026-10-08, about 3 s apart, one per simulated observer. All five bodies were byte-identical (`response.json`, Keccak-256 `0xf5dd7a5c490fe1584511f4629bb503fdb1a770af9c1b4c13fe07497c6d45bfc4`). Times and server `Date` headers are in `debt-to-penny-2026-10-08/fetches.json`. |
| Requests made while building this adapter | 13 in total on 2026-10-11 (2 exploratory, 1 format check, the 5 recorded, 5 by one live test run). |

The recorded body is the API's own response, unmodified. Onyx does not modify the value; it reads it
exactly at 2 decimals.

## Daily Treasury Statement: TGA closing balance (north-star loop NL25)

| Field | Value |
|---|---|
| Source | Same agency, API and terms as above |
| Dataset | Daily Treasury Statement, table I "Operating Cash Balance" (`/v1/accounting/dts/operating_cash_balance`), row `account_type` = "Treasury General Account (TGA) Closing Balance", field `open_today_bal` (millions of USD; the dataset files the closing amount in this column) |
| Terms | The two lines above cover every Fiscal Data dataset; both pages read again 2026-10-29 and still say so. |
| Onyx rights | The same rights record (`onyx:us-treasury-fiscal-data-open`, `Allowed`) |
| Recorded | 2026-10-29 04:46 UTC: one GET of the pinned URL for record date 2026-10-27 (`dts-tga-2026-10-27/response.json`, 658 bytes, Keccak-256 `0x1b93108c8d15f60c807df0944218b8410019464c33bcf4c4cbe064fdcbd57ce8`, server `Date` Thu, 29 Oct 2026 04:46:28 GMT). Value 1052530. |
| Requests made while building this series | 5 on 2026-10-28/29 (2 exploratory, 2 format checks, the 1 recorded). |
