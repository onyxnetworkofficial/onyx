//! The shared vectors of the generic JSON fetcher (ADR-0053, THREATS CL3-3):
//! `fixtures/json-fetcher/vectors.json` holds named descriptors (their JSON form, Borsh bytes
//! and fetcher id) and cases (descriptor, feed kind, subject date, body, answer and the reason
//! for no answer). Every implementation of the reader must give the same answers; this test is
//! the Rust side. Regenerate only with an ADR change: `ONYX_BLESS=1 cargo test -p onyx-json
//! --test vectors`.

use onyx_core::feed_json::{
    Access, Assert, Expect, JsonDescriptor, TimeForm, ValueRule, MAX_DEPTH,
};
use onyx_core::{Answer, ObservationKind};
use onyx_json::form::quote;
use onyx_json::{read, read_at};

fn path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/json-fetcher/vectors.json")
}

fn a(pointer: &str, expect: Expect) -> Assert {
    Assert {
        pointer: pointer.into(),
        expect,
    }
}

fn descriptors() -> Vec<(&'static str, JsonDescriptor)> {
    let d = |pointer: &str, value, unit: &str, asserts, max_bytes| JsonDescriptor {
        version: 1,
        pointer: pointer.into(),
        value,
        unit: unit.into(),
        asserts,
        max_bytes,
        access: Access::Public,
    };
    vec![
        (
            "tga",
            d(
                "/data/0/open_today_bal",
                ValueRule::DecimalString,
                "million USD",
                vec![
                    a("/data", Expect::ArrayLen(1)),
                    a(
                        "/data/0",
                        Expect::KeysExactly(vec![
                            "account_type".into(),
                            "open_today_bal".into(),
                            "record_date".into(),
                        ]),
                    ),
                    a(
                        "/data/0/account_type",
                        Expect::Str("Treasury General Account (TGA) Closing Balance".into()),
                    ),
                    a("/data/0/record_date", Expect::SubjectDate),
                    a("/meta/count", Expect::Int(1)),
                ],
                65_536,
            ),
        ),
        (
            "rate",
            d(
                "/rate",
                ValueRule::JsonNumber,
                "percent",
                vec![a("/date", Expect::SubjectDate)],
                4_096,
            ),
        ),
        (
            "either",
            d("/v", ValueRule::DecimalOrNumber, "degC", vec![], 4_096),
        ),
        (
            "count",
            d(
                "/features",
                ValueRule::ArrayLen,
                "events",
                vec![a("/metadata/status", Expect::Int(200))],
                4_096,
            ),
        ),
        (
            "name",
            d(
                "/name",
                ValueRule::Text { trim: true },
                "text",
                vec![],
                4_096,
            ),
        ),
        ("flag", d("/open", ValueRule::Bool, "flag", vec![], 4_096)),
        ("small", d("/v", ValueRule::JsonNumber, "units", vec![], 16)),
        // ADR-0058 (contributor loop CL8): one venue's closed bucket, pinned by its start time
        // (THREATS CL8-1) and a positive volume (CL8-11), in three time forms.
        (
            "candle",
            d(
                "/candles/0/close",
                ValueRule::DecimalString,
                "USD",
                vec![
                    a("/candles", Expect::ArrayLen(1)),
                    a("/candles/0/t", Expect::BucketStart(TimeForm::UnixSecs)),
                    a("/candles/0/volume", Expect::Positive),
                    a("/interval", Expect::Str("1h".into())),
                    a("/pair", Expect::Str("AAA-USD".into())),
                ],
                4_096,
            ),
        ),
        (
            "candle_ms",
            d(
                "/result/0/c",
                ValueRule::DecimalOrNumber,
                "USD",
                vec![
                    a("/result", Expect::ArrayLen(1)),
                    a("/result/0/o", Expect::BucketStart(TimeForm::UnixMillis)),
                    a("/result/0/v", Expect::Positive),
                ],
                4_096,
            ),
        ),
        (
            "candle_iso",
            d(
                "/data/close",
                ValueRule::JsonNumber,
                "USD",
                vec![
                    a("/data/start", Expect::BucketStart(TimeForm::Rfc3339)),
                    a("/data/volume", Expect::Positive),
                ],
                4_096,
            ),
        ),
    ]
}

struct Case {
    name: &'static str,
    descriptor: &'static str,
    kind: ObservationKind,
    date: Option<&'static str>,
    body: Vec<u8>,
    /// The bucket a bucketed subject names (ADR-0058), Unix seconds.
    bucket: Option<u64>,
}

/// 2026-10-27T00:00:00Z, the start of the hourly bucket the CL8 cases name.
const B0: u64 = 1_793_059_200;

fn candle(t: &str, close: &str, volume: &str) -> Vec<u8> {
    format!(
        r#"{{"pair":"AAA-USD","interval":"1h","candles":[{{"t":{t},"close":{close},"volume":{volume}}}]}}"#
    )
    .into_bytes()
}

fn num(scale: u8) -> ObservationKind {
    ObservationKind::Numeric {
        scale,
        tolerance: 0,
    }
}

fn exact(max_len: u16) -> ObservationKind {
    ObservationKind::Exact { max_len }
}

fn tga_body(date: &str, row: &str, extra: &str, value: &str) -> Vec<u8> {
    format!(
        r#"{{"data":[{{"record_date":"{date}","account_type":"{row}","open_today_bal":{value}{extra}}}],"meta":{{"count":1}}}}"#
    )
    .into_bytes()
}

const TGA: &str = "Treasury General Account (TGA) Closing Balance";

fn cases() -> Vec<Case> {
    let c = |name, descriptor, kind, date, body: Vec<u8>| Case {
        name,
        descriptor,
        kind,
        date,
        body,
        bucket: None,
    };
    let cb = |name, descriptor, bucket: Option<u64>, body: Vec<u8>| Case {
        name,
        descriptor,
        kind: num(2),
        date: None,
        body,
        bucket,
    };
    let b0 = Some(B0);
    let t = |s: u64| s.to_string();
    let d27 = Some("2026-10-27");
    // An object holding the value and `n - 1` nested arrays: nesting depth `n`.
    let deep = |n: usize| {
        format!(
            "{{\"v\":1,\"w\":{}{}}}",
            "[".repeat(n - 1),
            "]".repeat(n - 1)
        )
    };
    vec![
        c("tga row", "tga", num(0), d27, tga_body("2026-10-27", TGA, "", "\"1052530\"")),
        c("tga row at scale 2", "tga", num(2), d27, tga_body("2026-10-27", TGA, "", "\"1052530\"")),
        c("subject date one day off", "tga", num(0), Some("2026-10-28"), tga_body("2026-10-27", TGA, "", "\"1052530\"")),
        c("no subject date", "tga", num(0), None, tga_body("2026-10-27", TGA, "", "\"1052530\"")),
        c("another row", "tga", num(0), d27, tga_body("2026-10-27", "Treasury General Account (TGA) Opening Balance", "", "\"1020700\"")),
        c("keys exactly with one extra key", "tga", num(0), d27, tga_body("2026-10-27", TGA, ",\"x\":1", "\"1052530\"")),
        c("number where a decimal string is read", "tga", num(0), d27, tga_body("2026-10-27", TGA, "", "1052530")),
        c("decimal string with a leading zero", "tga", num(0), d27, tga_body("2026-10-27", TGA, "", "\"01052530\"")),
        c("decimal string with a separator", "tga", num(0), d27, tga_body("2026-10-27", TGA, "", "\"1,052,530\"")),
        c("decimal string with a plus sign", "tga", num(0), d27, tga_body("2026-10-27", TGA, "", "\"+1052530\"")),
        c("decimal string with spaces", "tga", num(0), d27, tga_body("2026-10-27", TGA, "", "\" 1052530\"")),
        c("extra precision", "tga", num(0), d27, tga_body("2026-10-27", TGA, "", "\"1052530.5\"")),
        c("zero digits beyond the scale", "tga", num(0), d27, tga_body("2026-10-27", TGA, "", "\"1052530.00\"")),
        c("duplicate key", "tga", num(0), d27, b"{\"data\":[{\"record_date\":\"2026-10-27\",\"account_type\":\"Treasury General Account (TGA) Closing Balance\",\"open_today_bal\":\"1\",\"open_today_bal\":\"2\"}],\"meta\":{\"count\":1}}".to_vec()),
        c("duplicate key elsewhere", "either", num(0), None, br#"{"v":"5","w":{"k":1,"k":1}}"#.to_vec()),
        c("byte-order mark", "either", num(0), None, b"\xef\xbb\xbf{\"v\":\"5\"}".to_vec()),
        c("not utf-8", "either", num(0), None, b"{\"v\":\"5\",\"w\":\"\xff\"}".to_vec()),
        c("lone surrogate", "either", num(0), None, br#"{"v":"5","w":"\ud800"}"#.to_vec()),
        c("trailing garbage", "either", num(0), None, br#"{"v":"5"} x"#.to_vec()),
        c("depth 64", "either", num(0), None, deep(MAX_DEPTH).into_bytes()),
        c("depth 65", "either", num(0), None, deep(MAX_DEPTH + 1).into_bytes()),
        c("json number at scale 2", "rate", num(2), d27, br#"{"date":"2026-10-27","rate":4.25}"#.to_vec()),
        c("negative json number", "rate", num(2), d27, br#"{"date":"2026-10-27","rate":-0.5}"#.to_vec()),
        c("exponent", "rate", num(2), d27, br#"{"date":"2026-10-27","rate":4.25e0}"#.to_vec()),
        c("string where a json number is read", "rate", num(2), d27, br#"{"date":"2026-10-27","rate":"4.25"}"#.to_vec()),
        c("either: string", "either", num(1), None, br#"{"v":"-3.5"}"#.to_vec()),
        c("either: number", "either", num(1), None, br#"{"v":-3.5}"#.to_vec()),
        c("either: null", "either", num(1), None, br#"{"v":null}"#.to_vec()),
        c("mantissa at the top of i128", "either", num(0), None, br#"{"v":170141183460469231731687303715884105727}"#.to_vec()),
        c("mantissa above i128", "either", num(0), None, br#"{"v":170141183460469231731687303715884105728}"#.to_vec()),
        c("mantissa above i128 after scaling", "either", num(2), None, br#"{"v":"1701411834604692317316873037158841058"}"#.to_vec()),
        c("array length", "count", num(0), None, br#"{"metadata":{"status":200},"features":[{},{},{}]}"#.to_vec()),
        c("array length, assert fails", "count", num(0), None, br#"{"metadata":{"status":"200"},"features":[]}"#.to_vec()),
        c("array length, not an array", "count", num(0), None, br#"{"metadata":{"status":200},"features":{}}"#.to_vec()),
        c("array length at scale 1", "count", num(1), None, br#"{"metadata":{"status":200},"features":[]}"#.to_vec()),
        c("int assert with a fraction", "count", num(0), None, br#"{"metadata":{"status":200.0},"features":[]}"#.to_vec()),
        c("text trimmed", "name", exact(32), None, br#"{"name":"  Denver Intl \u0041P "}"#.to_vec()),
        c("text too long", "name", exact(4), None, br#"{"name":"Denver"}"#.to_vec()),
        c("text empty after trim", "name", exact(32), None, br#"{"name":"   "}"#.to_vec()),
        c("text with a control character", "name", exact(32), None, br#"{"name":"a\u0007b"}"#.to_vec()),
        c("text read as a number", "name", num(0), None, br#"{"name":"5"}"#.to_vec()),
        c("bool true", "flag", exact(8), None, br#"{"open":true}"#.to_vec()),
        c("bool as a string", "flag", exact(8), None, br#"{"open":"true"}"#.to_vec()),
        c("max_bytes", "small", num(0), None, br#"{"v":1234567890}"#.to_vec()),
        c("max_bytes + 1", "small", num(0), None, br#"{"v":12345678901}"#.to_vec()),
        c("missing value", "either", num(0), None, br#"{"w":"5"}"#.to_vec()),
        cb("candle of the bucket", "candle", b0, candle(&t(B0), "\"67012.50\"", "\"12.5\"")),
        cb("candle of the next bucket", "candle", b0, candle(&t(B0 + 3_600), "\"67012.50\"", "\"12.5\"")),
        cb("candle of the previous bucket", "candle", b0, candle(&t(B0 - 3_600), "\"67012.50\"", "\"12.5\"")),
        cb("candle cut on a local-time grid (UTC+2)", "candle", b0, candle(&t(B0 - 7_200), "\"67012.50\"", "\"12.5\"")),
        cb("candle stamped by a clock 5 s fast", "candle", b0, candle(&t(B0 + 5), "\"67012.50\"", "\"12.5\"")),
        cb("candle time as a string", "candle", b0, candle(&format!("\"{B0}\""), "\"67012.50\"", "\"12.5\"")),
        cb("candle time with a fraction", "candle", b0, candle(&format!("{B0}.0"), "\"67012.50\"", "\"12.5\"")),
        cb("candle without a bucket subject", "candle", None, candle(&t(B0), "\"67012.50\"", "\"12.5\"")),
        cb("candle with zero volume (halted, last price repeated)", "candle", b0, candle(&t(B0), "\"67012.50\"", "\"0\"")),
        cb("candle with zero volume written 0.000", "candle", b0, candle(&t(B0), "\"67012.50\"", "\"0.000\"")),
        cb("candle with a negative volume", "candle", b0, candle(&t(B0), "\"67012.50\"", "\"-1\"")),
        cb("candle with a volume exponent", "candle", b0, candle(&t(B0), "\"67012.50\"", "1e3")),
        cb("candle with a tiny volume", "candle", b0, candle(&t(B0), "\"67012.50\"", "0.00000001")),
        cb("no candle (halted venue)", "candle", b0, br#"{"pair":"AAA-USD","interval":"1h","candles":[]}"#.to_vec()),
        cb("two candles", "candle", b0, format!(r#"{{"pair":"AAA-USD","interval":"1h","candles":[{{"t":{B0},"close":"1","volume":"1"}},{{"t":{},"close":"2","volume":"1"}}]}}"#, B0 + 3_600).into_bytes()),
        cb("candle of another interval", "candle", b0, format!(r#"{{"pair":"AAA-USD","interval":"1d","candles":[{{"t":{B0},"close":"1","volume":"1"}}]}}"#).into_bytes()),
        cb("candle in milliseconds", "candle_ms", b0, format!("{{\"result\":[{{\"o\":{},\"c\":\"67012.5\",\"v\":\"3\"}}]}}", B0 * 1_000).into_bytes()),
        cb("candle in milliseconds given seconds", "candle_ms", b0, format!("{{\"result\":[{{\"o\":{B0},\"c\":\"67012.5\",\"v\":\"3\"}}]}}").into_bytes()),
        cb("candle in milliseconds 1 ms late", "candle_ms", b0, format!("{{\"result\":[{{\"o\":{},\"c\":\"67012.5\",\"v\":\"3\"}}]}}", B0 * 1_000 + 1).into_bytes()),
        cb("candle with an RFC 3339 start", "candle_iso", b0, br#"{"data":{"start":"2026-10-27T00:00:00Z","close":67012.5,"volume":2}}"#.to_vec()),
        cb("RFC 3339 start with a fraction", "candle_iso", b0, br#"{"data":{"start":"2026-10-27T00:00:00.000Z","close":67012.5,"volume":2}}"#.to_vec()),
        cb("RFC 3339 start with an offset", "candle_iso", b0, br#"{"data":{"start":"2026-10-27T00:00:00+00:00","close":67012.5,"volume":2}}"#.to_vec()),
        cb("RFC 3339 start of a local midnight", "candle_iso", b0, br#"{"data":{"start":"2026-10-26T22:00:00Z","close":67012.5,"volume":2}}"#.to_vec()),
    ]
}

fn kind_json(k: ObservationKind) -> String {
    match k {
        ObservationKind::Numeric { scale, .. } => format!("{{\"numeric\":{scale}}}"),
        ObservationKind::Exact { max_len } => format!("{{\"exact\":{max_len}}}"),
    }
}

use onyx_json::vectors::answer_json;

fn body_json(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) if !s.starts_with('\u{feff}') => format!("\"body\":{}", quote(s)),
        _ => format!("\"body_hex\":\"{}\"", hex::encode(b)),
    }
}

fn render() -> String {
    let ds = descriptors();
    let mut out = String::from("{\n  \"about\": \"Shared vectors of the generic level-2 JSON fetcher (ADR-0053, THREATS CL3-3). Regenerate only with an ADR change: ONYX_BLESS=1 cargo test -p onyx-json --test vectors.\",\n  \"descriptors\": {\n");
    let rows: Vec<String> = ds
        .iter()
        .map(|(name, d)| {
            format!(
                "    {}: {{\"form\": {}, \"borsh_hex\": \"{}\", \"fetcher_id\": \"{}\"}}",
                quote(name),
                onyx_json::descriptor_to_json(d),
                hex::encode(borsh::to_vec(d).expect("borsh")),
                d.fetcher_id()
            )
        })
        .collect();
    out.push_str(&rows.join(",\n"));
    out.push_str("\n  },\n  \"cases\": [\n");
    let rows: Vec<String> = cases()
        .iter()
        .map(|c| {
            let d = &ds.iter().find(|(n, _)| *n == c.descriptor).expect("d").1;
            let r = read_at(d, c.kind, c.date, c.bucket, &c.body);
            format!(
                "    {{\"name\": {}, \"descriptor\": {}, \"kind\": {}, \"subject_date\": {}{}, {}, \"answer\": {}}}",
                quote(c.name),
                quote(c.descriptor),
                kind_json(c.kind),
                c.date.map_or("null".to_string(), quote),
                c.bucket.map_or(String::new(), |b| format!(", \"bucket_start\": {b}")),
                body_json(&c.body),
                answer_json(&r)
            )
        })
        .collect();
    out.push_str(&rows.join(",\n"));
    out.push_str("\n  ]\n}\n");
    out
}

/// THREATS CL3-3: the committed vectors equal what this build computes, read back through the
/// strict reader itself (each descriptor from its JSON form, its Borsh bytes and fetcher id).
#[test]
fn json_fetcher_vectors_match_in_rust() {
    let text = render();
    if std::env::var("ONYX_BLESS").is_ok() {
        std::fs::write(path(), &text).expect("write vectors");
    }
    let file = std::fs::read(path()).expect("fixtures/json-fetcher/vectors.json");
    assert_eq!(
        String::from_utf8_lossy(&file),
        text,
        "the vectors moved: regenerate only with an ADR change"
    );
    // Recompute every descriptor and case from the file alone (the same checker the wasm32
    // build runs, scripts/check-json-wasm.sh).
    let n = onyx_json::vectors::check(&file).expect("vectors");
    assert!(n >= 40);
    let unavailable = String::from_utf8_lossy(&file)
        .matches("{\"unavailable\":")
        .count();
    assert!(unavailable >= 25, "most vectors are refusals");
}

/// The ADR's named refusals each give their own reason.
#[test]
fn each_named_trick_is_refused_for_its_own_reason() {
    let ds = descriptors();
    let why = |name: &str| {
        let c = cases().into_iter().find(|c| c.name == name).expect("case");
        let d = &ds.iter().find(|(n, _)| *n == c.descriptor).expect("d").1;
        read_at(d, c.kind, c.date, c.bucket, &c.body).map_err(|e| e.code())
    };
    for (name, code) in [
        ("duplicate key", "duplicate_key"),
        ("duplicate key elsewhere", "duplicate_key"),
        ("exponent", "exponent"),
        ("extra precision", "extra_precision"),
        ("byte-order mark", "byte_order_mark"),
        ("depth 65", "too_deep"),
        ("max_bytes + 1", "too_large"),
        ("subject date one day off", "assert"),
        ("keys exactly with one extra key", "assert"),
        ("mantissa above i128", "overflow"),
        ("lone surrogate", "syntax"),
        ("not utf-8", "not_utf8"),
    ] {
        assert_eq!(why(name), Err(code), "{name}");
    }
    assert_eq!(why("tga row"), Ok(Answer::Number(1_052_530)));
    assert_eq!(why("tga row at scale 2"), Ok(Answer::Number(105_253_000)));
    assert_eq!(why("array length"), Ok(Answer::Number(3)));
    assert_eq!(why("depth 64"), Ok(Answer::Number(1)));
    assert_eq!(why("max_bytes"), Ok(Answer::Number(1_234_567_890)));
    assert_eq!(
        why("text trimmed"),
        Ok(Answer::Bytes(b"Denver Intl AP".to_vec()))
    );
}

/// ADR-0058 item 3 (THREATS CL8-1, CL8-8, CL8-11): a venue's candle gives a value only for exactly
/// the subject's bucket, in the declared time form, with trades in it.
#[test]
fn a_bucket_with_another_start_is_unavailable() {
    let ds = descriptors();
    let why = |name: &str| {
        let c = cases().into_iter().find(|c| c.name == name).expect("case");
        let d = &ds.iter().find(|(n, _)| *n == c.descriptor).expect("d").1;
        read_at(d, c.kind, c.date, c.bucket, &c.body).map_err(|e| e.code())
    };
    for ok in [
        "candle of the bucket",
        "candle in milliseconds",
        "candle with an RFC 3339 start",
    ] {
        assert_eq!(why(ok), Ok(Answer::Number(6_701_250)), "{ok}");
    }
    assert_eq!(
        why("candle with a tiny volume"),
        Ok(Answer::Number(6_701_250))
    );
    for (name, code) in [
        ("candle of the next bucket", "assert"),
        ("candle of the previous bucket", "assert"),
        ("candle cut on a local-time grid (UTC+2)", "assert"),
        ("candle stamped by a clock 5 s fast", "assert"),
        ("candle time as a string", "assert"),
        ("candle time with a fraction", "assert"),
        ("candle without a bucket subject", "no_bucket"),
        ("candle in milliseconds given seconds", "assert"),
        ("candle in milliseconds 1 ms late", "assert"),
        ("RFC 3339 start with a fraction", "assert"),
        ("RFC 3339 start with an offset", "assert"),
        ("RFC 3339 start of a local midnight", "assert"),
        ("two candles", "assert"),
        ("candle of another interval", "assert"),
    ] {
        assert_eq!(why(name), Err(code), "{name}");
    }
}

/// THREATS CL8-11: a halted or stale venue (no candle, or a candle with no trades that repeats the
/// last price) gives no value, so the venue is listed as `unavailable`, never counted.
#[test]
fn a_halted_or_stale_venue_gives_no_value() {
    let ds = descriptors();
    let why = |name: &str| {
        let c = cases().into_iter().find(|c| c.name == name).expect("case");
        let d = &ds.iter().find(|(n, _)| *n == c.descriptor).expect("d").1;
        read_at(d, c.kind, c.date, c.bucket, &c.body).map_err(|e| e.code())
    };
    for name in [
        "candle with zero volume (halted, last price repeated)",
        "candle with zero volume written 0.000",
        "candle with a negative volume",
        "candle with a volume exponent",
        "no candle (halted venue)",
    ] {
        assert_eq!(why(name), Err("assert"), "{name}");
    }
}

/// `read` never panics, on random bytes and on mutations of real bodies (a deterministic
/// property test: a fixed-seed generator, so every run checks the same inputs).
#[test]
fn extract_never_panics_on_any_bytes() {
    let ds = descriptors();
    let seeds: Vec<Vec<u8>> = cases().into_iter().map(|c| c.body).collect();
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let alphabet = b"{}[]\":,.-+eE0123456789 \\u\xef\xbb\xbftruefalsenull/~01abc\x00\xff";
    for i in 0..20_000u32 {
        let mut body = if i % 3 == 0 {
            (0..(next() % 64))
                .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
                .collect()
        } else {
            seeds[(next() % seeds.len() as u64) as usize].clone()
        };
        for _ in 0..(next() % 4) {
            if body.is_empty() {
                break;
            }
            let at = (next() % body.len() as u64) as usize;
            match next() % 3 {
                0 => body[at] = alphabet[(next() % alphabet.len() as u64) as usize],
                1 => {
                    body.remove(at);
                }
                _ => body.insert(at, alphabet[(next() % alphabet.len() as u64) as usize]),
            }
        }
        let (_, d) = &ds[(next() % ds.len() as u64) as usize];
        let kind = if next() % 2 == 0 {
            num((next() % 40) as u8)
        } else {
            exact(64)
        };
        let _ = read(d, kind, Some("2026-10-27"), &body);
    }
}
