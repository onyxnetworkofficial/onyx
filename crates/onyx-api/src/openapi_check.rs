//! Tests only (pre-public loop PP6): every response the API's own tests receive is checked
//! against docs/api/openapi.json. The route's template is matched from the URI; the status must
//! be one the operation lists; a JSON body must conform to the listed schema. The validator
//! covers the JSON Schema keywords the spec uses (`$ref`, `type`, `oneOf`, `enum`, `properties`,
//! `required`, `additionalProperties`, `items`, `minimum`, `maximum`, `maxLength`, `pattern`);
//! a keyword or pattern it does not know fails the test, so the spec cannot drift past it.

use serde_json::Value;
use std::sync::LazyLock;

static SPEC: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../docs/api/openapi.json")).expect("openapi.json")
});

const KNOWN: &[&str] = &[
    "$ref",
    "type",
    "oneOf",
    "enum",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "minimum",
    "maximum",
    "maxLength",
    "pattern",
    "description",
];

/// The spec's path template for `path` (no query), if any. Literal segments win over `{param}`.
pub fn template(path: &str) -> Option<String> {
    let segs: Vec<&str> = path.split('/').collect();
    let mut best: Option<(usize, String)> = None;
    for t in SPEC["paths"].as_object()?.keys() {
        let ts: Vec<&str> = t.split('/').collect();
        if ts.len() != segs.len() {
            continue;
        }
        let mut literal = 0;
        let fits = ts.iter().zip(&segs).all(|(a, b)| {
            if a.starts_with('{') {
                !b.is_empty()
            } else {
                literal += 1;
                a == b
            }
        });
        if fits && best.as_ref().is_none_or(|(n, _)| literal > *n) {
            best = Some((literal, t.clone()));
        }
    }
    best.map(|(_, t)| t)
}

/// Check one response. `uri` may carry a query. Panics with the reason on a mismatch.
pub fn check(method: &str, uri: &str, status: u16, body: &Value) {
    let path = uri.split('?').next().unwrap_or(uri);
    let Some(t) = template(path) else {
        // A route the spec does not have must be the API's own 404 fallback.
        assert_eq!(status, 404, "{method} {uri}: no spec path, status {status}");
        assert_eq!(body["error"]["code"], "not_found", "{method} {uri}");
        return;
    };
    let m = method.to_ascii_lowercase();
    let m = if m == "head" { "get".to_string() } else { m };
    let Some(op) = SPEC["paths"][&t].get(&m) else {
        assert_eq!(
            status, 405,
            "{method} {uri}: method not in the spec, status {status}"
        );
        return;
    };
    let s = status.to_string();
    // `ONYX_OPENAPI_COVERAGE=file`: append each checked operation and status (coverage report).
    if let Ok(f) = std::env::var("ONYX_OPENAPI_COVERAGE") {
        use std::io::Write;
        if let Ok(mut h) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(f)
        {
            let _ = h.write_all(format!("{m} {t} {status}\n").as_bytes());
        }
    }
    let Some(resp) = op["responses"].get(&s) else {
        panic!(
            "{method} {t}: status {status} is not listed in docs/api/openapi.json (body {body})"
        );
    };
    let resp = deref(resp);
    let Some(schema) = resp["content"]["application/json"].get("schema") else {
        assert!(
            body.is_null(),
            "{method} {t} {status}: the spec lists no body, got {body}"
        );
        return;
    };
    if method.eq_ignore_ascii_case("head") || status == 304 {
        return;
    }
    let mut at = String::from("$");
    if let Err(e) = validate(schema, body, &mut at) {
        panic!("{method} {t} {status}: {e}\nbody: {body}");
    }
    if status >= 400 {
        let code = body["error"]["code"].as_str().unwrap_or_default();
        let listed = &SPEC["x-onyx-error-codes"][code];
        assert!(
            listed.is_object(),
            "{method} {t}: error code {code} not in x-onyx-error-codes"
        );
        assert_eq!(
            listed["status"],
            Value::from(status),
            "{method} {t}: error code {code} listed at another status"
        );
    }
}

fn deref(v: &Value) -> &Value {
    match v.get("$ref").and_then(Value::as_str) {
        Some(r) => {
            let mut node = &*SPEC;
            for part in r.trim_start_matches("#/").split('/') {
                node = &node[part];
            }
            assert!(!node.is_null(), "unresolved $ref {r}");
            deref(node)
        }
        None => v,
    }
}

fn pattern_ok(p: &str, s: &str) -> bool {
    let hex = |s: &str| s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match p {
        "^0x[0-9a-f]{64}$" => s
            .strip_prefix("0x")
            .is_some_and(|h| h.len() == 64 && hex(h)),
        "^0x([0-9a-f]{2})*$" => s
            .strip_prefix("0x")
            .is_some_and(|h| h.len() % 2 == 0 && hex(h)),
        "^0x[0-9a-fA-F]{40}$" => s
            .strip_prefix("0x")
            .is_some_and(|h| h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit())),
        "^sub_[0-9a-f]{32}$" => s
            .strip_prefix("sub_")
            .is_some_and(|h| h.len() == 32 && hex(h)),
        "^\\d{4}-\\d{2}-\\d{2}$" => {
            s.len() == 10
                && s.bytes().enumerate().all(|(i, b)| {
                    if i == 4 || i == 7 {
                        b == b'-'
                    } else {
                        b.is_ascii_digit()
                    }
                })
        }
        "^(simulated|eip155:[1-9][0-9]{0,19})$" => {
            s == "simulated"
                || s.strip_prefix("eip155:")
                    .is_some_and(|c| digits(c) && c.len() <= 20 && !c.starts_with('0'))
        }
        "^[0-9]+$" => digits(s),
        "^(0|[1-9][0-9]*)$" => digits(s) && (s == "0" || !s.starts_with('0')),
        "^[a-z0-9._-]{1,128}$" => {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
                })
        }
        "^0[xX][0-9a-fA-F]{64}$" => {
            s.get(..2).is_some_and(|p| p.eq_ignore_ascii_case("0x"))
                && s.len() == 66
                && s[2..].bytes().all(|b| b.is_ascii_hexdigit())
        }
        // A paid observation ticket (ADR-0049): 32 bytes, lowercase hex, no prefix.
        "^[0-9a-f]{64}$" => {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }
        // The feed's decimal text (PP11; the same rule as `submit::feed::is_value`). Written out
        // here: this file is also compiled into adapters/source-btc/tests/api.rs.
        "^-?(0|[1-9][0-9]*)(\\.[0-9]+)?$" => {
            let body = s.strip_prefix('-').unwrap_or(s);
            let (int, frac) = body
                .split_once('.')
                .map_or((body, None), |(i, f)| (i, Some(f)));
            digits(int) && (int == "0" || !int.starts_with('0')) && frac.is_none_or(digits)
        }
        // An estimate metric (ADR-0050, R-EST-1): the namespace is the rule.
        "^estimate\\." => s.starts_with("estimate."),
        other => panic!("openapi_check: add a matcher for pattern {other}"),
    }
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Validate `v` against `schema`; `at` is the JSON path for messages.
pub fn validate(schema: &Value, v: &Value, at: &mut String) -> Result<(), String> {
    let s = deref(schema);
    let obj = s
        .as_object()
        .ok_or(format!("{at}: schema is not an object"))?;
    for k in obj.keys() {
        if !KNOWN.contains(&k.as_str()) {
            return Err(format!("{at}: schema keyword {k} is not checked here"));
        }
    }
    if let Some(alts) = s.get("oneOf").and_then(Value::as_array) {
        let n = alts
            .iter()
            .filter(|a| validate(a, v, &mut at.clone()).is_ok())
            .count();
        if n != 1 {
            return Err(format!(
                "{at}: {n} of {} oneOf alternatives match {v}",
                alts.len()
            ));
        }
    }
    if let Some(t) = s.get("type").and_then(Value::as_str) {
        let k = kind(v);
        if k != t && !(t == "number" && k == "integer") {
            return Err(format!("{at}: expected {t}, got {k} ({v})"));
        }
    }
    if let Some(e) = s.get("enum").and_then(Value::as_array) {
        if !e.contains(v) {
            return Err(format!("{at}: {v} not in {e:?}"));
        }
    }
    if let Some(n) = v.as_f64() {
        if s.get("minimum")
            .and_then(Value::as_f64)
            .is_some_and(|m| n < m)
        {
            return Err(format!("{at}: {n} below the minimum"));
        }
        if s.get("maximum")
            .and_then(Value::as_f64)
            .is_some_and(|m| n > m)
        {
            return Err(format!("{at}: {n} above the maximum"));
        }
    }
    if let Some(text) = v.as_str() {
        if let Some(p) = s.get("pattern").and_then(Value::as_str) {
            if !pattern_ok(p, text) {
                return Err(format!("{at}: {text:?} does not match {p}"));
            }
        }
        if s.get("maxLength")
            .and_then(Value::as_u64)
            .is_some_and(|m| text.chars().count() as u64 > m)
        {
            return Err(format!("{at}: longer than maxLength"));
        }
    }
    if let (Some(items), Some(arr)) = (s.get("items"), v.as_array()) {
        for (i, x) in arr.iter().enumerate() {
            let mut p = format!("{at}[{i}]");
            validate(items, x, &mut p)?;
        }
    }
    if let Some(o) = v.as_object() {
        let props = s.get("properties").and_then(Value::as_object);
        for r in s
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let r = r.as_str().unwrap_or_default();
            if !o.contains_key(r) {
                return Err(format!("{at}: required key {r} missing"));
            }
        }
        for (k, x) in o {
            let mut p = format!("{at}.{k}");
            match (props.and_then(|p| p.get(k)), s.get("additionalProperties")) {
                (Some(ps), _) => validate(ps, x, &mut p)?,
                (None, Some(Value::Bool(false))) => {
                    return Err(format!("{at}: key {k} is not in the spec"))
                }
                (None, Some(extra @ Value::Object(_))) => validate(extra, x, &mut p)?,
                (None, _) => {}
            }
        }
    }
    Ok(())
}

#[test]
fn the_checker_refuses_what_the_spec_does_not_say() {
    let mut at = String::from("$");
    let hash = serde_json::json!({"$ref": "#/components/schemas/Hash"});
    assert!(validate(
        &hash,
        &Value::from(format!("0x{}", "a".repeat(64))),
        &mut at
    )
    .is_ok());
    assert!(validate(&hash, &Value::from("0xABC"), &mut at).is_err());
    let status = serde_json::json!({"$ref": "#/components/schemas/ReceiptStatus"});
    let id = format!("sub_{}", "1".repeat(32));
    assert!(validate(
        &status,
        &serde_json::json!({"id": id, "status": "approved"}),
        &mut at
    )
    .is_ok());
    assert!(
        validate(
            &status,
            &serde_json::json!({"id": id, "status": "approved", "x": 1}),
            &mut at
        )
        .is_err(),
        "an extra key"
    );
    assert!(
        validate(&status, &serde_json::json!({"id": id}), &mut at).is_err(),
        "a missing key"
    );
    assert!(
        validate(
            &status,
            &serde_json::json!({"id": id, "status": "lost"}),
            &mut at
        )
        .is_err(),
        "an enum"
    );
    assert_eq!(template("/v1/track/log").as_deref(), Some("/v1/track/log"));
    assert_eq!(
        template("/v1/track/a.b/c.d").as_deref(),
        Some("/v1/track/{source}/{metric}")
    );
    assert_eq!(template("/v1/nothing"), None);
}
