# Provenance: NOAA NCEI GHCN-Daily, TMAX at Denver International Airport (NS3)

| Field | Value |
|---|---|
| Source | NOAA National Centers for Environmental Information (NCEI), Global Historical Climatology Network - Daily (GHCN-Daily), through the Access Data Service |
| Dataset | `https://www.ncei.noaa.gov/access/services/data/v1?dataset=daily-summaries&stations=USW00003017&startDate={day}&endDate={day}&dataTypes=TMAX&units=metric&format=json`: one row, the day's maximum temperature in °C to a tenth at station `USW00003017` (Denver International Airport) |
| Rights | No licence is stated by NCEI. The data are a work of the U.S. Government: "Copyright protection under this title is not available for any work of the United States Government" (17 U.S.C. 105(a), <https://www.law.cornell.edu/uscode/text/17/105>). The station's values come from the National Weather Service, whose pages "are in the public domain, unless specifically noted otherwise, and may be used without charge for any lawful purpose so long as you do not: 1) claim it is your own [...], 2) use it in a manner that implies an endorsement or affiliation with NOAA/NWS, or 3) modify its content and then present it as official government material." (<https://www.weather.gov/disclaimer>). Read 2026-10-26. |
| Duties | Cite GHCN-Daily ("Cite as": Menne et al. (2012), GHCN-Daily Version 3, doi:10.7289/V5D21VHZ, with the access date; <https://www.ncei.noaa.gov/access/metadata/landing-page/bin/iso?id=gov.noaa.ncdc:C00861>); never imply NOAA endorsement; never present the value as official NOAA material. The attribution line says both. |
| Account / key | None. The older CDO v2 API needs a token; this service does not. |
| Onyx rights | `license = onyx:us-government-work-noaa`, `redistribution = Allowed`, attribution line with the citation and "not an official NOAA product as shown here" |
| Revisions | GHCN-Daily appends updates and changed values days to weeks back (one sampled diff file changed values 5 to 25 days old): a day is observable only 30 to 60 days after it (short, so nobody picks a late window by its value). A full reprocessing can still change old values (RESEARCH.md "Daily public data outside finance"). |
| Recorded | 2026-10-26 21:10 UTC: five GETs of the pinned URL for the day 2026-09-01, about 4 s apart, one per simulated observer, User-Agent `onyx-dev`. All five bodies were byte-identical (`TMAX` "30.6"; `tmax-usw00003017-2026-09-01/response.json`). The service sent no cache header; its `Content-Disposition` header names a generation time, which is not part of the body. |
| Requests made while building this adapter | 2 by the builder on 2026-10-26 (1 probe, 1 format check) plus the 5 recorded and 5 by one live test run; the NS3 research pass made its own separately. |

## More series (north-star loop NL17, `src/series.rs`)

| Field | Value |
|---|---|
| Series | `PRCP` (daily precipitation, mm to a tenth) at `USW00003017` (Denver International Airport); `TMAX` and `PRCP` at `USW00094728` ("NY CITY CENTRAL PARK, NY US", the service's station name). Metrics `noaa.ghcnd.usw00003017.prcp`, `noaa.ghcnd.usw00094728.tmax`, `noaa.ghcnd.usw00094728.prcp` |
| Dataset | The same Access Data Service URL with the station and `dataTypes` changed, `units=metric`, `format=json`: one row `{DATE, STATION, <element>}` |
| Terms and rights | As above (S25): both stations are U.S. National Weather Service (`USW`) stations, so the NWS public-domain statement and its three conditions apply; the same Onyx rights, the attribution naming the station |
| Revisions | GHCN-Daily's late updates cover every element of a station: the same 30 to 60-day lag. Not measured separately for `PRCP` |
| Value format | The same as `TMAX` (a JSON string, tenths, metric units); a negative precipitation is refused |
| Recorded | 2026-10-27 22:09-22:10 UTC: one GET per series for the day 2026-09-01, User-Agent `onyx-dev`: Denver `PRCP` "0.0", Central Park `TMAX` "27.8", Central Park `PRCP` "9.9" (`series-2026-09-01/`); the same values the spec's probe read |
| Requests made while building | 3 recordings on 2026-10-27, plus the spec's probes |

## NWS forecast of the daily high (north-star loop NL4, `src/nws.rs`)

| Field | Value |
|---|---|
| Source | U.S. National Weather Service API, gridpoint `BOU/75,66` (`/points/39.8466,-104.6562` names this cell for Denver International Airport), series `maxTemperature` in `wmoUnit:degC` |
| Terms and rights | NWS information "is in the public domain, unless specifically noted otherwise", on the three conditions quoted above (<https://www.weather.gov/disclaimer>); the API needs no key and asks for a User-Agent (<https://www.weather.gov/documentation/services-web-api>). Read 2026-10-28. Onyx sends `onyx-dev`, no contact details |
| Use | Testnet track-record statements relayed by an Onyx test key (`docs/ops/nws-relay.md`), graded by the GHCN-Daily TMAX above; never a proof level |
| Recorded | 2026-10-28 06:05 UTC: one GET with User-Agent `onyx-dev` (update time 2026-10-27 18:46:20 UTC). The fixture `nws-bou-75-66-2026-10-28/gridpoint.json` keeps only `gridId`, `gridX`, `gridY`, `updateTime`, `validTimes` and `maxTemperature` of the 249,944-byte response, re-serialised with sorted keys; the values are unchanged |
| Requests made while building | 3 on 2026-10-28 (a points probe, a 12-hour forecast probe, the recording), plus one GET per relay run (`scripts/track-nws-sepolia.sh`, kept with the run's state) |
