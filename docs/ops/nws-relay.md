# NWS forecast relay (testnet only)

North-star loop NL4, ADR-0036 amendment 8. The U.S. National Weather Service signs nothing for
Onyx. On the Base Sepolia testnet an Onyx relay key transcribes the NWS gridpoint forecast of the
daily high at grid BOU 75,66 (the cell of station `USW00003017`, Denver International Airport) into
signed track-record statements of the source `noaa.nws.forecast.bou`, on metric
`noaa.ghcnd.usw00003017.tmax`. This document is the review record of that key: its hash is the
key's provenance in the example's registry (`adapters/source-ncei/examples/track-nws.rs`).

- **What the relay does.** It fetches `https://api.weather.gov/gridpoints/BOU/75,66` with
  User-Agent `onyx-dev` (no contact details), reads `maxTemperature`, and states each coming local
  day's high rounded to a tenth of a °C (`onyx_source_ncei::nws::parse_max_temperature`), with a
  declared confidence of 50%. Each fetched response is kept with the run, so a reader can redo the
  transcription.
- **What it does not do.** It never states a value the NWS response does not hold, and it never
  states on a day whose close is less than two hours away.
- **Who checks.** Nobody but the relay vouches that a statement matches the NWS response: the NWS
  keeps no public archive of past forecasts. A relayed statement is Onyx's word about the NWS.
- **Self-grading.** NOAA runs both the forecast and the NCEI record that grades it, so the record
  is a demonstration of the mechanism, never an independent track record (THREATS TR-R2-4 testnet
  exception, TRS-6, TRS-7).
- **Keys.** The relay key and the poster key live only on the operator's machine under
  `.onyx-testnet/keys/` (`track-nws-relay.key`, `track-nws-poster.key`); they are never printed or
  committed. The poster posts nothing but intake headers, so its nonce counts the posts.
- **Testnet only.** No mainnet use. A mainnet forecaster signs its own statements.
