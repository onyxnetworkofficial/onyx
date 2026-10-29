//! Level-5 track records in the browser (ROADMAP C6, ADR-0036 items 10-11).
//! Every check is [`onyx_track::wire::check_record_body`], the code a Rust
//! client runs: the record under the commitment, its metric's summary, and its
//! whole committed history recounted (`verify_history`). The page shows only
//! the fields returned here, never the JSON it loaded.
//!
//! No production metric has a track record yet (the Bitcoin flows have the
//! first close-time rule, C9, but no source states on them). The site
//! shows the example (`onyx_track::example`): its latest commitment is compiled
//! in below, so a data file with an older commitment (a stale one served as the
//! latest, THREATS TR-R2-5) or another one is refused even though its proofs
//! check. Fenced like every track file (scripts/check-boundaries.sh, TR-11).
//!
//! ROADMAP C20: the browser remembers the last commitment it saw per record
//! (the page keeps it, apps/web/src/lib/trackSeen.ts) and asks with `since`:
//! [`check_track_example_since`] checks that the served commitment chains
//! back to it, and refuses one the server does not know. The example's logs
//! are rerun here ([`check_track_example_log`]: `compute` with the example
//! registry, every served commitment extending the one before). A history
//! past 1,024 entries is checked over its pages ([`check_track_record_paged`]).
//!
//! ROADMAP C23: the rerun uses the registry the server serves
//! (`GET /v1/track/registry`; the site ships the example's as
//! `level5/track-example-registry.json`), rebuilt here with the browser's own
//! address rules ([`BtcRules`]: the Bitcoin canon and BIP-322 rule, nothing
//! else) and used only when it hashes to the commitment's `registry_root`.
//! [`check_track_log`] reruns a node's format-1 logs the same way, and
//! [`check_track_log_paged`] its chained (format-2) logs page by page
//! (ROADMAP C21). The site has no live node yet (LC-8), so only the example
//! runs on the page.

use onyx_core::{Hash32, StaticRegistry, Subject};
use onyx_source_btc::BtcRules;
use onyx_track::wire::{
    check_history_page, check_log_body, check_record_body, check_registry_body, complete_history,
    peek_commitment, resolver_level_numbers, Checked, CheckedLog, Expect, LogExpect, PagedLog,
    MAX_BODY_BYTES, MAX_HISTORY, MAX_LOG_BODY_BYTES, MAX_PAGED_LOG_BYTES, MAX_REGISTRY_BODY_BYTES,
};
use onyx_track::{Close, Outcome, Record};
use serde_json::{json, Value};
use wasm_bindgen::prelude::*;

/// The example's latest commitment (`onyx_track::example::chain()[1]`, as served
/// by `onyx_api::track_serve` into apps/web/static/level5/track-example.json).
pub const EXAMPLE_COMMITMENT: &str =
    "0xb0fe1f31095d1fb10968aa40f895527a80c0e00d82804b130e75d08a653f4324";
/// Its `as_of` (2026-10-07 00:00 UTC): the floor the example is checked with.
pub const EXAMPLE_AS_OF: u64 = 1_791_331_200;
/// The example's records, in the order the page shows them.
pub const EXAMPLE_SOURCES: [&str; 2] = ["example.forecast", "example.guess"];
/// The example metric.
pub const EXAMPLE_METRIC: &str = "example.signed.reserve";

fn subject_text(s: &Subject) -> &str {
    match s {
        Subject::Entity(x)
        | Subject::Address(x)
        | Subject::Instrument(x)
        | Subject::Aggregate(x) => x,
    }
}

fn outcome_text(o: Outcome) -> &'static str {
    match o {
        Outcome::Agreed => "agreed",
        Outcome::Disagreed => "disagreed",
        Outcome::Equivocated => "equivocated",
        Outcome::Late => "late",
        Outcome::Void => "void",
        Outcome::Contested => "contested",
        Outcome::Unresolved => "unresolved",
        Outcome::SelfResolved => "self-resolved",
    }
}

/// "91.1%" from ppm, floored to a tenth of a percent.
fn percent(ppm: u64) -> String {
    let pm = ppm / 1_000;
    format!("{}.{}%", pm / 10, pm % 10)
}

/// "+12.5 points" from a signed ppm difference of two rates, floored to a
/// tenth of a percentage point.
fn points(ppm: i64) -> String {
    let pm = ppm.div_euclid(1_000);
    let sign = if pm < 0 { "-" } else { "+" };
    let a = pm.unsigned_abs();
    format!("{sign}{}.{} points", a / 10, a % 10)
}

/// A Brier mean in units of 1e-12 as a decimal with four places, floored.
fn brier(e12: u128) -> String {
    let v = e12 / 100_000_000;
    format!("{}.{:04}", v / 10_000, v % 10_000)
}

/// What the page shows of one checked record (docs/spec/proof-ladder.md
/// "Display"): counts always; a rate and the mean Brier only when established.
fn view(c: &Checked) -> Value {
    let r: &Record = &c.record;
    let established = r.established();
    // Committed in fact-key order; shown by date (then subject). Display only: the check ran on
    // the committed order.
    let mut rows: Vec<&onyx_track::Entry> = c.entries.iter().collect();
    rows.sort_by(|a, b| {
        (a.fact.as_of, subject_text(&a.fact.subject))
            .cmp(&(b.fact.as_of, subject_text(&b.fact.subject)))
    });
    let rule = c.params.rule(&r.metric);
    let mut v = json!({
        "source": r.source.0,
        "metric": r.metric.0,
        "level": 5,
        "example": c.example,
        "established": established,
        "agreed": r.agreed,
        "resolved": r.resolved(),
        "rate": if established { r.agreement_ppm().map(percent) } else { None },
        "agreed_first": r.agreed_first,
        "mean_brier": if established { r.mean_brier_e12().map(brier) } else { None },
        "disagreed": r.disagreed,
        "equivocated": r.equivocated,
        "late": r.late,
        "void": r.void,
        "contested": r.contested,
        "self_resolved": r.self_resolved,
        "unresolved": r.unresolved,
        "distinct_subjects": r.distinct_subjects,
        "min_resolved": onyx_track::MIN_RESOLVED,
        "min_subjects": onyx_track::MIN_DISTINCT_SUBJECTS,
        "metric_sources": r.metric_sources,
        "metric_resolved_facts": r.metric_resolved_facts,
        "statements": r.entries_count,
        "history_checked": c.history_checked,
        "close_after_secs": rule.and_then(|x| match x.close {
            Close::AfterAsOf(secs) => Some(secs),
            Close::PeriodStart { .. } => None,
        }),
        "tolerance": rule.map(|x| x.tolerance.to_string()),
        "resolver_levels": resolver_level_numbers(&c.params),
        "intake": c.params.intake != Hash32::ZERO,
        "history": rows.iter().map(|e| json!({
            "subject": subject_text(&e.fact.subject),
            "as_of": e.fact.as_of,
            "outcome": outcome_text(e.outcome),
            "first": e.first,
            "confidence_ppm": e.confidence.0,
            "committed_at": e.committed_at,
        })).collect::<Vec<_>>(),
    });
    // C9: the period of a period rule, and the skill against persistence (a
    // signed difference of two rates; only when established, like the rate).
    let (period, lead) = match rule.map(|x| x.close) {
        Some(Close::PeriodStart {
            period_secs,
            lead_secs,
        }) => (Some(period_secs), Some(lead_secs)),
        _ => (None, None),
    };
    v["period_secs"] = json!(period);
    v["lead_secs"] = json!(lead);
    v["baseline_entries"] = json!(r.baseline_entries);
    v["baseline_source_agreed"] = json!(r.baseline_source_agreed);
    v["baseline_agreed"] = json!(r.baseline_agreed);
    v["skill"] = json!(if established {
        r.skill_ppm().map(points)
    } else {
        None
    });
    v
}

fn check_one(body: &Value, source: &str, metric: &str, min_as_of: u64) -> Result<Checked, String> {
    check_known(body, source, metric, min_as_of, None)
}

fn check_known(
    body: &Value,
    source: &str,
    metric: &str,
    min_as_of: u64,
    known: Option<Hash32>,
) -> Result<Checked, String> {
    check_record_body(
        body,
        &Expect {
            source,
            metric,
            min_as_of,
            known,
            latest: None,
        },
    )
    .map_err(|e| format!("{source}: {e}"))
}

/// `0x` and 64 lowercase hex digits, or `None`.
fn parse_hash(s: &str) -> Option<Hash32> {
    let d = s.strip_prefix("0x")?;
    if d.len() != 64 || d.bytes().any(|b| b.is_ascii_uppercase()) {
        return None;
    }
    let b: [u8; 32] = hex::decode(d).ok()?.try_into().ok()?;
    Some(Hash32(b))
}

/// What this browser remembered for `source` (`{commitment, as_of}`), or why
/// it cannot be used.
fn seen_of(seen: &Value, source: &str) -> Result<(Hash32, u64), String> {
    let s = &seen[source];
    match (
        s["commitment"].as_str().and_then(parse_hash),
        s["as_of"].as_u64(),
    ) {
        (Some(h), Some(t)) => Ok((h, t)),
        _ => Err(format!(
            "{source}: the commitment this browser remembered is not readable"
        )),
    }
}

/// Check the site's example file (`level5/track-example.json`): every record
/// passes [`check_record_body`] with the pinned `as_of` as its floor, is marked
/// example, and is under the pinned commitment. Returns `{ok, commitment,
/// as_of, records, problems}`; `records` is empty unless every check passed.
#[wasm_bindgen]
pub fn check_track_example(doc_json: &str) -> Result<String, JsError> {
    if doc_json.len() > MAX_BODY_BYTES {
        return Err(JsError::new("track example file too large"));
    }
    let doc: Value = serde_json::from_str(doc_json).map_err(|e| JsError::new(&e.to_string()))?;
    Ok(example_view(&doc).to_string())
}

fn example_view(doc: &Value) -> Value {
    let mut problems = Vec::new();
    let mut records = Vec::new();
    if doc["example"] != Value::Bool(true) {
        problems.push("the file is not marked example".to_string());
    }
    for source in EXAMPLE_SOURCES {
        match check_one(
            &doc["records"][source],
            source,
            EXAMPLE_METRIC,
            EXAMPLE_AS_OF,
        ) {
            Ok(c) if !c.example => problems.push(format!("{source}: not marked example")),
            // The example is small: its whole history must be recounted (review TR-S-R1-7).
            Ok(c) if !c.history_checked => problems.push(format!("{source}: history not served")),
            Ok(c) if c.commitment.hash().to_string() != EXAMPLE_COMMITMENT => {
                problems.push(format!("{source}: not under the pinned commitment"));
            }
            Ok(c) => records.push(view(&c)),
            Err(e) => problems.push(e),
        }
    }
    let ok = problems.is_empty();
    json!({
        "ok": ok,
        "commitment": EXAMPLE_COMMITMENT,
        "as_of": EXAMPLE_AS_OF,
        "records": if ok { records } else { Vec::new() },
        "problems": problems,
    })
}

/// Check the site's `since` file (`level5/track-example-since.json`: each
/// example record as the server answers `?since=` each example commitment)
/// for the commitments this browser saw last (`seen_json`: `{source:
/// {commitment, as_of}}`, ROADMAP C20 item 4): each record must chain back
/// by hash to its remembered commitment with `as_of` rising, be no older than
/// it, and be under the pinned commitment. A remembered commitment the server
/// does not know (a fork, another server) shows nothing and says so
/// (`unknown: true`). Returns the example view plus `since` per record.
#[wasm_bindgen]
pub fn check_track_example_since(doc_json: &str, seen_json: &str) -> Result<String, JsError> {
    if doc_json.len() > 2 * MAX_BODY_BYTES || seen_json.len() > 4096 {
        return Err(JsError::new("track since file too large"));
    }
    let doc: Value = serde_json::from_str(doc_json).map_err(|e| JsError::new(&e.to_string()))?;
    let seen: Value = serde_json::from_str(seen_json).map_err(|e| JsError::new(&e.to_string()))?;
    Ok(example_since_view(&doc, &seen).to_string())
}

fn example_since_view(doc: &Value, seen: &Value) -> Value {
    let mut problems = Vec::new();
    let mut records = Vec::new();
    let mut unknown = false;
    if doc["example"] != Value::Bool(true) {
        problems.push("the file is not marked example".to_string());
    }
    for source in EXAMPLE_SOURCES {
        let (known, as_of) = match seen_of(seen, source) {
            Ok(k) => k,
            Err(e) => {
                problems.push(e);
                continue;
            }
        };
        let body = &doc["since"][source][known.to_string()];
        if body.is_null() {
            unknown = true;
            problems.push(format!(
                "{source}: the server does not know the commitment this browser saw last (a fork, or another server)"
            ));
            continue;
        }
        match check_known(
            body,
            source,
            EXAMPLE_METRIC,
            EXAMPLE_AS_OF.max(as_of),
            Some(known),
        ) {
            Ok(c) if !c.example => problems.push(format!("{source}: not marked example")),
            Ok(c) if !c.history_checked => problems.push(format!("{source}: history not served")),
            Ok(c) if c.commitment.hash().to_string() != EXAMPLE_COMMITMENT => {
                problems.push(format!("{source}: not under the pinned commitment"));
            }
            Ok(c) => {
                let mut v = view(&c);
                v["since"] = json!(known.to_string());
                records.push(v);
            }
            Err(e) => problems.push(e),
        }
    }
    let ok = problems.is_empty();
    json!({
        "ok": ok,
        "commitment": EXAMPLE_COMMITMENT,
        "as_of": EXAMPLE_AS_OF,
        "records": if ok { records } else { Vec::new() },
        "problems": problems,
        "unknown": unknown,
    })
}

/// Rerun the example's logs (`level5/track-example-log.json`, the body of
/// `GET /v1/track/log?since=` the first example commitment; ROADMAP C20 item
/// 1): every served commitment's log hashes to it and extends the one
/// before, the latest is the pinned commitment, and `compute` on the logs
/// with the example registry gives it again. `known` (or empty) is the
/// commitment this browser saw last: the served commitments must include it.
/// ROADMAP C23: the registry is the served one (`registry_json`, the body of
/// `GET /v1/track/registry`), rebuilt with the browser's rules and checked to
/// the commitment's `registry_root` before the rerun. Returns `{ok,
/// commitment, links, statements, answers, recomputed, intake, registry_root,
/// problems}`.
#[wasm_bindgen]
pub fn check_track_example_log(
    log_json: &str,
    known: &str,
    registry_json: &str,
) -> Result<String, JsError> {
    if log_json.len() > MAX_LOG_BODY_BYTES || registry_json.len() > MAX_REGISTRY_BODY_BYTES {
        return Err(JsError::new("track log or registry too large"));
    }
    let body: Value = serde_json::from_str(log_json).map_err(|e| JsError::new(&e.to_string()))?;
    let reg: Value =
        serde_json::from_str(registry_json).map_err(|e| JsError::new(&e.to_string()))?;
    Ok(example_log_view(&body, known, &reg).to_string())
}

/// The registry the logs' latest commitment names, from the served body:
/// rebuilt with [`BtcRules`] and hashing to `registry_root`, or why not.
fn served_registry(logs: &Value, registry: &Value) -> Result<StaticRegistry, String> {
    let c = peek_commitment(logs).ok_or("the logs name no commitment")?;
    check_registry_body(registry, &c, &BtcRules)
        .map_err(|e| format!("the served registry does not check: {e}"))
}

fn example_log_view(body: &Value, known: &str, registry: &Value) -> Value {
    let known = match known {
        "" => Ok(None),
        k => parse_hash(k)
            .map(Some)
            .ok_or("the commitment this browser remembered is not readable"),
    };
    let checked = known.map_err(str::to_string).and_then(|known| {
        let reg = served_registry(body, registry)?;
        check_log_body(
            body,
            &LogExpect {
                min_as_of: EXAMPLE_AS_OF,
                known,
                latest: None,
                registry: Some(&reg),
                reviewed: None,
                lives: None,
            },
        )
        .map_err(|e| e.to_string())
    });
    let mut problems = Vec::new();
    let mut out = json!({});
    match checked {
        Ok(c) if !c.example => problems.push("the logs are not marked example".to_string()),
        Ok(c) if c.commitment.hash().to_string() != EXAMPLE_COMMITMENT => {
            problems.push("the logs are not under the pinned commitment".to_string());
        }
        Ok(c) => out = log_view(&c),
        Err(e) => problems.push(e),
    }
    let ok = problems.is_empty();
    json!({
        "ok": ok,
        "commitment": EXAMPLE_COMMITMENT,
        "links": out["links"],
        "statements": out["statements"],
        "answers": out["answers"],
        "recomputed": out["recomputed"].as_bool().unwrap_or(false),
        "intake": out["intake"],
        "registry_root": out["registry_root"],
        "problems": problems,
    })
}

fn log_view(c: &CheckedLog) -> Value {
    json!({
        "commitment": c.commitment.hash().to_string(),
        "as_of": c.commitment.as_of,
        "example": c.example,
        "links": c.links,
        "statements": c.statements.len(),
        "answers": c.resolutions.len(),
        "recomputed": c.recomputed,
        "intake": if c.has_intake { "served" } else { "none" },
        "intake_checked": c.intake_checked,
        "resolutions_from_chain": c.resolutions_from_chain,
        "registry_root": c.commitment.registry_root.to_string(),
    })
}

/// What a client brings to a node's logs: its floor, the commitment it saw
/// last (or none) and the latest commitment, the one the record it checked
/// names. Review C21-R2-1: without that pin a node's own self-consistent
/// chain (commitments it made up, chained and recomputing) would check.
#[derive(Clone, Copy)]
struct Pins {
    min_as_of: u64,
    known: Option<Hash32>,
    latest: Hash32,
}

fn parse_latest(latest: &str) -> Result<Hash32, JsError> {
    parse_hash(latest).ok_or_else(|| {
        JsError::new(
            "latest must be the checked record's commitment: 0x and 64 lowercase hex digits",
        )
    })
}

/// Rerun a node's format-1 logs (`GET /v1/track/log[?since=]`) with its
/// served registry (`GET /v1/track/registry`, checked to `registry_root`),
/// the client's floor, the commitment it saw last (`known`, or empty) and the
/// latest commitment (`latest`: the one the record it checked names; the
/// logs must end there, `NotLatest`). Returns the checked counts or an error
/// naming the failed check.
#[wasm_bindgen]
pub fn check_track_log(
    log_json: &str,
    registry_json: &str,
    min_as_of: u64,
    known: &str,
    latest: &str,
) -> Result<String, JsError> {
    if log_json.len() > MAX_LOG_BODY_BYTES || registry_json.len() > MAX_REGISTRY_BODY_BYTES {
        return Err(JsError::new("track log or registry too large"));
    }
    let pins = Pins {
        min_as_of,
        known: parse_known(known)?,
        latest: parse_latest(latest)?,
    };
    let body: Value = serde_json::from_str(log_json).map_err(|e| JsError::new(&e.to_string()))?;
    let reg: Value =
        serde_json::from_str(registry_json).map_err(|e| JsError::new(&e.to_string()))?;
    track_log_view(&body, &reg, pins)
        .map(|v| v.to_string())
        .map_err(|e| JsError::new(&e))
}

fn track_log_view(body: &Value, registry: &Value, pins: Pins) -> Result<Value, String> {
    let registry = served_registry(body, registry)?;
    let expect = LogExpect {
        min_as_of: pins.min_as_of,
        known: pins.known,
        latest: Some(pins.latest),
        registry: Some(&registry),
        ..Default::default()
    };
    check_log_body(body, &expect)
        .map(|c| log_view(&c))
        .map_err(|e| e.to_string())
}

/// Rerun a node's chained (format-2) logs page by page (ROADMAP C21): the
/// head (`GET /v1/track/log[?since=]`), its rows pages in order (`pages_json`,
/// a list of `GET /v1/track/log?at=&from=&since=` bodies from 0 in steps of
/// 4,096), the intake's posts pages (`posts_json`, a list, empty without an
/// intake) and the served registry, with the same `min_as_of`, `known` and
/// `latest` as [`check_track_log`]. Each page is checked as it is taken.
#[wasm_bindgen]
pub fn check_track_log_paged(
    head_json: &str,
    pages_json: &str,
    posts_json: &str,
    registry_json: &str,
    min_as_of: u64,
    known: &str,
    latest: &str,
) -> Result<String, JsError> {
    let all = pages_json.len().saturating_add(posts_json.len());
    if head_json.len() > MAX_BODY_BYTES
        || registry_json.len() > MAX_REGISTRY_BODY_BYTES
        || all as u64 > MAX_PAGED_LOG_BYTES.saturating_mul(2)
    {
        return Err(JsError::new("track log head, pages or registry too large"));
    }
    let pins = Pins {
        min_as_of,
        known: parse_known(known)?,
        latest: parse_latest(latest)?,
    };
    let parse =
        |t: &str| serde_json::from_str::<Value>(t).map_err(|e| JsError::new(&e.to_string()));
    let (head, reg) = (parse(head_json)?, parse(registry_json)?);
    // The head (and its budget) is checked before any page is parsed
    // (review C21-R1-3).
    let start = start_paged(&head, &reg, pins).map_err(|e| JsError::new(&e))?;
    let (pages, posts) = (parse(pages_json)?, parse(posts_json)?);
    paged_log_finish(start, &pages, &posts)
        .map(|v| v.to_string())
        .map_err(|e| JsError::new(&e))
}

/// The served registry, rebuilt and checked, and the head's check.
fn start_paged(
    head: &Value,
    registry: &Value,
    pins: Pins,
) -> Result<(StaticRegistry, PagedLog, Pins), String> {
    let reg = served_registry(head, registry)?;
    let expect = LogExpect {
        min_as_of: pins.min_as_of,
        known: pins.known,
        latest: Some(pins.latest),
        ..Default::default()
    };
    let log = PagedLog::start(head, &expect).map_err(|e| e.to_string())?;
    Ok((reg, log, pins))
}

fn paged_log_finish(
    (reg, mut log, pins): (StaticRegistry, PagedLog, Pins),
    pages: &Value,
    posts: &Value,
) -> Result<Value, String> {
    let expect = LogExpect {
        min_as_of: pins.min_as_of,
        known: pins.known,
        latest: Some(pins.latest),
        registry: Some(&reg),
        ..Default::default()
    };
    let list = |v: &Value| v.as_array().cloned().ok_or("pages must be a list");
    for p in list(pages)? {
        log.take_rows(&p).map_err(|e| e.to_string())?;
    }
    for p in list(posts)? {
        log.take_posts(&p).map_err(|e| e.to_string())?;
    }
    log.finish(&expect)
        .map(|c| log_view(&c))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
fn paged_log_view(
    head: &Value,
    pages: &Value,
    posts: &Value,
    registry: &Value,
    min_as_of: u64,
    known: Option<Hash32>,
    latest: Hash32,
) -> Result<Value, String> {
    let pins = Pins {
        min_as_of,
        known,
        latest,
    };
    paged_log_finish(start_paged(head, registry, pins)?, pages, posts)
}

fn parse_known(known: &str) -> Result<Option<Hash32>, JsError> {
    match known {
        "" => Ok(None),
        k => parse_hash(k)
            .map(Some)
            .ok_or_else(|| JsError::new("known must be 0x and 64 lowercase hex digits")),
    }
}

/// Check one served body (`GET /v1/track/{source}/{metric}[?since=]`) with
/// the client's floor `min_as_of` and the commitment it saw last (`known`,
/// or empty); the same view as the example, no pin.
#[wasm_bindgen]
pub fn check_track_record(
    body_json: &str,
    source: &str,
    metric: &str,
    min_as_of: u64,
    known: &str,
) -> Result<String, JsError> {
    if body_json.len() > MAX_BODY_BYTES {
        return Err(JsError::new("track record body too large"));
    }
    let known = parse_known(known)?;
    let body: Value = serde_json::from_str(body_json).map_err(|e| JsError::new(&e.to_string()))?;
    check_known(&body, source, metric, min_as_of, known)
        .map(|c| view(&c).to_string())
        .map_err(|e| JsError::new(&e))
}

/// Check a record served without its history (past 1,024 entries) with its
/// pages (`GET .../history?at=&from=`, `pages_json` a list of page bodies
/// from 0 in steps of 1,024): every page under the record's commitment, then
/// the record recounted over all of them (ROADMAP C20 item 3).
#[wasm_bindgen]
pub fn check_track_record_paged(
    body_json: &str,
    pages_json: &str,
    source: &str,
    metric: &str,
    min_as_of: u64,
    known: &str,
) -> Result<String, JsError> {
    if body_json.len() > MAX_BODY_BYTES {
        return Err(JsError::new("track record body too large"));
    }
    let known = parse_known(known)?;
    let body: Value = serde_json::from_str(body_json).map_err(|e| JsError::new(&e.to_string()))?;
    let pages: Value =
        serde_json::from_str(pages_json).map_err(|e| JsError::new(&e.to_string()))?;
    paged_view(&body, &pages, source, metric, min_as_of, known)
        .map(|v| v.to_string())
        .map_err(|e| JsError::new(&e))
}

fn paged_view(
    body: &Value,
    pages: &Value,
    source: &str,
    metric: &str,
    min_as_of: u64,
    known: Option<Hash32>,
) -> Result<Value, String> {
    let mut c = check_known(body, source, metric, min_as_of, known)?;
    if !c.history_checked {
        let list = pages.as_array().ok_or("pages must be a list")?;
        let checked = list
            .iter()
            .enumerate()
            .map(|(i, p)| check_history_page(p, &c, i as u64 * MAX_HISTORY))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("{source}: {e}"))?;
        complete_history(&mut c, &checked).map_err(|e| format!("{source}: {e}"))?;
    }
    Ok(view(&c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use onyx_core::MetricId;
    use onyx_track::example;
    use onyx_track::wire::{record_body, Served};

    fn site_file() -> Value {
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../static/level5/track-example.json"
        ))
        .expect("site example");
        serde_json::from_str(&text).expect("json")
    }

    /// The pins are the example's latest commitment.
    #[test]
    fn the_pins_are_the_examples_latest_commitment() {
        let [_, (c, _)] = example::chain().expect("chain");
        assert_eq!(c.hash().to_string(), EXAMPLE_COMMITMENT);
        assert_eq!(c.as_of, EXAMPLE_AS_OF);
        assert_eq!(example::METRIC, EXAMPLE_METRIC);
        assert_eq!([example::FORECASTER, example::GUESSER], EXAMPLE_SOURCES);
    }

    /// The shipped file checks, and the view follows the display rules: a rate
    /// only for the established record, counts and the threshold for the other.
    #[test]
    fn the_site_example_checks_and_shows_counts_by_the_rules() {
        let v = example_view(&site_file());
        assert_eq!(v["ok"], true, "{v}");
        let f = &v["records"][0];
        assert_eq!(
            (f["established"].as_bool(), f["rate"].as_str()),
            (Some(true), Some("91.1%"))
        );
        assert_eq!(f["mean_brier"], "0.0929");
        // C9: the example's metric has a point rule (no period grid), so no
        // persistence baseline and no skill is shown (R-TR-11).
        assert_eq!(
            (
                f["baseline_entries"].as_u64(),
                f["baseline_source_agreed"].as_u64(),
                f["baseline_agreed"].as_u64()
            ),
            (Some(0), Some(0), Some(0))
        );
        assert_eq!(f["skill"], Value::Null);
        assert_eq!(
            (f["period_secs"].clone(), f["lead_secs"].clone()),
            (Value::Null, Value::Null)
        );
        assert_eq!(
            (f["agreed"].as_u64(), f["resolved"].as_u64()),
            (Some(31), Some(34))
        );
        let h = f["history"].as_array().expect("history");
        assert_eq!(h.len(), 36);
        assert!(
            h.windows(2)
                .all(|w| w[0]["as_of"].as_u64() <= w[1]["as_of"].as_u64()),
            "by date"
        );
        assert_eq!(f["resolver_levels"], json!([1, 3]));
        let g = &v["records"][1];
        assert_eq!(g["established"], false);
        assert_eq!(
            (g["rate"].clone(), g["mean_brier"].clone()),
            (Value::Null, Value::Null)
        );
        assert_eq!(g["equivocated"], 1);
        assert_eq!(g["skill"], Value::Null);
    }

    /// C9: skill is shown as a signed difference of two rates, floored to a
    /// tenth of a point.
    #[test]
    fn skill_points_are_floored_and_signed() {
        assert_eq!(points(928_571), "+92.8 points");
        assert_eq!(points(0), "+0.0 points");
        assert_eq!(points(-1), "-0.1 points");
        assert_eq!(points(-333_334), "-33.4 points");
        assert_eq!(points(1_000_000), "+100.0 points");
        assert_eq!(points(-1_000_000), "-100.0 points");
    }

    /// TR-S-2/TR-R2-5 on the site: a cut history, the older commitment, or a
    /// record not marked example are each refused and nothing is shown.
    #[test]
    fn a_tampered_or_stale_example_file_shows_nothing() {
        let mut cut = site_file();
        cut["records"]["example.guess"]["history"]["entries"]
            .as_array_mut()
            .expect("entries")
            .pop();
        let v = example_view(&cut);
        assert_eq!(
            (v["ok"].as_bool(), v["records"].as_array().map(Vec::len)),
            (Some(false), Some(0))
        );

        // The older commitment, served honestly in every other way: the
        // proofs check, the floor and the pin refuse it.
        let [(c1, b1), _] = example::chain().expect("chain");
        let p = example::params();
        let served = Served {
            book: &b1,
            commitment: &c1,
            params: &p,
            chain: None,
        };
        let mut stale = site_file();
        for s in EXAMPLE_SOURCES {
            stale["records"][s] = record_body(
                &served,
                &onyx_core::SourceId(s.into()),
                &MetricId(EXAMPLE_METRIC.into()),
            )
            .expect("record");
        }
        assert!(check_one(
            &stale["records"]["example.forecast"],
            "example.forecast",
            EXAMPLE_METRIC,
            0
        )
        .is_ok());
        let v = example_view(&stale);
        assert_eq!(v["ok"], false);
        assert!(
            v["problems"][0]
                .as_str()
                .is_some_and(|p| p.contains("older than the floor")),
            "{v}"
        );

        let mut unmarked = site_file();
        unmarked["example"] = Value::Bool(false);
        assert_eq!(example_view(&unmarked)["ok"], false);
        let mut flag = site_file();
        flag["records"]["example.forecast"]["example"] = Value::Bool(false);
        assert_eq!(example_view(&flag)["ok"], false);
    }

    fn since_file() -> Value {
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../static/level5/track-example-since.json"
        ))
        .expect("since file");
        serde_json::from_str(&text).expect("json")
    }

    fn log_file() -> Value {
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../static/level5/track-example-log.json"
        ))
        .expect("log file");
        serde_json::from_str(&text).expect("json")
    }

    fn registry_file() -> Value {
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../static/level5/track-example-registry.json"
        ))
        .expect("registry file");
        serde_json::from_str(&text).expect("json")
    }

    fn seen(commitment: &str, as_of: u64) -> Value {
        let one = json!({ "commitment": commitment, "as_of": as_of });
        json!({ "example.forecast": one, "example.guess": one })
    }

    /// C20 item 4: asked with the commitment it saw last (the earlier one or
    /// the latest), the browser checks the served record chains back to it;
    /// one the server does not know shows nothing and says so.
    #[test]
    fn the_browser_asks_with_the_commitment_it_saw_last() {
        let [(c1, _), (c2, _)] = example::chain().expect("chain");
        for (c, as_of) in [(&c1, c1.as_of), (&c2, c2.as_of)] {
            let v = example_since_view(&since_file(), &seen(&c.hash().to_string(), as_of));
            assert_eq!(v["ok"], true, "{v}");
            assert_eq!(v["records"][0]["since"], c.hash().to_string());
            assert_eq!(v["records"][0]["history_checked"], true);
        }
        let fork = Hash32([9; 32]).to_string();
        let v = example_since_view(&since_file(), &seen(&fork, EXAMPLE_AS_OF));
        assert_eq!(
            (v["ok"].as_bool(), v["unknown"].as_bool()),
            (Some(false), Some(true))
        );
        assert_eq!(v["records"], json!([]));
        // A remembered as_of later than the served one: refused by the floor.
        let v = example_since_view(
            &since_file(),
            &seen(&c2.hash().to_string(), EXAMPLE_AS_OF + 1),
        );
        assert_eq!(v["ok"], false);
        // The since body for c1 checked as if the browser had seen c2: the
        // chain does not lead from c2, refused.
        let mut swapped = since_file();
        let c1h = c1.hash().to_string();
        let c2h = c2.hash().to_string();
        for s in EXAMPLE_SOURCES {
            swapped["since"][s][&c2h] = swapped["since"][s][&c1h].clone();
        }
        assert_eq!(
            example_since_view(&swapped, &seen(&c2h, c2.as_of))["ok"],
            false
        );
        // An unreadable remembered value is refused, not ignored.
        let v = example_since_view(&since_file(), &seen("0xABC", 0));
        assert_eq!(v["ok"], false);
    }

    /// C20 item 1 in the browser: the example's logs rerun to the pinned
    /// commitment and extend the first; a changed answer is refused.
    #[test]
    fn the_example_logs_rerun_in_the_browser() {
        let [(c1, _), _] = example::chain().expect("chain");
        let v = example_log_view(&log_file(), "", &registry_file());
        assert_eq!(v["ok"], true, "{v}");
        assert_eq!(
            (
                v["links"].as_u64(),
                v["recomputed"].as_bool(),
                v["intake"].as_str()
            ),
            (Some(2), Some(true), Some("none"))
        );
        assert_eq!(
            example_log_view(&log_file(), &c1.hash().to_string(), &registry_file())["ok"],
            true
        );
        assert_eq!(
            example_log_view(&log_file(), &Hash32([9; 32]).to_string(), &registry_file())["ok"],
            false
        );
        let mut changed = log_file();
        let r = changed["resolutions"].as_str().expect("hex").to_string();
        let flipped = format!(
            "{}{}",
            &r[..r.len() - 1],
            if r.ends_with('0') { '1' } else { '0' }
        );
        changed["resolutions"] = json!(flipped);
        let v = example_log_view(&changed, "", &registry_file());
        assert_eq!(v["ok"], false, "{v}");
    }

    /// C20 item 3: the paged check runs the same recount (here on the
    /// example, whose history fits one page and one body).
    #[test]
    fn a_paged_record_checks_like_a_whole_one() {
        let doc = site_file();
        let b = &doc["records"]["example.forecast"];
        let v = paged_view(
            b,
            &json!([]),
            "example.forecast",
            EXAMPLE_METRIC,
            EXAMPLE_AS_OF,
            None,
        )
        .expect("checks");
        assert_eq!(v["history_checked"], true);
    }

    /// ROADMAP C23 in the browser: the served example registry rebuilds with
    /// the browser's rules to the commitment's `registry_root` and is what the
    /// rerun uses; a registry with an entry added is refused and nothing runs.
    #[test]
    fn the_rerun_uses_the_served_registry_only_when_it_hashes_to_the_root() {
        let v = example_log_view(&log_file(), "", &registry_file());
        assert_eq!(v["ok"], true, "{v}");
        assert_eq!(
            v["registry_root"],
            onyx_core::Registry::root(&example::registry()).to_string()
        );
        let mut forged = registry_file();
        let mut data: onyx_core::RegistryData = borsh::from_slice(
            &hex::decode(
                forged["registry"]
                    .as_str()
                    .expect("hex")
                    .trim_start_matches("0x"),
            )
            .expect("hex"),
        )
        .expect("borsh");
        data.aggregates.push("example.other".into());
        forged["registry"] = json!(format!(
            "0x{}",
            hex::encode(borsh::to_vec(&data).expect("borsh"))
        ));
        let v = example_log_view(&log_file(), "", &forged);
        assert_eq!(v["ok"], false, "{v}");
        assert!(
            v["problems"][0]
                .as_str()
                .is_some_and(|p| p.contains("registry")),
            "{v}"
        );
        // The same files through the generic check a node's logs get, pinned
        // to the record's commitment.
        let got: Value = serde_json::from_str(
            &check_track_log(
                &log_file().to_string(),
                &registry_file().to_string(),
                EXAMPLE_AS_OF,
                "",
                EXAMPLE_COMMITMENT,
            )
            .expect("checks"),
        )
        .expect("json");
        assert_eq!(
            (got["recomputed"].as_bool(), got["links"].as_u64()),
            (Some(true), Some(2))
        );
    }

    /// ROADMAP C21 in the browser: chained logs checked page by page with the
    /// served registry; a page from another commitment is refused.
    #[test]
    fn chained_logs_are_checked_page_by_page_in_the_browser() {
        use onyx_track::publication::Publication;
        use onyx_track::wire::{chained_log_body, log_rows_page, registry_body};
        let (p, reg) = (example::params(), example::registry());
        let mut pubn = Publication::chained();
        for as_of in [example::AS_OF_EARLY, example::AS_OF] {
            let (s, r) = example::logs(as_of);
            let book = onyx_track::compute(&p, &reg, &s, &r, as_of).expect("compute");
            pubn.push_with(&p, &s, &r, &book, None, Some(&reg))
                .expect("push");
        }
        let head = chained_log_body(&pubn, 0).expect("head");
        let pages = json!([log_rows_page(&pubn, 0, 0).expect("page")]);
        let registry = registry_body(&pubn).expect("registry");
        let c1 = pubn.steps[0].commitment.hash();
        let c2 = pubn.steps[1].commitment.hash();
        let v =
            paged_log_view(&head, &pages, &json!([]), &registry, 0, Some(c1), c2).expect("checks");
        assert_eq!(
            (v["recomputed"].as_bool(), v["links"].as_u64()),
            (Some(true), Some(2))
        );
        let mut other = pages.clone();
        other[0]["commitmentHash"] = json!(c1.to_string());
        assert!(paged_log_view(&head, &other, &json!([]), &registry, 0, None, c2).is_err());
    }

    /// Review C21-R2-1: a node's logs check only up to the commitment the
    /// checked record names. A self-consistent chain the node made up (here
    /// the honest one cut at its first commitment, which chains, recomputes
    /// and passes every other check) is refused in both formats.
    #[test]
    fn node_logs_check_only_to_the_records_commitment() {
        use onyx_track::publication::Publication;
        use onyx_track::wire::{chained_log_body, log_rows_page, registry_body};
        let (p, reg) = (example::params(), example::registry());
        let mut short = Publication::chained();
        let (s, r) = example::logs(example::AS_OF_EARLY);
        let book = onyx_track::compute(&p, &reg, &s, &r, example::AS_OF_EARLY).expect("compute");
        short
            .push_with(&p, &s, &r, &book, None, Some(&reg))
            .expect("push");
        let head = chained_log_body(&short, 0).expect("head");
        let pages = json!([log_rows_page(&short, 0, 0).expect("page")]);
        let registry = registry_body(&short).expect("registry");
        let own = short.steps[0].commitment.hash();
        assert!(paged_log_view(&head, &pages, &json!([]), &registry, 0, None, own).is_ok());
        let pinned = Hash32([9; 32]);
        let refused = paged_log_view(&head, &pages, &json!([]), &registry, 0, None, pinned);
        assert!(
            refused.as_ref().is_err_and(|e| e.contains("latest")),
            "{refused:?}"
        );
        // Format 1: the example logs end at the pinned commitment, not another.
        let checked = |latest: Hash32| {
            let pins = Pins {
                min_as_of: EXAMPLE_AS_OF,
                known: None,
                latest,
            };
            track_log_view(&log_file(), &registry_file(), pins)
        };
        let pinned = parse_hash(EXAMPLE_COMMITMENT).expect("pinned");
        assert!(checked(pinned).is_ok());
        assert!(checked(Hash32([9; 32])).is_err_and(|e| e.contains("latest")));
    }

    #[test]
    fn display_numbers_are_floored_integers() {
        assert_eq!(percent(911_764), "91.1%");
        assert_eq!(percent(1_000_000), "100.0%");
        assert_eq!(brier(1_000_000_000_000), "1.0000");
        assert_eq!(brier(135_294_117_647), "0.1352");
    }
}
