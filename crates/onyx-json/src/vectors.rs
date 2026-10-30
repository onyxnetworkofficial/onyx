//! The shared vectors' checker (`fixtures/json-fetcher/vectors.json`, THREATS CL3-3): every
//! descriptor is read back from its JSON form and must give its Borsh bytes and fetcher id;
//! every case is recomputed from the file alone and must give its answer. Native tests and the
//! wasm32 build (`scripts/check-json-wasm.sh`) run this same function.

use crate::form::{descriptor_from_node, quote};
use crate::reader::{parse, Node};
use crate::{read_at, Why};
use onyx_core::feed_json::JsonDescriptor;
use onyx_core::{Answer, ObservationKind};

/// The answer's JSON form in the vectors file.
pub fn answer_json(r: &Result<Answer, Why>) -> String {
    match r {
        Ok(Answer::Number(n)) => format!("{{\"number\":\"{n}\"}}"),
        Ok(Answer::Bytes(b)) => match std::str::from_utf8(b) {
            Ok(s) => format!("{{\"bytes\":{}}}", quote(s)),
            Err(_) => "{\"bytes\":null}".into(),
        },
        Ok(Answer::Unavailable) => "\"unavailable\"".into(),
        Err(why) => format!("{{\"unavailable\":\"{}\"}}", why.code()),
    }
}

/// A case's feed kind: `{"numeric": scale}` or `{"exact": max_len}`.
pub fn kind_of(n: &Node) -> Option<ObservationKind> {
    if let Some(Node::Num(s)) = n.get("numeric") {
        return Some(ObservationKind::Numeric {
            scale: s.parse().ok()?,
            tolerance: 0,
        });
    }
    match n.get("exact") {
        Some(Node::Num(m)) => Some(ObservationKind::Exact {
            max_len: m.parse().ok()?,
        }),
        _ => None,
    }
}

fn unhex(h: &str) -> Option<Vec<u8>> {
    if !h.len().is_multiple_of(2) {
        return None;
    }
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(h.get(i..i + 2)?, 16).ok())
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Check every descriptor and case of the vectors file `file`; the number of cases.
pub fn check(file: &[u8]) -> Result<usize, String> {
    let doc = parse(file, 1 << 20).map_err(|e| format!("vectors: {e}"))?;
    let Some(Node::Obj(ds)) = doc.get("descriptors") else {
        return Err("vectors: no descriptors".into());
    };
    let mut loaded: Vec<(&str, JsonDescriptor)> = vec![];
    for (name, entry) in ds {
        let form = entry.get("form").ok_or("descriptor without form")?;
        let d = descriptor_from_node(form).map_err(|e| format!("{name}: {e}"))?;
        let borsh = borsh::to_vec(&d).map_err(|e| e.to_string())?;
        if entry.get("borsh_hex").and_then(Node::as_str) != Some(hex(&borsh).as_str()) {
            return Err(format!("{name}: Borsh bytes differ"));
        }
        if entry.get("fetcher_id").and_then(Node::as_str)
            != Some(d.fetcher_id().to_string().as_str())
        {
            return Err(format!("{name}: fetcher id differs"));
        }
        loaded.push((name, d));
    }
    let Some(Node::Arr(cases)) = doc.get("cases") else {
        return Err("vectors: no cases".into());
    };
    for c in cases {
        let name = c.get("name").and_then(Node::as_str).unwrap_or("?");
        let dn = c
            .get("descriptor")
            .and_then(Node::as_str)
            .ok_or("case descriptor")?;
        let d = &loaded
            .iter()
            .find(|(n, _)| *n == dn)
            .ok_or(format!("{name}: unknown descriptor"))?
            .1;
        let body = match (c.get("body"), c.get("body_hex")) {
            (Some(Node::Str(s)), None) => s.as_bytes().to_vec(),
            (None, Some(Node::Str(h))) => unhex(h).ok_or(format!("{name}: body_hex"))?,
            _ => return Err(format!("{name}: body")),
        };
        let kind = c
            .get("kind")
            .and_then(kind_of)
            .ok_or(format!("{name}: kind"))?;
        let date = c.get("subject_date").and_then(Node::as_str);
        // ADR-0058: a case of a bucketed subject names its bucket's start (Unix seconds).
        let bucket = match c.get("bucket_start") {
            None => None,
            Some(Node::Num(t)) => {
                Some(onyx_core::feed::canonical_u64(t).ok_or(format!("{name}: bucket_start"))?)
            }
            Some(_) => return Err(format!("{name}: bucket_start")),
        };
        let got = parse(
            answer_json(&read_at(d, kind, date, bucket, &body)).as_bytes(),
            4096,
        )
        .map_err(|e| e.to_string())?;
        if Some(&got) != c.get("answer") {
            return Err(format!(
                "{name}: answer {got:?}, vectors say {:?}",
                c.get("answer")
            ));
        }
    }
    Ok(cases.len())
}
