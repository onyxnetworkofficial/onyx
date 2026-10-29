//! `/v1/labels/{set}/{address}` (pre-public loop PP3): the node's answers are the shared vectors'
//! (fixtures/dated/salted-vectors.json) byte for byte, verify against the pinned content hash,
//! carry only the asked address's rows, and only test sets load.

use super::*;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use onyx_core::{Confidence, EntityKind};
use onyx_labels::dated::Membership;
use onyx_labels::salted::verify_salted_answer;
use onyx_labels::Label;
use tower::ServiceExt;

fn vectors() -> Value {
    serde_json::from_str(include_str!("../../../fixtures/dated/salted-vectors.json"))
        .expect("vectors")
}

fn m(addr: &str, entity: &str, from: u64, to: Option<u64>) -> Membership {
    Membership {
        label: Label {
            address: addr.into(),
            entity: entity.into(),
            kind: EntityKind::Exchange,
            confidence: Confidence(900_000),
            evidence: vec![],
        },
        from,
        to,
    }
}

/// The vectors' synthetic set, as a store file.
fn file(name: &str, members: Vec<Membership>) -> Value {
    let set = DatedLabelSet {
        name: name.into(),
        version: 1,
        published: 2_000,
        members,
    };
    j!({
        "set": format!("0x{}", hex::encode(borsh::to_vec(&set).expect("borsh"))),
        "secret": vectors()["saltSeedHex"],
    })
}

fn members() -> Vec<Membership> {
    vec![
        m("test:addr-1", "entity:example-a", 100, None),
        m("test:addr-2", "entity:example-b", 200, Some(499)),
        m("test:addr-2", "entity:example-a", 500, None),
        m("test:addr-3", "entity:example-b", 150, None),
    ]
}

async fn call(app: &Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = resp.status();
    let cc = resp.headers().get(header::CACHE_CONTROL).cloned();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let body = serde_json::from_slice(&bytes).expect("json");
    crate::openapi_check::check("GET", uri, status.as_u16(), &body);
    assert_eq!(
        cc.as_ref().and_then(|v| v.to_str().ok()),
        Some("no-store"),
        "{uri}"
    );
    (status, body)
}

#[tokio::test]
async fn the_node_serves_the_shared_vectors_rows_and_only_the_asked_address() {
    let v = vectors();
    let store =
        SaltedLabelStore::from_files(&[file("test.onyx.salted", members())]).expect("store");
    let pin: Hash32 = crate::parse_hash(v["contentHash"].as_str().expect("pin"), "pin")
        .expect("hash")
        .0;
    assert_eq!(store.content_hashes(), vec![pin]);
    let app = routes(Arc::new(store));
    let (st, b) = call(&app, &format!("/v1/labels/{pin}/test:addr-2")).await;
    assert_eq!(st, StatusCode::OK);
    let answers = b["answers"].as_array().expect("answers");
    assert_eq!(answers.len(), 2, "both ranges of the address, nothing else");
    for (k, a) in answers.iter().enumerate() {
        assert_eq!(
            a,
            &v["cases"][k + 1]["answer"],
            "row {} is the vectors'",
            k + 1
        );
        let parsed = json::from_json(a).expect("wire form");
        let (mem, _) = verify_salted_answer(&parsed, &pin).expect("verifies against the pin");
        assert_eq!(mem.label.address, "test:addr-2");
    }
    let (st, b) = call(&app, &format!("/v1/labels/{pin}/test:addr-9")).await;
    assert_eq!(
        (st, b["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("no_label_row"))
    );
    let other = Hash32([5; 32]);
    let (st, b) = call(&app, &format!("/v1/labels/{other}/test:addr-1")).await;
    assert_eq!(
        (st, b["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("unknown_label_set"))
    );
    let upper = pin.to_string().to_uppercase().replacen("0X", "0x", 1);
    let (st, _) = call(&app, &format!("/v1/labels/{upper}/test:addr-1")).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(&app, &format!("/v1/labels/{pin}/test:addr-1?x=1")).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn answers_are_rate_limited_per_client() {
    let store =
        SaltedLabelStore::from_files(&[file("test.onyx.salted", members())]).expect("store");
    let pin = store.content_hashes()[0];
    let app = routes(Arc::new(store));
    for _ in 0..FREE_PER_MINUTE {
        assert_eq!(
            call(&app, &format!("/v1/labels/{pin}/test:addr-1")).await.0,
            StatusCode::OK
        );
    }
    let (st, b) = call(&app, &format!("/v1/labels/{pin}/test:addr-1")).await;
    assert_eq!(
        (st, b["error"]["code"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
}

#[test]
fn only_sorted_test_sets_load() {
    assert!(matches!(
        SaltedLabelStore::from_files(&[file("onyx.entities.btc", members())]),
        Err(StoreError::NotTest(_))
    ));
    let mut shuffled = members();
    shuffled.swap(0, 3);
    assert!(matches!(
        SaltedLabelStore::from_files(&[file("test.onyx.salted", shuffled)]),
        Err(StoreError::NotSorted(_))
    ));
    assert!(matches!(
        SaltedLabelStore::from_files(&[file("test.x", members()), file("test.x", members())]),
        Err(StoreError::Duplicate(_))
    ));
    let mut extra = file("test.x", members());
    extra["note"] = "x".into();
    assert!(matches!(
        SaltedLabelStore::from_files(&[extra]),
        Err(StoreError::Format(_))
    ));
    let mut short = file("test.x", members());
    short["secret"] = "0x0102".into();
    assert!(matches!(
        SaltedLabelStore::from_files(&[short]),
        Err(StoreError::Format(_))
    ));
}
