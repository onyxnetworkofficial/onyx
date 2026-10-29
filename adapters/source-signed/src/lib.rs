//! Example level-3 source (ADR-0028): a source's published keys document and
//! its signed reports, as JSON. Verification is `onyx_coordinator::signed`.

use onyx_core::{
    keccak256, Hash32, KeyProvenance, MetricId, SignedPayload, SignedReport, SourceId, SourceKey,
    Subject, Value,
};
use serde_json::Value as Json;

/// Parse error.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// Not JSON, or a field is missing or has the wrong shape.
    #[error("bad document: {0}")]
    Bad(&'static str),
}

fn field<'a>(j: &'a Json, name: &'static str) -> Result<&'a Json, ParseError> {
    j.get(name).ok_or(ParseError::Bad(name))
}

fn string<'a>(j: &'a Json, name: &'static str) -> Result<&'a str, ParseError> {
    field(j, name)?.as_str().ok_or(ParseError::Bad(name))
}

fn uint(j: &Json, name: &'static str) -> Result<u64, ParseError> {
    field(j, name)?.as_u64().ok_or(ParseError::Bad(name))
}

fn hex_bytes(s: &str, what: &'static str) -> Result<Vec<u8>, ParseError> {
    let body = s.strip_prefix("0x").ok_or(ParseError::Bad(what))?;
    if body.bytes().any(|c| c.is_ascii_uppercase()) {
        return Err(ParseError::Bad(what));
    }
    hex::decode(body).map_err(|_| ParseError::Bad(what))
}

fn fixed<const N: usize>(s: &str, what: &'static str) -> Result<[u8; N], ParseError> {
    hex_bytes(s, what)?
        .try_into()
        .map_err(|_| ParseError::Bad(what))
}

/// The subject grammar used in documents: `entity:<slug>` only (sources sign
/// facts about organisations; addresses, instruments and aggregates are not
/// needed by the example).
fn subject(s: &str) -> Result<Subject, ParseError> {
    if s.starts_with("entity:") {
        Ok(Subject::Entity(s.into()))
    } else {
        Err(ParseError::Bad("subject"))
    }
}

/// `{"uint": "<decimal>"}` and friends; numbers are strings, never JSON
/// numbers (no floats, ADR-0009).
fn dec(v: &Json) -> Result<&str, ParseError> {
    v.as_str().ok_or(ParseError::Bad("value"))
}

fn value(j: &Json) -> Result<Value, ParseError> {
    let obj = j.as_object().ok_or(ParseError::Bad("value"))?;
    if obj.len() != 1 {
        return Err(ParseError::Bad("value"));
    }
    let digits = |s: &str| {
        let body = s.strip_prefix('-').unwrap_or(s);
        !body.is_empty() && body.bytes().all(|c| c.is_ascii_digit())
    };
    let (k, v) = obj.iter().next().ok_or(ParseError::Bad("value"))?;
    match k.as_str() {
        "uint" => {
            let s = dec(v)?;
            if s.starts_with('-') || !digits(s) {
                return Err(ParseError::Bad("value"));
            }
            s.parse()
                .map(Value::UInt)
                .map_err(|_| ParseError::Bad("value"))
        }
        "int" => {
            let s = dec(v)?;
            if !digits(s) {
                return Err(ParseError::Bad("value"));
            }
            s.parse()
                .map(Value::Int)
                .map_err(|_| ParseError::Bad("value"))
        }
        "fixed" => {
            let m = dec(field(v, "mantissa")?)?;
            if !digits(m) {
                return Err(ParseError::Bad("value"));
            }
            let mantissa = m.parse().map_err(|_| ParseError::Bad("value"))?;
            let scale = u8::try_from(uint(v, "scale")?).map_err(|_| ParseError::Bad("value"))?;
            Ok(Value::Fixed { mantissa, scale })
        }
        "bool" => v.as_bool().map(Value::Bool).ok_or(ParseError::Bad("value")),
        "bytes" => Ok(Value::Bytes(hex_bytes(dec(v)?, "value")?)),
        _ => Err(ParseError::Bad("value")),
    }
}

/// The source's keys from its published keys document, with the provenance
/// the reviewer records: the origin it was fetched from, the Keccak-256 of the
/// exact bytes, and when. Every key must be an `eip191` signer for the one
/// source the document names.
pub fn keys_from_document(
    bytes: &[u8],
    origin: &str,
    fetched_unix: u64,
) -> Result<Vec<SourceKey>, ParseError> {
    let j: Json = serde_json::from_slice(bytes).map_err(|_| ParseError::Bad("json"))?;
    if uint(&j, "version")? != 1 {
        return Err(ParseError::Bad("version"));
    }
    let source = SourceId(string(&j, "source")?.into());
    let keys = field(&j, "keys")?
        .as_array()
        .ok_or(ParseError::Bad("keys"))?;
    if keys.is_empty() {
        return Err(ParseError::Bad("keys"));
    }
    let document_hash = keccak256(bytes);
    keys.iter()
        .map(|k| {
            if string(k, "scheme")? != "eip191" {
                return Err(ParseError::Bad("scheme"));
            }
            Ok(SourceKey {
                source: source.clone(),
                signer: fixed::<20>(string(k, "signer")?, "signer")?,
                provenance: KeyProvenance::WellKnown {
                    origin: origin.into(),
                    document_hash,
                    fetched_unix,
                },
            })
        })
        .collect()
}

/// A signed report from its JSON form.
pub fn report_from_json(bytes: &[u8]) -> Result<SignedReport, ParseError> {
    let j: Json = serde_json::from_slice(bytes).map_err(|_| ParseError::Bad("json"))?;
    let payload = SignedPayload {
        source: SourceId(string(&j, "source")?.into()),
        key: Hash32(fixed::<32>(string(&j, "key")?, "key")?),
        metric: MetricId(string(&j, "metric")?.into()),
        subject: subject(string(&j, "subject")?)?,
        value: value(field(&j, "value")?)?,
        observed_unix: uint(&j, "observedUnix")?,
        sequence: uint(&j, "sequence")?,
    };
    Ok(SignedReport {
        payload,
        signature: fixed::<65>(string(&j, "signature")?, "signature")?,
    })
}

/// The JSON form of `v` (inverse of the parser above).
pub fn value_json(v: &Value) -> Json {
    match v {
        Value::UInt(n) => serde_json::json!({ "uint": n.to_string() }),
        Value::Int(n) => serde_json::json!({ "int": n.to_string() }),
        Value::Fixed { mantissa, scale } => {
            serde_json::json!({ "fixed": { "mantissa": mantissa.to_string(), "scale": scale } })
        }
        Value::Bool(b) => serde_json::json!({ "bool": b }),
        Value::Bytes(b) => serde_json::json!({ "bytes": format!("0x{}", hex::encode(b)) }),
    }
}

/// The JSON form of a report.
pub fn report_json(r: &SignedReport) -> Json {
    let p = &r.payload;
    let subject = match &p.subject {
        Subject::Entity(s)
        | Subject::Address(s)
        | Subject::Instrument(s)
        | Subject::Aggregate(s) => s.clone(),
    };
    serde_json::json!({
        "source": p.source.0,
        "key": format!("0x{}", p.key.to_hex()),
        "metric": p.metric.0,
        "subject": subject,
        "value": value_json(&p.value),
        "observedUnix": p.observed_unix,
        "sequence": p.sequence,
        "signature": format!("0x{}", hex::encode(r.signature)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_parse_strictly() {
        let ok = [
            (r#"{"uint":"42"}"#, Value::UInt(42)),
            (r#"{"int":"-7"}"#, Value::Int(-7)),
            (
                r#"{"fixed":{"mantissa":"6412380","scale":2}}"#,
                Value::Fixed {
                    mantissa: 6_412_380,
                    scale: 2,
                },
            ),
            (r#"{"bool":true}"#, Value::Bool(true)),
            (r#"{"bytes":"0x00ff"}"#, Value::Bytes(vec![0, 255])),
        ];
        for (text, want) in ok {
            let j: Json = serde_json::from_str(text).expect("json");
            assert_eq!(value(&j), Ok(want.clone()), "{text}");
            assert_eq!(value(&value_json(&want)), Ok(want), "{text} roundtrip");
        }
        for bad in [
            r#"{"uint":42}"#,
            r#"{"uint":"-1"}"#,
            r#"{"uint":"4.2"}"#,
            r#"{"uint":""}"#,
            r#"{"int":"1e3"}"#,
            r#"{"bytes":"00ff"}"#,
            r#"{"bytes":"0x00FF"}"#,
            r#"{"uint":"1","int":"1"}"#,
            r#"{"float":"1.5"}"#,
            r#"42"#,
        ] {
            let j: Json = serde_json::from_str(bad).expect("json");
            assert!(value(&j).is_err(), "{bad}");
        }
    }

    #[test]
    fn keys_document_needs_eip191_and_one_source() {
        let doc = br#"{"version":1,"source":"example.signed","keys":[{"signer":"0x0101010101010101010101010101010101010101","scheme":"eip191"}]}"#;
        let keys = keys_from_document(doc, "data.example.org", 1_000).expect("keys");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].validate(), Ok(()));
        assert_eq!(
            keys[0].provenance,
            KeyProvenance::WellKnown {
                origin: "data.example.org".into(),
                document_hash: keccak256(doc),
                fetched_unix: 1_000,
            }
        );
        for bad in [
            &br#"{"version":2,"source":"example.signed","keys":[]}"#[..],
            br#"{"version":1,"source":"example.signed","keys":[]}"#,
            br#"{"version":1,"source":"example.signed","keys":[{"signer":"0x01","scheme":"eip191"}]}"#,
            br#"{"version":1,"source":"example.signed","keys":[{"signer":"0x0101010101010101010101010101010101010101","scheme":"ed25519"}]}"#,
        ] {
            assert!(keys_from_document(bad, "data.example.org", 1).is_err());
        }
    }
}
