# Spec: track records served for real (draft, NORTH-STAR NS16, north-star loop NL4, ROADMAP C66)

Status: **Slices 1 and 2 built 2026-10-28** (ADR-0036 amendment 7: the daily rule, resolutions only from
recorded rounds, a publication served and checked; amendment 8: the NWS forecast as a labelled
testnet forecaster, through the intake on Base Sepolia, checked on the data page). Drafted by lane B 2026-10-27. Builds on ADR-0036
(level 5), `crates/onyx-track` and `crates/onyx-api/src/track_serve.rs` (`GET /v1/track...`,
ROADMAP C6, C20, C21, C23). Prior art: RESEARCH.md "Lane B batch 2 (2026-10-27)".

## Scope

The machinery to score sources exists and is served, but no node has a publication: no metric
with a reviewed close-time rule has statements from a source (track_serve.rs module doc), so every
`/v1/track` answer is a 404 and the site shows the labelled example only
(`apps/web/src/lib/components/TrackRecords.svelte`). NL4 makes the first real records:

- **Facts:** the two daily level-2 metrics now live on Base Sepolia (B6):
  `usgs.earthquakes.m5_count` and `noaa.ghcnd.usw00003017.tmax`.
- **Forecaster scores:** a source states a value for a day before the day starts; the day's
  final level-2 round resolves it. First candidate: the U.S. National Weather Service's own
  forecast of the Denver high (api.weather.gov, open data, a U.S. government work; its rights
  record and terms review come first, as for every source), against the NCEI observation.
- **Source scores:** a level-4 claim of a submitted source (S1) later re-observed at level 2 is an
  agreement or a disagreement on the same fact (the C64 contradiction rule gives the pairing).
- **Baseline:** persistence (yesterday's value), which the record already computes for period
  metrics (`Baseline`), so a score is shown against "no skill".

Out of scope: money for forecasters (level 5 moves no money, ADR-0036), and people as sources
(sources are organisations, never individuals).

## Interfaces

- **Rules** (`crates/onyx-track`, `MetricRule`): `Close::PeriodStart { period_secs: 86_400,
  lead_secs }` for both metrics, a day aligned to UTC midnight, `lead_secs` long enough that no
  part of the day is public before statements close; `tolerance` 0 for the count and 5 for the temperature (mantissa units of
  `Fixed { scale: 1 }`, so 0.5 °C), recorded with the reason.
- **Statements:** `scripts/track-statements.sh <source>` (new) fetches the forecast with
  User-Agent `onyx-dev`, writes a `Statement` (value, a declared confidence) and posts it through
  the existing intake (`crates/onyx-track/src/intake.rs`, ROADMAP C5), so `committed_at` is a
  finalized block time before the close.
- **Resolutions:** built from a level-2 results store only for a round the registry shows final
  (`evidence` = the round's result hash, `level` = 2, `public_from` = the round's final block
  time); `crates/onyx-level2/src/live.rs` gains `resolution_of(store, round)`.
- **Publication:** the coordinator's publication file (`ONYX_TRACK_PUBLICATIONS`) for the hosted
  node, built by the existing `onyx_track::compute` and committed on chain as today (C7).
- **Site:** `TrackRecords.svelte` shows the live records (agreed, disagreed, Brier, the baseline)
  next to the example, the example still labelled; the browser reruns `check_record_body` as now.

## Rules

1. **R-TRS-1.** A resolution exists only for a final round of the fact's own metric and subject
   (the node's `ObservationStore::check_on_chain` in `crates/onyx-api/src/observations.rs` passes); an open, failed or other-registry round yields none.
2. **R-TRS-2.** A statement counts only if committed before the close (`late` otherwise, as now).
3. **R-TRS-3.** A source is never judged by its own resolver or one with the same declared operator
   (TR-R1-7, TR-R2-4): Onyx's observers resolve, so Onyx never states as a source on these metrics.
4. **R-TRS-4.** A record with fewer than N resolved entries shows "too few to score" instead of a
   Brier figure (N decided in the ADR; ten is the working number).

## Tests that prove the Done-when

- `onyx-track` `daily::tests::no_resolution_from_an_open_round` and
  `::a_final_round_gives_one_level_2_resolution` (the "test rejects a score from an open round").
- `onyx-track` `a_statement_after_the_daily_close_is_late` for the new rule.
- `onyx-api` `track_serve_tests::a_live_publication_serves_the_daily_records`.
- e2e "live track records show scores and the baseline" plus the existing example checks.

## Assumptions

- api.weather.gov stays keyless (its documentation says a User-Agent identifies the caller and a
  key may come later) and its forecast for the Denver grid point maps to the NCEI station's day.
- NCEI's daily record lags by days to weeks, so resolutions arrive late; the record shows
  `unresolved` until then, which the format already has.

## Open questions

1. Confidence: the NWS states a value, not a probability. Use a declared fixed confidence per
   source (stated in the rule), or score agreement only and show no Brier for such sources?
2. Which NWS product: the 12-hour `forecast` text periods or the raw `forecastGridData`
   maximum temperature? The raw grid is machine-readable; its field names are not checked yet.
3. Earthquake counts have no public forecaster; that metric may only get source scores (claims
   re-observed), not forecaster scores.
