//! Spot bitcoin ETF holdings from SEC filings (ROADMAP P2c): a level-2
//! source. The committed data are recorded fetches replayed through the
//! pinned fetcher; no bonded observer round has run on these feeds yet
//! (ROADMAP P2h), so no value here is a settled level-2 value.
//!
//! What a fact says: "fund F's filings tag `us-gaap:<concept>` = N bitcoin
//! (unit U) at the period end D", read from EDGAR's `companyconcept` API
//! (data.sec.gov) by a pinned fetcher and converted exactly to satoshis.
//! It is a statement about a filing, never about coins on chain: the
//! fund reported it to a regulator, and Onyx does not check that the coins
//! exist (SEC-2).
//!
//! Which funds: every US spot bitcoin ETF or trust with an SEC filing was
//! found on EDGAR itself (full-text search and company search, 2026-10-19).
//! A fund is covered ([`FUNDS`]) only where its filings tag the whole
//! bitcoin holding as a fact without XBRL dimensions in a standard
//! (`us-gaap`) concept, since `companyconcept` serves only those; each one
//! was cross-checked (the tagged fair value divided by the tagged quantity
//! gives the same bitcoin price as the other funds' filings for the same
//! day; `fixtures/PROVENANCE-ETF.md`). The others are listed with the reason
//! ([`US_NOT_COVERED`], [`OUTSIDE_US`]).
//!
//! Flows ([`flows`]) are the change in reported holdings between two
//! consecutive filed period ends, with both dates. Filings are quarterly (a
//! 10-K once a year, some funds tag the concept only there): no daily figure
//! is ever derived from them (ETF-3).
//!
//! Fetcher contract (pinned by [`FETCHER_TAG`]): the feed must equal its
//! fund's template for its own subject and window; the response must be that
//! fund's `companyconcept` document (its `cik`, taxonomy `us-gaap`, the
//! fund's concept); every row of the fund's unit whose `end` is the period
//! end must be an instant fact (no `start`) whose `val` token converts
//! exactly to satoshis ([`btc_to_sats`]: a plain decimal, at most 8
//! significant fraction digits, at most 21,000,000 BTC), all rows must agree,
//! and there must be at least one; no other unit may hold a row for that
//! period. The answer is the satoshi count (scale 8). Anything else is
//! [`Answer::Unavailable`].

use crate::{rights, MAX_DAYS, MIN_DAYS, OBSERVERS, TAXONOMY, WINDOW_SECS};
use onyx_core::claim::{MetricId, Subject};
use onyx_core::{
    keccak256, observe, Answer, FeedTemplate, Fetcher, Hash32, MetricInfo, ObservationKind,
    ObservationSpec, StaticRegistry, SubjectRule, ValueKind,
};
use serde_json::value::RawValue;
use serde_json::{json, Value as Json};
use std::collections::BTreeMap;

/// Version tag the fetcher id is derived from.
pub const FETCHER_TAG: &[u8] = b"onyx-fetcher:edgar-etf-btc-holdings:v1";
/// Satoshis per bitcoin.
pub const SATS_PER_BTC: u64 = 100_000_000;
/// Most satoshis a holding may state: the 21,000,000 BTC supply cap.
pub const MAX_SATS: u64 = 21_000_000 * SATS_PER_BTC;
/// Decimal places of a feed value (satoshis as BTC with 8 decimals).
pub const SCALE: u8 = 8;
/// The User-Agent the recorder declares (SEC fair access asks for one with
/// a contact). The project has no role contact yet; never a personal
/// address (ETF-5, SEC-4).
pub const USER_AGENT: &str = "onyx-dev (contact pending)";
/// What has run on these feeds, as the page says it.
pub const STATUS: &str = "Level-2 source, recorded: three fetches from one machine replayed through the pinned fetcher; no bonded observer round has run on these feeds yet (ROADMAP P2h).";
/// The rerun command shown on the website.
pub const RERUN: &str = "cargo run -p onyx-source-edgar --example etf-holdings -- --out apps/web/static/level2/etf-holdings.json";
/// Why the funds in [`US_NOT_COVERED`] that lack a figure are not covered, said once on the page.
pub const NOT_COVERED_NOTE: &str = "These funds file 10-Qs and 10-Ks with the SEC. EDGAR's data.sec.gov API serves only standard facts without XBRL dimensions; reading a filing's own XBRL needs www.sec.gov, which refuses Onyx's User-Agent until it names a contact address.";
/// The flows note shown with the series (ETF-3).
pub const FLOWS_NOTE: &str = "Filings are quarterly (some funds tag the figure only in their annual 10-K), so a flow here is the change between two filed period ends, with both dates. Daily flows need the issuers' own daily holdings files: a separate source with its own terms check, not used here.";

/// A covered fund: its filings tag the whole bitcoin holding as a
/// dimensionless `us-gaap` fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fund {
    /// Short key used in metric ids and file names.
    pub key: &'static str,
    /// Exchange ticker.
    pub ticker: &'static str,
    /// The registrant's name as EDGAR lists it.
    pub name: &'static str,
    /// The fund's own name where the registrant is a trust of several.
    pub fund_name: &'static str,
    /// Sponsor.
    pub sponsor: &'static str,
    /// The ten-digit CIK.
    pub cik: &'static str,
    /// The `us-gaap` concept that carries the bitcoin quantity.
    pub concept: &'static str,
    /// The XBRL unit the fund tags it with.
    pub unit: &'static str,
}

/// The covered funds, by size of holding at the last common period.
pub const FUNDS: [Fund; 7] = [
    Fund {
        key: "ibit",
        ticker: "IBIT",
        name: "iShares Bitcoin Trust ETF",
        fund_name: "iShares Bitcoin Trust ETF",
        sponsor: "BlackRock (iShares Delaware Trust Sponsor)",
        cik: "0001980994",
        concept: "CryptoAssetNumberOfUnits",
        unit: "pure",
    },
    Fund {
        key: "fbtc",
        ticker: "FBTC",
        name: "Fidelity® Wise Origin® Bitcoin Fund",
        fund_name: "Fidelity Wise Origin Bitcoin Fund",
        sponsor: "Fidelity (FD Funds Management)",
        cik: "0001852317",
        concept: "InvestmentOwnedBalanceShares",
        unit: "shares",
    },
    Fund {
        key: "gbtc",
        ticker: "GBTC",
        name: "Grayscale Bitcoin Trust ETF",
        fund_name: "Grayscale Bitcoin Trust ETF",
        sponsor: "Grayscale Investments",
        cik: "0001588489",
        concept: "InvestmentOwnedBalanceContracts",
        unit: "Number",
    },
    Fund {
        key: "btc_mini",
        ticker: "BTC",
        name: "Grayscale Bitcoin Mini Trust ETF",
        fund_name: "Grayscale Bitcoin Mini Trust ETF",
        sponsor: "Grayscale Investments",
        cik: "0002015034",
        concept: "InvestmentOwnedBalanceContracts",
        unit: "Bitcoin",
    },
    Fund {
        key: "arkb",
        ticker: "ARKB",
        name: "ARK 21Shares Bitcoin ETF",
        fund_name: "ARK 21Shares Bitcoin ETF",
        sponsor: "21Shares US (with ARK Investment Management)",
        cik: "0001869699",
        concept: "InvestmentOwnedBalanceContracts",
        unit: "pure",
    },
    Fund {
        key: "ezbc",
        ticker: "EZBC",
        name: "Franklin Templeton Digital Holdings Trust",
        fund_name: "Franklin Bitcoin ETF",
        sponsor: "Franklin Templeton (Franklin Holdings)",
        cik: "0001992870",
        concept: "CryptoAssetNumberOfUnits",
        unit: "Bitcoin",
    },
    Fund {
        key: "defi",
        ticker: "DEFI",
        name: "Hashdex Commodities Trust",
        fund_name: "Hashdex Bitcoin ETF",
        sponsor: "Hashdex Asset Management",
        cik: "0001985840",
        concept: "InvestmentOwnedBalanceShares",
        unit: "shares",
    },
];

/// A product not covered, with the reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotCovered {
    /// Ticker(s) or product family.
    pub ticker: &'static str,
    /// Name.
    pub name: &'static str,
    /// The CIK, where the product is an SEC registrant.
    pub cik: Option<&'static str>,
    /// Why it is not covered.
    pub reason: &'static str,
}

/// Why a US fund's filings are not read yet: its bitcoin quantity is not a
/// dimensionless `us-gaap` fact, and the filing's own XBRL is on www.sec.gov,
/// which refuses a User-Agent without a contact (ROADMAP P2g).
const NEEDS_ARCHIVES: &str = "its quantity is tagged only in its own taxonomy or with XBRL dimensions, so it needs the filing itself on www.sec.gov (ROADMAP P2g)";

/// US spot bitcoin funds that file with the SEC but are not covered yet.
pub const US_NOT_COVERED: [NotCovered; 7] = [
    NotCovered {
        ticker: "BITB",
        name: "Bitwise Bitcoin ETF",
        cik: Some("0001763415"),
        reason: NEEDS_ARCHIVES,
    },
    NotCovered {
        ticker: "HODL",
        name: "VanEck Bitcoin ETF",
        cik: Some("0001838028"),
        reason: NEEDS_ARCHIVES,
    },
    NotCovered {
        ticker: "BRRR",
        name: "CoinShares Bitcoin ETF",
        cik: Some("0001841175"),
        reason: NEEDS_ARCHIVES,
    },
    NotCovered {
        ticker: "BTCO",
        name: "Invesco Galaxy Bitcoin ETF",
        cik: Some("0001855781"),
        reason: NEEDS_ARCHIVES,
    },
    NotCovered {
        ticker: "BTCW",
        name: "WisdomTree Bitcoin Fund",
        cik: Some("0001850391"),
        reason: NEEDS_ARCHIVES,
    },
    NotCovered {
        ticker: "MSBT",
        name: "Morgan Stanley Bitcoin Trust",
        cik: Some("0002103612"),
        reason: NEEDS_ARCHIVES,
    },
    NotCovered {
        ticker: "OBTC",
        name: "Osprey Bitcoin Trust",
        cik: Some("0001767057"),
        reason: "a US trust traded over the counter (not an ETF) that files with the SEC; its tagged quantity and fair value imply a bitcoin price far from every other fund's filing for the same two days (2025-12-31 below, 2026-06-30 above), so the tag is not taken as its whole holding until the filing text is read (ROADMAP P2g)",
    },
];

/// Products outside the US: no SEC periodic report exists for them.
pub const OUTSIDE_US: [NotCovered; 6] = [
    NotCovered {
        ticker: "21Shares ETPs",
        name: "21Shares bitcoin exchange-traded products (Europe)",
        cik: None,
        reason: "listed in Europe under an EU prospectus; EDGAR's company search lists 21Shares' US funds (ARKB is covered above) but no 21Shares bitcoin ETP",
    },
    NotCovered {
        ticker: "3iQ",
        name: "3iQ Bitcoin ETF (Canada)",
        cik: None,
        reason: "listed in Canada and reports to Canadian regulators (SEDAR+); EDGAR lists other 3iQ entities but not this fund",
    },
    NotCovered {
        ticker: "Bosera / HashKey",
        name: "Bosera HashKey Bitcoin ETF (Hong Kong)",
        cik: None,
        reason: "authorized in Hong Kong and listed on HKEX; not an SEC registrant",
    },
    NotCovered {
        ticker: "CoinShares ETPs",
        name: "CoinShares physical bitcoin ETPs (Europe)",
        cik: None,
        reason: "listed in Europe; not an SEC registrant (CoinShares' US fund BRRR is listed above)",
    },
    NotCovered {
        ticker: "ETC Group",
        name: "ETC Group bitcoin ETC (Germany)",
        cik: None,
        reason: "listed in Germany under an EU prospectus; EDGAR has no entity for it",
    },
    NotCovered {
        ticker: "Purpose",
        name: "Purpose Bitcoin ETF (Canada)",
        cik: None,
        reason: "listed in Canada and reports to Canadian regulators (SEDAR+); EDGAR's company search finds no Purpose bitcoin entity",
    },
];

/// The fund with key `key`.
pub fn fund(key: &str) -> Option<Fund> {
    FUNDS.iter().copied().find(|f| f.key == key)
}

/// The metric id of a fund's feed.
pub fn metric(f: &Fund) -> String {
    format!("sec.edgar.etf_btc_sats.{}", f.key)
}

/// The subject prefix: the fund by CIK.
pub fn subject_prefix(f: &Fund) -> String {
    format!("sec.cik{}.etf_btc_held", f.cik)
}

/// The period subject for one period end.
pub fn subject(f: &Fund, period_end: &str) -> String {
    format!("{}:{period_end}", subject_prefix(f))
}

/// The `companyconcept` URL for a fund's concept.
pub fn url(f: &Fund) -> String {
    format!(
        "https://data.sec.gov/api/xbrl/companyconcept/CIK{}/{TAXONOMY}/{}.json",
        f.cik, f.concept
    )
}

/// The EDGAR index page of a filing (for people; www.sec.gov).
pub fn filing_url(f: &Fund, accn: &str) -> String {
    let cik = f.cik.trim_start_matches('0');
    format!(
        "https://www.sec.gov/Archives/edgar/data/{cik}/{}/{accn}-index.htm",
        accn.replace('-', "")
    )
}

/// A fund's one canonical feed.
pub fn template(f: &Fund) -> FeedTemplate {
    FeedTemplate {
        feed: metric(f),
        metric: MetricId(metric(f)),
        fetcher: EtfFetcher.id(),
        url_template: url(f),
        selector: format!("units/{}/end={{date}}", f.unit),
        // A filed value does not move: every honest observer reads the same
        // satoshi count.
        kind: ObservationKind::Numeric {
            scale: SCALE,
            tolerance: 0,
        },
        window_secs: WINDOW_SECS,
        min_observers: OBSERVERS,
        rights: rights(),
        subject: SubjectRule::Period {
            prefix: subject_prefix(f),
            min_days: MIN_DAYS,
            max_days: MAX_DAYS,
        },
    }
}

/// The fund whose metric `m` is.
pub fn fund_of(m: &MetricId) -> Option<Fund> {
    FUNDS.iter().copied().find(|f| metric(f) == m.0)
}

/// The canonical feed for a fund's period, observed from `window_start`.
pub fn spec(f: &Fund, period_end: &str, window_start: u64) -> Result<ObservationSpec, String> {
    template(f)
        .instantiate(&Subject::Aggregate(subject(f, period_end)), window_start)
        .map_err(|e| format!("{} period {period_end}: {e}", f.key))
}

/// Whether `feed` is exactly its fund's canonical feed.
pub fn is_canonical(feed: &ObservationSpec) -> bool {
    fund_of(&feed.metric).is_some_and(|f| template(&f).matches(feed).is_ok())
}

/// A registry that knows the fund's metric (BTC with 8 decimals), the
/// period subject and the source.
pub fn registry(f: &Fund, period_end: &str) -> Result<StaticRegistry, String> {
    let mut r = StaticRegistry::new();
    r.add_metric(
        &metric(f),
        MetricInfo {
            value_kind: ValueKind::Fixed { scale: SCALE },
            needs_label_set: false,
        },
    )
    .and_then(|r| r.add_aggregate(&subject(f, period_end)))
    .and_then(|r| r.add_source(rights()))
    .map_err(|e| e.to_string())?;
    Ok(r)
}

/// Why a decimal string is not an exact satoshi amount.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SatsError {
    /// Not a plain non-negative decimal (sign, exponent, empty part, a
    /// leading zero, any other character).
    #[error("not a plain non-negative decimal")]
    NotDecimal,
    /// A non-zero digit past the 8th fraction digit: not whole satoshis.
    #[error("more precision than one satoshi")]
    SubSatoshi,
    /// More than the 21,000,000 BTC supply.
    #[error("more than the bitcoin supply cap")]
    AboveSupply,
}

/// The exact satoshi count of a decimal BTC string (`"138505.756804"` ->
/// 13,850,575,680,400). Accepts `0|[1-9][0-9]*` with an optional `.` and at
/// least one digit after it; digits past the 8th fraction digit must be zero.
/// No floats anywhere.
pub fn btc_to_sats(s: &str) -> Result<u64, SatsError> {
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (s, None),
    };
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    if !digits(int) || (int.len() > 1 && int.starts_with('0')) || frac.is_some_and(|f| !digits(f)) {
        return Err(SatsError::NotDecimal);
    }
    let frac = frac.unwrap_or("");
    let (kept, rest) = frac.split_at(frac.len().min(8));
    if rest.bytes().any(|b| b != b'0') {
        return Err(SatsError::SubSatoshi);
    }
    // 21,000,000 has 8 digits: anything longer is above the supply.
    if int.len() > 8 {
        return Err(SatsError::AboveSupply);
    }
    let whole: u64 = int.parse().map_err(|_| SatsError::NotDecimal)?;
    let mut part: u64 = if kept.is_empty() {
        0
    } else {
        kept.parse().map_err(|_| SatsError::NotDecimal)?
    };
    for _ in kept.len()..8 {
        part *= 10;
    }
    let sats = whole
        .checked_mul(SATS_PER_BTC)
        .and_then(|w| w.checked_add(part))
        .ok_or(SatsError::AboveSupply)?;
    if sats > MAX_SATS {
        return Err(SatsError::AboveSupply);
    }
    Ok(sats)
}

/// `sats` as BTC with 8 decimals (`"138505.75680400"`), exact.
pub fn sats_to_btc(sats: u64) -> String {
    format!("{}.{:08}", sats / SATS_PER_BTC, sats % SATS_PER_BTC)
}

/// A signed satoshi change as BTC with 8 decimals and a sign (`"-12.50000000"`).
pub fn signed_btc(change: i128) -> String {
    let sign = if change < 0 { "-" } else { "+" };
    let a = change.unsigned_abs();
    let w = u128::from(SATS_PER_BTC);
    format!("{sign}{}.{:08}", a / w, a % w)
}

/// Why a document does not state the period's holding.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// The body is not the expected JSON.
    #[error("not a companyconcept document")]
    NotJson,
    /// Another company, taxonomy or concept.
    #[error("another company, taxonomy or concept")]
    WrongDocument,
    /// No row of the fund's unit ends on the period end.
    #[error("no row for the period")]
    NoRow,
    /// Another unit also tags the period: the filing is ambiguous.
    #[error("another unit tags the same period")]
    OtherUnit,
    /// A row for the period has a start date (a duration, not a balance).
    #[error("a row for the period is a duration, not a balance")]
    Duration,
    /// A row's value is not an exact satoshi amount.
    #[error("value {0:?}: {1}")]
    BadNumber(String, SatsError),
    /// Rows for the period disagree (an amendment or a restatement).
    #[error("rows for the period disagree")]
    Disagree,
    /// A row for the period does not name its filing (accession, form, date).
    #[error("a row for the period names no filing")]
    NoProvenance,
}

/// One row of the document as EDGAR serves it.
type Row = BTreeMap<String, Box<RawValue>>;

/// The filing that first stated a period's value under the fund's concept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filed {
    /// Accession number.
    pub accn: String,
    /// Form (10-Q, 10-K, ...).
    pub form: String,
    /// Filing date.
    pub filed: String,
    /// Fiscal year and period EDGAR assigns.
    pub fy: String,
    /// Fiscal period.
    pub fp: String,
}

/// What a document states for one period.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    /// The holding in satoshis.
    pub sats: u64,
    /// The value token exactly as EDGAR serves it.
    pub as_filed: String,
    /// The earliest filing (by filing date, then accession) stating it.
    pub first: Filed,
    /// How many rows (filings and comparatives) state it.
    pub rows: usize,
}

fn text(row: &Row, key: &str) -> Option<String> {
    serde_json::from_str::<String>(row.get(key)?.get()).ok()
}

fn number_text(row: &Row, key: &str) -> Option<String> {
    row.get(key).map(|v| v.get().to_owned())
}

/// Parse a fund's `companyconcept` document into its unit rows, after
/// checking it is that fund's document.
fn units_of(f: &Fund, body: &[u8]) -> Result<BTreeMap<String, Vec<Row>>, Refusal> {
    let top: BTreeMap<String, Box<RawValue>> =
        serde_json::from_slice(body).map_err(|_| Refusal::NotJson)?;
    let cik_ok = top
        .get("cik")
        .and_then(|v| serde_json::from_str::<u64>(v.get()).ok())
        == f.cik.parse::<u64>().ok();
    let is = |k: &str, want: &str| {
        top.get(k)
            .and_then(|v| serde_json::from_str::<String>(v.get()).ok())
            .as_deref()
            == Some(want)
    };
    if !cik_ok || !is("taxonomy", TAXONOMY) || !is("tag", f.concept) {
        return Err(Refusal::WrongDocument);
    }
    let units = top.get("units").ok_or(Refusal::NotJson)?;
    serde_json::from_str(units.get()).map_err(|_| Refusal::NotJson)
}

/// What fund `f`'s document `body` states for period end `date`.
pub fn read(f: &Fund, date: &str, body: &[u8]) -> Result<Reading, Refusal> {
    let units = units_of(f, body)?;
    for (unit, rows) in &units {
        if unit != f.unit && rows.iter().any(|r| text(r, "end").as_deref() == Some(date)) {
            return Err(Refusal::OtherUnit);
        }
    }
    let rows: Vec<&Row> = units
        .get(f.unit)
        .map(|rs| {
            rs.iter()
                .filter(|r| text(r, "end").as_deref() == Some(date))
                .collect()
        })
        .unwrap_or_default();
    if rows.is_empty() {
        return Err(Refusal::NoRow);
    }
    // Every row: a balance, an exact satoshi amount, and its filing named.
    let mut read_rows: Vec<(u64, String, Filed)> = Vec::new();
    for r in &rows {
        if r.contains_key("start") {
            return Err(Refusal::Duration);
        }
        let tok = number_text(r, "val").ok_or(Refusal::NotJson)?;
        let sats = btc_to_sats(&tok).map_err(|e| Refusal::BadNumber(tok.clone(), e))?;
        let filed = (|| {
            Some(Filed {
                accn: text(r, "accn")?,
                form: text(r, "form")?,
                filed: text(r, "filed")?,
                fy: number_text(r, "fy").unwrap_or_default(),
                fp: text(r, "fp").unwrap_or_default(),
            })
        })()
        .ok_or(Refusal::NoProvenance)?;
        read_rows.push((sats, tok, filed));
    }
    let (sats, as_filed, first) = read_rows
        .iter()
        .min_by(|a, b| (&a.2.filed, &a.2.accn).cmp(&(&b.2.filed, &b.2.accn)))
        .cloned()
        .ok_or(Refusal::NoRow)?;
    if read_rows.iter().any(|(v, _, _)| *v != sats) {
        return Err(Refusal::Disagree);
    }
    Ok(Reading {
        sats,
        as_filed,
        first,
        rows: rows.len(),
    })
}

/// Every period end the fund's unit tags in `body`, ascending.
pub fn period_ends(f: &Fund, body: &[u8]) -> Result<Vec<String>, Refusal> {
    let units = units_of(f, body)?;
    let mut ends: Vec<String> = units
        .get(f.unit)
        .into_iter()
        .flatten()
        .filter_map(|r| text(r, "end"))
        .collect();
    ends.sort();
    ends.dedup();
    Ok(ends)
}

/// The ETF holdings fetcher.
#[derive(Clone, Copy, Debug, Default)]
pub struct EtfFetcher;

impl Fetcher for EtfFetcher {
    fn id(&self) -> Hash32 {
        keccak256(FETCHER_TAG)
    }

    fn extract(&self, spec: &ObservationSpec, page: &[u8]) -> Answer {
        let Some(f) = fund_of(&spec.metric) else {
            return Answer::Unavailable;
        };
        let t = template(&f);
        if t.matches(spec).is_err() {
            return Answer::Unavailable;
        }
        let Some(date) = t.date_of(&spec.subject) else {
            return Answer::Unavailable;
        };
        match read(&f, date, page) {
            Ok(r) => Answer::Number(i128::from(r.sats)),
            Err(_) => Answer::Unavailable,
        }
    }
}

/// One recorded observer fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetch {
    /// Observer number (1-based).
    pub observer: u8,
    /// Local clock just before the request.
    pub fetched_unix: u64,
    /// The server's `Date` header.
    pub http_date: String,
    /// SHA-256 of the body, as the recorder wrote it.
    pub sha256: String,
}

/// One fund's recording.
#[derive(Clone, Debug)]
pub struct EtfRecording {
    /// The fund.
    pub fund: Fund,
    /// Window start of the recording.
    pub window_start: u64,
    /// Fetches in observer order (all received `body`).
    pub fetches: Vec<Fetch>,
    /// The response body.
    pub body: &'static [u8],
    /// SHA-256 of the body the manifest states.
    pub sha256: String,
}

/// The recording manifest (scripts/record-sec-etf-holdings.py).
pub const MANIFEST: &str = include_str!("../fixtures/etf/manifest.json");

/// The recorded body of fund `key`.
pub fn body(key: &str) -> Option<&'static [u8]> {
    Some(match key {
        "ibit" => include_bytes!("../fixtures/etf/ibit.json"),
        "fbtc" => include_bytes!("../fixtures/etf/fbtc.json"),
        "gbtc" => include_bytes!("../fixtures/etf/gbtc.json"),
        "btc_mini" => include_bytes!("../fixtures/etf/btc_mini.json"),
        "arkb" => include_bytes!("../fixtures/etf/arkb.json"),
        "ezbc" => include_bytes!("../fixtures/etf/ezbc.json"),
        "defi" => include_bytes!("../fixtures/etf/defi.json"),
        _ => return None,
    })
}

/// Parse the manifest: one recording per fund in [`FUNDS`] order. Refuses a
/// manifest whose fund list, URLs or body hashes differ from the registry
/// and the committed bodies.
pub fn recordings(manifest: &str) -> Result<Vec<EtfRecording>, String> {
    let m: Json = serde_json::from_str(manifest).map_err(|e| e.to_string())?;
    let window_start = m
        .get("window_start")
        .and_then(Json::as_u64)
        .ok_or("manifest window_start missing")?;
    let rows = m
        .get("funds")
        .and_then(Json::as_array)
        .ok_or("manifest funds missing")?;
    if rows.len() != FUNDS.len() {
        return Err("manifest funds differ from the registry".into());
    }
    let mut out = Vec::new();
    for (f, r) in FUNDS.iter().zip(rows) {
        if r.get("key").and_then(Json::as_str) != Some(f.key) {
            return Err(format!("manifest fund order differs at {}", f.key));
        }
        if r.get("url").and_then(Json::as_str) != Some(url(f).as_str()) {
            return Err(format!(
                "{}: manifest url is not the fund's pinned url",
                f.key
            ));
        }
        let body = body(f.key).ok_or("no body")?;
        let sha256 = r
            .get("sha256")
            .and_then(Json::as_str)
            .ok_or("sha256 missing")?
            .to_owned();
        let fetches = r
            .get("fetches")
            .and_then(Json::as_array)
            .ok_or("fetches missing")?
            .iter()
            .map(|x| {
                Some(Fetch {
                    observer: u8::try_from(x.get("observer")?.as_u64()?).ok()?,
                    fetched_unix: x.get("fetched_unix")?.as_u64()?,
                    http_date: x.get("http_date")?.as_str()?.to_owned(),
                    sha256: x.get("sha256")?.as_str()?.to_owned(),
                })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or(format!("{}: a fetch is malformed", f.key))?;
        if fetches.iter().any(|x| x.sha256 != sha256) {
            return Err(format!("{}: observers' bodies differ", f.key));
        }
        if sha256_hex(body) != sha256 {
            return Err(format!("{}: body does not match its SHA-256", f.key));
        }
        out.push(EtfRecording {
            fund: *f,
            window_start,
            fetches,
            body,
            sha256,
        });
    }
    Ok(out)
}

/// Lowercase hex SHA-256 (the recorder's fixture hash; protocol hashes stay
/// Keccak-256).
pub fn sha256_hex(b: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(b))
}

/// The committed recordings.
pub fn recorded() -> Result<Vec<EtfRecording>, String> {
    recordings(MANIFEST)
}

/// One filed period of a fund, read by every recorded observer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Point {
    /// Period end.
    pub period_end: String,
    /// What the document states.
    pub reading: Reading,
    /// Observers whose recorded fetch gave this value.
    pub agreed: usize,
}

/// A period end the fetcher refused, with the reason.
pub type Refused = (String, String);

/// A recorded fund's observable periods: each read by every observer
/// through `observe` on the canonical feed. Periods outside the 110..=730
/// day rule are skipped; a period the fetcher refuses is returned with its
/// reason.
pub fn series(rec: &EtfRecording) -> Result<(Vec<Point>, Vec<Refused>), String> {
    let f = &rec.fund;
    let mut points = Vec::new();
    let mut refused = Vec::new();
    for end in period_ends(f, rec.body).map_err(|e| e.to_string())? {
        let Ok(s) = spec(f, &end, rec.window_start) else {
            continue;
        };
        match read(f, &end, rec.body) {
            Ok(reading) => {
                let mut agreed = 0;
                for x in &rec.fetches {
                    let o = observe(&EtfFetcher, &s, Some(rec.body), x.fetched_unix)
                        .map_err(|e| format!("{} {end}: {e}", f.key))?;
                    if o.answer == Answer::Number(i128::from(reading.sats)) {
                        agreed += 1;
                    }
                }
                points.push(Point {
                    period_end: end,
                    reading,
                    agreed,
                });
            }
            Err(e) => refused.push((end, e.to_string())),
        }
    }
    Ok((points, refused))
}

/// The change between two consecutive filed periods.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Flow {
    /// Earlier period end.
    pub from: String,
    /// Later period end.
    pub to: String,
    /// Calendar days between them.
    pub days: i64,
    /// Holding at `from`.
    pub from_sats: u64,
    /// Holding at `to`.
    pub to_sats: u64,
    /// `to_sats - from_sats`.
    pub change_sats: i128,
    /// Filing that stated `from`.
    pub from_accn: String,
    /// Filing that stated `to`.
    pub to_accn: String,
}

/// Flows between consecutive points (ascending period ends); never a
/// figure for a day no filing states, and never across a period end in
/// `refused` (a tagged period the fetcher refused breaks the chain).
pub fn flows(points: &[Point], refused: &[String]) -> Vec<Flow> {
    points
        .windows(2)
        .filter(|w| {
            !refused.iter().any(|d| {
                d.as_str() > w[0].period_end.as_str() && d.as_str() < w[1].period_end.as_str()
            })
        })
        .map(|w| {
            let (a, b) = (&w[0], &w[1]);
            let day = |d: &str| onyx_core::day_number(d).unwrap_or_default();
            Flow {
                from: a.period_end.clone(),
                to: b.period_end.clone(),
                days: day(&b.period_end) - day(&a.period_end),
                from_sats: a.reading.sats,
                to_sats: b.reading.sats,
                change_sats: i128::from(b.reading.sats) - i128::from(a.reading.sats),
                from_accn: a.reading.first.accn.clone(),
                to_accn: b.reading.first.accn.clone(),
            }
        })
        .collect()
}

fn not_covered_json(n: &NotCovered) -> Json {
    json!({"ticker": n.ticker, "name": n.name, "cik": n.cik, "reason": n.reason})
}

/// The website summary (`apps/web/static/level2/etf-holdings.json`),
/// computed from the committed recordings only. The browser verifier
/// recomputes it and refuses any other file (ETF-4).
pub fn site_summary() -> Result<Json, String> {
    let recs = recorded()?;
    let mut funds = Vec::new();
    for rec in &recs {
        let f = &rec.fund;
        let (points, refused) = series(rec)?;
        let gaps: Vec<String> = refused.iter().map(|(d, _)| d.clone()).collect();
        let fl = flows(&points, &gaps);
        funds.push(json!({
            "key": f.key,
            "ticker": f.ticker,
            "name": f.name,
            "fund_name": f.fund_name,
            "sponsor": f.sponsor,
            "cik": f.cik,
            "fact": format!("{TAXONOMY}:{}", f.concept),
            "unit": f.unit,
            "url": url(f),
            "metric": metric(f),
            "template_id": template(f).id().to_string(),
            "body_sha256": rec.sha256,
            "observers": rec.fetches.len(),
            "holdings": points.iter().map(|p| json!({
                "period_end": p.period_end,
                "held_sats": p.reading.sats.to_string(),
                "held_btc": sats_to_btc(p.reading.sats),
                "as_filed": p.reading.as_filed,
                "accn": p.reading.first.accn,
                "form": p.reading.first.form,
                "filed": p.reading.first.filed,
                "filing_url": filing_url(f, &p.reading.first.accn),
                "rows": p.reading.rows,
            })).collect::<Vec<_>>(),
            "refused": refused.iter().map(|(d, why)| json!({"period_end": d, "reason": why})).collect::<Vec<_>>(),
            "flows": fl.iter().map(|x| json!({
                "from": x.from,
                "to": x.to,
                "days": x.days,
                "from_sats": x.from_sats.to_string(),
                "to_sats": x.to_sats.to_string(),
                "change_sats": x.change_sats.to_string(),
                "change_btc": signed_btc(x.change_sats),
                "from_accn": x.from_accn,
                "to_accn": x.to_accn,
            })).collect::<Vec<_>>(),
        }));
    }
    let window_start = recs.first().map(|r| r.window_start).unwrap_or_default();
    Ok(json!({
        "level": 2,
        "status": STATUS,
        "source": {
            "name": crate::SOURCE_NAME,
            "terms_url": crate::TERMS_URL,
            "terms_quote": crate::TERMS_QUOTE,
            "attribution": rights().attribution,
            "user_agent": USER_AGENT,
        },
        "fetcher_id": EtfFetcher.id().to_string(),
        "window_start": window_start,
        "rerun": RERUN,
        "flows_note": FLOWS_NOTE,
        "funds": funds,
        "not_covered_note": NOT_COVERED_NOTE,
        "us_not_covered": US_NOT_COVERED.iter().map(not_covered_json).collect::<Vec<_>>(),
        "outside_us": OUTSIDE_US.iter().map(not_covered_json).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(key: &str) -> EtfRecording {
        recorded()
            .expect("manifest")
            .into_iter()
            .find(|r| r.fund.key == key)
            .expect("fund")
    }

    /// ETF-1: decimals convert to satoshis exactly or are refused.
    #[test]
    fn decimal_strings_convert_to_satoshis_exactly_or_are_refused() {
        let ok = [
            ("0", 0),
            ("1", 100_000_000),
            ("0.00000001", 1),
            ("138505.756804", 13_850_575_680_400),
            ("945.07", 94_507_000_000),
            ("32178.228", 3_217_822_800_000),
            ("1.000000000000", 100_000_000),
            ("21000000", MAX_SATS),
            ("20999999.99999999", MAX_SATS - 1),
        ];
        for (s, want) in ok {
            assert_eq!(btc_to_sats(s), Ok(want), "{s}");
            assert_eq!(btc_to_sats(&sats_to_btc(want)), Ok(want), "round trip {s}");
        }
        let bad = [
            ("", SatsError::NotDecimal),
            ("-1", SatsError::NotDecimal),
            ("+1", SatsError::NotDecimal),
            ("1e3", SatsError::NotDecimal),
            ("1.5E2", SatsError::NotDecimal),
            ("01", SatsError::NotDecimal),
            (".5", SatsError::NotDecimal),
            ("5.", SatsError::NotDecimal),
            ("1,000", SatsError::NotDecimal),
            (" 1", SatsError::NotDecimal),
            ("\"1\"", SatsError::NotDecimal),
            ("NaN", SatsError::NotDecimal),
            ("0.000000001", SatsError::SubSatoshi),
            ("1.123456789", SatsError::SubSatoshi),
            ("21000000.00000001", SatsError::AboveSupply),
            ("99999999999999999999", SatsError::AboveSupply),
        ];
        for (s, want) in bad {
            assert_eq!(btc_to_sats(s), Err(want), "{s}");
        }
        assert_eq!(sats_to_btc(13_850_575_680_400), "138505.75680400");
        assert_eq!(signed_btc(-1_250_000_000), "-12.50000000");
        assert_eq!(signed_btc(1), "+0.00000001");
    }

    /// The recorded filings read exactly, with provenance (ETF-2).
    #[test]
    fn the_recorded_filings_read_exactly_with_their_accession() {
        let want = [
            (
                "ibit",
                "2026-06-30",
                73_426_100_000_000u64,
                "0001437749-26-026004",
                "10-Q",
            ),
            (
                "fbtc",
                "2025-12-31",
                20_168_400_000_000,
                "0001193125-26-071484",
                "10-K",
            ),
            (
                "gbtc",
                "2026-06-30",
                13_850_575_680_400,
                "0001588489-26-000005",
                "10-Q",
            ),
            (
                "btc_mini",
                "2026-06-30",
                5_428_040_050_100,
                "0002015034-26-000008",
                "10-Q",
            ),
            (
                "arkb",
                "2026-06-30",
                3_217_822_800_000,
                "0001213900-26-086191",
                "10-Q",
            ),
            (
                "ezbc",
                "2026-06-30",
                566_401_630_000,
                "0001140361-26-033208",
                "10-Q",
            ),
            (
                "defi",
                "2026-06-30",
                22_558_000_000,
                "0001213900-26-088483",
                "10-Q",
            ),
        ];
        for (key, date, sats, accn, form) in want {
            let r = rec(key);
            let got = read(&r.fund, date, r.body).expect(key);
            assert_eq!(
                (got.sats, got.first.accn.as_str(), got.first.form.as_str()),
                (sats, accn, form),
                "{key}"
            );
            let s = spec(&r.fund, date, r.window_start).expect("canonical");
            s.validate().expect("valid feed");
            for x in &r.fetches {
                let o = observe(&EtfFetcher, &s, Some(r.body), x.fetched_unix).expect("window");
                assert_eq!(o.answer, Answer::Number(i128::from(sats)), "{key}");
                assert_eq!(o.page_hash, keccak256(r.body));
            }
        }
    }

    /// ETF-6: every committed body matches the manifest's SHA-256, and the
    /// manifest matches the registry.
    #[test]
    fn the_manifest_matches_the_registry() {
        let recs = recorded().expect("manifest");
        assert_eq!(recs.len(), FUNDS.len());
        for r in &recs {
            assert_eq!(r.fetches.len(), usize::from(OBSERVERS));
            assert!(r.fetches.iter().all(|x| x.fetched_unix >= r.window_start
                && x.fetched_unix < r.window_start + WINDOW_SECS));
        }
        let bad = MANIFEST.replace("CIK0001980994", "CIK0001852317");
        assert!(recordings(&bad).is_err(), "another fund's url");
        let bad = MANIFEST.replacen("\"key\": \"fbtc\"", "\"key\": \"gbtc\"", 1);
        assert!(recordings(&bad).is_err(), "fund order");
        let sha = &recs[0].sha256;
        let bad = MANIFEST.replace(sha.as_str(), &"0".repeat(64));
        assert!(recordings(&bad).is_err(), "edited body hash");
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// ETF-2: ambiguous, edited or foreign documents are refused.
    #[test]
    fn ambiguous_or_foreign_documents_are_refused() {
        let r = rec("gbtc");
        let f = r.fund;
        let body = std::str::from_utf8(r.body).expect("utf8");
        let row = "\"end\":\"2026-06-30\",\"val\":138505.756804,";
        assert!(body.contains(row), "fixture holds the row");
        let edit = |to: &str| body.replace(row, to);
        let cases = [
            (
                edit("\"end\":\"2026-06-30\",\"val\":-138505.756804,"),
                "sign",
            ),
            (
                edit("\"end\":\"2026-06-30\",\"val\":1.38505756804e5,"),
                "exponent",
            ),
            (
                edit("\"end\":\"2026-06-30\",\"val\":\"138505.756804\","),
                "string",
            ),
            (
                edit("\"end\":\"2026-06-30\",\"val\":138505.756804001,"),
                "sub-satoshi",
            ),
            (
                edit("\"end\":\"2026-06-30\",\"val\":21000000.1,"),
                "above supply",
            ),
            (
                edit("\"end\":\"2026-06-30\",\"start\":\"2026-04-01\",\"val\":138505.756804,"),
                "duration",
            ),
            (
                edit("\"end\":\"2026-06-30\",\"val\":138505.756804,\"x\":0},{\"end\":\"2026-06-30\",\"val\":138505.756805,"),
                "amendment disagrees",
            ),
            (body.replace("\"cik\":1588489", "\"cik\":2015034"), "cik"),
            (
                body.replace(
                    "\"tag\":\"InvestmentOwnedBalanceContracts\"",
                    "\"tag\":\"InvestmentOwnedBalanceShares\"",
                ),
                "concept",
            ),
            (
                body.replace("\"taxonomy\":\"us-gaap\"", "\"taxonomy\":\"gbtc\""),
                "taxonomy",
            ),
            (
                body.replace("\"end\":\"2026-06-30\"", "\"end\":\"2026-06-29\""),
                "no row",
            ),
            ("<html>Request Rate Threshold Exceeded</html>".into(), "html"),
        ];
        for (c, why) in &cases {
            assert!(read(&f, "2026-06-30", c.as_bytes()).is_err(), "{why}");
        }
        // Another unit tagging the same period is ambiguous.
        let other = body.replacen(
            "\"NUMBER\":[",
            "\"NUMBER\":[{\"end\":\"2026-06-30\",\"val\":1,\"accn\":\"x\",\"form\":\"10-Q\",\"filed\":\"2026-08-04\"},",
            1,
        );
        assert_eq!(
            read(&f, "2026-06-30", other.as_bytes()),
            Err(Refusal::OtherUnit)
        );
        // Agreeing repeats (a comparative in a later filing) are fine and the
        // earliest filing is the provenance.
        let agree = edit("\"end\":\"2026-06-30\",\"val\":138505.756804,\"accn\":\"0000000000-27-000001\",\"form\":\"10-K\",\"filed\":\"2027-02-25\"},{\"end\":\"2026-06-30\",\"val\":138505.7568040,");
        let got = read(&f, "2026-06-30", agree.as_bytes()).expect("agree");
        assert_eq!(got.rows, 2);
        assert_eq!(got.first.accn, "0001588489-26-000005");
        assert_eq!(got.as_filed, "138505.7568040", "token of the chosen row");
        // A row that names no filing is refused, not skipped.
        let anon = edit("\"end\":\"2026-06-30\",\"val\":138505.756804,\"x\":0},{\"end\":\"2026-06-30\",\"val\":138505.756804}],\"y\":[{\"end\":\"1999-01-01\",\"val\":1,");
        assert_eq!(
            read(&f, "2026-06-30", anon.as_bytes()),
            Err(Refusal::NoProvenance)
        );
        // ARKB's own 2024-03-31 rows disagree (44667.3162 vs 44667.3165).
        let a = rec("arkb");
        assert_eq!(read(&a.fund, "2024-03-31", a.body), Err(Refusal::Disagree));
    }

    /// SEC-1 for ETFs: only the fund's canonical feed is read.
    #[test]
    fn only_the_funds_canonical_feed_is_read() {
        let r = rec("ibit");
        let canon = spec(&r.fund, "2026-06-30", r.window_start).expect("canonical");
        assert!(is_canonical(&canon));
        assert_eq!(
            EtfFetcher.extract(&canon, r.body),
            Answer::Number(73_426_100_000_000)
        );
        let mut other_url = canon.clone();
        other_url.url = url(&FUNDS[1]);
        let mut host = canon.clone();
        host.url = host.url.replace("data.sec.gov", "evil.example");
        let mut unit = canon.clone();
        unit.selector = "units/Bitcoin/end=2026-06-30".into();
        let mut scale = canon.clone();
        scale.kind = ObservationKind::Numeric {
            scale: 0,
            tolerance: 0,
        };
        let mut subj = canon.clone();
        subj.subject = Subject::Aggregate(subject(&FUNDS[2], "2026-06-30"));
        let mut tol = canon.clone();
        tol.kind = ObservationKind::Numeric {
            scale: SCALE,
            tolerance: 1,
        };
        for (name, s) in [
            ("url", other_url),
            ("host", host),
            ("unit", unit),
            ("scale", scale),
            ("subject", subj),
            ("tolerance", tol),
        ] {
            assert!(!is_canonical(&s), "{name}");
            assert_eq!(
                EtfFetcher.extract(&s, r.body),
                Answer::Unavailable,
                "{name}"
            );
        }
        // The company fetcher's feed is not read by the ETF fetcher.
        let c = crate::COMPANIES[0];
        let cs = crate::spec(&c, "2026-03-31", r.window_start).expect("company");
        assert_eq!(EtfFetcher.extract(&cs, r.body), Answer::Unavailable);
        assert_ne!(EtfFetcher.id(), crate::EdgarFetcher.id());
    }

    /// ETF-3: flows are only between filed period ends, with both dates.
    #[test]
    fn flows_are_between_filed_period_ends_only() {
        let r = rec("ibit");
        let (points, refused) = series(&r).expect("series");
        assert!(refused.is_empty());
        // IBIT's concept skips 2025-09-30: the flow spans two quarters and
        // says so, never inventing the missing quarter.
        let ends: Vec<&str> = points.iter().map(|p| p.period_end.as_str()).collect();
        assert_eq!(
            ends,
            [
                "2024-12-31",
                "2025-03-31",
                "2025-06-30",
                "2025-12-31",
                "2026-03-31",
                "2026-06-30"
            ]
        );
        let fl = flows(&points, &[]);
        assert_eq!(fl.len(), points.len() - 1);
        let gap = &fl[2];
        assert_eq!(
            (gap.from.as_str(), gap.to.as_str(), gap.days),
            ("2025-06-30", "2025-12-31", 184)
        );
        assert_eq!(gap.change_sats, 77_079_200_000_000 - 69_687_500_000_000);
        let last = fl.last().expect("flow");
        assert_eq!(last.change_sats, 73_426_100_000_000 - 78_374_400_000_000);
        // Only the fund's own unit names its periods: GBTC's older "NUMBER" rows
        // (to 2024-03-31) are not periods of its "Number" feed (review mutant).
        let r2 = rec("gbtc");
        let ends = period_ends(&r2.fund, r2.body).expect("ends");
        assert!(ends.iter().any(|e| e == "2024-06-30"));
        assert!(!ends.iter().any(|e| e == "2024-03-31" || e == "2022-06-30"));
        // A row of another unit at a date the fund's unit never tags is not a
        // period: no refused gap appears and no flow is broken by it.
        let body = std::str::from_utf8(r2.body).expect("utf8").replacen(
            "\"NUMBER\":[",
            "\"NUMBER\":[{\"end\":\"2025-08-31\",\"val\":1,\"accn\":\"x\",\"form\":\"10-Q\",\"filed\":\"2025-10-01\"},",
            1,
        );
        let leaked: &'static [u8] = Box::leak(body.into_bytes().into_boxed_slice());
        let (p2, ref2) = series(&EtfRecording { body: leaked, ..r2 }).expect("series");
        assert!(ref2.is_empty(), "{ref2:?}");
        assert!(p2.iter().all(|p| p.period_end != "2025-08-31"));
        // A period outside 110..=730 days of the recording is not served.
        let g = rec("gbtc");
        let (gp, _) = series(&g).expect("series");
        assert_eq!(
            gp.first().map(|p| p.period_end.as_str()),
            Some("2024-12-31")
        );
        assert_eq!(flows(&[], &[]).len(), 0);
        assert_eq!(flows(&gp[..1], &[]).len(), 0);
        // A refused period between two points breaks the chain (no bridged flow).
        let bridged = flows(&points, &["2025-09-30".to_owned()]);
        assert_eq!(bridged.len(), fl.len() - 1);
        assert!(bridged.iter().all(|x| x.from != "2025-06-30"));
        // Every fetch read the same body, so every observer gave the value.
        assert!(points.iter().all(|p| p.agreed == r.fetches.len()));
    }

    #[test]
    fn templates_are_valid_distinct_and_registered() {
        let ids: std::collections::BTreeSet<_> = FUNDS.iter().map(|f| template(f).id()).collect();
        assert_eq!(ids.len(), FUNDS.len());
        for f in &FUNDS {
            template(f).validate().expect("valid");
            assert!(template(f).is_dated());
            assert_eq!(fund_of(&MetricId(metric(f))), Some(*f));
            registry(f, "2026-06-30").expect("registry");
            assert!(crate::COMPANIES.iter().all(|c| c.cik != f.cik));
        }
        let ciks: std::collections::BTreeSet<_> = FUNDS
            .iter()
            .map(|f| f.cik)
            .chain(US_NOT_COVERED.iter().filter_map(|n| n.cik))
            .collect();
        assert_eq!(
            ciks.len(),
            FUNDS.len() + US_NOT_COVERED.len(),
            "no fund listed twice"
        );
        assert!(OUTSIDE_US.iter().all(|n| n.cik.is_none()));
    }

    /// The committed website file is exactly the summary of the recordings
    /// (`ONYX_BLESS=1` rewrites it).
    #[test]
    fn the_site_file_is_the_summary_of_the_recordings() {
        let want = site_summary().expect("summary");
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../apps/web/static/level2/etf-holdings.json"
        );
        let text = serde_json::to_string_pretty(&want).expect("json") + "\n";
        if std::env::var("ONYX_BLESS").is_ok() {
            std::fs::write(path, &text).expect("write");
        }
        let got = std::fs::read_to_string(path).expect("site file");
        assert_eq!(got, text, "rerun: {RERUN}");
    }
}
