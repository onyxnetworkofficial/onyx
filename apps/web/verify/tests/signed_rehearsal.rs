//! ROADMAP C61: the browser's check of a level-3 answer (`signed_view`).
//! Its mirrors are the coordinator's and the API's own code, the recorded
//! rehearsal checks out under the compiled pin and is the body the API's
//! `signed_serve` writes for those chain answers, and every forgery a node or
//! an RPC can make is refused (THREATS C61-*).

use onyx_api::roots::JsonRpc;
use onyx_api::signed_serve::{SignedStore, SERVABLE_AFTER_SECS as API_SERVABLE};
use onyx_api::x402::BoxFuture;
use onyx_coordinator::signed::signed_fact_key as coordinator_fact_key;
use onyx_coordinator::signed_chain::value_hash as coordinator_value_hash;
use onyx_coordinator::signed_intake::{PostEntry, PostLog, PostStatus};
use onyx_core::{keccak256, Hash32, SignedPayload, SignedReport, SourceKey, Value as OnyxValue};
use onyx_labels::attest::{eip191_sign_with, secret_address};
use onyx_web_verify::signed_view::{
    self, check, check_body, check_rehearsal_doc, decode_reads, forge_value, key_id, reads_digest,
    rehearsal, rehearsal_pin, signed_fact_key, value_hash, BookPin, Reads, BODY_KEYS,
    DISPUTE_WINDOW_SECS, MAX_BLOCK_AHEAD_SECS, MAX_FINALIZED_AGE_SECS, SERVABLE_AFTER_SECS,
};
use serde_json::{json, Value};
use std::sync::Arc;

const DOC: &str = include_str!("../../static/level3/signed-rehearsal.json");

fn doc() -> Value {
    serde_json::from_str(DOC).expect("rehearsal json")
}

fn body() -> Value {
    doc()["body"].clone()
}

fn reads() -> Reads {
    decode_reads(&doc()["reads"]).expect("reads")
}

fn now() -> u64 {
    rehearsal::RECORDED_UNIX + rehearsal::CLOCK_STAND_IN_SECS
}

fn pin() -> BookPin {
    rehearsal_pin().expect("pin")
}

fn unhex(s: &str) -> Vec<u8> {
    hex::decode(s.trim_start_matches("0x")).expect("hex")
}

fn payload(b: &Value) -> SignedPayload {
    borsh::from_slice(&unhex(b["payload"].as_str().expect("payload"))).expect("payload")
}

fn refused(b: &Value, r: &Reads, now: u64, want: &str) {
    match check(b, &pin(), r, now) {
        Ok(_) => panic!("passed, wanted a refusal naming {want:?}"),
        Err(p) => assert!(
            p.iter().any(|x| x.contains(want)),
            "wanted {want:?}, got {p:?}"
        ),
    }
}

fn w(n: u64) -> [u8; 32] {
    let mut x = [0u8; 32];
    x[24..].copy_from_slice(&n.to_be_bytes());
    x
}

#[test]
fn the_mirrors_are_the_coordinators_and_the_apis() {
    let p = payload(&body());
    assert_eq!(signed_fact_key(&p), coordinator_fact_key(&p));
    let r = SignedReport {
        payload: p.clone(),
        signature: [1; 65],
    };
    assert_eq!(value_hash(&p.value), coordinator_value_hash(&r));
    assert_eq!(SERVABLE_AFTER_SECS, API_SERVABLE);
    assert_eq!(
        DISPUTE_WINDOW_SECS,
        onyx_coordinator::signed::DISPUTE_WINDOW_SECS
    );
    // The key id is `SourceKey::id` (provenance is not part of it).
    let k = SourceKey {
        source: p.source.clone(),
        signer: [9; 20],
        provenance: onyx_core::KeyProvenance::WellKnown {
            origin: "data.example.org".into(),
            document_hash: keccak256(b"doc"),
            fetched_unix: 1,
        },
    };
    assert_eq!(key_id(&p.source, &[9; 20]), k.id());
    // Every key of the served body, and nothing else.
    let b = body();
    let mut keys: Vec<&str> = b
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    let mut want = BODY_KEYS.to_vec();
    want.extend(signed_view::OPTIONAL_KEYS);
    want.sort_unstable();
    assert_eq!(keys, want);
}

#[test]
fn the_recorded_rehearsal_checks_out_under_the_compiled_pin() {
    let c = check_rehearsal_doc(&doc()).unwrap_or_else(|p| panic!("{p:?}"));
    assert_eq!(c.payload.source.0, "example.signed");
    assert_eq!(reads_digest(&c.reads).to_string(), rehearsal::READS_DIGEST);
    assert_eq!(c.reads.chain_id, 31_337, "a local anvil, not a live chain");
    assert!(c.reads.time >= c.accepted_at + SERVABLE_AFTER_SECS);
    let d = doc();
    assert_eq!(d["rehearsal"]["readsDigest"], rehearsal::READS_DIGEST);
    assert!(d["rehearsal"]["what"]
        .as_str()
        .expect("what")
        .contains("no live deployment"));
    // The wasm export says it is a rehearsal and that the reads are recorded.
    let out: Value = serde_json::from_str(
        &signed_view::check_signed_rehearsal(DOC).unwrap_or_else(|_| panic!("errored")),
    )
    .expect("json");
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["rehearsal"], true);
    assert_eq!(out["chain_reads"], "recorded");
    assert_eq!(out["value"], signed_view::value_text(&c.payload.value));
}

#[test]
fn an_edited_record_is_refused_by_the_pin() {
    // The recorded answers are pinned: changing any of them is refused even
    // when the edit is self-consistent.
    for k in ["settleable", "factView", "code"] {
        let mut d = doc();
        let s = d["reads"][k].as_str().expect("hex").to_string();
        let last = if s.ends_with('0') { "1" } else { "0" };
        d["reads"][k] = json!(format!("{}{last}", &s[..s.len() - 1]));
        let e = check_rehearsal_doc(&d).expect_err(k);
        assert!(
            e[0].contains("not the ones this verifier pins"),
            "{k}: {e:?}"
        );
    }
    let mut d = doc();
    d["reads"]["finalized"]["timestamp"] = json!("0x1");
    assert!(check_rehearsal_doc(&d).is_err());
    let mut d = doc();
    d["rehearsal"]["clockStandInSecs"] = json!(999_999);
    assert!(check_rehearsal_doc(&d).is_err());
    let mut d = doc();
    d["kind"] = json!("onyx-level3-live");
    assert!(check_rehearsal_doc(&d).is_err());
}

#[test]
fn c61_1_every_body_field_is_bound_to_the_signed_payload() {
    let (r, t) = (reads(), now());
    let edits: Vec<(&str, Value, &str)> = vec![
        (
            "value",
            json!({"uint": "8"}),
            "value is not the signed value",
        ),
        (
            "value",
            json!({"int": "7"}),
            "value is not the signed value",
        ),
        ("source", json!("example.other"), "source"),
        ("metric", json!("example.signed.other"), "metric"),
        ("subject", json!("entity:other"), "subject"),
        ("observedUnix", json!(1), "observedUnix"),
        ("sequence", json!(99), "sequence"),
        ("fact", json!(Hash32([3; 32]).to_string()), "fact"),
        (
            "reportHash",
            json!(Hash32([3; 32]).to_string()),
            "reportHash",
        ),
        ("valueHash", json!(Hash32([3; 32]).to_string()), "valueHash"),
        ("key", json!(Hash32([3; 32]).to_string()), "key"),
        ("level", json!(2), "not a level-3 answer"),
        ("chainId", json!(84_532), "another chain"),
        (
            "book",
            json!(format!("0x{}", "11".repeat(20))),
            "another book",
        ),
        ("acceptedAt", json!(1), "acceptedAt is not the book's"),
        ("payload", json!("0x00"), "payload"),
        ("signature", json!("0x00"), "signature"),
    ];
    for (k, v, want) in edits {
        let mut b = body();
        b[k] = v;
        if k == "acceptedAt" {
            // Moved in the ladder too, so the book read is what refuses it.
            b["ladder"]["steps"][0]["at"] = b["acceptedAt"].clone();
        }
        refused(&b, &r, t, want);
    }
    // No unknown key, no missing key.
    let mut b = body();
    b["note"] = json!("trust me");
    refused(&b, &r, t, "does not take: note");
    let mut b = body();
    b.as_object_mut().expect("object").remove("valueHash");
    refused(&b, &r, t, "has no valueHash");
    // A payload with a trailing byte is not canonical.
    let mut b = body();
    b["payload"] = json!(format!("{}00", b["payload"].as_str().expect("payload")));
    refused(&b, &r, t, "payload");
    assert!(check_body(&json!([]), &pin()).is_err());
}

#[test]
fn c61_2_another_signer_is_refused_even_naming_the_real_key() {
    // An attacker signs another value with its own key but keeps the real
    // key id: the recovered signer's id is not the payload's key.
    let (r, t) = (reads(), now());
    let mut p = payload(&body());
    p.value = OnyxValue::UInt(8);
    let attacker = keccak256(b"attacker").0;
    let sig = eip191_sign_with(&attacker, p.message().as_bytes()).expect("sign");
    let forged = |p: &SignedPayload, sig: [u8; 65]| {
        let rep = SignedReport {
            payload: p.clone(),
            signature: sig,
        };
        let mut b = body();
        b["payload"] = json!(format!(
            "0x{}",
            hex::encode(borsh::to_vec(p).expect("borsh"))
        ));
        b["signature"] = json!(format!("0x{}", hex::encode(sig)));
        b["value"] = signed_view::value_json(&p.value);
        b["key"] = json!(p.key.to_string());
        b["fact"] = json!(signed_fact_key(p).to_string());
        b["reportHash"] = json!(rep.hash().to_string());
        b["valueHash"] = json!(value_hash(&p.value).to_string());
        b["ladder"] = signed_view::expected_ladder(
            p,
            &rep.hash(),
            b["acceptedAt"].as_u64().expect("acceptedAt"),
        );
        b
    };
    refused(&forged(&p, sig), &r, t, "someone else signed");
    // With its own key id the body is self-consistent: only the chain read
    // refuses it (the book settles the fact on another report).
    p.key = key_id(&p.source, &secret_address(&attacker).expect("address"));
    let sig = eip191_sign_with(&attacker, p.message().as_bytes()).expect("sign");
    let b = forged(&p, sig);
    assert!(check_body(&b, &pin()).is_ok(), "self-consistent");
    refused(&b, &r, t, "another report");
    // A non-canonical signature (high s) does not recover.
    let mut b = body();
    let mut s = unhex(b["signature"].as_str().expect("sig"));
    s[64] = 29;
    b["signature"] = json!(format!("0x{}", hex::encode(&s)));
    refused(&b, &r, t, "does not recover");
}

#[test]
fn c61_3_a_forged_value_with_every_hash_recomputed_is_refused() {
    let b = forge_value(&body()).expect("forge");
    let p = check(&b, &pin(), &reads(), now()).expect_err("forged");
    assert!(p.iter().any(|x| x.contains("someone else signed")), "{p:?}");
    // The wasm export forges the same way.
    let d: Value = serde_json::from_str(
        &signed_view::signed_forged_value(DOC).unwrap_or_else(|_| panic!("errored")),
    )
    .expect("json");
    assert_eq!(d["body"], b);
}

#[test]
fn c61_4_the_clients_own_chain_read_decides() {
    let (b, t) = (body(), now());
    let base = reads();
    let at = |f: &dyn Fn(&mut Reads)| {
        let mut r = base.clone();
        f(&mut r);
        r
    };
    // Another chain (a public Base Sepolia RPC against the rehearsal pin).
    refused(
        &b,
        &at(&|r| r.chain_id = 84_532),
        t,
        "your RPC is on chain 84532",
    );
    // Not settleable at the client's block (unknown, disputed or in window).
    refused(
        &b,
        &at(&|r| r.settleable = [w(0), [0; 32], [0; 32]].concat()),
        t,
        "does not let anyone settle",
    );
    // Settleable on another report or value.
    let rh = Hash32(base.settleable[32..64].try_into().expect("word"));
    let vh = Hash32(base.settleable[64..96].try_into().expect("word"));
    refused(
        &b,
        &at(&|r| r.settleable = [w(1), [5; 32], vh.0].concat()),
        t,
        "another report",
    );
    refused(
        &b,
        &at(&|r| r.settleable = [w(1), rh.0, [5; 32]].concat()),
        t,
        "another value",
    );
    refused(
        &b,
        &at(&|r| r.settleable = vec![1, 2, 3]),
        t,
        "does not decode",
    );
    refused(
        &b,
        &at(&|r| r.settleable = [w(2), rh.0, vh.0].concat()),
        t,
        "does not decode",
    );
    // factView: disputed, nothing, another accepted report, another time.
    let mut fv = base.fact_view.clone();
    fv[..32].copy_from_slice(&w(2));
    let disputed = fv.clone();
    refused(
        &b,
        &at(&|r| r.fact_view = disputed.clone()),
        t,
        "holds a contradiction",
    );
    fv[..32].copy_from_slice(&w(0));
    let none = fv.clone();
    refused(&b, &at(&|r| r.fact_view = none.clone()), t, "has no value");
    let mut fv = base.fact_view.clone();
    fv[32..64].copy_from_slice(&[6; 32]);
    let other = fv.clone();
    refused(
        &b,
        &at(&|r| r.fact_view = other.clone()),
        t,
        "accepted another report",
    );
    let mut fv = base.fact_view.clone();
    fv[64..96].copy_from_slice(&w(1));
    let when = fv.clone();
    refused(
        &b,
        &at(&|r| r.fact_view = when.clone()),
        t,
        "acceptedAt is not the book's",
    );
    // Another build at the address (a proxy, another reviewer).
    refused(
        &b,
        &at(&|r| r.code_hash = keccak256(b"proxy")),
        t,
        "not the pinned OnyxSignedBook",
    );
}

#[test]
fn c61_5_the_margin_and_freshness_run_on_the_block_and_the_clients_clock() {
    let b = body();
    let base = reads();
    let accepted = check(&b, &pin(), &base, now()).expect("ok").accepted_at;
    let servable = accepted + SERVABLE_AFTER_SECS;
    // The block at the margin: ok; one second short: refused (the client's
    // clock well past it, the block fresh).
    let mut r = base.clone();
    r.time = servable;
    assert!(check(&b, &pin(), &r, servable + 10).is_ok());
    r.time = servable - 1;
    refused(
        &b,
        &r,
        servable + 10,
        "at your finalized block the fact is 1 s short",
    );
    // The client's clock at the margin: ok; one second short: refused.
    let mut r = base.clone();
    r.time = servable;
    assert!(check(&b, &pin(), &r, servable).is_ok());
    refused(&b, &r, servable - 1, "on your clock the fact is 1 s short");
    // A stale RPC: the block at most MAX_FINALIZED_AGE_SECS old.
    let mut r = base.clone();
    r.time = servable;
    assert!(check(&b, &pin(), &r, servable + MAX_FINALIZED_AGE_SECS).is_ok());
    refused(
        &b,
        &r,
        servable + MAX_FINALIZED_AGE_SECS + 1,
        "stale or stuck RPC",
    );
    // Review C61-R2-3: the freshness runs on the reader's clock, and the
    // refusal says so (a clock ahead fails closed on an honest RPC).
    refused(
        &b,
        &r,
        servable + MAX_FINALIZED_AGE_SECS + 1,
        "or your clock is ahead",
    );
    // A block far ahead of the client's clock.
    let mut r = base.clone();
    r.time = servable + MAX_BLOCK_AHEAD_SECS;
    assert!(check(&b, &pin(), &r, servable).is_ok());
    r.time = servable + MAX_BLOCK_AHEAD_SECS + 1;
    refused(&b, &r, servable, "ahead of your clock");
}

#[test]
fn c61_6_reads_must_be_raw_and_complete() {
    let d = doc();
    let mut x = d["reads"].clone();
    x["finalized"] = Value::Null;
    assert!(decode_reads(&x)
        .expect_err("null")
        .contains("no finalized block"));
    let mut x = d["reads"].clone();
    x["extra"] = json!(1);
    assert!(decode_reads(&x).is_err());
    let mut x = d["reads"].clone();
    x["chainId"] = json!(31_337);
    assert!(decode_reads(&x).is_err(), "a quantity is a hex string");
    let mut x = d["reads"].clone();
    x["finalized"]["hash"] = json!("0x1234");
    assert!(decode_reads(&x).is_err());
}

/// A node that answers every read with the recorded rehearsal's answers.
struct Recorded(Value);

impl JsonRpc for Recorded {
    fn call<'a>(&'a self, method: &'a str, p: Value) -> BoxFuture<'a, Result<Value, String>> {
        let r = &self.0;
        let out = match method {
            "eth_getBlockByNumber" => Ok(r["finalized"].clone()),
            "eth_call" => {
                let data = p[0]["data"].as_str().unwrap_or("");
                if p[1]["blockHash"] != r["finalized"]["hash"] {
                    Err("read at another block".to_string())
                } else if data.starts_with(&format!(
                    "0x{}",
                    hex::encode(&keccak256(b"settleable(bytes32)").0[..4])
                )) {
                    Ok(r["settleable"].clone())
                } else if data.starts_with(&format!(
                    "0x{}",
                    hex::encode(&keccak256(b"factView(bytes32)").0[..4])
                )) {
                    Ok(r["factView"].clone())
                } else {
                    // report(bytes32): recorded (a non-zero key word).
                    Ok(json!(format!("0x{}{}", "11".repeat(32), "00".repeat(192))))
                }
            }
            m => Err(format!("unexpected {m}")),
        };
        Box::pin(async move { out })
    }
}

#[tokio::test]
async fn the_recorded_body_is_what_the_apis_signed_serve_writes() {
    // ROADMAP C61: the shipped body is byte for byte what onyx-api's
    // `SignedStore::answer` writes for the recorded chain answers, so the
    // browser checks the real served form, not a hand-made one.
    let d = doc();
    let b = &d["body"];
    let p = payload(b);
    let sig: [u8; 65] = unhex(b["signature"].as_str().expect("sig"))
        .try_into()
        .expect("65");
    let report = SignedReport {
        payload: p,
        signature: sig,
    };
    let pin = pin();
    let book_pin = onyx_api::signed_serve::BookPin {
        chain_id: pin.chain_id,
        book: pin.book,
        code_hash: pin.code_hash,
        reviewer: [0; 20],
    };
    let mut log = PostLog::new(pin.chain_id, pin.book);
    log.entries.push(PostEntry {
        report,
        received_unix: rehearsal::RECORDED_UNIX - SERVABLE_AFTER_SECS,
        received_block_time: 1,
        status: PostStatus::Recorded {
            outcome: None,
            block: 1,
            block_time: 1,
        },
    });
    let store = SignedStore::new(
        book_pin,
        None,
        Arc::new(Recorded(d["reads"].clone())),
        Arc::new(|| rehearsal::RECORDED_UNIX),
    )
    .with_log(log);
    let fact = Hash32(
        unhex(b["fact"].as_str().expect("fact"))
            .try_into()
            .expect("32"),
    );
    let served = store.answer(&fact).await.expect("served");
    assert_eq!(&served, b);
}

#[test]
fn c61_r1_1_a_lying_rpc_timestamp_is_not_caught() {
    // Review C61-R1-1, THREATS C61-4: the check trusts the finalized block's
    // timestamp as the RPC reports it. Old state labelled with a fresh time
    // passes: asserted here so the claim cannot drift (TODO C63).
    let mut r = reads();
    let later = now() + 30 * 86_400;
    r.time = later;
    assert!(check(&body(), &pin(), &r, later).is_ok());
}

#[test]
fn c61_r1_6_returns_are_decoded_exactly() {
    let (b, t) = (body(), now());
    let base = reads();
    assert_eq!(base.settleable.len(), 96);
    assert_eq!(base.fact_view.len(), 160);
    let mut r = base.clone();
    r.settleable.push(0);
    refused(&b, &r, t, "settleable(): the answer does not decode");
    let mut r = base.clone();
    r.fact_view.extend([0u8; 32]);
    refused(&b, &r, t, "factView(): the answer does not decode");
    let mut r = base.clone();
    r.fact_view[96..128].copy_from_slice(&w(0x60));
    refused(&b, &r, t, "factView(): the answer does not decode");
    // A dispute array of one word, laid out right, decodes (and is refused
    // for another reason: the state).
    let mut r = base.clone();
    r.fact_view[..32].copy_from_slice(&w(2));
    r.fact_view[128..160].copy_from_slice(&w(1));
    r.fact_view.extend([9u8; 32]);
    refused(&b, &r, t, "holds a contradiction");
}

#[test]
fn signed_answers_carry_a_ladder() {
    // ADR-0048 amendment 2 (ROADMAP C64): the served level-3 body carries
    // its one-step ladder, at the book's acceptance; the browser takes it
    // only when it is exactly that step.
    let (b, r, t) = (body(), reads(), now());
    let l = &b["ladder"];
    assert_eq!(l["level"], 3);
    assert_eq!(l["steps"][0]["record"], "OnyxSignedBook.ReportRecorded");
    assert_eq!(l["steps"][0]["evidence"], b["reportHash"]);
    assert_eq!(l["steps"][0]["at"], b["acceptedAt"]);
    check(&b, &pin(), &r, t).unwrap_or_else(|p| panic!("{p:?}"));
    // A node from before the field: still taken.
    let mut old = b.clone();
    old.as_object_mut().expect("obj").remove("ladder");
    check(&old, &pin(), &r, t).unwrap_or_else(|p| panic!("{p:?}"));
    // A lifted level, a moved time or another value: refused.
    let want = "the body's ladder is not this report's one step at level 3";
    let mut lifted = b.clone();
    lifted["ladder"]["level"] = serde_json::json!(2);
    lifted["ladder"]["steps"][0]["level"] = serde_json::json!(2);
    lifted["ladder"]["steps"][0]["name"] = serde_json::json!("re-observed");
    refused(&lifted, &r, t, want);
    let mut moved = b.clone();
    moved["ladder"]["steps"][0]["at"] = serde_json::json!(1);
    refused(&moved, &r, t, want);
    let mut other = b.clone();
    other["ladder"]["value"] = serde_json::json!(format!("0x{}", "11".repeat(32)));
    refused(&other, &r, t, want);
}
