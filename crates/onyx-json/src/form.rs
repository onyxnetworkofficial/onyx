//! The descriptor's JSON form, as a source manifest and the vectors file write it.
//!
//! ```json
//! {"version": 1, "pointer": "/data/0/open_today_bal", "value": "decimal_string",
//!  "unit": "million USD", "max_bytes": 65536, "access": "public",
//!  "asserts": [{"pointer": "/data", "expect": {"array_len": 1}},
//!              {"pointer": "/data/0/record_date", "expect": "subject_date"}]}
//! ```
//! `value` is `decimal_string`, `json_number`, `decimal_or_number`, `array_len`, `bool` or
//! `{"text": {"trim": true}}`; `expect` is `{"str": "..."}`, `{"int": 5}`, `"subject_date"`,
//! `{"keys_exactly": [...]}` or `{"array_len": 1}`. Read strictly: unknown or missing fields,
//! duplicate keys and wrong types refuse the form; the result must also pass
//! [`JsonDescriptor::validate`].

use crate::reader::{parse, Node};
use onyx_core::feed_json::{
    Access, Assert, Expect, JsonDescriptor, KeyPlace, KeyScheme, TimeForm, ValueRule,
};

/// Why a descriptor's JSON form is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FormError {
    /// Not strict JSON.
    #[error("{0}")]
    Read(#[from] crate::reader::ReadError),
    /// A field is missing, unknown or of the wrong type.
    #[error("descriptor field {0}")]
    Field(&'static str),
    /// The descriptor is out of bounds.
    #[error("{0}")]
    Invalid(#[from] onyx_core::feed_json::DescriptorError),
}

fn fields<'a>(
    n: &'a Node,
    allowed: &[&'static str],
    what: &'static str,
) -> Result<&'a [(String, Node)], FormError> {
    let Node::Obj(kv) = n else {
        return Err(FormError::Field(what));
    };
    if kv.iter().any(|(k, _)| !allowed.contains(&k.as_str())) {
        return Err(FormError::Field(what));
    }
    Ok(kv)
}

fn need<'a>(n: &'a Node, key: &str, what: &'static str) -> Result<&'a Node, FormError> {
    n.get(key).ok_or(FormError::Field(what))
}

fn uint(n: &Node, what: &'static str) -> Result<u64, FormError> {
    match n {
        Node::Num(t)
            if t.bytes().all(|c| c.is_ascii_digit()) && (t.len() == 1 || !t.starts_with('0')) =>
        {
            t.parse().map_err(|_| FormError::Field(what))
        }
        _ => Err(FormError::Field(what)),
    }
}

fn text(n: &Node, what: &'static str) -> Result<String, FormError> {
    n.as_str().map(str::to_owned).ok_or(FormError::Field(what))
}

/// The descriptor of a parsed JSON form.
pub fn descriptor_from_node(n: &Node) -> Result<JsonDescriptor, FormError> {
    fields(
        n,
        &[
            "access",
            "asserts",
            "max_bytes",
            "pointer",
            "unit",
            "value",
            "version",
        ],
        "descriptor",
    )?;
    let version = u8::try_from(uint(need(n, "version", "version")?, "version")?)
        .map_err(|_| FormError::Field("version"))?;
    let value = match need(n, "value", "value")? {
        Node::Str(s) => match s.as_str() {
            "decimal_string" => ValueRule::DecimalString,
            "json_number" => ValueRule::JsonNumber,
            "decimal_or_number" => ValueRule::DecimalOrNumber,
            "array_len" => ValueRule::ArrayLen,
            "bool" => ValueRule::Bool,
            _ => return Err(FormError::Field("value")),
        },
        o @ Node::Obj(_) => {
            fields(o, &["text"], "value")?;
            let t = need(o, "text", "value")?;
            fields(t, &["trim"], "value.text")?;
            match need(t, "trim", "value.text.trim")? {
                Node::Bool(trim) => ValueRule::Text { trim: *trim },
                _ => return Err(FormError::Field("value.text.trim")),
            }
        }
        _ => return Err(FormError::Field("value")),
    };
    let Node::Arr(items) = need(n, "asserts", "asserts")? else {
        return Err(FormError::Field("asserts"));
    };
    let mut asserts = Vec::with_capacity(items.len());
    for a in items {
        fields(a, &["expect", "pointer"], "assert")?;
        let pointer = text(need(a, "pointer", "assert.pointer")?, "assert.pointer")?;
        let expect = match need(a, "expect", "assert.expect")? {
            Node::Str(s) if s == "subject_date" => Expect::SubjectDate,
            Node::Str(s) if s == "positive" => Expect::Positive,
            Node::Obj(kv) if kv.len() == 1 => {
                let (k, v) = &kv[0];
                match k.as_str() {
                    "str" => Expect::Str(text(v, "expect.str")?),
                    "int" => match v {
                        Node::Num(t) => Expect::Int(
                            t.parse::<i128>()
                                .ok()
                                .filter(|i| i.to_string() == *t)
                                .ok_or(FormError::Field("expect.int"))?,
                        ),
                        _ => return Err(FormError::Field("expect.int")),
                    },
                    "array_len" => Expect::ArrayLen(
                        u32::try_from(uint(v, "expect.array_len")?)
                            .map_err(|_| FormError::Field("expect.array_len"))?,
                    ),
                    // ADR-0058: {"bucket_start": "unix_secs" | "unix_millis" | "rfc3339"}.
                    "bucket_start" => {
                        Expect::BucketStart(match text(v, "expect.bucket_start")?.as_str() {
                            "unix_secs" => TimeForm::UnixSecs,
                            "unix_millis" => TimeForm::UnixMillis,
                            "rfc3339" => TimeForm::Rfc3339,
                            _ => return Err(FormError::Field("expect.bucket_start")),
                        })
                    }
                    "keys_exactly" => match v {
                        Node::Arr(keys) => Expect::KeysExactly(
                            keys.iter()
                                .map(|k| text(k, "expect.keys_exactly"))
                                .collect::<Result<_, _>>()?,
                        ),
                        _ => return Err(FormError::Field("expect.keys_exactly")),
                    },
                    _ => return Err(FormError::Field("assert.expect")),
                }
            }
            _ => return Err(FormError::Field("assert.expect")),
        };
        asserts.push(Assert { pointer, expect });
    }
    let max_bytes = u32::try_from(uint(need(n, "max_bytes", "max_bytes")?, "max_bytes")?)
        .map_err(|_| FormError::Field("max_bytes"))?;
    let access = match need(n, "access", "access")? {
        Node::Str(s) if s == "public" => Access::Public,
        // ADR-0054 item 1: {"keyed": {"place": "header"|"query", "name": "...",
        // "scheme": "raw"|"bearer"}}. Never a key: there is no field for one.
        o @ Node::Obj(_) => {
            fields(o, &["keyed"], "access")?;
            let k = need(o, "keyed", "access")?;
            fields(k, &["name", "place", "scheme"], "access.keyed")?;
            let place = match text(
                need(k, "place", "access.keyed.place")?,
                "access.keyed.place",
            )?
            .as_str()
            {
                "header" => KeyPlace::Header,
                "query" => KeyPlace::Query,
                _ => return Err(FormError::Field("access.keyed.place")),
            };
            let scheme = match text(
                need(k, "scheme", "access.keyed.scheme")?,
                "access.keyed.scheme",
            )?
            .as_str()
            {
                "raw" => KeyScheme::Raw,
                "bearer" => KeyScheme::Bearer,
                _ => return Err(FormError::Field("access.keyed.scheme")),
            };
            Access::Keyed {
                place,
                name: text(need(k, "name", "access.keyed.name")?, "access.keyed.name")?,
                scheme,
            }
        }
        _ => return Err(FormError::Field("access")),
    };
    let d = JsonDescriptor {
        version,
        pointer: text(need(n, "pointer", "pointer")?, "pointer")?,
        value,
        unit: text(need(n, "unit", "unit")?, "unit")?,
        asserts,
        max_bytes,
        access,
    };
    d.validate()?;
    Ok(d)
}

/// The descriptor of a JSON form's bytes (strict: [`parse`]).
pub fn descriptor_from_json(bytes: &[u8]) -> Result<JsonDescriptor, FormError> {
    descriptor_from_node(&parse(bytes, 64 * 1024)?)
}

/// A JSON string literal of `s`.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The JSON form of `d` (one line; fields in a fixed order).
pub fn descriptor_to_json(d: &JsonDescriptor) -> String {
    let value = match d.value {
        ValueRule::DecimalString => "\"decimal_string\"".to_string(),
        ValueRule::JsonNumber => "\"json_number\"".into(),
        ValueRule::DecimalOrNumber => "\"decimal_or_number\"".into(),
        ValueRule::ArrayLen => "\"array_len\"".into(),
        ValueRule::Bool => "\"bool\"".into(),
        ValueRule::Text { trim } => format!("{{\"text\":{{\"trim\":{trim}}}}}"),
    };
    let asserts: Vec<String> = d
        .asserts
        .iter()
        .map(|a| {
            let e = match &a.expect {
                Expect::Str(s) => format!("{{\"str\":{}}}", quote(s)),
                Expect::Int(i) => format!("{{\"int\":{i}}}"),
                Expect::SubjectDate => "\"subject_date\"".into(),
                Expect::Positive => "\"positive\"".into(),
                Expect::BucketStart(f) => format!(
                    "{{\"bucket_start\":\"{}\"}}",
                    match f {
                        TimeForm::UnixSecs => "unix_secs",
                        TimeForm::UnixMillis => "unix_millis",
                        TimeForm::Rfc3339 => "rfc3339",
                    }
                ),
                Expect::ArrayLen(n) => format!("{{\"array_len\":{n}}}"),
                Expect::KeysExactly(k) => format!(
                    "{{\"keys_exactly\":[{}]}}",
                    k.iter().map(|k| quote(k)).collect::<Vec<_>>().join(",")
                ),
            };
            format!("{{\"pointer\":{},\"expect\":{e}}}", quote(&a.pointer))
        })
        .collect();
    let access = match &d.access {
        Access::Public => "\"public\"".to_string(),
        Access::Keyed {
            place,
            name,
            scheme,
        } => format!(
            "{{\"keyed\":{{\"place\":\"{}\",\"name\":{},\"scheme\":\"{}\"}}}}",
            match place {
                KeyPlace::Header => "header",
                KeyPlace::Query => "query",
            },
            quote(name),
            match scheme {
                KeyScheme::Raw => "raw",
                KeyScheme::Bearer => "bearer",
            }
        ),
    };
    format!(
        "{{\"version\":{},\"pointer\":{},\"value\":{value},\"unit\":{},\"asserts\":[{}],\"max_bytes\":{},\"access\":{access}}}",
        d.version,
        quote(&d.pointer),
        quote(&d.unit),
        asserts.join(","),
        d.max_bytes
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORM: &str = r#"{"version":1,"pointer":"/data/0/v","value":{"text":{"trim":false}},"unit":"degC","asserts":[{"pointer":"/data","expect":{"array_len":1}},{"pointer":"/data/0","expect":{"keys_exactly":["d","v"]}},{"pointer":"/data/0/d","expect":"subject_date"},{"pointer":"/n","expect":{"int":-3}},{"pointer":"/s","expect":{"str":"a\"b"}}],"max_bytes":4096,"access":"public"}"#;

    #[test]
    fn the_form_round_trips() {
        let d = descriptor_from_json(FORM.as_bytes()).expect("form");
        assert_eq!(descriptor_to_json(&d), FORM);
    }

    #[test]
    fn a_loose_form_is_refused() {
        for bad in [
            FORM.replace("\"access\":\"public\"", "\"access\":\"keyed\""),
            FORM.replace("\"version\":1", "\"version\":1,\"extra\":0"),
            FORM.replace("\"version\":1,", ""),
            FORM.replace("\"max_bytes\":4096", "\"max_bytes\":4096.0"),
            FORM.replace("\"max_bytes\":4096", "\"max_bytes\":\"4096\""),
            FORM.replace("{\"int\":-3}", "{\"int\":-3.0}"),
            FORM.replace("{\"int\":-3}", "{\"int\":-3,\"str\":\"x\"}"),
            FORM.replace("\"unit\":\"degC\"", "\"unit\":\"degC\",\"unit\":\"degF\""),
            FORM.replace("[\"d\",\"v\"]", "[\"v\",\"d\"]"),
        ] {
            assert!(descriptor_from_json(bad.as_bytes()).is_err(), "{bad}");
        }
    }

    /// ADR-0054 item 1: the keyed access form round-trips, is strict, and has no key field.
    #[test]
    fn the_keyed_form_round_trips_and_has_no_key_field() {
        let keyed = FORM.replace(
            "\"access\":\"public\"",
            "\"access\":{\"keyed\":{\"place\":\"query\",\"name\":\"api_key\",\"scheme\":\"raw\"}}",
        );
        let d = descriptor_from_json(keyed.as_bytes()).expect("keyed form");
        assert!(d.access.is_keyed());
        assert_eq!(descriptor_to_json(&d), keyed);
        for bad in [
            keyed.replace("\"scheme\":\"raw\"", "\"scheme\":\"raw\",\"key\":\"abc\""),
            keyed.replace("\"place\":\"query\"", "\"place\":\"body\""),
            keyed.replace("\"scheme\":\"raw\"", "\"scheme\":\"bearer\""),
            keyed.replace("\"name\":\"api_key\"", "\"name\":\"\""),
            keyed.replace("{\"keyed\":", "{\"public\":"),
        ] {
            assert!(descriptor_from_json(bad.as_bytes()).is_err(), "{bad}");
        }
    }
}
