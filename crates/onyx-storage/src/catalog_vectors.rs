//! The catalog cases of the shared Rust/Solidity vectors (ADR-0067): the
//! `catalog` part of `fixtures/storage/vectors.json`, read by
//! `contracts/test/OnyxStorageVectors.t.sol` and
//! `contracts/test/OnyxStorageCatalog.t.sol`.
//!
//! Every byte string here comes from the real encoders of `onyx-bulk`
//! (`Manifest::build`, `Catalog::build`, `catalog::sign`, Borsh of a
//! `SignedPayload`), so the contract is checked against what a publisher
//! really signs. A refused case changes one thing in a good case. Where no
//! encoder can make the bad bytes (a cut header, one extra byte), the case
//! says so in its name.

use crate::challenge::tests::dataset;
use crate::deal::{
    catalog_payload, catalog_root, catalog_sequence, check_catalog_payload, check_deal_in_catalog,
    check_in_catalog, leaf_row, next_catalog_sequence, read_catalog, read_header, DealError,
    TAIL_BYTES,
};
use onyx_bulk::catalog::{self, Catalog, CatalogHeader, CatalogLeaf};
use onyx_bulk::manifest::{Manifest, NewVersion, ShardEntry, FORMAT, FORMAT_2};
use onyx_bulk::Schema;
use onyx_core::{
    keccak256, DataRights, Hash32, MetricId, Redistribution, SignedPayload, SourceId, Subject,
    Value as SignedValue,
};
use serde_json::{json, Value};

/// The test key every vector report is signed with (never a real key).
const SECRET: [u8; 32] = [7u8; 32];
const PUBLISHER: &str = "synthetic.publisher";
/// Another publisher whose id has the same length, and one with another length.
const OTHER_SAME_LEN: &str = "synthetic.publishes";
const OTHER: &str = "synthetic.other";
/// Leaves of the large catalog: a path of 14 levels, as a catalog of about
/// 9,000 datasets has.
const LARGE: usize = 9_000;

fn hx(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn hs(v: &[Hash32]) -> Vec<String> {
    v.iter().map(ToString::to_string).collect()
}

fn schema(scale: u8) -> Schema {
    Schema::from_json(&json!({"columns":[{"name":"t","kind":"int"},
        {"name":"v","kind":"decimal","scale":scale}],"key":"t"}))
    .expect("schema")
}

fn entries(n: u32) -> Vec<ShardEntry> {
    (0..n)
        .map(|i| ShardEntry {
            index: i,
            root: keccak256(&i.to_le_bytes()),
            rows: 10,
            bytes: 1_000,
            key_min: i128::from(i),
            key_max: i128::from(i),
        })
        .collect()
}

/// A small manifest of `publisher` (version 1, or the successor of `prev`).
fn manifest(
    dataset: &str,
    publisher: &str,
    created: u64,
    scale: u8,
    prev: Option<&Manifest>,
) -> Manifest {
    let n = prev.map_or(2, |p| p.header.shard_count + 1);
    Manifest::build(
        NewVersion {
            dataset: dataset.into(),
            publisher: SourceId(publisher.into()),
            created_unix: created,
            schema: schema(scale),
            rights: DataRights {
                license: "CC0-1.0".into(),
                redistribution: Redistribution::Allowed,
                source: SourceId(publisher.into()),
                attribution: None,
                terms: String::new(),
            },
        },
        prev,
        entries(n),
    )
    .expect("manifest")
}

fn header_bytes(m: &Manifest) -> Vec<u8> {
    m.header.to_bytes()
}

fn catalog_bytes(h: &CatalogHeader) -> Vec<u8> {
    borsh::to_vec(h).expect("borsh")
}

fn signer() -> [u8; 20] {
    onyx_bulk::anchor::address_of(&SECRET).expect("address")
}

/// The Borsh payload of catalog header `h`, signed with the test key.
fn signed(h: &CatalogHeader) -> (Vec<u8>, [u8; 65]) {
    let r = catalog::sign(&SECRET, h).expect("signs");
    (borsh::to_vec(&r.payload).expect("borsh"), r.signature)
}

/// One input of `createDealInCatalog`.
#[derive(Clone)]
struct Case {
    header: Vec<u8>,
    catalog: Vec<u8>,
    leaf_index: u64,
    siblings: Vec<Hash32>,
    payload: Vec<u8>,
}

impl Case {
    /// The manifest `m` as a leaf of catalog `c`, with `c`'s signed payload.
    fn of(m: &Manifest, c: &Catalog) -> Case {
        let i = c.find(&m.header.dataset).expect("listed");
        let lp = c.leaf_proof(i).expect("proof");
        assert!(lp.leaf.names(&m.header), "the leaf names this version");
        Case {
            header: header_bytes(m),
            catalog: catalog_bytes(&c.header),
            leaf_index: lp.proof.index,
            siblings: lp.proof.siblings,
            payload: signed(&c.header).0,
        }
    }

    fn json(&self) -> Value {
        json!({"header": hx(&self.header), "catalog": hx(&self.catalog),
               "leafIndex": self.leaf_index, "siblings": hs(&self.siblings),
               "payload": hx(&self.payload)})
    }

    fn check(&self) -> Result<Hash32, DealError> {
        check_deal_in_catalog(
            &self.header,
            &self.catalog,
            self.leaf_index,
            &self.siblings,
            &self.payload,
        )
        .map(|(_, digest)| digest)
    }
}

/// The contract error each refusal maps to.
fn kind(e: &DealError) -> &'static str {
    match e {
        DealError::Header(_) => "header",
        DealError::Payload(_) => "payload",
        DealError::Catalog(_) => "catalog",
        DealError::Leaf(_) => "leaf",
    }
}

/// A good case with its expected reads.
fn deal(name: &str, m: &Manifest, c: &Catalog) -> Value {
    let case = Case::of(m, c);
    let (t, head) = check_in_catalog(&case.header, &case.catalog, case.leaf_index, &case.siblings)
        .expect("in the catalog");
    let digest = check_catalog_payload(&head, &case.payload).expect("payload");
    // The mirror against the real encoders, byte for byte.
    let leaf = CatalogLeaf::of(&m.header);
    assert_eq!(leaf_row(&case.header).expect("row"), leaf.row());
    assert_eq!(head.catalog_root, c.header.root());
    assert_eq!(t.manifest_root, m.header.root());
    assert_eq!(
        catalog_sequence(c.header.number),
        catalog::sequence_for_catalog(c.header.number)
    );
    let p = catalog::payload(&c.header, &signer());
    assert_eq!(catalog_payload(&head, &p.key), case.payload);
    assert_eq!(digest, p.digest());
    let mut v = case.json();
    let extra = json!({"name": name, "format": m.header.format, "version": m.header.version,
        "manifestRoot": t.manifest_root.to_string(), "leafRow": hx(&leaf.row()),
        "leafCount": c.header.leaf_count, "catalogRoot": head.catalog_root.to_string(),
        "digest": digest.to_string(), "shardCount": t.shard_count, "byteCount": t.byte_count,
        "shardsRoot": t.shards_root.to_string()});
    for (k, x) in extra.as_object().expect("object") {
        v[k] = x.clone();
    }
    v
}

/// A refused case: `error` is the refusal of the Rust mirror, and the
/// contract must refuse it with the matching error.
fn refused(out: &mut Vec<Value>, name: &str, want: &str, case: &Case) {
    let e = case.check().expect_err(name);
    assert_eq!(kind(&e), want, "{name}: {e}");
    let mut v = case.json();
    v["name"] = json!(name);
    v["error"] = json!(want);
    out.push(v);
}

/// A changed payload of catalog `h`, as Borsh of a real `SignedPayload`.
fn payload_with(h: &CatalogHeader, change: impl FnOnce(&mut SignedPayload)) -> Vec<u8> {
    let mut p = catalog::payload(h, &signer());
    change(&mut p);
    borsh::to_vec(&p).expect("borsh")
}

/// The `catalog` part of the vectors.
pub(crate) fn build() -> Value {
    let publisher = SourceId(PUBLISHER.into());
    let t0 = 1_700_000_000u64;
    // Five real manifests. In dataset order: removed, storage-test, widened,
    // zz-a, zz-b.
    let (main, _) = dataset(5, 11);
    let removed = manifest("synthetic.removed", PUBLISHER, t0, 2, None);
    let wide1 = manifest("synthetic.widened", PUBLISHER, t0, 2, None);
    let za = manifest("synthetic.zz-a", PUBLISHER, t0, 2, None);
    let zb = manifest("synthetic.zz-b", PUBLISHER, t0, 2, None);
    assert_eq!(main.header.format, FORMAT);
    let first: Vec<CatalogLeaf> = [&removed, &main, &wide1, &za, &zb]
        .iter()
        .map(|m| CatalogLeaf::of(&m.header))
        .collect();
    // Catalog 0: five leaves (the last one's path has a promoted level).
    let small = Catalog::build(publisher.clone(), None, t0, first.clone()).expect("catalog 0");

    // Catalog 1: 9,000 leaves. `removed` ends in a tombstone, `widened` moves
    // to a format-2 version, the rest stays, and filler leaves are added.
    let wide2 = manifest("synthetic.widened", PUBLISHER, t0 + 20, 4, Some(&wide1));
    assert_eq!((wide2.header.format, wide2.header.version), (FORMAT_2, 2));
    let tomb = first[0].tombstone().expect("tombstone");
    let mut leaves = vec![
        tomb,
        first[1].clone(),
        CatalogLeaf::of(&wide2.header),
        first[3].clone(),
        first[4].clone(),
    ];
    leaves.extend((leaves.len()..LARGE).map(|i| CatalogLeaf {
        publisher: publisher.clone(),
        dataset: format!("synthetic.zz-fill-{i:05}"),
        version: 1,
        prev: Hash32::ZERO,
        root: keccak256(&(i as u64).to_le_bytes()),
    }));
    let large =
        Catalog::build(publisher.clone(), Some(&small), t0 + 30, leaves).expect("catalog 1");
    assert_eq!(large.header.number, 1);

    // A catalog of one leaf, so its path is empty. It is numbered 2 and names
    // catalog 1 as the one before it, so its report has a sequence of its
    // own in the book; it does not carry catalog 1's leaves on, which the
    // contract does not look at (a node that holds whole catalogs does).
    let solo = manifest("synthetic.solo", PUBLISHER, t0 + 40, 2, None);
    let mut single = Catalog::build(
        publisher.clone(),
        None,
        t0 + 50,
        vec![CatalogLeaf::of(&solo.header)],
    )
    .expect("a catalog of one leaf");
    single.header.number = 2;
    single.header.prev = large.header.root();
    assert_eq!(single.header.leaf_count, 1);

    let deals = vec![
        deal("first catalog, format 1", &main, &small),
        deal("first catalog, last leaf (a promoted level)", &zb, &small),
        deal("large catalog, 14 levels", &main, &large),
        deal("large catalog, format 2, version 2", &wide2, &large),
        deal(
            "a catalog of one leaf, an empty path (number 2)",
            &solo,
            &single,
        ),
    ];
    assert_eq!(deals[2]["siblings"].as_array().map(Vec::len), Some(14));
    assert_eq!(deals[4]["siblings"].as_array().map(Vec::len), Some(0));

    // The whole call of the first deal, as the tool sends it.
    let first_case = Case::of(&main, &small);
    let (call_r, call_epochs, call_url) = (3u8, 4u32, "http://127.0.0.1:2/");
    let call = crate::abi::create_deal_in_catalog(
        &first_case.header,
        &first_case.catalog,
        first_case.leaf_index,
        &first_case.siblings,
        &first_case.payload,
        call_r,
        call_epochs,
        call_url,
    );
    // The last catalog number there is: no catalog follows it.
    let mut last = small.header.clone();
    last.number = u64::MAX;
    assert_eq!(next_catalog_sequence(u64::MAX), None);
    let reserved = u64::MAX.to_string();

    let catalogs: Vec<Value> = [&small, &large, &single]
        .iter()
        .map(|c| {
            let (payload, signature) = signed(&c.header);
            let bytes = catalog_bytes(&c.header);
            let head = read_catalog(&bytes, PUBLISHER.as_bytes()).expect("reads");
            json!({"header": hx(&bytes), "root": c.header.root().to_string(),
                   "number": c.header.number, "createdUnix": c.header.created_unix,
                   "leafCount": c.header.leaf_count,
                   "leavesRoot": c.header.leaves_root.to_string(),
                   "sequence": catalog_sequence(c.header.number).to_string(),
                   "nextSequence": next_catalog_sequence(c.header.number).expect("next").to_string(),
                   "payload": hx(&payload), "signature": hx(&signature),
                   "digest": check_catalog_payload(&head, &payload).expect("ok").to_string()})
        })
        .collect();

    // ---- refused cases, each one change away from a good one.
    let good = Case::of(&main, &small);
    let deep = Case::of(&main, &large);
    let mut bad = Vec::new();
    let with = |f: &dyn Fn(&mut Case)| {
        let mut c = good.clone();
        f(&mut c);
        c
    };

    // The leaf and its path.
    refused(
        &mut bad,
        "wrong leaf index",
        "leaf",
        &with(&|c| c.leaf_index = 0),
    );
    refused(
        &mut bad,
        "leaf index past the leaf count",
        "leaf",
        &with(&|c| c.leaf_index = 5),
    );
    refused(
        &mut bad,
        "a changed sibling",
        "leaf",
        &with(&|c| c.siblings[1].0[31] ^= 1),
    );
    refused(
        &mut bad,
        "one sibling missing",
        "leaf",
        &with(&|c| {
            c.siblings.pop();
        }),
    );
    refused(
        &mut bad,
        "one sibling too many",
        "leaf",
        &with(&|c| {
            c.siblings.push(Hash32::ZERO);
        }),
    );
    refused(
        &mut bad,
        "a changed sibling in a path of 14 levels",
        "leaf",
        &{
            let mut c = deep.clone();
            c.siblings[13].0[0] ^= 0x80;
            c
        },
    );
    // A catalog header with another leaf count, and its own signed payload.
    let mut counted = small.header.clone();
    counted.leaf_count += 1;
    refused(
        &mut bad,
        "wrong leaf count",
        "leaf",
        &with(&|c| {
            c.catalog = catalog_bytes(&counted);
            c.payload = signed(&counted).0;
        }),
    );
    refused(
        &mut bad,
        "a path from another catalog of the publisher",
        "leaf",
        &with(&|c| {
            c.catalog = deep.catalog.clone();
            c.payload = deep.payload.clone();
        }),
    );
    // The removed dataset: its version 1 against the catalog that holds its
    // tombstone, with the tombstone's path.
    let gone = large.find("synthetic.removed").expect("listed");
    let gone_proof = large.leaf_proof(gone).expect("proof");
    assert!(gone_proof.leaf.is_tombstone());
    refused(
        &mut bad,
        "a tombstone leaf",
        "leaf",
        &Case {
            header: header_bytes(&removed),
            catalog: deep.catalog.clone(),
            leaf_index: gone_proof.proof.index,
            siblings: gone_proof.proof.siblings,
            payload: deep.payload.clone(),
        },
    );
    // A version the catalog no longer lists (the path of its successor).
    let newer = Case::of(&wide2, &large);
    refused(
        &mut bad,
        "a replaced version",
        "leaf",
        &Case {
            header: header_bytes(&wide1),
            ..newer.clone()
        },
    );

    // Another publisher.
    for (name, other) in [
        (
            "another publisher's catalog (id of the same length)",
            OTHER_SAME_LEN,
        ),
        ("another publisher's catalog (id of another length)", OTHER),
    ] {
        let m = manifest(&main.header.dataset, other, t0, 2, None);
        let c = Catalog::build(
            SourceId(other.into()),
            None,
            t0,
            vec![CatalogLeaf::of(&m.header)],
        )
        .expect("their catalog");
        let theirs = Case::of(&m, &c);
        assert!(theirs.check().is_ok(), "their own deal reads");
        refused(
            &mut bad,
            name,
            "catalog",
            &with(&|x| {
                x.catalog = theirs.catalog.clone();
                x.leaf_index = theirs.leaf_index;
                x.siblings = theirs.siblings.clone();
                x.payload = theirs.payload.clone();
            }),
        );
        if other == OTHER_SAME_LEN {
            refused(
                &mut bad,
                "another publisher's manifest in this catalog",
                "catalog",
                &with(&|x| x.header = theirs.header.clone()),
            );
        }
    }

    // The catalog header's shape.
    let mut format2 = small.header.clone();
    format2.format = 2;
    refused(
        &mut bad,
        "catalog header of format 2",
        "catalog",
        &with(&|c| {
            c.catalog = catalog_bytes(&format2);
            c.payload = signed(&format2).0;
        }),
    );
    refused(
        &mut bad,
        "catalog header with one extra byte (not from an encoder)",
        "catalog",
        &with(&|c| c.catalog.push(0)),
    );
    refused(
        &mut bad,
        "catalog header one byte short (not from an encoder)",
        "catalog",
        &with(&|c| {
            c.catalog.pop();
        }),
    );
    refused(
        &mut bad,
        "an empty catalog header (not from an encoder)",
        "catalog",
        &with(&|c| {
            c.catalog.clear();
        }),
    );

    // The manifest header's shape.
    for f in [0u8, 3] {
        let mut m = main.clone();
        m.header.format = f;
        refused(
            &mut bad,
            &format!("manifest header of format {f}"),
            "header",
            &with(&|c| {
                c.header = header_bytes(&m);
            }),
        );
    }
    // The shortest header `createDeal` reads has no room for a version and a
    // previous root.
    let prefix = [1u8, 1, 0, 0, 0, b'd', 1, 0, 0, 0, b'p'];
    let tail = &good.header[good.header.len() - TAIL_BYTES..];
    let cut = |between: usize| {
        let mut b = prefix.to_vec();
        b.extend_from_slice(&vec![0u8; between]);
        b.extend_from_slice(tail);
        b
    };
    assert!(read_header(&cut(0)).is_ok());
    assert!(leaf_row(&cut(36)).is_ok());
    refused(
        &mut bad,
        "a header with no version and previous root (not from an encoder)",
        "header",
        &with(&|c| c.header = cut(0)),
    );
    refused(
        &mut bad,
        "a header one byte short of a version and previous root (not from an encoder)",
        "header",
        &with(&|c| c.header = cut(35)),
    );

    // The payload.
    let h = &small.header;
    let mut p = |name: &str, payload: Vec<u8>| {
        refused(
            &mut bad,
            name,
            "payload",
            &with(&|c| c.payload = payload.clone()),
        );
    };
    p(
        "payload with the manifest metric",
        payload_with(h, |p| {
            p.metric = MetricId(onyx_bulk::ANCHOR_METRIC.into());
        }),
    );
    p(
        "payload with another metric of the same length",
        payload_with(h, |p| {
            p.metric = MetricId("bulk.catalot".into());
        }),
    );
    p(
        "payload with another subject",
        payload_with(h, |p| {
            p.subject = Subject::Aggregate("catalot".into());
        }),
    );
    p(
        "payload with the dataset as subject",
        payload_with(h, |p| {
            p.subject = Subject::Aggregate(main.header.dataset.clone());
        }),
    );
    p(
        "payload with another catalog root",
        payload_with(h, |p| {
            p.value = SignedValue::Bytes(large.header.root().0.to_vec());
        }),
    );
    p(
        "payload with the manifest root as value",
        payload_with(h, |p| {
            p.value = SignedValue::Bytes(main.header.root().0.to_vec());
        }),
    );
    p(
        "payload with a later time",
        payload_with(h, |p| p.observed_unix += 1),
    );
    p(
        "payload with an earlier time",
        payload_with(h, |p| p.observed_unix -= 1),
    );
    p(
        "payload with the next catalog's sequence",
        payload_with(h, |p| {
            p.sequence = catalog::sequence_for_catalog(1);
        }),
    );
    p(
        "payload with sequence 0",
        payload_with(h, |p| p.sequence = 0),
    );
    p(
        "payload with the reserved sequence",
        payload_with(h, |p| p.sequence = u64::MAX),
    );
    p(
        "payload of another source",
        payload_with(h, |p| {
            p.source = SourceId(OTHER_SAME_LEN.into());
        }),
    );
    p("payload of the next catalog", deep.payload.clone());
    p("the manifest's own report", {
        let r = onyx_bulk::anchor::sign(&SECRET, &main.header).expect("signs");
        borsh::to_vec(&r.payload).expect("borsh")
    });
    p("payload with one extra byte (not from an encoder)", {
        let mut b = good.payload.clone();
        b.push(0);
        b
    });
    p(
        "payload one byte short (not from an encoder)",
        good.payload[..good.payload.len() - 1].to_vec(),
    );

    json!({
        "comment": "ADR-0067: createDealInCatalog. Every header, catalog and payload is the output of the onyx-bulk encoders; a refused case changes one thing in a good one.",
        "publisher": PUBLISHER,
        "signer": hx(&signer()),
        "catalogs": catalogs,
        "dealCount": deals.len(),
        "deals": deals,
        "call": {"comment": "deals[0] as the whole calldata of createDealInCatalog (onyx_storage::abi::create_deal_in_catalog)",
                 "deal": 0, "r": call_r, "epochs": call_epochs, "sourceUrl": call_url,
                 "calldata": hx(&call)},
        "lastCatalog": {"comment": "catalog 0's header with the largest number: the contract asks the book about the reserved sequence, which no report can use",
                        "header": hx(&catalog_bytes(&last)), "nextSequence": reserved},
        "refusedCount": bad.len(),
        "refused": bad,
    })
}

fn bytes_of(v: &Value, k: &str) -> Vec<u8> {
    let s = v[k].as_str().expect("hex string");
    hex::decode(s.trim_start_matches("0x")).expect("hex")
}

fn case_of(v: &Value) -> Case {
    Case {
        header: bytes_of(v, "header"),
        catalog: bytes_of(v, "catalog"),
        leaf_index: v["leafIndex"].as_u64().expect("index"),
        siblings: v["siblings"]
            .as_array()
            .expect("siblings")
            .iter()
            .map(|s| {
                let b = hex::decode(s.as_str().expect("hash").trim_start_matches("0x"));
                Hash32(b.expect("hex").try_into().expect("32 bytes"))
            })
            .collect(),
        payload: bytes_of(v, "payload"),
    }
}

/// The file on disk through the Rust mirror: the same reads the Solidity
/// tests make of it (`test_catalogDeals`, `test_catalogRefusals`).
#[test]
fn the_file_on_disk_passes_the_mirror() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/storage/vectors.json"
    );
    let text = std::fs::read_to_string(path).expect("fixtures/storage/vectors.json");
    let v: Value = serde_json::from_str(&text).expect("json");
    let c = &v["catalog"];
    let deals = c["deals"].as_array().expect("deals");
    assert_eq!(deals.len(), 5);
    assert_eq!(c["dealCount"], 5);
    for d in deals {
        let case = case_of(d);
        let (t, digest) = check_deal_in_catalog(
            &case.header,
            &case.catalog,
            case.leaf_index,
            &case.siblings,
            &case.payload,
        )
        .expect("a good case");
        assert_eq!(digest.to_string(), d["digest"]);
        assert_eq!(t.manifest_root.to_string(), d["manifestRoot"]);
        assert_eq!(hx(&leaf_row(&case.header).expect("row")), d["leafRow"]);
        assert_eq!(catalog_root(&case.catalog).to_string(), d["catalogRoot"]);
        assert_eq!(case.header[0], d["format"].as_u64().expect("format") as u8);
    }
    let bad = c["refused"].as_array().expect("refused");
    // The count is written in the file: a case lost on either side shows.
    assert_eq!(bad.len(), 37);
    assert_eq!(c["refusedCount"], 37);
    // The whole call decodes to the first deal's inputs.
    let call = bytes_of(&c["call"], "calldata");
    let first = case_of(&deals[0]);
    assert_eq!(
        call,
        crate::abi::create_deal_in_catalog(
            &first.header,
            &first.catalog,
            first.leaf_index,
            &first.siblings,
            &first.payload,
            3,
            4,
            "http://127.0.0.1:2/"
        )
    );
    assert_eq!(
        crate::abi::dyn_bytes(&call[4..], 0).expect("header"),
        first.header
    );
    assert_eq!(
        crate::abi::dyn_words(&call[4..], 3).expect("path"),
        first.siblings
    );
    // The sequence the contract asks the book about, per catalog.
    for k in c["catalogs"].as_array().expect("catalogs") {
        let n = k["number"].as_u64().expect("number");
        assert_eq!(
            k["nextSequence"],
            next_catalog_sequence(n).expect("next").to_string()
        );
        assert_eq!(k["nextSequence"], catalog_sequence(n + 1).to_string());
    }
    assert_eq!(c["lastCatalog"]["nextSequence"], u64::MAX.to_string());
    for b in bad {
        let e = case_of(b).check().expect_err("a refused case");
        assert_eq!(kind(&e), b["error"], "{}", b["name"]);
    }
    // Every kind of refusal is there.
    for k in ["header", "catalog", "leaf", "payload"] {
        assert!(bad.iter().any(|b| b["error"] == k), "{k}");
    }
}

/// A format-2 header reads like a format-1 header: the same first fields and
/// the same tail.
#[test]
fn a_format_2_header_reads() {
    let w1 = manifest("synthetic.widened", PUBLISHER, 100, 2, None);
    let w2 = manifest("synthetic.widened", PUBLISHER, 200, 4, Some(&w1));
    let hb = header_bytes(&w2);
    assert_eq!(hb[0], FORMAT_2);
    let t = read_header(&hb).expect("reads");
    assert_eq!(t.manifest_root, w2.header.root());
    assert_eq!(t.shard_count, 3);
    assert_eq!(t.shards_root, w2.header.shards_root);
    assert_eq!(t.publisher, PUBLISHER.as_bytes());
    assert_eq!(
        leaf_row(&hb).expect("row"),
        CatalogLeaf::of(&w2.header).row()
    );
    let mut f3 = hb.clone();
    f3[0] = 3;
    assert_eq!(read_header(&f3), Err(DealError::Header("format")));
}

/// The sequence of a catalog number, written out as the contract computes
/// it, equals the publisher's rule for many numbers.
#[test]
fn the_sequence_matches_the_publishers_rule() {
    for n in (0..2_000u64).chain([u64::MAX - 1, u64::MAX, 1 << 40]) {
        assert_eq!(catalog_sequence(n), catalog::sequence_for_catalog(n));
    }
}
