//! Tests of the publisher application (ADR-0066 slice 1; THREATS PUBON).

use super::*;
use crate::anchor::address_of;
use crate::input::{Format, Records};
use crate::manifest::{NewVersion, ShardEntry};
use crate::row::encode_row;
use crate::shard::encode_shard;
use crate::Schema;
use onyx_core::{DataRights, SourceId};
use serde_json::json;

/// Made-up test keys (never real ones).
pub(crate) const KEY_A: [u8; 32] = [0x21; 32];
const KEY_B: [u8; 32] = [0x22; 32];
const KEY_C: [u8; 32] = [0x23; 32];
const NET: Network = Network {
    chain_id: 84_532,
    book: [0xB0; 20],
};
const T0: u64 = 1_794_873_600;
const ID: &str = "river-levels.open";
const ORIGIN: &str = "data.example.org/river-levels";

fn addr(k: &[u8; 32]) -> [u8; 20] {
    address_of(k).expect("key")
}

fn keys_doc(id: &str, keys: &[&[u8; 32]]) -> Vec<u8> {
    let keys: Vec<_> = keys
        .iter()
        .map(|k| json!({"signer": hex0x(&addr(k)), "scheme": "eip191"}))
        .collect();
    format!("{}\n", json!({"version": 1, "source": id, "keys": keys})).into_bytes()
}

/// A built and signed sample: (manifest, report, shard) bytes.
pub(crate) struct Built {
    pub manifest: Manifest,
    pub manifest_bytes: Vec<u8>,
    pub report: Vec<u8>,
    pub shard: Vec<u8>,
}

/// Version 1 of `river-levels.daily` with `rows` rows cut into shards of
/// `per_shard` rows, signed by `key`.
pub(crate) fn sample_with(
    publisher: &str,
    created: u64,
    rows: u64,
    per_shard: u64,
    key: &[u8; 32],
) -> Built {
    let schema = Schema::from_json(&json!({"columns": [
        {"name": "t", "kind": "int"},
        {"name": "level", "kind": "decimal", "scale": 2}
    ], "key": "t"}))
    .expect("schema");
    let mut csv = String::from("t,level\n");
    for i in 0..rows {
        csv.push_str(&format!("{},{}.25\n", 1_700_000_000 + i * 86_400, i));
    }
    let mut records = Records::new(csv.as_bytes(), schema.clone(), Format::Csv).expect("records");
    let mut encoded = Vec::new();
    while let Some((_, cells)) = records.next_record().expect("row") {
        encoded.push(encode_row(&schema, &cells).expect("encode"));
    }
    let mut entries = Vec::new();
    let mut shards = Vec::new();
    for (i, chunk) in encoded.chunks(per_shard as usize).enumerate() {
        let file = encode_shard(chunk);
        let c = check_shard(&schema, &file).expect("shard");
        entries.push(ShardEntry::new(i as u32, &c.summary));
        shards.push(file);
    }
    let v = NewVersion {
        dataset: "river-levels.daily".into(),
        publisher: SourceId(publisher.into()),
        created_unix: created,
        schema,
        rights: DataRights {
            license: "CC0-1.0".into(),
            redistribution: Redistribution::Allowed,
            source: SourceId(publisher.into()),
            attribution: None,
            terms: String::new(),
        },
    };
    let manifest = Manifest::build(v, None, entries).expect("manifest");
    let report = crate::anchor::sign(key, &manifest.header).expect("sign");
    Built {
        manifest_bytes: manifest.to_bytes(),
        report: borsh::to_vec(&report).expect("report"),
        shard: shards.remove(0),
        manifest,
    }
}

fn sample(created: u64, key: &[u8; 32]) -> Built {
    sample_with(ID, created, 3, 100, key)
}

pub(crate) fn fields_for(b: &Built, key: &[u8; 32], doc: &[u8], created: u64) -> Fields {
    Fields {
        version: VERSION,
        chain_id: NET.chain_id,
        book: NET.book,
        publisher: b.manifest.header.publisher.0.clone(),
        origin: ORIGIN.into(),
        signer: addr(key),
        keys_document_keccak: keccak256(doc),
        data: "Daily water levels of 40 public gauges, from the publisher's own sensors.".into(),
        sample: Sample {
            dataset: b.manifest.header.dataset.clone(),
            manifest_root: b.manifest.header.root(),
            bytes: b.manifest.header.byte_count,
            path: "sample/".into(),
        },
        asks: Asks {
            quota_gib: DEFAULT_QUOTA_GIB,
            datasets: DEFAULT_DATASETS,
        },
        created_unix: created,
        expires_unix: created + LIFETIME_SECS,
    }
}

/// A first application by key A, its keys document and its sample.
fn first() -> (Application, Vec<u8>, Built) {
    let b = sample(T0 - 60, &KEY_A);
    let doc = keys_doc(ID, &[&KEY_A]);
    let a = Application::sign(fields_for(&b, &KEY_A, &doc, T0), &KEY_A).expect("sign");
    (a, doc, b)
}

fn reg(key: &[u8; 32], life: KeyLife) -> RegisteredKey {
    RegisteredKey {
        publisher: ID.into(),
        origin: ORIGIN.into(),
        signer: addr(key),
        life,
    }
}

#[test]
fn round_trips_and_the_signature_binds_every_field() {
    let (a, doc, b) = first();
    let text = a.to_json();
    assert!(text.len() < MAX_APPLICATION_BYTES);
    let back = Application::from_json(text.as_bytes()).expect("parse");
    assert_eq!(back, a);
    assert_eq!(back.to_json(), text);
    assert_eq!(a.verify(&NET, T0 + 10, &doc, &[], &[]), Ok(Standing::First));
    let facts = a
        .check_sample(&Standing::First, &b.manifest_bytes, &b.report, &b.shard)
        .expect("sample");
    assert_eq!((facts.rows, facts.license.as_str()), (3, "CC0-1.0"));
    assert_eq!(facts.report_signer, addr(&KEY_A));
    assert_eq!(facts.stale_unix, T0 - 60 + REPORT_LIFE_SECS);

    // Every field is under the signature: change one, keep the rest well
    // formed, and the signer no longer recovers.
    type Change = Box<dyn Fn(&mut Fields)>;
    let changes: Vec<(&str, Change)> = vec![
        ("chain_id", Box::new(|f| f.chain_id += 1)),
        ("book", Box::new(|f| f.book[0] ^= 1)),
        (
            "publisher",
            Box::new(|f| f.publisher = "lake-levels.open".into()),
        ),
        (
            "origin",
            Box::new(|f| f.origin = "data.example.org/other".into()),
        ),
        ("signer", Box::new(|f| f.signer = addr(&KEY_B))),
        (
            "keys_document_keccak",
            Box::new(|f| f.keys_document_keccak.0[0] ^= 1),
        ),
        ("data", Box::new(|f| f.data.push('!'))),
        ("sample.dataset", Box::new(|f| f.sample.dataset.push('x'))),
        (
            "sample.manifest_root",
            Box::new(|f| f.sample.manifest_root.0[31] ^= 1),
        ),
        ("sample.bytes", Box::new(|f| f.sample.bytes += 1)),
        ("sample.path", Box::new(|f| f.sample.path = "other/".into())),
        ("asks.quota_gib", Box::new(|f| f.asks.quota_gib = 16)),
        ("asks.datasets", Box::new(|f| f.asks.datasets = 50_000)),
        (
            "created and expires",
            Box::new(|f| {
                f.created_unix += 1;
                f.expires_unix += 1;
            }),
        ),
    ];
    for (name, change) in changes {
        let mut t = a.clone();
        change(&mut t.fields);
        assert_eq!(t.fields.check_form(), Ok(()), "{name}");
        assert_eq!(
            t.verify(&NET, T0 + 10, &doc, &[], &[]),
            Err(Refusal::Signature),
            "{name}"
        );
        // The same change in the file's text.
        let again = Application::from_json(t.to_json().as_bytes()).expect("parse");
        assert_ne!(again.recovered(), Some(again.fields.signer), "{name}");
    }
    // A tampered signature, and a signature by another key.
    let mut t = a.clone();
    t.signature[5] ^= 1;
    assert_eq!(t.verify(&NET, T0, &doc, &[], &[]), Err(Refusal::Signature));
    assert_eq!(
        Application::sign(a.fields.clone(), &KEY_B).map(|_| ()),
        Err(Refusal::Signature)
    );
    // A version or an expiry of the applicant's choice never signs.
    let mut f = a.fields.clone();
    f.expires_unix += 86_400;
    assert!(matches!(
        Application::sign(f, &KEY_A),
        Err(Refusal::Time(_))
    ));
    let mut f = a.fields.clone();
    f.version = 2;
    assert!(matches!(
        Application::sign(f, &KEY_A),
        Err(Refusal::Form(_))
    ));
}

#[test]
fn another_book_or_chain_or_an_expired_one_is_refused() {
    let (a, doc, _) = first();
    let v = |net: &Network, now| a.verify(net, now, &doc, &[], &[]);
    let chain = Network {
        chain_id: 8_453,
        ..NET
    };
    assert_eq!(v(&chain, T0), Err(Refusal::Network("chain id")));
    let book = Network {
        book: [0xB1; 20],
        ..NET
    };
    assert_eq!(v(&book, T0), Err(Refusal::Network("book")));
    // Six days, to the second.
    assert_eq!(v(&NET, T0 + LIFETIME_SECS), Ok(Standing::First));
    assert_eq!(
        v(&NET, T0 + LIFETIME_SECS + 1),
        Err(Refusal::Expired(T0 + LIFETIME_SECS))
    );
    assert_eq!(
        v(&NET, T0 + LIFETIME_SECS + 1).expect_err("refused").code(),
        "expired"
    );
    // Made in the future beyond the skew.
    assert!(matches!(v(&NET, T0 - SKEW_SECS - 1), Err(Refusal::Time(_))));
    assert_eq!(v(&NET, T0 - SKEW_SECS), Ok(Standing::First));
}

#[test]
fn is_never_a_valid_book_report() {
    let (a, _, b) = first();
    let report = SignedReport::try_from_slice(&b.report).expect("report");
    // Another domain, so another signed text and another digest.
    let book_text = report.payload.message();
    let text = a.fields.message();
    assert!(text.starts_with("onyx-publisher-application-v1:0x"));
    assert!(book_text.starts_with("onyx-signed-source-v1:0x"));
    assert_ne!(a.fields.digest(), report.payload.digest());
    // The application's signature over a book report's text recovers
    // someone else, so `postReport` would not find the key.
    assert_ne!(
        eip191_recover(book_text.as_bytes(), &a.signature),
        Some(a.fields.signer)
    );
    // And the report's signature is no application signature.
    let mut swapped = a.clone();
    swapped.signature = report.signature;
    assert_ne!(swapped.recovered(), Some(a.fields.signer));
    // A later request has a third domain.
    assert_ne!(APPLICATION_DOMAIN, REQUEST_DOMAIN);
}

#[test]
fn unknown_fields_and_contact_text_are_refused() {
    let (a, _, _) = first();
    let good: Value = serde_json::from_str(&a.to_json()).expect("json");
    let parse = |v: &Value| Application::from_json(v.to_string().as_bytes());
    assert!(parse(&good).is_ok());
    for (place, field) in [
        ("", "contact"),
        ("", "name"),
        ("", "wallet"),
        ("sample", "url"),
        ("asks", "note"),
    ] {
        let mut v = good.clone();
        let o = if place.is_empty() {
            &mut v
        } else {
            &mut v[place]
        };
        o[field] = json!("x");
        assert!(
            matches!(parse(&v), Err(Refusal::Form(m)) if m.contains("unknown field")),
            "{place} {field}"
        );
    }
    let mut v = good.clone();
    v.as_object_mut().expect("object").remove("cosignature");
    assert!(matches!(parse(&v), Err(Refusal::Form(m)) if m.contains("lacks")));

    // E-mail-shaped text and mailto links (THREATS SUB-4, PUBON-13). The
    // shape is put together here, so no address stands in this file.
    let shaped = format!("Water levels; write to gauges{}example.org for more.", '@');
    for data in [
        shaped.as_str(),
        "Water levels, see mailto:gauges",
        "",
        "   ",
        "two\nlines",
        &"a".repeat(MAX_DATA_BYTES + 1),
    ] {
        let mut v = good.clone();
        v["data"] = json!(data);
        assert!(matches!(parse(&v), Err(Refusal::Form(_))), "{data:?}");
    }
    let mut v = good.clone();
    v["data"] = json!("a".repeat(MAX_DATA_BYTES));
    assert!(parse(&v).is_ok());

    // The sample is a path under the origin, never another host.
    for path in [
        "https://other.example.org/s/",
        "//other.example.org/",
        "../x/",
        "sample",
        "/sample/",
        ".well-known/",
        "a//b/",
        "S/",
    ] {
        let mut v = good.clone();
        v["sample"]["path"] = json!(path);
        assert!(matches!(parse(&v), Err(Refusal::Form(_))), "{path}");
    }
    // The size cap, a wrong type, uppercase hex.
    let mut big = a.to_json();
    big.push_str(&" ".repeat(MAX_APPLICATION_BYTES));
    assert!(
        matches!(Application::from_json(big.as_bytes()), Err(Refusal::Form(m)) if m.contains("over"))
    );
    let mut v = good.clone();
    v["chain_id"] = json!("84532");
    assert!(parse(&v).is_err());
    let mut v = good.clone();
    v["signer"] = json!(hex0x(&a.fields.signer).to_uppercase().replace("0X", "0x"));
    assert!(parse(&v).is_err());
    // An id or origin that breaks its rule never parses.
    for (field, value) in [
        ("publisher", "onyx-official.data"),
        ("publisher", "river-official.data"),
        ("origin", "someone.github.io/x"),
        ("origin", "https://data.example.org"),
    ] {
        let mut v = good.clone();
        v[field] = json!(value);
        assert!(parse(&v).is_err(), "{value}");
    }
}

#[test]
fn the_keys_document_must_name_the_id_and_the_key() {
    let (a, doc, _) = first();
    let v = |d: &[u8]| a.verify(&NET, T0, d, &[], &[]);
    assert_eq!(v(&doc), Ok(Standing::First));
    // Other bytes (even a space) no longer hash to the signed Keccak.
    let mut spaced = doc.clone();
    spaced.push(b' ');
    assert!(matches!(v(&spaced), Err(Refusal::KeysDocument(_))));
    // A document that hashes right but names another id, or lacks the key.
    for d in [
        keys_doc("lake-levels.open", &[&KEY_A]),
        keys_doc(ID, &[&KEY_B]),
        b"{}".to_vec(),
    ] {
        let b = sample(T0 - 60, &KEY_A);
        let a = Application::sign(fields_for(&b, &KEY_A, &d, T0), &KEY_A).expect("sign");
        let r = a.verify(&NET, T0, &d, &[], &[]);
        assert!(matches!(r, Err(Refusal::KeysDocument(_))), "{r:?}");
    }
}

#[test]
fn the_book_decides_what_is_free() {
    let (a, doc, _) = first();
    let other = |id: &str, origin: &str, key: &[u8; 32]| RegisteredKey {
        publisher: id.into(),
        origin: origin.into(),
        signer: addr(key),
        life: KeyLife::Active,
    };
    let v = |r: &[RegisteredKey]| a.verify(&NET, T0, &doc, r, &[]);
    // Unrelated publishers do not matter, also two paths under one host.
    assert_eq!(
        v(&[other("lake-levels.open", "data.example.org/lakes", &KEY_B)]),
        Ok(Standing::First)
    );
    // A look-alike id, the same id on another origin, a taken origin, an
    // overlapping origin, a key the book already holds.
    let look = v(&[other("river_levels-open", "other.example.org", &KEY_B)]);
    assert!(
        matches!(look, Err(Refusal::Id(IdError::LookAlike(_)))),
        "{look:?}"
    );
    assert_eq!(look.expect_err("refused").code(), "name-taken");
    assert!(matches!(
        v(&[other(ID, "other.example.org", &KEY_B)]),
        Err(Refusal::Id(IdError::LookAlike(_)))
    ));
    assert!(matches!(
        v(&[other("lake-levels.open", ORIGIN, &KEY_B)]),
        Err(Refusal::Taken(_))
    ));
    for origin in ["data.example.org", "data.example.org/river-levels/v2"] {
        let r = v(&[other("lake-levels.open", origin, &KEY_B)]);
        assert!(
            matches!(r, Err(Refusal::Origin(OriginError::Overlap(_)))),
            "{r:?}"
        );
        assert_eq!(r.expect_err("refused").code(), "origin-overlap");
    }
    assert!(matches!(
        v(&[other("lake-levels.open", "other.example.org", &KEY_A)]),
        Err(Refusal::Taken(_))
    ));
    // The list a reviewer hands in.
    let list = json!([{"publisher": "lake-levels.open", "origin": "data.example.org",
        "signer": hex0x(&addr(&KEY_B)), "state": "active"}]);
    let parsed = RegisteredKey::list_from_json(list.to_string().as_bytes()).expect("list");
    assert!(matches!(v(&parsed), Err(Refusal::Origin(_))));
    assert!(RegisteredKey::list_from_json(b"[{\"publisher\": \"a.b\"}]").is_err());
}

/// A second-key application by key B for the id key A registered.
fn second(cosign: Option<&[u8; 32]>, doc_keys: &[&[u8; 32]]) -> (Application, Vec<u8>, Built) {
    // The sample is the id's first dataset, signed long ago by key A.
    let b = sample(T0 - 30 * 86_400, &KEY_A);
    let doc = keys_doc(ID, doc_keys);
    let mut a = Application::sign(fields_for(&b, &KEY_B, &doc, T0), &KEY_B).expect("sign");
    if let Some(k) = cosign {
        a.cosign(k).expect("cosign");
    }
    (a, doc, b)
}

#[test]
fn a_second_key_needs_a_cosignature_of_an_active_key() {
    let book = [reg(&KEY_A, KeyLife::Active)];
    // The origin alone gives nothing (ADR-0066 fact 2, review H1): whoever
    // took the origin can write the keys document, and is still refused.
    let (a, doc, _) = second(None, &[&KEY_A, &KEY_B]);
    let r = a.verify(&NET, T0, &doc, &book, &[]);
    assert!(matches!(r, Err(Refusal::Cosignature(_))), "{r:?}");
    assert_eq!(r.expect_err("refused").code(), "no-cosignature");
    // Cosigned by itself, or by a key that is not the id's.
    for k in [&KEY_B, &KEY_C] {
        let (a, doc, _) = second(Some(k), &[&KEY_A, &KEY_B]);
        let r = a.verify(&NET, T0, &doc, &book, &[]);
        assert!(matches!(r, Err(Refusal::Cosignature(_))), "{r:?}");
    }
    // A cosignature over another application does not carry over.
    let (mut a, doc, _) = second(None, &[&KEY_A, &KEY_B]);
    let (other, _, _) = first();
    a.cosignature = Some(eip191_sign_with(&KEY_A, other.fields.message().as_bytes()).expect("sig"));
    assert!(matches!(
        a.verify(&NET, T0, &doc, &book, &[]),
        Err(Refusal::Cosignature(_))
    ));
    // The keys document must list both keys: the stolen-key case, where the
    // thief can cosign but cannot write at the origin (THREATS PUBON-7).
    let (a, doc, _) = second(Some(&KEY_A), &[&KEY_B]);
    assert!(matches!(
        a.verify(&NET, T0, &doc, &book, &[]),
        Err(Refusal::Cosignature(_))
    ));

    // With the cosignature of the active key and both keys listed it passes,
    // and the old sample (signed by the first key) is taken as it is.
    let (a, doc, b) = second(Some(&KEY_A), &[&KEY_A, &KEY_B]);
    let standing = a.verify(&NET, T0, &doc, &book, &[]).expect("second key");
    assert_eq!(
        standing,
        Standing::SecondKey {
            cosigner: addr(&KEY_A)
        }
    );
    assert!(a
        .check_sample(&standing, &b.manifest_bytes, &b.report, &b.shard)
        .is_ok());
    let back = Application::from_json(a.to_json().as_bytes()).expect("parse");
    assert_eq!(back.cosigner(), Some(addr(&KEY_A)));
    // A cosignature for an id nobody registered is refused, not ignored.
    assert!(a.verify(&NET, T0, &doc, &[], &[]).is_err());
    let (lone, lone_doc, _) = second(Some(&KEY_A), &[&KEY_B]);
    assert!(matches!(
        lone.verify(&NET, T0, &lone_doc, &[], &[]),
        Err(Refusal::Cosignature(_))
    ));
}

#[test]
fn a_cosignature_of_a_retired_compromised_or_suspended_key_is_refused() {
    let (a, doc, _) = second(Some(&KEY_A), &[&KEY_A, &KEY_B]);
    for life in [KeyLife::Retired, KeyLife::Compromised, KeyLife::Suspended] {
        let r = a.verify(&NET, T0, &doc, &[reg(&KEY_A, life)], &[]);
        assert!(matches!(r, Err(Refusal::Cosignature(_))), "{life:?} {r:?}");
        // Another key of the id being active does not help: the cosigner counts.
        let r = a.verify(
            &NET,
            T0,
            &doc,
            &[reg(&KEY_A, life), reg(&KEY_C, KeyLife::Active)],
            &[],
        );
        assert!(matches!(r, Err(Refusal::Cosignature(_))), "{life:?} {r:?}");
    }
    assert!(a
        .verify(&NET, T0, &doc, &[reg(&KEY_A, KeyLife::Active)], &[])
        .is_ok());
}

#[test]
fn a_refused_signer_is_blocked_until_its_report_is_stale() {
    // Refused: the record keeps the signer and the time its sample's report
    // can no longer be relayed (THREATS PUBON-16, review H2).
    let (refused, _, b) = first();
    let until = blocks_signer_until(b.manifest.header.created_unix);
    assert_eq!(until, T0 - 60 + 7 * 86_400 + 300);
    let record = Record::from_json(&json!({"state": "refused", "reason": "rights-unclear",
        "signer": hex0x(&refused.fields.signer), "blocks_signer_until": until}))
    .expect("record")
    .expect("blocks");
    let records = [record];

    // A re-apply with the same key and a new sample, one day later.
    let again = |key: &[u8; 32], now: u64| {
        let b = sample(now - 60, key);
        let doc = keys_doc(ID, &[key]);
        let a = Application::sign(fields_for(&b, key, &doc, now), key).expect("sign");
        a.verify(&NET, now, &doc, &[], &records)
    };
    let r = again(&KEY_A, T0 + 86_400);
    assert_eq!(r, Err(Refusal::Blocked(until)));
    assert_eq!(r.expect_err("refused").code(), "signer-blocked");
    assert_eq!(again(&KEY_A, until - 1), Err(Refusal::Blocked(until)));
    // A new key is never blocked; the old key is free once the report is stale.
    assert_eq!(again(&KEY_B, T0 + 86_400), Ok(Standing::First));
    assert_eq!(again(&KEY_A, until), Ok(Standing::First));

    // Other states block nothing; a refusal without its fields is an error.
    assert_eq!(Record::from_json(&json!({"state": "approved"})), Ok(None));
    assert!(Record::from_json(&json!({"state": "refused"})).is_err());
    assert!(Record::from_json(&json!({})).is_err());
}

#[test]
fn an_expired_application_is_made_again_with_the_same_sample() {
    let (old, _, b) = first();
    let records = [Record::from_json(&json!({"state": "expired",
        "signer": hex0x(&old.fields.signer),
        "sample_manifest_root": hex0x(&old.fields.sample.manifest_root.0)}))
    .expect("record")
    .expect("holds")];
    let now = T0 + 8 * 86_400;
    let doc = keys_doc(ID, &[&KEY_A]);
    // The same folder: taken although the sample is older than an hour.
    let a = Application::sign(fields_for(&b, &KEY_A, &doc, now), &KEY_A).expect("sign");
    let standing = a.verify(&NET, now, &doc, &[], &records).expect("re-apply");
    assert_eq!(standing, Standing::Reapply);
    assert!(a
        .check_sample(&standing, &b.manifest_bytes, &b.report, &b.shard)
        .is_ok());
    // Without the record the old sample is refused.
    assert!(matches!(
        a.check_sample(&Standing::First, &b.manifest_bytes, &b.report, &b.shard),
        Err(Refusal::Sample(_))
    ));
    // Rows built again have a new creation time and a new root: a second
    // version 1, which the book would suspend the key for (review M1).
    let rebuilt = sample(now - 60, &KEY_A);
    let a = Application::sign(fields_for(&rebuilt, &KEY_A, &doc, now), &KEY_A).expect("sign");
    assert!(matches!(
        a.verify(&NET, now, &doc, &[], &records),
        Err(Refusal::Sample(_))
    ));
}

#[test]
fn the_sample_is_checked_like_any_dataset() {
    let (a, _, b) = first();
    let check = |a: &Application, b: &Built| {
        a.check_sample(&Standing::First, &b.manifest_bytes, &b.report, &b.shard)
    };
    assert!(check(&a, &b).is_ok());
    let doc = keys_doc(ID, &[&KEY_A]);
    let apply =
        |b: &Built| Application::sign(fields_for(b, &KEY_A, &doc, T0), &KEY_A).expect("sign");

    // A changed row.
    let mut t = sample(T0 - 60, &KEY_A);
    let n = t.shard.len() - 1;
    t.shard[n] ^= 1;
    assert!(matches!(check(&a, &t), Err(Refusal::Sample(_))));
    // Another manifest than the application names.
    let other = sample(T0 - 61, &KEY_A);
    assert!(matches!(check(&a, &other), Err(Refusal::Sample(m)) if m.contains("manifest_root")));
    // A report by another key.
    let foreign = sample(T0 - 60, &KEY_B);
    let r = check(&a, &foreign);
    assert!(
        matches!(&r, Err(Refusal::Sample(m)) if m.contains("another key")),
        "{r:?}"
    );
    // Two shards.
    let two = sample_with(ID, T0 - 60, 4, 2, &KEY_A);
    let r = check(&apply(&two), &two);
    assert!(
        matches!(&r, Err(Refusal::Sample(m)) if m.contains("one shard")),
        "{r:?}"
    );
    // Rights that are not Allowed never make a manifest (`Manifest::build`
    // and `validate` refuse them), so the sample check inherits that.
    // Built more than an hour before, or after, the application.
    for created in [T0 - SAMPLE_MAX_AGE_SECS - 1, T0 + 1] {
        let late = sample(created, &KEY_A);
        let r = check(&apply(&late), &late);
        assert!(
            matches!(&r, Err(Refusal::Sample(m)) if m.contains("hour")),
            "{r:?}"
        );
    }
    let edge = sample(T0 - SAMPLE_MAX_AGE_SECS, &KEY_A);
    assert!(check(&apply(&edge), &edge).is_ok());
    // Oversized pieces are refused before they are read.
    let big = vec![0u8; MAX_SAMPLE_BYTES as usize + 1];
    assert!(matches!(
        a.check_sample(&Standing::First, &b.manifest_bytes, &b.report, &big),
        Err(Refusal::Sample(m)) if m.contains("8 MiB")
    ));
    assert!(a
        .check_sample(&Standing::First, &big, &b.report, &b.shard)
        .is_err());
    assert!(a
        .check_sample(&Standing::First, &b.manifest_bytes, &big, &b.shard)
        .is_err());
}

fn request_seq(ask: Ask, key: &[u8; 32], sequence: u64) -> Request {
    Request {
        chain_id: NET.chain_id,
        book: NET.book,
        publisher: ID.into(),
        origin: ORIGIN.into(),
        signer: addr(key),
        ask,
        sequence,
        created_unix: T0,
        expires_unix: T0 + LIFETIME_SECS,
        signature: [0; 65],
    }
    .signed(key)
    .expect("sign")
}

fn request(ask: Ask, key: &[u8; 32]) -> Request {
    request_seq(ask, key, T0)
}

fn with_app(application: Option<&[u8]>, last_sequence: Option<u64>) -> RequestContext<'_> {
    RequestContext {
        keys_document: &[],
        registered: &[],
        application,
        last_sequence,
    }
}

fn ctx<'a>(doc: &'a [u8], book: &'a [RegisteredKey]) -> RequestContext<'a> {
    RequestContext {
        keys_document: doc,
        registered: book,
        application: None,
        last_sequence: None,
    }
}

#[test]
fn key_stolen_needs_the_key_and_the_changed_keys_document() {
    let book = [reg(&KEY_A, KeyLife::Active), reg(&KEY_B, KeyLife::Active)];
    let stolen = request(
        Ask::KeyStolen {
            since_unix: T0 - 3_600,
        },
        &KEY_A,
    );
    let without = keys_doc(ID, &[&KEY_B]);
    let with = keys_doc(ID, &[&KEY_A, &KEY_B]);
    // Both proofs: the stolen key signs, and the origin no longer lists it.
    assert_eq!(stolen.verify(&NET, T0, &ctx(&without, &book)), Ok(()));
    // The key alone (a thief, or anyone who says "stolen"): the document
    // still lists it.
    let r = stolen.verify(&NET, T0, &ctx(&with, &book));
    assert!(
        matches!(&r, Err(Refusal::Request(m)) if m.contains("still lists")),
        "{r:?}"
    );
    // The origin alone (whoever took it): signed by a key that is not the id's.
    let taker = request(
        Ask::KeyStolen {
            since_unix: T0 - 3_600,
        },
        &KEY_C,
    );
    assert!(matches!(
        taker.verify(&NET, T0, &ctx(&without, &book)),
        Err(Refusal::Request(_))
    ));
    // Another active key cannot report a theft of key A in A's name: the
    // request is about its own signer.
    let b = request(
        Ask::KeyStolen {
            since_unix: T0 - 3_600,
        },
        &KEY_B,
    );
    assert!(matches!(
        b.verify(&NET, T0, &ctx(&without, &book)),
        Err(Refusal::Request(_))
    ));
    // A changed field, another network, an expired request, a future theft.
    let mut t = stolen.clone();
    t.ask = Ask::KeyStolen { since_unix: 1 };
    assert_eq!(
        t.verify(&NET, T0, &ctx(&without, &book)),
        Err(Refusal::Signature)
    );
    let mut t = stolen.clone();
    t.sequence += 1;
    assert_eq!(
        t.verify(&NET, T0, &ctx(&without, &book)),
        Err(Refusal::Signature)
    );
    let chain = Network { chain_id: 1, ..NET };
    assert_eq!(
        stolen.verify(&chain, T0, &ctx(&without, &book)),
        Err(Refusal::Network("chain id"))
    );
    assert!(matches!(
        stolen.verify(&NET, T0 + LIFETIME_SECS + 1, &ctx(&without, &book)),
        Err(Refusal::Expired(_))
    ));
    let future = request(
        Ask::KeyStolen {
            since_unix: T0 + 10,
        },
        &KEY_A,
    );
    assert!(future.verify(&NET, T0, &ctx(&without, &book)).is_err());
    // A document of another id is no proof.
    let other = keys_doc("lake-levels.open", &[&KEY_B]);
    assert!(stolen.verify(&NET, T0, &ctx(&other, &book)).is_err());

    // The file round-trips, refuses unknown fields, and is no application.
    let text = stolen.to_json();
    assert_eq!(Request::from_json(text.as_bytes()), Ok(stolen.clone()));
    let mut v: Value = serde_json::from_str(&text).expect("json");
    v["contact"] = json!("x");
    assert!(Request::from_json(v.to_string().as_bytes()).is_err());
    assert!(Application::from_json(text.as_bytes()).is_err());
    assert!(stolen.message().starts_with("onyx-publisher-request-v1:0x"));
}

#[test]
fn later_requests_are_signed_by_a_key_that_may_ask() {
    let book = [reg(&KEY_A, KeyLife::Active), reg(&KEY_B, KeyLife::Retired)];
    let doc = keys_doc(ID, &[&KEY_A]);
    let ok = |r: &Request| r.verify(&NET, T0, &ctx(&doc, &book));
    let raise = Ask::Raise(Asks {
        quota_gib: 4,
        datasets: 64,
    });
    assert_eq!(ok(&request(raise, &KEY_A)), Ok(()));
    assert!(ok(&request(raise, &KEY_B)).is_err());
    assert!(ok(&request(raise, &KEY_C)).is_err());
    let zero = Ask::Raise(Asks {
        quota_gib: 0,
        datasets: 1,
    });
    assert!(ok(&request(zero, &KEY_A)).is_err());
    // A key retires itself, or an active key retires another key of the id.
    let retire = |key: &[u8; 32]| Ask::RetireKey {
        key: addr(key),
        at_unix: T0,
    };
    assert_eq!(ok(&request(retire(&KEY_A), &KEY_A)), Ok(()));
    assert_eq!(ok(&request(retire(&KEY_B), &KEY_A)), Ok(()));
    assert!(ok(&request(retire(&KEY_A), &KEY_B)).is_err());
    assert!(ok(&request(retire(&KEY_C), &KEY_A)).is_err());
    let (app, _, _) = first();
    let withdraw = Ask::Withdraw {
        application: keccak256(app.to_json().as_bytes()),
    };
    for ask in [raise, retire(&KEY_B), withdraw] {
        let r = request(ask, &KEY_A);
        assert_eq!(Request::from_json(r.to_json().as_bytes()), Ok(r));
    }
}

// ---------------------------------------------------------------- review APPLY-R1

/// APPLY-R1 H2: the key alone must not withdraw what it signed. A retirement
/// from an earlier time needs the second proof a theft needs; a time after
/// the request lapses is refused.
#[test]
fn retire_key_from_an_earlier_time_needs_the_second_proof() {
    let book = [reg(&KEY_A, KeyLife::Active), reg(&KEY_B, KeyLife::Active)];
    // The owner's document still lists both keys: no second proof.
    let listed = keys_doc(ID, &[&KEY_A, &KEY_B]);
    let retire = |key: &[u8; 32], at_unix: u64| {
        request(
            Ask::RetireKey {
                key: addr(key),
                at_unix,
            },
            &KEY_A,
        )
    };
    let early = "an earlier retirement time";
    for (key, at) in [
        (&KEY_A, 0),
        (&KEY_B, 0),
        (&KEY_A, T0 - RETIRE_WINDOW_SECS - 1),
        (&KEY_A, T0 - 30 * 86_400),
    ] {
        let r = retire(key, at).verify(&NET, T0, &ctx(&listed, &book));
        assert!(
            matches!(&r, Err(Refusal::Request(m)) if m.contains(early)),
            "{at} {r:?}"
        );
    }
    // Far in the future: the book would revert, and a request must not
    // outlive itself.
    for at in [u64::MAX, T0 + LIFETIME_SECS + 1] {
        let r = retire(&KEY_A, at).verify(&NET, T0, &ctx(&listed, &book));
        assert!(
            matches!(&r, Err(Refusal::Request(m)) if m.contains("after the request lapses")),
            "{at} {r:?}"
        );
    }
    // From about now on, the key alone is enough: nothing signed before is
    // withdrawn.
    for at in [T0 - RETIRE_WINDOW_SECS, T0, T0 + 86_400, T0 + LIFETIME_SECS] {
        assert_eq!(
            retire(&KEY_A, at).verify(&NET, T0, &ctx(&listed, &book)),
            Ok(()),
            "{at}"
        );
    }
    // With the second proof (the key is out of the keys document at the
    // origin) an earlier time passes, for that key only.
    let gone = keys_doc(ID, &[&KEY_B]);
    assert_eq!(
        retire(&KEY_A, 0).verify(&NET, T0, &ctx(&gone, &book)),
        Ok(())
    );
    assert!(retire(&KEY_B, 0)
        .verify(&NET, T0, &ctx(&gone, &book))
        .is_err());
    // A document of another id is no proof.
    let other = keys_doc("lake-levels.open", &[&KEY_B]);
    assert!(retire(&KEY_A, 0)
        .verify(&NET, T0, &ctx(&other, &book))
        .is_err());
}

/// APPLY-R1 M1: a retired key is out of the keys document already, so its
/// "second proof" proves nothing: it cannot be reported stolen.
#[test]
fn a_retired_key_cannot_be_reported_stolen() {
    let doc = keys_doc(ID, &[&KEY_B]);
    for since_unix in [0, T0 - 60] {
        let r = request(Ask::KeyStolen { since_unix }, &KEY_A);
        for life in [KeyLife::Retired, KeyLife::Compromised] {
            let book = [reg(&KEY_A, life), reg(&KEY_B, KeyLife::Active)];
            let v = r.verify(&NET, T0, &ctx(&doc, &book));
            assert!(
                matches!(&v, Err(Refusal::Request(m)) if m.contains("retired or marked compromised")),
                "{life:?} {v:?}"
            );
        }
        // An active or a suspended key can.
        for life in [KeyLife::Active, KeyLife::Suspended] {
            let book = [reg(&KEY_A, life), reg(&KEY_B, KeyLife::Active)];
            assert_eq!(r.verify(&NET, T0, &ctx(&doc, &book)), Ok(()), "{life:?}");
        }
    }
}

/// APPLY-R1 M2: a withdraw names one application and only that
/// application's key may send it; a request is taken once; the id and the
/// origin of a request are never free text.
#[test]
fn a_withdraw_names_its_application_and_a_request_is_taken_once() {
    let (app, _, _) = first();
    let bytes = app.to_json().into_bytes();
    let withdraw = Ask::Withdraw {
        application: keccak256(&bytes),
    };
    // Bound to nothing, it is refused: for any key and without the file.
    let r = request(withdraw, &KEY_A);
    assert!(matches!(
        r.verify(&NET, T0, &with_app(None, None)),
        Err(Refusal::Request(_))
    ));
    assert_eq!(r.verify(&NET, T0, &with_app(Some(&bytes), None)), Ok(()));
    // Another key cannot withdraw it; another application is not this one.
    let c = request(withdraw, &KEY_C);
    let v = c.verify(&NET, T0, &with_app(Some(&bytes), None));
    assert!(
        matches!(&v, Err(Refusal::Request(m)) if m.contains("own key")),
        "{v:?}"
    );
    let b = sample(T0 - 90, &KEY_A);
    let doc = keys_doc(ID, &[&KEY_A]);
    let other = Application::sign(fields_for(&b, &KEY_A, &doc, T0), &KEY_A).expect("sign");
    let v = r.verify(&NET, T0, &with_app(Some(other.to_json().as_bytes()), None));
    assert!(
        matches!(&v, Err(Refusal::Request(m)) if m.contains("not the one it names")),
        "{v:?}"
    );
    // The same bytes are not taken a second time, on day 5 or any day.
    let later = T0 + 5 * 86_400;
    assert_eq!(
        r.verify(&NET, later, &with_app(Some(&bytes), Some(r.sequence - 1))),
        Ok(())
    );
    let v = r.verify(&NET, later, &with_app(Some(&bytes), Some(r.sequence)));
    assert!(
        matches!(&v, Err(Refusal::Request(m)) if m.contains("taken once")),
        "{v:?}"
    );
    let book = [reg(&KEY_A, KeyLife::Active)];
    let raise = request_seq(
        Ask::Raise(Asks {
            quota_gib: 1024,
            datasets: 50_000,
        }),
        &KEY_A,
        7,
    );
    let seen = |last_sequence| RequestContext {
        keys_document: &[],
        registered: &book,
        application: None,
        last_sequence,
    };
    assert_eq!(raise.verify(&NET, later, &seen(Some(6))), Ok(()));
    assert!(raise.verify(&NET, later, &seen(Some(7))).is_err());
    assert!(raise.verify(&NET, later, &seen(Some(9))).is_err());

    // The publisher and the origin are in their grammar: signing refuses
    // free text, and so does reading and checking a file that carries it.
    for (publisher, origin) in [
        ("Call Me, any text at all", ORIGIN),
        (ID, "not an origin"),
        (ID, "https://data.example.org"),
    ] {
        let mut t = r.clone();
        t.publisher = publisher.into();
        t.origin = origin.into();
        assert!(t.clone().signed(&KEY_A).is_err(), "{publisher} {origin}");
        assert!(Request::from_json(t.to_json().as_bytes()).is_err());
        assert!(t.verify(&NET, T0, &with_app(Some(&bytes), None)).is_err());
    }
}

/// APPLY-R1 M4: a registered origin that the list spells another way (a
/// trailing slash, a scheme, upper case, too many parts) is an error, not a
/// free origin.
#[test]
fn a_registered_origin_spelled_otherwise_is_an_error() {
    let (a, doc, _) = first();
    for spelled in [
        "data.example.org/river-levels/",
        "https://data.example.org/river-levels",
        "Data.example.org/river-levels",
        "data.example.org/river-levels/v1/",
    ] {
        let book = [RegisteredKey {
            publisher: "lake-levels.open".into(),
            origin: spelled.into(),
            signer: addr(&KEY_B),
            life: KeyLife::Active,
        }];
        let r = a.verify(&NET, T0, &doc, &book, &[]);
        assert!(matches!(r, Err(Refusal::Book(_))), "{spelled} {r:?}");
        assert_eq!(r.expect_err("refused").code(), "bad-registered-list");
        // The file a reviewer hands in is refused when it is read.
        let list = json!([{"publisher": "lake-levels.open", "origin": spelled,
            "signer": hex0x(&addr(&KEY_B)), "state": "active"}]);
        assert!(
            matches!(
                RegisteredKey::list_from_json(list.to_string().as_bytes()),
                Err(Refusal::Book(_))
            ),
            "{spelled}"
        );
        // A request is checked against the same list.
        let q = request(
            Ask::Raise(Asks {
                quota_gib: 2,
                datasets: 16,
            }),
            &KEY_A,
        );
        assert!(matches!(
            q.verify(&NET, T0, &ctx(&doc, &book)),
            Err(Refusal::Book(_))
        ));
    }
    let list = json!([{"publisher": "Lake Levels", "origin": "data.example.org",
        "signer": hex0x(&addr(&KEY_B)), "state": "active"}]);
    assert!(RegisteredKey::list_from_json(list.to_string().as_bytes()).is_err());
}

/// APPLY-R1 L3: the keys document's hash goes on chain, so a first one
/// lists exactly the applicant's key: no foreign key, no key twice.
#[test]
fn a_first_keys_document_lists_only_the_signer() {
    let b = sample(T0 - 60, &KEY_A);
    let book = [RegisteredKey {
        publisher: "lake-levels.open".into(),
        origin: "other.example.org".into(),
        signer: addr(&KEY_B),
        life: KeyLife::Active,
    }];
    for keys in [
        vec![&KEY_A, &KEY_B, &KEY_C],
        vec![&KEY_A, &KEY_C],
        vec![&KEY_A, &KEY_A],
    ] {
        let doc = keys_doc(ID, &keys);
        let a = Application::sign(fields_for(&b, &KEY_A, &doc, T0), &KEY_A).expect("sign");
        for book in [&book[..], &[]] {
            let r = a.verify(&NET, T0, &doc, book, &[]);
            assert!(
                matches!(&r, Err(Refusal::KeysDocument(m)) if m.contains("exactly")),
                "{r:?}"
            );
        }
    }
    // A further key's document lists the id's registered keys and the new
    // one, and nothing else.
    let (a, doc, _) = second(Some(&KEY_A), &[&KEY_A, &KEY_B, &KEY_C]);
    let r = a.verify(&NET, T0, &doc, &[reg(&KEY_A, KeyLife::Active)], &[]);
    assert!(matches!(r, Err(Refusal::KeysDocument(_))), "{r:?}");
    let both = [reg(&KEY_A, KeyLife::Active), reg(&KEY_C, KeyLife::Retired)];
    assert!(a.verify(&NET, T0, &doc, &both, &[]).is_ok());
}

/// APPLY-R1 L3: a field that stands twice is refused, in every file the
/// tools read: a person would read the first value, the tool the last.
#[test]
fn a_duplicated_json_field_is_refused() {
    let (a, _, _) = first();
    let text = a.to_json().replacen(
        "{\"version\": 1,",
        "{\"publisher\": \"national-weather.data\", \"version\": 1,",
        1,
    );
    let r = Application::from_json(text.as_bytes());
    assert!(
        matches!(&r, Err(Refusal::Form(m)) if m.contains("stands twice")),
        "{r:?}"
    );
    // Nested, and in a request, a keys document and the registered list.
    let nested = a
        .to_json()
        .replacen("\"asks\": {", "\"asks\": {\"quota_gib\": 900, ", 1);
    assert!(Application::from_json(nested.as_bytes()).is_err());
    let q = request(
        Ask::Raise(Asks {
            quota_gib: 2,
            datasets: 16,
        }),
        &KEY_A,
    );
    let twice = q
        .to_json()
        .replacen("\"sequence\":", "\"sequence\": 1, \"sequence\":", 1);
    assert!(Request::from_json(twice.as_bytes()).is_err());
    let doc = format!(
        "{{\"source\": \"lake-levels.open\", \"version\": 1, \"source\": \"{ID}\", \"keys\": []}}"
    );
    assert!(KeysDocument::parse(doc.as_bytes()).is_err());
    let list = "[{\"publisher\": \"a.b\", \"publisher\": \"c.d\", \"origin\": \"example.org\", \"signer\": \"0x0000000000000000000000000000000000000001\", \"state\": \"active\"}]";
    assert!(RegisteredKey::list_from_json(list.as_bytes()).is_err());
    assert_eq!(
        strict_json(b"{\"a\": 1, \"b\": [1, {\"c\": null}]}").map(|v| v["a"].clone()),
        Ok(json!(1))
    );
}
