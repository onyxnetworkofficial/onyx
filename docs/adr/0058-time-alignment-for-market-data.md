# ADR-0058: Time alignment for market data feeds: closed buckets on a UTC grid, four stated clocks, a median with its spread

## Status
Accepted with amendment 1, 2026-10-30; built in part (docs/contributor-loop.md CL8), on fixtures
and loopback venues only. Built: items 1-6 and the client check of item 7 (`SubjectRule::Bucketed`,
`Expect::BucketStart`, `onyx_core::aligned`, `onyx_level2::aligned`, the tool `level2-aligned`,
`scripts/level2-aligned-anvil.sh`), item 8's enforcement (a venue manifest loads only under a
cleared terms row; strictest rights). Not built (ROADMAP S14): the `GET /v1/aligned/{metric}`
route, its OpenAPI entry and the hold of item 7, the browser's check, and every live venue: no
venue is reviewed or wired (docs/ops/market-venues.md). Proposed 2026-10-29. Builds on ADR-0027
(rounds, the lower median, the `Unavailable` answer), ADR-0017 (a value published with a min-max
band over its inputs), ADR-0049 (rounds only on payment) and ADR-0053 (manifest templates,
`SubjectDate`-style asserts).

## Context
A unified market-data feed (say one trading pair's price from several venues) mixes clocks: each
venue stamps its trades and candles with its own clock, observers fetch at their own times, the
round is recorded at a chain block, and the buyer asks about a moment. Without a stated rule two
honest feeds can disagree because they cut time differently.

What exists:
1. Level-2 values have two time rules: one fact per window end (`SubjectRule::Fixed`; numeric
   windows at most 300 s) or one fact per date (`Dated`, `Period`, `Daily`). Each `Observation`
   carries the observer's self-reported `observed_at`, which must lie in the window.
2. Observers of a moving value disagree by the move between their fetch times; ADR-0027 L2R-F1
   handles that with a tolerance band ("drift is not a lie"), at the cost of a value that is
   only as exact as the tolerance.
3. A round costs about 14 to 15 transactions on Base Sepolia (ADR-0049), so a feed cannot record
   every minute of every venue on chain.

## Decision
1. **Observe closed buckets, never live tickers.** A venue enters only through an endpoint that
   returns a closed, historical bucket (a candle or a period summary) for a given start time.
   Every observer then reads the same finished record, and agreement is exact (tolerance 0), as
   for a dated government series. Live tickers stay out of v1: they make observers disagree by
   design.
2. **The bucket grid.** Buckets are `[k × B, (k + 1) × B)` in UTC Unix seconds, end exclusive,
   with `B` in `{3600, 86400}` in v1 (hourly and daily; finer buckets wait for batched rounds,
   open question 2). New subject rule `SubjectRule::Bucketed { prefix, bucket_secs, min_delay_secs,
   max_delay_secs }`, appended last (no existing template id changes): the subject is
   `prefix:<bucket start, Unix seconds>`; the URL holds `{bucket_start}` or `{bucket_start_ms}`
   (filled with that start; at most two holes); a bucket is observable in a window starting
   `min_delay_secs..=max_delay_secs` after its end (late trades and venue corrections settle by
   `min_delay_secs`); one fact per subject, as dated facts (`OnyxRegistry` kind `DatedFeed`).
3. **Each venue is its own level-2 feed.** One manifest per (venue, pair, bucket size), reviewed
   like any other (ADR-0053), whose asserts pin the returned bucket: its start time equals the
   subject's (a `BucketStart` expectation added to `Expect`), the pair symbol, and the interval.
   A venue whose buckets are not aligned to the UTC grid (a local-time daily candle, an offset) is
   refused at review: Onyx never re-cuts a venue's buckets.
4. **Four clocks, all stated in every answer.**

   | Field | Clock | Meaning |
   |---|---|---|
   | `bucket.start`, `bucket.end` | the UTC grid | the period the value is about (end exclusive) |
   | `venues[].venueTime` | the venue's | the bucket start the venue returned (asserted equal to `bucket.start`) |
   | `venues[].observedFrom`, `observedTo` | the observers' | earliest and latest `observed_at` of the agreeing reveals |
   | `venues[].recordedAt` | the chain's | the block (and its time) of `ObservationRecorded` |

   The composite's `asOf` is `bucket.end`. No field is "now".
5. **The alignment rule (the composite).** `aligned(metric, bucket)` is a pure function of the
   venue facts recorded for that bucket:
   - inputs: the venue feeds listed in the composite's manifest (sorted, 2..=16); a venue counts
     only if its fact for this bucket is recorded at level 2 with a value; all venues share one
     unit and one scale (enforced at review; a stablecoin quote is another unit and never mixed
     with a currency quote, and no conversion is done);
   - value: the lower median of the included values (the ADR-0027 rule); band: their minimum and
     maximum (as ADR-0017's per-set band);
   - at least `MIN_VENUES` (proposed 3) included, else the answer is `insufficient` with no value;
   - level: the weakest input, so 2; anyone recomputes the composite from the venue facts, so it
     adds no trust.
   Venues not included are listed with a closed reason: `not_observed` (no round yet),
   `unavailable` (the round settled `SourceUnavailable`), `no_result`, `rights_withheld`.
6. **Disagreement is shown, never hidden.** Every answer carries every included venue's value and
   clocks, the band, and `disagreement: true` when `(max - min) × 10^6 / |median|` exceeds
   `DISAGREE_PPM` (proposed 5,000, 0.5%). The composite is never trimmed to hide an outlier: a
   venue far from the others is shown as such, with its own level-2 evidence.
7. **Serving.** `GET /v1/aligned/{metric}?bucket_start=` (OpenAPI, ADR-0045) returns item 4's
   fields, the venues, the band, the flag and each venue's `/v1/observations` link; a venue
   bucket with no round yet is offered through ADR-0049 (a hold, one round per fact), so nothing
   runs unpaid. The client check (`onyx-check`, the browser) verifies each venue's round as a
   level-2 answer and recomputes item 5.
8. **Data rights, per venue, before anything is served.** A venue's market data may carry its own
   licence terms, which can differ per venue and per kind of data (live, delayed, historical);
   this ADR assumes nothing about any venue. Each venue's terms are reviewed and recorded (docs/ops/data-sources.md section 5,
   `PROVENANCE.md`) before its manifest is registered: a venue whose terms forbid redistribution
   is refused, `Restricted` terms make the venue feed and every composite that includes it
   `Restricted` (the strictest input wins), and a venue whose terms need a key or account falls
   under ADR-0054's rules. No venue is named or assumed cleared in this ADR.

## Consequences
- A buyer always knows which period a value covers, whose clock said so, and how far the venues
  were apart.
- Hourly and daily buckets only, and only when paid for: this is not a streaming price feed.
- A venue that revises a closed bucket after `min_delay_secs` can still make a recheck differ
  (ADR-0057 handles that as "out of date").
- No venue can be served until its terms are reviewed; the first composite needs at least three
  cleared venues.

## Alternatives
- **Live tickers with a tolerance band**: observers never agree exactly, the value is only as good
  as the band, and fetch times decide it.
- **Last trade at or before the bucket end with a staleness limit**: needs trade-level endpoints
  and a staleness rule per venue; candles give the same cut with less data.
- **Volume-weighted composite**: needs volume figures that are comparable across venues, which
  this design cannot check; the median trusts no venue's volume.
- **Onyx re-cuts each venue's trades into its own buckets**: needs every trade (large, often under
  different terms) and turns a level-2 reading into a computation over unchecked inputs.

## Threats (in THREATS.md section CL8 with their tests, with CL8-8..CL8-12 added when built)
| ID | Threat | Mitigation | Test to write |
|---|---|---|---|
| CL8-1 | A venue returns another bucket (off by one interval, local time) | `BucketStart` assert; unaligned venues refused at review | `a_bucket_with_another_start_is_unavailable` |
| CL8-2 | One venue (or a manipulated thin venue) moves the composite | Lower median of at least 3; band and flag shown | `one_far_venue_does_not_move_the_median_and_raises_the_flag` |
| CL8-3 | Venues in different units mixed (stablecoin and currency quotes) | One unit and scale per composite, checked at load | `a_composite_with_two_units_does_not_load` |
| CL8-4 | The operator leaves out a venue that disagrees | Venue list fixed by the composite's manifest; a missing venue is listed with its reason; the client checks the list | `the_client_refuses_a_composite_missing_a_listed_venue` |
| CL8-5 | A bucket read before late trades settle | `min_delay_secs` in the subject rule | `a_bucket_is_observable_only_after_its_delay` |
| CL8-6 | A coordinator picks among windows for a preferred value | Short `max_delay_secs - min_delay_secs` (as NS3-9); one fact per bucket | `the_bucketed_observable_range_is_short` |
| CL8-7 | Restricted venue data leaks through a composite | Strictest input's rights on the composite | `a_composite_with_a_restricted_venue_is_restricted` |

## Amendment 1 (2026-10-30, as built)
Decided while building, each with its test (THREATS CL8):
1. **Bounds of the subject rule.** `bucket_secs` in `{3600, 86400}`; `min_delay_secs >= 60`;
   `max_delay_secs <= 7 days`; `max_delay_secs - min_delay_secs <= 6 hours` (CL8-6). The subject's
   start is canonical decimal (no sign, no leading zero) and on the grid; `{bucket_start_ms}` is
   the start times 1,000. A bucket ages for pricing from `end + min_delay_secs + window_secs`;
   its last window starts at `end + max_delay_secs`. Only a bucketed template holds a bucket
   placeholder.
2. **Asserts.** `Expect::BucketStart(form)` with form `unix_secs`, `unix_millis` or `rfc3339`
   (exactly `YYYY-MM-DDTHH:MM:SSZ`), and `Expect::Positive` (a decimal strictly above zero, used on
   the bucket's volume: a stale or halted venue's empty bucket gives no value, CL8-11). Both are
   appended, so no existing descriptor, fetcher id or template id moved (the shared vectors
   `fixtures/json-fetcher/vectors.json` are unchanged and gained the CL8 cases). A bucketed
   template needs a `BucketStart` and a `Positive` assert on the value's own object (review
   CL8-R1-4/5: never another row of a list), no date assert, a numeric kind with tolerance 0.
3. **A fifth reason, `clock_mismatch`.** A recorded venue fact is counted only when its clocks
   agree: the venue's time equals the bucket's start, the window opens at or after the bucket's
   end, every agreeing reveal lies in the window, the chain recorded the fact after the window
   (CL8-8). A value round counts only once `observationLevel` is 2 for it on chain; until then the
   venue is `not_observed`. A client asks the chain about every venue its store shows without a
   value and refuses a store that hides a recorded fact (review CL8-R1-2).
3a. **Who picks the venues (review CL8-R1-1).** Rounds run only when paid, one per venue fact, so a
   payer could pay for a chosen subset. The answer is `pending` (no value) while any listed venue
   has no recorded round and its bucket can still be observed; a venue whose range passed with no
   round is `lapsed` (a sixth reason) and the answer is `insufficient`; a value also needs more
   than half of the listed venues included. A halted venue is still observed (`unavailable`), so it
   never blocks a value on its own. Open question 4 is answered: no `partial` value.
4. **The flag is exact.** `disagreement` is `(max - min) × 10^6 > disagree_ppm × |median|` with
   256-bit products; `spreadPpm` is the floor of the ratio, saturated at `u64::MAX` (display
   only). Exactly at the threshold is not "exceeds".
5. **Composites.** A composite file (`composite.json`: metric, unit, scale, bucket length,
   `min_venues >= 3`, `disagree_ppm` in 1..=10^6, venue manifest ids) loads only when every venue
   manifest loads (cleared terms), the units, scales and bucket lengths agree, it lists 3 to 16
   venues, and no venue (`rights.source`), host or terms row is listed twice; there are no weights
   (CL8-10, review CL8-R1-3). Its id is
   `keccak256("onyx-composite-v1" ‖ borsh)`, an answer's `keccak256("onyx-aligned-v1" ‖ borsh)`.
   The composite's rights are the strictest of all listed venues, included or not (CL8-7).
6. **The venue's own time** is read from the venue's page at the `BucketStart` pointer when the
   client has a page whose Keccak-256 is an agreeing reveal's page hash; otherwise it is the
   bucket start that assert held for every agreeing reveal.

## Open questions
1. Which venues and pairs first? Each needs a terms review before any work on it
   (docs/ops/market-venues.md lists the candidates; none is reviewed).
2. Minute buckets need many facts per round (a Merkle batch of buckets per round); worth a later
   ADR if buyers ask for them.
3. `MIN_VENUES` and `DISAGREE_PPM`: proposed 3 and 5,000 ppm.
4. Should a composite be allowed with fewer venues when the others are `unavailable`, marked
   `partial`?
