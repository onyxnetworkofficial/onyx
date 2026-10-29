# Provenance: spot bitcoin ETF holdings from SEC EDGAR (ROADMAP P2c)

| Field | Value |
|---|---|
| Source | U.S. Securities and Exchange Commission, EDGAR XBRL API, `companyconcept` (`https://data.sec.gov/api/xbrl/companyconcept/CIK##########/us-gaap/<concept>.json`) |
| Reuse terms | Same as `PROVENANCE.md`: <https://www.sec.gov/privacy>, "Information presented on sec.gov is considered public information and may be copied or further distributed by users of the web site without the SEC's permission." |
| Access policy | <https://www.sec.gov/os/accessing-edgar-data>: declared User-Agent, at most 10 requests/second. The recorder (`scripts/record-sec-etf-holdings.py`) sends at most 2 per second, to data.sec.gov only. |
| User-Agent | `onyx-dev (contact pending)`. SEC asks for a contact; the project has none yet (SECURITY.md is a draft), so no address is sent (ROADMAP P2g, THREATS ETF-5). data.sec.gov and efts.sec.gov answered it (HTTP 200); www.sec.gov answered 403 "Request Rate Threshold Exceeded" three times (company tickers file, a filing index, the access-policy page), so filing documents were not read. |
| Recorded | 2026-10-19, window from unix 1792382708 (04:05:08 UTC): three GETs per fund, about 1 s apart, one per simulated observer; every fund's three bodies were byte-identical. Bodies and SHA-256: `etf/manifest.json`. |
| Fund discovery | EDGAR itself: full-text search (efts.sec.gov, 10-Q/10-K filed 2026, phrases "bitcoin custodian", "bitcoin held", "creation baskets") and company search; `companyfacts` read once per candidate fund to find which concept, if any, carries the quantity. About 45 requests in total while building, all logged locally. |
| Prior work read (not copied) | The local entity indexer's SEC inline-XBRL notes named the issuer-specific concepts of BTCO, BTCW, HODL and BITB; every value here was fetched by Onyx. |

## Why these seven funds, and the cross-check

`companyconcept` serves only standard (`us-gaap`) facts without XBRL dimensions. A fund is covered
when such a fact carries its bitcoin quantity. Check: the same filing's dimensionless fair value
(`CryptoAssetFairValue` or `InvestmentOwnedAtFairValue`, USD) divided by the quantity must give the
bitcoin price the other funds' filings give for the same day (they use different benchmark rates,
so agreement is within about 1%, not exact). Values from `companyfacts`, 2026-10-19.

| Fund | Fact (unit) | 2025-12-31 implied USD/BTC | 2026-06-30 quantity | 2026-06-30 implied USD/BTC |
|---|---|---:|---:|---:|
| IBIT | `CryptoAssetNumberOfUnits` (pure) | 87,463 | 734,261 | 59,101 |
| FBTC | `InvestmentOwnedBalanceShares` (shares; 10-K only) | 87,463 | (no 10-Q tag) | |
| GBTC | `InvestmentOwnedBalanceContracts` (Number) | 87,549 | 138,505.756804 | 58,745 |
| Grayscale Bitcoin Mini (BTC) | `InvestmentOwnedBalanceContracts` (Bitcoin) | 87,549 | 54,280.400501 | 58,745 |
| ARKB | `InvestmentOwnedBalanceContracts` (pure) | 87,515 | 32,178.228 | 58,714 |
| EZBC | `CryptoAssetNumberOfUnits` (Bitcoin) | (no tag) | 5,664.0163 | 59,101 |
| DEFI | `InvestmentOwnedBalanceShares` (shares) | 87,583 | 225.58 | 58,670 |
| OBTC (not covered) | `CryptoAssetNumberOfUnits` (BTC) | 55,776 | 945.07 | 62,012 |

OBTC's implied prices are about 36% low and 5% high against every other fund, so its tag is not
taken as its whole holding (listed as not covered). Units such as "shares" or "pure" are the
issuer's own choice of XBRL unit; the price check is what shows they count bitcoin.

DEFI: the tag is 134.87 BTC for three quarters in a row (2025-09-30 to 2026-03-31), then 225.58 at
2026-06-30; the implied prices agree with the other funds for each of those days, so it is taken as
filed. Press reports say the fund closed in August 2026; not checked on EDGAR.

## What the values are

- "As tagged": EDGAR's API omits the XBRL `decimals` attribute. IBIT and FBTC tag whole bitcoin
  (rounded); the others tag four to six decimals. A value is converted to satoshis exactly from
  the JSON token (no floats); more than 8 decimals that are not zero, a sign, an exponent or more
  than 21,000,000 BTC is refused.
- Provenance per period: the earliest filing (by filing date) that tags it under the fund's
  concept. Where a fund adopted the concept late (IBIT from its 2026 10-Qs), the earliest such
  filing is a later report that repeats the period as a comparative, not the period's own 10-Q.
- A period is served only 110 to 730 days after its end (the company feeds' rule, SEC-3), so this
  recording covers period ends from 2024-12-31 to 2026-06-30.
- Rows of one period that disagree (ARKB 2024-03-31: 44,667.3162 and 44,667.3165) make the
  period unavailable rather than picking one.
