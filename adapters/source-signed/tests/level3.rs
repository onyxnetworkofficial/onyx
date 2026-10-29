//! Level 3 end to end on the synthetic fixtures: the source's keys document is
//! reviewed into a registered key, its signed report becomes a claim through
//! the one intake, and tampering, replay and an equivocation are refused.

use onyx_coordinator::signed::{SignedError, SignedSources};
use onyx_core::signed::REPORT_EVIDENCE;
use onyx_core::{
    keccak256, Claim, DataRights, EntityKind, EvidenceRef, MetricId, MetricInfo, ProofLevel,
    Redistribution, SignedPayload, SignedReport, Snapshot, SourceId, SourceRef, StaticRegistry,
    Subject, Value, ValueKind, VerificationMethod,
};
use onyx_labels::attest::eip191_digest;
use onyx_source_signed::{keys_from_document, report_from_json, report_json};
use secp256k1::{Message, Secp256k1, SecretKey};

const ORIGIN: &str = "data.example.org";
const FETCHED: u64 = 1_699_990_000;
const OBSERVED: u64 = 1_700_000_000;
const NOW: u64 = 1_700_000_060;

fn fixture(name: &str) -> String {
    format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// Test key: keccak256("onyx test signed source key 1"). Not a real key.
fn test_key() -> SecretKey {
    SecretKey::from_slice(&keccak256(b"onyx test signed source key 1").0).expect("key")
}

fn address(sk: &SecretKey) -> [u8; 20] {
    let pk = secp256k1::PublicKey::from_secret_key(&Secp256k1::new(), sk);
    let h = keccak256(&pk.serialize_uncompressed()[1..]);
    let mut a = [0u8; 20];
    a.copy_from_slice(&h.0[12..]);
    a
}

fn sign(sk: &SecretKey, p: SignedPayload) -> SignedReport {
    let digest = Message::from_digest(eip191_digest(p.message().as_bytes()).0);
    let (rid, bytes) = Secp256k1::new()
        .sign_ecdsa_recoverable(&digest, sk)
        .serialize_compact();
    let mut signature = [0u8; 65];
    signature[..64].copy_from_slice(&bytes);
    signature[64] = 27 + u8::try_from(rid.to_i32()).expect("recid");
    SignedReport {
        payload: p,
        signature,
    }
}

fn rights() -> DataRights {
    DataRights {
        license: "CC0-1.0".into(),
        redistribution: Redistribution::Allowed,
        source: SourceId("example.signed".into()),
        attribution: None,
        terms: String::new(),
    }
}

fn reg() -> StaticRegistry {
    let mut r = StaticRegistry::new();
    r.add_metric(
        "example.signed.reserve",
        MetricInfo {
            value_kind: ValueKind::UInt,
            needs_label_set: false,
        },
    )
    .and_then(|r| r.add_entity("entity:example", EntityKind::Exchange))
    .and_then(|r| r.add_source(rights()))
    .expect("registry");
    r
}

fn keys_doc() -> String {
    let doc = serde_json::json!({
        "version": 1,
        "source": "example.signed",
        "keys": [{
            "signer": format!("0x{}", hex::encode(address(&test_key()))),
            "scheme": "eip191",
        }],
    });
    serde_json::to_string_pretty(&doc).expect("json") + "\n"
}

fn example_report(key: onyx_core::Hash32) -> SignedReport {
    sign(
        &test_key(),
        SignedPayload {
            source: SourceId("example.signed".into()),
            key,
            metric: MetricId("example.signed.reserve".into()),
            subject: Subject::Entity("entity:example".into()),
            value: Value::UInt(12_345_678),
            observed_unix: OBSERVED,
            sequence: 1,
        },
    )
}

fn claim_for(r: &SignedReport) -> Claim {
    let h = r.hash();
    Claim {
        metric: r.payload.metric.clone(),
        subject: r.payload.subject.clone(),
        value: r.payload.value.clone(),
        source: SourceRef {
            source: r.payload.source.clone(),
            snapshot: Snapshot::Document {
                published_unix: r.payload.observed_unix,
                content_hash: h,
            },
        },
        method: VerificationMethod::SignedSource {
            key: r.payload.key,
            report: h,
        },
        label_set: None,
        rights: rights(),
        evidence: vec![EvidenceRef {
            kind: REPORT_EVIDENCE.into(),
            content_hash: h,
        }],
        confidence: None,
    }
}

/// The committed fixtures are exactly what the test key produces
/// (RFC 6979 signatures are deterministic). Bless with `ONYX_BLESS=1`.
#[test]
fn fixtures_are_reproducible() {
    let doc = keys_doc();
    let keys = keys_from_document(doc.as_bytes(), ORIGIN, FETCHED).expect("keys");
    let report = example_report(keys[0].id());
    let text = serde_json::to_string_pretty(&report_json(&report)).expect("json") + "\n";
    if std::env::var_os("ONYX_BLESS").is_some() {
        std::fs::write(fixture("onyx-source-keys.json"), &doc).expect("write keys");
        std::fs::write(fixture("example-report.json"), &text).expect("write report");
    }
    let on_disk = |n| std::fs::read_to_string(fixture(n)).expect("fixture missing; ONYX_BLESS=1");
    assert_eq!(on_disk("onyx-source-keys.json"), doc);
    assert_eq!(on_disk("example-report.json"), text);
}

#[test]
fn a_reviewed_key_and_its_signed_report_make_a_level_three_claim() {
    let reg = reg();
    let doc = std::fs::read(fixture("onyx-source-keys.json")).expect("keys doc");
    let keys = keys_from_document(&doc, ORIGIN, FETCHED).expect("keys");
    let (mut store, reviewer) = SignedSources::with_reviewer("test reviewer");
    let id = store
        .register_key(&reviewer, keys[0].clone(), &reg, NOW, "test")
        .expect("register");
    let report = report_from_json(&std::fs::read(fixture("example-report.json")).expect("report"))
        .expect("parse");
    assert_eq!(report.payload.key, id);
    let claim = claim_for(&report);
    assert_eq!(
        store
            .accept_claim(&claim, &report, &reg, NOW)
            .map(|i| i.level),
        Ok(ProofLevel::SourceProven)
    );
    assert_eq!(claim.method.proof_level(), ProofLevel::SourceProven);
    // Replaying the fixture is refused.
    assert_eq!(
        store
            .accept_claim(&claim, &report, &reg, NOW)
            .map(|i| i.level),
        Err(SignedError::Replay)
    );
}

#[test]
fn a_tampered_fixture_or_another_documents_key_is_refused() {
    let reg = reg();
    let doc = std::fs::read(fixture("onyx-source-keys.json")).expect("keys doc");
    let keys = keys_from_document(&doc, ORIGIN, FETCHED).expect("keys");
    let (mut store, reviewer) = SignedSources::with_reviewer("test reviewer");
    store
        .register_key(&reviewer, keys[0].clone(), &reg, NOW, "test")
        .expect("register");
    let mut report = report_from_json(&std::fs::read(fixture("example-report.json")).expect("r"))
        .expect("parse");
    report.payload.value = Value::UInt(99_999_999);
    // The claim matches the edited payload, but the signature does not.
    assert_eq!(
        store
            .accept_claim(&claim_for(&report), &report, &reg, NOW)
            .map(|i| i.level),
        Err(SignedError::BadSignature)
    );
    // A second "keys document" for the same source on another origin is
    // refused at registration.
    let other = keys_from_document(&doc, "data.example.net", FETCHED).expect("keys");
    let mut moved = other[0].clone();
    moved.signer = [9; 20];
    assert_eq!(
        store.register_key(&reviewer, moved, &reg, NOW, "test"),
        Err(SignedError::OriginMismatch)
    );
}
