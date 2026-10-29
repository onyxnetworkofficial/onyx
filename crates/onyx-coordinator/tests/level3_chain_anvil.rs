//! The chain book on a local anvil (ADR-0028 amendment 6, THREATS L3-F7,
//! L3R-F8): two coordinators with their own `SignedSources` stores each
//! accept one side of an equivocation locally, post to one `OnyxSignedBook`,
//! and every client reading the chain sees the fact disputed; a clean fact
//! becomes settleable only after the dispute window of block time.
//!
//! Gated behind `ONYX_SIGNED_BOOK_ANVIL=1`: it needs Foundry's `anvil` and
//! `cast` (in `$FOUNDRY_BIN` or `~/.foundry/bin`), a free local port
//! (`ONYX_SIGNED_BOOK_ANVIL_PORT`, default 18557) and the contract built
//! (`cd contracts && forge build`). It starts its own anvil and stops it.
//!
//! ```text
//! (cd contracts && forge build)
//! ONYX_SIGNED_BOOK_ANVIL=1 cargo test -p onyx-coordinator --test level3_chain_anvil
//! ```

use onyx_coordinator::signed::{signed_fact_key, SignedError, SignedSources};
use onyx_coordinator::signed_chain::{PostOutcome, SignedBookCast};
use onyx_core::signed::REPORT_EVIDENCE;
use onyx_core::{
    keccak256, Claim, DataRights, EntityKind, EvidenceRef, Hash32, KeyProvenance, MetricId,
    MetricInfo, Redistribution, SignedPayload, SignedReport, Snapshot, SourceId, SourceKey,
    SourceRef, StaticRegistry, Subject, Value, ValueKind, VerificationMethod,
};
use onyx_labels::attest::eip191_digest;
use secp256k1::{Message, Secp256k1, SecretKey};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

const SOURCE: &str = "example.signed";
const ORIGIN: &str = "data.example.org";
const METRIC: &str = "example.signed.reserve";

fn sk(label: &str) -> SecretKey {
    SecretKey::from_slice(&keccak256(label.as_bytes()).0).expect("test key")
}

fn address(sk: &SecretKey) -> [u8; 20] {
    let pk = secp256k1::PublicKey::from_secret_key(&Secp256k1::new(), sk);
    let h = keccak256(&pk.serialize_uncompressed()[1..]);
    let mut a = [0u8; 20];
    a.copy_from_slice(&h.0[12..]);
    a
}

fn report(label: &str, p: SignedPayload) -> SignedReport {
    let digest = Message::from_digest(eip191_digest(p.message().as_bytes()).0);
    let (rid, bytes) = Secp256k1::new()
        .sign_ecdsa_recoverable(&digest, &sk(label))
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
        source: SourceId(SOURCE.into()),
        attribution: None,
        terms: String::new(),
    }
}

fn reg() -> StaticRegistry {
    let mut r = StaticRegistry::new();
    r.add_metric(
        METRIC,
        MetricInfo {
            value_kind: ValueKind::UInt,
            needs_label_set: false,
        },
    )
    .and_then(|r| r.add_entity("entity:example", EntityKind::Exchange))
    .expect("registry");
    r.add_source(rights()).expect("source");
    r
}

fn source_key(label: &str) -> SourceKey {
    SourceKey {
        source: SourceId(SOURCE.into()),
        signer: address(&sk(label)),
        provenance: KeyProvenance::WellKnown {
            origin: ORIGIN.into(),
            document_hash: keccak256(b"keys document"),
            fetched_unix: 1,
        },
    }
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

fn payload(key: Hash32, value: u128, observed: u64, sequence: u64) -> SignedPayload {
    SignedPayload {
        source: SourceId(SOURCE.into()),
        key,
        metric: MetricId(METRIC.into()),
        subject: Subject::Entity("entity:example".into()),
        value: Value::UInt(value),
        observed_unix: observed,
        sequence,
    }
}

/// Stops the anvil this test started, whatever happens.
struct Anvil(Child);

impl Drop for Anvil {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn foundry(bin: &str) -> PathBuf {
    std::env::var_os("FOUNDRY_BIN")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".foundry/bin")))
        .expect("FOUNDRY_BIN or HOME")
        .join(bin)
}

fn ax(a: &[u8; 20]) -> String {
    format!("0x{}", hex::encode(a))
}

fn parse_addr(s: &str) -> [u8; 20] {
    let mut a = [0u8; 20];
    hex::decode_to_slice(s.trim().trim_matches('"').trim_start_matches("0x"), &mut a)
        .expect("address");
    a
}

#[test]
fn two_coordinators_meet_in_the_chain_book() {
    if std::env::var("ONYX_SIGNED_BOOK_ANVIL").as_deref() != Ok("1") {
        eprintln!("skipped: set ONYX_SIGNED_BOOK_ANVIL=1 (needs anvil, cast and a built contract)");
        return;
    }
    let repo = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let artifact = repo.join("contracts/out/OnyxSignedBook.sol/OnyxSignedBook.json");
    let art: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&artifact).expect("contract artifact: run forge build first"),
    )
    .expect("artifact json");
    let bytecode = art["bytecode"]["object"]
        .as_str()
        .expect("bytecode")
        .to_string();
    let port = std::env::var("ONYX_SIGNED_BOOK_ANVIL_PORT").unwrap_or_else(|_| "18557".into());
    let work = std::env::temp_dir().join(format!("onyx-signed-book-{}", std::process::id()));
    std::fs::create_dir_all(&work).expect("work dir");
    let _anvil = Anvil(
        Command::new(foundry("anvil"))
            .current_dir(&work)
            .args(["--port", &port, "--silent"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start anvil"),
    );
    let rpc = format!("http://127.0.0.1:{port}");
    let mut chain = SignedBookCast {
        cast: foundry("cast"),
        rpc: rpc.clone(),
        work: work.clone(),
        book: [0; 20],
    };
    let cast = |args: &[&str]| -> Result<String, String> {
        let out = Command::new(foundry("cast"))
            .current_dir(&work)
            .args(args)
            .env("ETH_RPC_URL", &rpc)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    };
    let mut up = false;
    for _ in 0..100 {
        if cast(&["chain-id"]).is_ok() {
            up = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(up, "anvil did not start on port {port}");
    let accounts: Vec<String> =
        serde_json::from_str(&cast(&["rpc", "eth_accounts"]).expect("accounts")).expect("json");
    let [deployer, reviewer, coord_a, coord_b] = [0, 1, 2, 3].map(|i| parse_addr(&accounts[i]));

    // Deploy with the reviewer as the constructor argument.
    let args = cast(&["abi-encode", "constructor(address)", &ax(&reviewer)]).expect("args");
    let create = format!("{bytecode}{}", args.trim_start_matches("0x"));
    let out = cast(&[
        "send",
        "--unlocked",
        "--from",
        &ax(&deployer),
        "--create",
        &create,
        "--json",
    ])
    .expect("deploy");
    let receipt: serde_json::Value = serde_json::from_str(&out).expect("receipt");
    chain.book = parse_addr(receipt["contractAddress"].as_str().expect("address"));

    // The reviewer registers three keys on chain; each coordinator's store
    // holds the same registry.
    let registry = reg();
    let mut stores: Vec<SignedSources> = Vec::new();
    let mut ids = Vec::new();
    let t0 = chain.block_time("latest").expect("block time");
    for _ in 0..2 {
        let (mut s, admin) = SignedSources::with_reviewer("reviewer");
        ids = ["K0", "K1", "K2"]
            .iter()
            .map(|l| {
                s.register_key(&admin, source_key(l), &registry, t0, "setup")
                    .expect("register")
            })
            .collect();
        stores.push(s);
    }
    for l in ["K0", "K1", "K2"] {
        chain
            .send(
                &reviewer,
                "registerKey(string,address,string,bytes32,string)",
                &[
                    SOURCE.into(),
                    ax(&address(&sk(l))),
                    ORIGIN.into(),
                    format!("0x{}", hex::encode(keccak256(b"keys document").0)),
                    "setup".into(),
                ],
            )
            .expect("registerKey");
    }

    // L3-F7: coordinator A sees only X, coordinator B only Y (same fact,
    // different values). Each store accepts its side locally.
    let now = chain.block_time("latest").expect("block time");
    let x = report("K0", payload(ids[0], 1, now - 60, 1));
    let y = report("K1", payload(ids[1], 2, now - 60, 1));
    assert_eq!(signed_fact_key(&x.payload), signed_fact_key(&y.payload));
    for (s, r) in stores.iter_mut().zip([&x, &y]) {
        s.accept_claim(&claim_for(r), r, &registry, now)
            .expect("each store alone accepts its side");
    }
    // On chain they meet.
    assert_eq!(chain.post_report(&coord_a, &x), Ok(PostOutcome::Accepted));
    assert_eq!(
        chain.post_report(&coord_b, &y),
        Ok(PostOutcome::Equivocation)
    );
    // A replay through the other coordinator is refused and changes nothing.
    assert!(chain.post_report(&coord_b, &x).is_err());

    // A clean fact from the third key.
    let z = report("K2", payload(ids[2], 7, now - 120, 1));
    assert_eq!(chain.post_report(&coord_b, &z), Ok(PostOutcome::Accepted));
    assert!(
        chain.check_settleable(&z, "latest").is_err(),
        "inside the window"
    );

    // L3R-F8: the window is block time; nobody's `now` moves it.
    cast(&["rpc", "evm_increaseTime", "3600"]).expect("increase time");
    cast(&["rpc", "evm_mine"]).expect("mine");
    let later = chain.block_time("latest").expect("block time");
    assert!(later >= now + 3_600, "block time moved: {now} -> {later}");
    assert_eq!(chain.check_settleable(&z, "latest"), Ok(()));
    // Every client sees the equivocated fact without a value, whichever
    // side its coordinator accepted.
    assert!(chain.check_settleable(&x, "latest").is_err());
    assert!(chain.check_settleable(&y, "latest").is_err());
    // A report from the future by block time is refused.
    let ahead = report("K2", payload(ids[2], 9, later + 400, 2));
    assert!(chain.post_report(&coord_a, &ahead).is_err());
    // A coordinator's own store refuses a step back of its clock.
    let back = report("K2", payload(ids[2], 9, now - 30, 3));
    assert!(matches!(
        stores[0].accept_claim(&claim_for(&back), &back, &registry, now - 10),
        Err(SignedError::ClockWentBack { .. })
    ));
    let _ = std::fs::remove_dir_all(&work);
}
