//! ROADMAP C50 interop: ciphertexts made by drand/tlock (the Go reference,
//! Kudelski-reviewed) and tlock-js decrypt under published drand
//! signatures, prove to be the age file key (header MAC), and re-encrypt to
//! identical bytes; published quicknet rounds verify under the pinned key;
//! Onyx's own vectors with `tau` ending in zero bytes re-create byte for byte
//! (the same checks run on wasm32: `scripts/check-tlock-wasm.sh`).
//!
//! Regenerate `fixtures/tlock/onyx-zero-tail.json` only with a scheme change:
//! `ONYX_BLESS=1 cargo test -p onyx-tlock --test interop`.

mod kat;

use onyx_tlock::{BeaconKey, QUICKNET_CHAIN_HASH, QUICKNET_PUBLIC_KEY};
use serde_json::{json, Value};

fn manifest() -> Value {
    serde_json::from_str(kat::file("vectors.json").expect("fixture")).expect("fixture")
}

fn interop(i: usize) -> Value {
    manifest()["interop"][i].clone()
}

#[test]
fn c50_every_fixture_check_passes_with_the_expected_counts() {
    assert_eq!(kat::run_all().expect("fixture"), kat::EXPECTED);
}

#[test]
fn c50_go_tlock_lorem_quicknet_t_opens_and_re_encrypts() {
    let v = interop(0);
    assert!(v["file"]
        .as_str()
        .expect("fixture")
        .starts_with("go-tlock/"));
    let o = kat::check_interop(&v).expect("fixture");
    assert_ne!(o.msg, [0u8; 16]);
}

#[test]
fn c50_go_tlock_interop_quicknet_t_opens_and_re_encrypts() {
    kat::check_interop(&interop(1)).expect("fixture");
}

#[test]
fn c50_go_made_quicknet_vector_opens_under_the_pinned_key() {
    let v = interop(2);
    assert_eq!(v["chain"], "quicknet");
    kat::check_interop(&v).expect("fixture");
    // The fixture's chain is the pinned one, byte for byte.
    let (key, hash) = kat::chain("quicknet").expect("fixture");
    assert_eq!(key.to_compressed(), QUICKNET_PUBLIC_KEY);
    assert_eq!(hash, hex::encode(QUICKNET_CHAIN_HASH));
    let f = kat::parse_age(kat::file(v["file"].as_str().expect("fixture")).expect("fixture"))
        .expect("fixture");
    let b = kat::beacon("quicknet", v["beacon"].as_str().expect("fixture")).expect("fixture");
    let pinned = BeaconKey::quicknet().expect("fixture");
    let o = pinned
        .open(f.round, &b.signature, &f.ciphertext)
        .expect("fixture");
    assert_eq!(
        pinned
            .lock(f.round, &o.msg, &o.sigma)
            .expect("fixture")
            .to_vec(),
        f.ciphertext
    );
}

#[test]
fn c50_a_wrong_file_key_fails_the_age_header_mac() {
    // The MAC check is what proves the opening is age's file key; it must
    // not pass for a neighbour.
    let v = interop(0);
    let f = kat::parse_age(kat::file(v["file"].as_str().expect("fixture")).expect("fixture"))
        .expect("fixture");
    let o = kat::check_interop(&v).expect("fixture");
    assert!(kat::age_mac_checks(&f, &o.msg));
    let mut k = o.msg;
    k[0] ^= 1;
    assert!(!kat::age_mac_checks(&f, &k));
}

#[test]
fn c50_a_quicknet_t_vector_does_not_open_under_quicknet() {
    // Another chain's ciphertext and signature: the pinned chain's key
    // refuses the signature, and the bytes do not open with its own round
    // signature either.
    let v = interop(0);
    let f = kat::parse_age(kat::file(v["file"].as_str().expect("fixture")).expect("fixture"))
        .expect("fixture");
    let b = kat::beacon("quicknet-t", v["beacon"].as_str().expect("fixture")).expect("fixture");
    let q = BeaconKey::quicknet().expect("fixture");
    assert!(!q.verify(b.round, &b.signature));
    assert!(q.open(b.round, &b.signature, &f.ciphertext).is_err());
    let real = kat::beacon("quicknet", "drand/quicknet-round-746511.json").expect("fixture");
    assert!(q.open(real.round, &real.signature, &f.ciphertext).is_err());
}

#[test]
fn c50_the_zero_tail_vectors_end_in_zero_bytes() {
    let zt: Value =
        serde_json::from_str(kat::file("onyx-zero-tail.json").expect("fixture")).expect("fixture");
    let vs = zt["vectors"].as_array().expect("fixture");
    assert_eq!(vs.len(), kat::EXPECTED.zero_tail);
    for v in vs {
        let m = hex::decode(v["msg"].as_str().expect("fixture")).expect("fixture");
        assert_eq!(m[15], 0, "every zero-tail tau ends in a zero byte");
        kat::check_zero_tail(v).expect("fixture");
    }
}

/// ROADMAP C52: the far-date vectors (made by onyx-coordinator) check on this
/// target as on wasm32, and a vector whose sigma, body key or ciphertext is
/// changed by one bit does not (so the check is no constant pass).
#[test]
fn c52_far_date_vectors_check_and_a_changed_one_fails() {
    let fd: Value =
        serde_json::from_str(kat::file("onyx-far-date.json").expect("fixture")).expect("fixture");
    let vs = fd["vectors"].as_array().expect("fixture");
    assert_eq!(vs.len(), kat::EXPECTED.far_date);
    for v in vs {
        kat::check_far_date(v).expect("fixture");
        for field in ["sigma", "kappa", "ciphertext"] {
            let mut b = hex::decode(v[field].as_str().expect("fixture")).expect("fixture");
            b[3] ^= 1;
            let mut bad = v.clone();
            bad[field] = Value::from(hex::encode(b));
            assert!(kat::check_far_date(&bad).is_err(), "{field} changed");
        }
    }
}

/// The zero-tail vectors: `tau` ending in 1, 2, 8, 15 and 16 zero bytes
/// (and `sigma` with zero tails too), each locked to a published quicknet
/// round so it opens under the real signature.
fn zero_tail_cases() -> Vec<(u64, &'static str, [u8; 16], [u8; 16])> {
    let tail = |fill: u8, zeros: usize| {
        let mut m = [fill; 16];
        m[16 - zeros..].fill(0);
        m
    };
    vec![
        (1, "drand/quicknet-round-1.json", tail(0x11, 1), [0x21; 16]),
        (2, "drand/quicknet-round-2.json", tail(0x12, 2), [0x22; 16]),
        (
            1000,
            "drand/quicknet-round-1000.json",
            tail(0x13, 8),
            [0x23; 16],
        ),
        (
            746_511,
            "drand/quicknet-round-746511.json",
            tail(0x14, 15),
            tail(0x24, 1),
        ),
        (
            33_154_000,
            "drand/quicknet-round-33154000.json",
            tail(0x15, 16),
            [0x25; 16],
        ),
        (
            1,
            "drand/quicknet-round-1.json",
            tail(0xff, 1),
            tail(0x26, 16),
        ),
        (
            1000,
            "drand/quicknet-round-1000.json",
            tail(0x80, 4),
            tail(0x27, 8),
        ),
        (
            33_154_000,
            "drand/quicknet-round-33154000.json",
            tail(0x01, 3),
            [0x28; 16],
        ),
    ]
}

#[test]
fn c50_zero_tail_vectors_are_what_this_implementation_makes() {
    let k = BeaconKey::quicknet().expect("fixture");
    let vectors: Vec<Value> = zero_tail_cases()
        .into_iter()
        .map(|(round, beacon, msg, sigma)| {
            json!({
                "round": round,
                "beacon": beacon,
                "msg": hex::encode(msg),
                "sigma": hex::encode(sigma),
                "ciphertext": hex::encode(k.lock(round, &msg, &sigma).expect("fixture")),
            })
        })
        .collect();
    let doc = json!({
        "about": "Made by onyx-tlock (NOT by drand/tlock): tau (msg) ending in zero bytes, each locked to a published quicknet round in fixtures/tlock/drand and opened with its real signature. Cross-target consistency vectors (native and wasm32), not interop vectors. Regenerate: ONYX_BLESS=1 cargo test -p onyx-tlock --test interop.",
        "vectors": vectors,
    });
    let text = serde_json::to_string_pretty(&doc).expect("fixture") + "\n";
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/tlock/onyx-zero-tail.json"
    );
    if std::env::var("ONYX_BLESS").as_deref() == Ok("1") {
        std::fs::write(path, &text).expect("fixture");
    }
    assert_eq!(std::fs::read_to_string(path).expect("fixture"), text);
}
