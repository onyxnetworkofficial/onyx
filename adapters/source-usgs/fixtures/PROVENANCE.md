# Provenance: USGS earthquake count (NS3)

| Field | Value |
|---|---|
| Source | U.S. Geological Survey, ANSS Comprehensive Earthquake Catalog (ComCat), FDSN event web service |
| Dataset | `/fdsnws/event/1/count` with `starttime={day}T00:00:00`, `endtime={day}T23:59:59.999`, `minmagnitude=5.0`, `eventtype=earthquake`: the number of earthquakes of magnitude 5.0 or more worldwide on one UTC day, as plain text (without `eventtype` explosions count too) |
| Terms | <https://www.usgs.gov/information-policies-and-instructions/copyrights-and-credits>: "USGS-authored or produced data and information are considered to be in the U.S. Public Domain." and "When using information from USGS information products, publications, or websites, we ask that proper credit be given." Read 2026-10-26 (the page refuses curl with 403; read through a browser fetch). |
| Contributed data | ComCat merges events from contributing seismic networks. Onyx keeps only the day's count (a fact), never the networks' event records. |
| Account / key | None. Plain HTTPS GET. The service page names no rate limit. |
| Onyx rights | `license = onyx:facts-from-usgs-comcat` (a count derived from the catalog; USGS-authored parts are public domain, contributed records are not copied), `redistribution = Allowed`, attribution line shown with the data (the credit USGS asks for) |
| Revisions | The catalog revises a day for weeks (late events, magnitude changes, merged duplicates): a day is observable only 100 to 120 days after it (short, so nobody picks a late window by its value) (RESEARCH.md "Daily public data outside finance"). |
| Recorded | 2026-10-26 21:28-21:33 UTC: five GETs of the pinned URL for the day 2026-07-08 (110 days earlier, inside the 100..=120-day range), about 62 s apart (the CDN caches answers for 60 s), one per simulated observer, User-Agent `onyx-dev`. All five were CloudFront misses (fresh reads) and byte-identical (`9`, no newline; `quakes-m5-2026-07-08/response.txt`). Times and the `Date`, `Age` and `X-Cache` headers are in `quakes-m5-2026-07-08/fetches.json`. |
| Requests made while building this adapter | About 16 by the builder on 2026-10-26: probes and checks, a first recording of 5 discarded (the CDN served one cached copy), a second of 5 discarded (it lacked `eventtype=earthquake`, review NS3-R1), a third of 5 discarded (its day fell outside the narrowed range), and the 5 recorded; plus 5 per live test run. The research and review passes made their own separately. |

The recorded body is the service's own response, unmodified. Onyx reads it exactly as a whole
number.

## Regional counts (north-star loop NL17, `src/region.rs`)

| Field | Value |
|---|---|
| Dataset | The same `count` endpoint with the same day bounds and `eventtype=earthquake`, plus a circle: `latitude=34.05`, `longitude=-118.25`, `maxradiuskm=300`, `minmagnitude=2.5` (earthquakes of magnitude 2.5 or more within 300 km of Los Angeles). Metric `usgs.earthquakes.la300km_m2_5_count`, subject `usgs.earthquakes.la300km_m2_5:YYYY-MM-DD` |
| Terms and rights | As above (S24): the same service and statement; Onyx keeps only the count. The same Onyx rights (`onyx:facts-from-usgs-comcat`, `Allowed`, the same attribution) |
| Revisions | The same 100 to 120-day lag as the world feed. Small events are revised by the regional network (in southern California mostly within days), so the world feed's measurement is the conservative bound; not measured separately for M2.5 |
| Recorded | 2026-10-27 22:10 UTC: one GET of the pinned URL for the day 2026-07-08 (110 days earlier), User-Agent `onyx-dev`: body `0` (`region-la300-2026-07-08/response.txt`). One more GET the same minute counted 2026-07-01 to 2026-07-15 (14 events) to check that the circle is not empty |
| Requests made while building | 3 on 2026-10-27 (the spec's probe of 2026-07-01, the recording, the 15-day check) |
