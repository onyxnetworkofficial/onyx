//! The U.S. National Weather Service's forecast of the daily high at the
//! station's grid point, as a testnet forecaster for track records (north-star
//! loop NL4, ADR-0036 amendment 8, docs/spec/track-records-served.md).
//!
//! The forecast is the NWS gridpoint `maxTemperature` series of
//! [`FORECAST_URL`] (the grid cell of station [`crate::STATION`], Denver
//! International Airport): one value per local daytime, in °C with decimals.
//! [`parse_max_temperature`] turns a response into one value per day of the
//! station's local standard time (GHCN-Daily days are local standard time,
//! UTC-7 at Denver), rounded half away from zero to a tenth, the unit the
//! NCEI value that grades it uses. The number's JSON text is rounded with
//! integer arithmetic; no float reaches a value (ADR-0009).
//!
//! Rights: NWS information is "in the public domain, unless specifically noted
//! otherwise" (<https://www.weather.gov/disclaimer>), with the same three
//! conditions as the NCEI record (S25); the API needs no key and asks callers
//! for a User-Agent (Onyx sends `onyx-dev`, no contact details). See
//! `docs/ops/data-sources.md` S26.
//!
//! Testnet only, with a recorded exception (THREATS TR-R2-4, TRS-6): the NWS
//! forecast and the NCEI record that grades it are both NOAA, so the score is
//! a demonstration of the mechanism, never an independent track record.
//!
//! This module never touches the network: a caller does the HTTPS GET and
//! hands the bytes in.

use onyx_core::day_number;

/// The NWS gridpoint whose cell holds station `USW00003017`
/// (`/points/39.8466,-104.6562` answers office BOU, x 75, y 66).
pub const GRID_OFFICE: &str = "BOU";
/// Grid x of the station's cell.
pub const GRID_X: u64 = 75;
/// Grid y of the station's cell.
pub const GRID_Y: u64 = 66;
/// The raw gridpoint forecast (its `maxTemperature` series is read).
pub const FORECAST_URL: &str = "https://api.weather.gov/gridpoints/BOU/75,66";
/// The forecaster's source id in track records.
pub const FORECAST_SOURCE: &str = "noaa.nws.forecast.bou";
/// The NWS API documentation (User-Agent, no key).
pub const API_TERMS_URL: &str = "https://www.weather.gov/documentation/services-web-api";
/// The station's local standard time offset (GHCN-Daily days, MST = UTC-7).
pub const LST_OFFSET_SECS: i64 = -7 * 3_600;
/// Most days one response may carry (the NWS grid covers about a week).
pub const MAX_DAYS: usize = 16;

/// One forecast daily high.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DailyHigh {
    /// The station's local standard day, `YYYY-MM-DD`.
    pub day: String,
    /// The forecast high in tenths of a °C.
    pub tenths: i128,
}

/// A parsed forecast: when the NWS last updated the grid, and its daily highs
/// in ascending day order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Forecast {
    /// `updateTime` of the grid (unix seconds).
    pub updated: u64,
    /// One high per local day.
    pub highs: Vec<DailyHigh>,
}

/// `YYYY-MM-DD` of a day number (days since 1970-01-01), the inverse of
/// [`day_number`] (H. Hinnant's civil-from-days).
pub fn date_of_day(z: i64) -> Option<String> {
    let z = z.checked_add(719_468)?;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (1..=9999)
        .contains(&y)
        .then(|| format!("{y:04}-{m:02}-{d:02}"))
}

/// Unix seconds of `YYYY-MM-DDTHH:MM:SS+00:00` (UTC only).
pub fn parse_utc(s: &str) -> Option<u64> {
    let (date, rest) = s.split_once('T')?;
    let time = rest.strip_suffix("+00:00")?;
    let b = time.as_bytes();
    if b.len() != 8 || b[2] != b':' || b[5] != b':' {
        return None;
    }
    let num = |p: &str| -> Option<i64> {
        (p.len() == 2 && p.bytes().all(|c| c.is_ascii_digit())).then(|| p.parse().ok())?
    };
    let (h, m, sec) = (num(&time[0..2])?, num(&time[3..5])?, num(&time[6..8])?);
    if h > 23 || m > 59 || sec > 59 {
        return None;
    }
    let day = day_number(date)?;
    u64::try_from(day * 86_400 + h * 3_600 + m * 60 + sec).ok()
}

/// Tenths of a JSON number's text, rounded half away from zero: `20` is 200,
/// `17.77777777777778` is 178, `-0.05` is -1. Only plain decimals (an
/// optional `-`, digits, an optional point and digits) are accepted.
pub fn round_tenths(text: &str) -> Option<i128> {
    let (neg, rest) = match text.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, text),
    };
    let (int, frac) = rest.split_once('.').unwrap_or((rest, ""));
    let digits = |p: &str| p.bytes().all(|c| c.is_ascii_digit());
    if int.is_empty() || int.len() > 3 || !digits(int) || !digits(frac) {
        return None;
    }
    if rest.contains('.') && frac.is_empty() {
        return None;
    }
    let mut f = frac.bytes();
    let tenth = i128::from(f.next().map_or(0, |c| c - b'0'));
    let up = f.next().is_some_and(|c| c >= b'5');
    let v = int.parse::<i128>().ok()? * 10 + tenth + i128::from(up);
    Some(if neg { -v } else { v })
}

/// The station's local standard day of a forecast period starting at
/// `start` (unix seconds).
fn local_day(start: u64) -> Option<String> {
    let t = i64::try_from(start).ok()?.checked_add(LST_OFFSET_SECS)?;
    date_of_day(t.div_euclid(86_400))
}

/// Parse a gridpoint response: the grid must be [`GRID_OFFICE`] [`GRID_X`],
/// [`GRID_Y`], the unit `wmoUnit:degC`, every period start UTC and on a
/// distinct local day, every value a plain number. Anything else is an
/// error, never a guess.
pub fn parse_max_temperature(body: &[u8]) -> Result<Forecast, String> {
    let v: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("not JSON: {e}"))?;
    let p = &v["properties"];
    if p["gridId"].as_str() != Some(GRID_OFFICE)
        || p["gridX"].as_u64() != Some(GRID_X)
        || p["gridY"].as_u64() != Some(GRID_Y)
    {
        return Err("not the grid cell of the station".into());
    }
    let updated = p["updateTime"]
        .as_str()
        .and_then(parse_utc)
        .ok_or("no UTC updateTime")?;
    let m = &p["maxTemperature"];
    if m["uom"].as_str() != Some("wmoUnit:degC") {
        return Err("maxTemperature is not in degC".into());
    }
    let values = m["values"].as_array().ok_or("no maxTemperature values")?;
    if values.len() > MAX_DAYS {
        return Err("too many periods".into());
    }
    let mut highs: Vec<DailyHigh> = Vec::with_capacity(values.len());
    for x in values {
        let valid = x["validTime"].as_str().ok_or("no validTime")?;
        let (start, _) = valid
            .split_once('/')
            .ok_or("validTime without a duration")?;
        let start = parse_utc(start).ok_or_else(|| format!("validTime {valid} is not UTC"))?;
        let day = local_day(start).ok_or("validTime out of range")?;
        let n = x["value"].as_number().ok_or("value is not a number")?;
        let tenths = round_tenths(&n.to_string()).ok_or_else(|| format!("value {n}"))?;
        if highs.last().is_some_and(|h| h.day >= day) {
            return Err(format!("day {day} twice or out of order"));
        }
        highs.push(DailyHigh { day, tenths });
    }
    Ok(Forecast { updated, highs })
}

/// The forecast recorded for this module's tests (2026-10-28, User-Agent
/// `onyx-dev`; `fixtures/PROVENANCE.md`).
pub const RECORDED: &[u8] = include_bytes!("../fixtures/nws-bou-75-66-2026-10-28/gridpoint.json");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recorded_forecast_gives_one_high_per_local_day() {
        let f = parse_max_temperature(RECORDED).expect("forecast");
        assert_eq!(
            f.updated,
            parse_utc("2026-10-27T18:46:20+00:00").expect("t")
        );
        let days: Vec<(&str, i128)> = f.highs.iter().map(|h| (h.day.as_str(), h.tenths)).collect();
        assert_eq!(
            days,
            vec![
                ("2026-10-27", 172),
                ("2026-10-28", 200),
                ("2026-10-29", 178),
                ("2026-10-30", 161),
                ("2026-10-31", 161),
                ("2026-11-01", 150),
                ("2026-11-02", 106),
                ("2026-11-03", 106),
            ]
        );
    }

    #[test]
    fn rounding_is_half_away_from_zero_on_the_text() {
        for (s, t) in [
            ("20", 200),
            ("17.22222222222222", 172),
            ("17.25", 173),
            ("17.24999", 172),
            ("-0.05", -1),
            ("-3.33", -33),
            ("0", 0),
        ] {
            assert_eq!(round_tenths(s), Some(t), "{s}");
        }
        for s in ["", "-", "1e3", "1.", ".5", "1000", "+1", "1.2.3"] {
            assert_eq!(round_tenths(s), None, "{s}");
        }
    }

    #[test]
    fn days_and_times_round_trip() {
        for d in ["1970-01-01", "2026-10-28", "2000-02-29", "2026-12-31"] {
            assert_eq!(date_of_day(day_number(d).expect("day")).as_deref(), Some(d));
        }
        // 2026-10-28T06:00Z is 2026-10-27 23:00 MST: the station's day is the 27th.
        assert_eq!(
            local_day(parse_utc("2026-10-28T06:00:00+00:00").expect("t")).as_deref(),
            Some("2026-10-27")
        );
        for s in [
            "2026-10-28T14:00:00-06:00",
            "2026-10-28T24:00:00+00:00",
            "2026-10-28 14:00:00+00:00",
        ] {
            assert_eq!(parse_utc(s), None, "{s}");
        }
    }

    #[test]
    fn another_grid_unit_or_offset_is_refused() {
        let base: serde_json::Value = serde_json::from_slice(RECORDED).expect("json");
        let bad = |edit: &dyn Fn(&mut serde_json::Value)| {
            let mut v = base.clone();
            edit(&mut v);
            parse_max_temperature(v.to_string().as_bytes()).is_err()
        };
        assert!(bad(&|v| v["properties"]["gridX"] = 76.into()));
        assert!(bad(&|v| v["properties"]["gridId"] = "PUB".into()));
        assert!(bad(
            &|v| v["properties"]["maxTemperature"]["uom"] = "wmoUnit:degF".into()
        ));
        assert!(bad(&|v| v["properties"]["maxTemperature"]["values"][1]
            ["validTime"] =
            "2026-10-28T08:00:00-06:00/PT13H".into()));
        assert!(bad(
            &|v| v["properties"]["maxTemperature"]["values"][1]["value"] = "20".into()
        ));
        assert!(bad(&|v| v["properties"]["maxTemperature"]["values"][2] =
            v["properties"]["maxTemperature"]["values"][1].clone()));
    }
}
