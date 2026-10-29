# Spec: more strong datasets (draft, north-star loop NL17)

Status: **Proposed 2026-10-27** (lane B, research only; nothing is built). Every source still needs
its rights record and terms review before any code (Phase F rule, docs/ops/data-sources.md).
Prior art and the probes: RESEARCH.md "More datasets and parametric insurance (NL17, NL18)".

## Scope

Three to five more public sources that a level-2 round can observe without a key, whose terms allow
redistribution, and that fit the two first uses: markets on a future value (NL6) and parametric
insurance triggers (NL18). Each reuses an existing adapter where it can, so the work is a template,
a rights record, fixtures and a recorded round, not a new crate.

| # | Source and value | Rights (link) | Fits | Probe 2026-10-27 (read-only, User-Agent `onyx-dev`) | Reuses |
|---|---|---|---|---|---|
| 1 | NCEI GHCN-Daily `PRCP` (daily precipitation), Denver International Airport `USW00003017` | U.S. Government work; NWS station values public domain unless noted (data-sources S25, N20) | rainfall and drought triggers | the Access Data Service answered `PRCP` for 2026-09-01 and 2026-09-02 (0.0 mm each, metric units) | `adapters/source-ncei` (a second element) |
| 2 | NCEI GHCN-Daily `TMAX` and `PRCP`, New York Central Park `USW00094728` | as row 1 | heat and rain markets for a second large city | answered both elements for 2026-09-01 (27.8 °C, 9.9 mm); `includeStationName=true` names it "NY CITY CENTRAL PARK, NY US" | `adapters/source-ncei` |
| 3 | USGS FDSN event `count` in a circle (`latitude`, `longitude`, `maxradiuskm`) above a magnitude, e.g. 300 km around Los Angeles | U.S. Public Domain with a credit request ([USGS copyrights and credits](https://www.usgs.gov/information-policies-and-instructions/copyrights-and-credits); data-sources S24) | earthquake triggers for one region | answered `0` for M2.5+ within 300 km of 34.05, -118.25 on 2026-07-01 | `adapters/source-usgs` (a regional template) |
| 4 | BLS CPI-U, all items, not seasonally adjusted (`CUUR0000SA0`), monthly, public API v1 (no registration) | public domain except copyrighted photos and illustrations; a citation and disclaimer requested ([BLS developers](https://www.bls.gov/developers/); data-sources N8) | inflation markets | v1 answered `REQUEST_SUCCEEDED`, latest 2026 M09 = 336.940 | a new dated monthly source (`SubjectRule::Daily` is daily; a monthly rule or a period subject is needed) |
| 5 | EIA weekly U.S. retail gasoline price | public domain with an acknowledgment that includes the publication date ([EIA copyrights and reuse](https://www.eia.gov/about/copyrights_reuse.php)) | energy price markets | the API v2 answered `API_KEY_MISSING` without a key: not keyless | parked: a keyless EIA endpoint, or the keyed path (NL11) |

Proposed order: 1, 2, 3 (existing adapters and rights records, about a day together), then 4 (a
monthly subject rule). 5 waits.

## Interfaces

- A template per (source, element, subject rule): metric ids in the existing pattern, e.g.
  `noaa.ghcnd.usw00003017.prcp`, `noaa.ghcnd.usw00094728.tmax`, `usgs.earthquakes.m4_r300_la_count`,
  `bls.cpi.cuur0000sa0`.
- Fixtures and a `PROVENANCE.md` entry per source (as NS3); a recorded round test per template.
- `scripts/level2-sepolia.sh <source>` runs each live round; `scripts/deploy-api.sh` ships each
  store with its template pin (B6).

## Assumptions

- The NCEI rights record (S25) covers other stations' and elements' values from the same service;
  the review confirms it per station (some GHCN stations come from non-U.S. networks with other
  terms, so only U.S. `USW`/`USC` stations are proposed).
- Revisions: precipitation and temperature at a station can be corrected after the day, as TMAX
  can (NS3 measured `min_days` for it); each new element gets its own revision measurement before
  its `min_days` is set.
- BLS v1 is enough for one series a day; its limits are on the API FAQ page, not read yet.

## Open questions

1. Which second city and which USGS region matter most for the first paying users (launch plan:
   demand first)?
2. CPI is monthly: read BLS's revision policy for the not-seasonally-adjusted series before
   choosing `min_days` (not checked yet).
3. Does a precipitation trigger need a sum over several days (a period subject) rather than one day?
