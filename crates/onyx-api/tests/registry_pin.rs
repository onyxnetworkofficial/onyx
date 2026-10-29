//! AUD-2-F8: a level-2 store file never chooses the registry the node trusts.
//! The registry it names must be the deployment manifest's, and the code at
//! that address must hash to the manifest's `registryCodeHash`, before the
//! node asks it anything.
use onyx_api::observations::{verify_registered, ObservationFeed, OnchainPointer};
use onyx_api::roots::JsonRpc;
use onyx_api::x402::BoxFuture;
use onyx_core::claim::MetricId;
use onyx_core::{
    keccak256, DataRights, FeedTemplate, ObservationKind, Redistribution, SourceId, Subject,
    SubjectRule,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU32, Ordering};

const REGISTRY: [u8; 20] = [0x11; 20];
const CODE: &[u8] = b"\x60\x80\x60\x40 registry runtime";

/// A node with `CODE` at every address; counts `eth_call`s (none may happen
/// before the pin is checked).
struct Node {
    calls: AtomicU32,
}

impl JsonRpc for Node {
    fn call<'a>(&'a self, method: &'a str, _: Value) -> BoxFuture<'a, Result<Value, String>> {
        let v = match method {
            "eth_getCode" => json!(format!("0x{}", hex::encode(CODE))),
            "eth_call" => {
                self.calls.fetch_add(1, Ordering::SeqCst);
                json!("0x")
            }
            _ => return Box::pin(async { Err("unexpected".to_string()) }),
        };
        Box::pin(async move { Ok(v) })
    }
}

fn feed() -> ObservationFeed {
    let mut f = ObservationFeed::new(FeedTemplate {
        feed: "test.index.v1".into(),
        metric: MetricId("test.index".into()),
        fetcher: keccak256(b"test fetcher"),
        url_template: "https://example.test/index.json".into(),
        selector: "/value".into(),
        kind: ObservationKind::Numeric {
            scale: 2,
            tolerance: 0,
        },
        window_secs: 300,
        min_observers: 3,
        rights: DataRights {
            license: "CC0-1.0".into(),
            redistribution: Redistribution::Allowed,
            source: SourceId("test.page".into()),
            attribution: None,
            terms: String::new(),
        },
        subject: SubjectRule::Fixed(Subject::Aggregate("test.index".into())),
    })
    .expect("feed");
    f.onchain = Some(OnchainPointer {
        chain_id: 84532,
        registry: REGISTRY,
    });
    f
}

#[tokio::test]
async fn a_store_cannot_choose_the_registry_or_its_code() {
    let node = Node {
        calls: AtomicU32::new(0),
    };
    let f = feed();
    let pinned = keccak256(CODE);
    let refused = |r: Result<(), onyx_api::observations::ObservationError>, why: &str| {
        let e = r.expect_err(why).to_string();
        assert!(e.contains(why), "{e}");
    };
    // No registry in the manifest.
    refused(
        verify_registered(&node, 84532, None, &f).await,
        "deployment manifest has none",
    );
    // Another registry in the manifest.
    refused(
        verify_registered(&node, 84532, Some(([0x22; 20], pinned)), &f).await,
        "the deployment's is",
    );
    // The manifest's registry, but its code is not the pinned build.
    refused(
        verify_registered(&node, 84532, Some((REGISTRY, keccak256(b"other"))), &f).await,
        "not the pinned build",
    );
    assert_eq!(node.calls.load(Ordering::SeqCst), 0, "asked before the pin");
    // Pinned registry and code: the registry is asked (and its empty answer refused).
    assert!(
        verify_registered(&node, 84532, Some((REGISTRY, pinned)), &f)
            .await
            .is_err()
    );
    assert_eq!(node.calls.load(Ordering::SeqCst), 1);
}
