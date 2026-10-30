# Market venues: per-venue rights review for aligned feeds (CL8, ADR-0058)

Status 2026-10-30: **no venue is cleared.** No venue's market data is fetched, recorded or served
by Onyx. The aligned-feed code (ADR-0058, contributor loop CL8) is built and tested against
synthetic loopback venues only (`fixtures/aligned-demo`, `scripts/level2-aligned-anvil.sh`); the
live part is parked until a person completes this checklist for at least three venues
(ROADMAP S14).

Rule (CLAUDE.md hard limits, ADR-0058 item 8): a venue enters only after a person has read its
terms and recorded them. A venue whose terms forbid passing its data on is refused; `Restricted`
terms make the venue feed and every composite that lists it `Restricted`; a venue that needs a key
or an account falls under ADR-0054 (an `OK keyed` row). The manifest loader enforces the recorded
decision (docs/ops/data-sources.md sections 1 and 6, `onyx_level2::manifest::ClearedHosts`): a
venue manifest whose host has no `OK` row does not load, and neither does any composite over it
(THREATS CL8-12, `onyx-level2::a_venue_without_cleared_terms_never_loads`).

## 1. What a reviewer answers for each venue

Answer each item with a quote, its link and the date read. "Not found" is an answer; a guess is
not. Nothing here is legal advice.

1. **Terms that govern API market data.** Which document governs data obtained through the
   venue's public market-data API (a market-data licence, API terms, the general terms of
   service)? Quote the clauses on use, copying and redistribution.
2. **Redistribution of historical closed buckets.** May a third party copy and pass on historical
   candles (closed hourly and daily buckets), commercially, to anyone? Do the terms treat
   real-time, delayed and historical data differently? Is a delay required?
3. **Duties that travel with the data.** Attribution text, a notice, a link, a non-commercial
   limit (any of these makes the row `OK with duties` or `Restricted`).
4. **Access.** Is the candle endpoint public with no account, key or payment? If a key is needed,
   do the terms allow passing on data obtained with a personal key (ADR-0054, `OK keyed`)?
5. **Automated access and rate limits.** Are automated requests allowed? The published limits
   (they become the manifest's `limits`).
6. **The grid.** Are hourly and daily candles cut on the UTC grid (`[k × 3600, (k + 1) × 3600)`,
   days from 00:00 UTC)? A venue that cuts days on local time is refused (ADR-0058 item 3). How is
   the bucket start written (Unix seconds, milliseconds, `YYYY-MM-DDTHH:MM:SSZ`)?
7. **Corrections.** Does the venue revise closed candles, and how late (this sets
   `min_delay_secs`; THREATS CL8-5)?
8. **Unit.** The quote currency of the pair (a currency and a stablecoin are different units and
   never mixed in one composite; THREATS CL8-3).
9. **Host.** The API host serves only this venue's data (no shared host; CL3-R1-8).
10. **Record.** A row in docs/ops/data-sources.md section 1 (quote, link, date, decision), a host
    row in section 6 when the decision is `OK`, and the manifest's `PROVENANCE.md`.

## 2. Candidate venues (nothing read, nothing decided)

The pages below are the ones our fetch reached on 2026-10-30 (one GET each, `User-Agent:
onyx-dev`, HTTP status and the page title only). Whether a page is the document that governs API
market data is item 1's question; none of the texts was read for a decision, and no row here
clears anything.

| Venue | Page our fetch reached (2026-10-30) | Fetch result | Terms read | Decision |
|---|---|---|---|---|
| Coinbase | <https://www.coinbase.com/legal/market_data>; also <https://www.coinbase.com/legal/user_agreement> | 200, title "Market Data Terms of Use"; 200, empty title | no | none (unreviewed) |
| Kraken | <https://www.kraken.com/legal> | 200, title "Terms of Service" | no | none (unreviewed) |
| Bitstamp | <https://www.bitstamp.net/terms-of-use/> | 200, no title in the fetched HTML | no | none (unreviewed) |
| Gemini | <https://www.gemini.com/legal/user-agreement>; <https://www.gemini.com/legal/market-data-terms> | 200 each; titles not informative ("Icon/Instagram", "Gemini"): whether the second URL is a market-data document is unverified | no | none (unreviewed) |
| Bitfinex | <https://www.bitfinex.com/legal/exchange/terms/> | 200, title "Bitfinex \| Terms of Service" | no | none (unreviewed) |
| Crypto.com Exchange | <https://crypto.com/exchange/document/terms> | 200, title "Crypto.com Exchange" | no | none (unreviewed) |
| Binance | <https://www.binance.com/en/terms> | 202 with no readable page (as on 2026-10-14, docs/ops/data-sources.md S8: "not readable by fetch") | no | none (a human read is needed) |
| OKX | <https://www.okx.com/help/terms-of-service> (redirected to `/en-us/help/terms-of-service`) | 200, title "Terms of Service \| OKX United States" | earlier read recorded: docs/ops/data-sources.md section 4 quotes OKX ToS 8.1 as forbidding distributing "any of our products and Services" (read 2026-10-14) | none for market data; on the project's existing record it is not a candidate until a person reads the current text |

Venues not listed are not excluded; they are simply not looked at yet.

## 3. To clear a venue

1. Complete section 1 for it; record the quotes in docs/ops/data-sources.md section 1 with a new
   row and the decision.
2. Only on `OK`: add the host to section 6, write `sources/<venue>.<pair>.<interval>/manifest.json`
   (subject `bucketed`, a `bucket_start` assert, a `positive` volume assert, tolerance 0) and its
   `PROVENANCE.md`, record one response with `onyx-source draft` and check it with
   `onyx-source check`.
3. With three cleared venues in one unit: a `composite.json`, a dry run with
   `scripts/level2-aligned-anvil.sh <composite>` against recorded responses, then ROADMAP S14.

## Scope, assumptions, open questions

- Scope: market venues for aligned feeds only; every other source is in docs/ops/data-sources.md.
- Assumption: a venue's public candle endpoint and its terms can differ by region; the reviewer
  records which page was read.
- Open: which pair and venues first (ADR-0058 open question 1); whether a venue's market-data
  licence that allows display but not redistribution can ever be served (it cannot under the
  current rule).
