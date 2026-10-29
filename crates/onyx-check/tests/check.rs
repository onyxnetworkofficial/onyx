//! `onyx-check` (NS8, ADR-0047) on answers recorded from the demo server
//! (`serve-api --dev`, the golden BTC fixture shard), against a stand-in for
//! the caller's chain node. THREATS AGT-1..AGT-4.
#![allow(clippy::unwrap_used)]

use onyx_api::roots::JsonRpc;
use onyx_api::x402::BoxFuture;
use onyx_check::{check_offer, check_on, check_unchecked, parse_asked, Asked, Outcome};
use serde_json::{json, Value};
use std::process::Command;

const SHARD: &str = "0xacbcbfa203b99d9cba691cc1958b3c6563f04f8eab94db5a0402597b1ca34244";
const ROOT: &str = "0xf912872a71710d45d000e4c2c3771e7f31e51e0fe3d0bad0557d8fedca3d86b2";
const BINDING: &str = "0x12215c14d34fd6b947582fd3dde75b7e6d018094d5d2610a8466bec26c88a985";
const CHAIN: u64 = 84_532;
const CONTRACT: [u8; 20] = [0x11; 20];

fn fixture(name: &str) -> Value {
    let p = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn proof_asked(index: u64) -> Asked {
    parse_asked(&json!({"kind": "proof", "round": 1, "shard": SHARD, "index": index})).unwrap()
}

/// A stand-in for the caller's own node: its chain id and its
/// `getFinalizedRoot` answer (root, binding, finalizedAt), or an outage.
struct Node {
    chain: u64,
    answer: Option<(String, String, u64)>,
    down: bool,
}

impl JsonRpc for Node {
    fn call<'a>(&'a self, method: &'a str, _p: Value) -> BoxFuture<'a, Result<Value, String>> {
        let r = if self.down {
            Err("connection refused".to_string())
        } else {
            match method {
                "eth_chainId" => Ok(json!(format!("0x{:x}", self.chain))),
                "eth_call" => {
                    let mut ret = vec![0u8; 96];
                    if let Some((root, binding, at)) = &self.answer {
                        ret[..32].copy_from_slice(&hex::decode(&root[2..]).unwrap());
                        ret[32..64].copy_from_slice(&hex::decode(&binding[2..]).unwrap());
                        ret[88..].copy_from_slice(&at.to_be_bytes());
                    }
                    Ok(json!(format!("0x{}", hex::encode(ret))))
                }
                m => Err(m.to_string()),
            }
        };
        Box::pin(async move { r })
    }
}

fn final_node() -> Node {
    Node {
        chain: CHAIN,
        answer: Some((ROOT.into(), BINDING.into(), 400)),
        down: false,
    }
}

fn on(answer: &Value, asked: &Asked, node: Node) -> Outcome {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(check_on(answer, asked, node, CHAIN, CONTRACT))
}

#[test]
fn verified_when_the_callers_node_has_the_root_final() {
    let o = on(&fixture("proof"), &proof_asked(0), final_node());
    assert_eq!(o.code(), 0);
    let r = o.report();
    assert_eq!(r["ok"], true);
    assert_eq!(r["chainChecked"], true);
    assert_eq!(r["root"], ROOT);
    assert_eq!(r["binding"], BINDING);
    assert!(r.get("warning").is_none());
}

#[test]
fn every_answer_kind_verifies() {
    let export = parse_asked(&json!({"kind": "export", "round": 1, "shard": SHARD})).unwrap();
    assert_eq!(on(&fixture("export"), &export, final_node()).code(), 0);
    let settle = parse_asked(
        &json!({"kind": "settlement", "market": "okx-net-flow-921080-921082",
        "round": 1, "shard": SHARD, "subject": "entity:okx"}),
    )
    .unwrap();
    assert_eq!(on(&fixture("settle"), &settle, final_node()).code(), 0);
}

/// AGT-1: the operator's node lies about the root; only the caller's own read catches it.
#[test]
fn another_root_on_the_callers_node_is_refused() {
    let node = Node {
        answer: Some((format!("0x{}", "22".repeat(32)), BINDING.into(), 400)),
        ..final_node()
    };
    let r = on(&fixture("proof"), &proof_asked(0), node).report();
    assert_eq!(
        (r["ok"].clone(), r["code"].clone()),
        (json!(false), json!("chain_mismatch"))
    );
}

#[test]
fn a_root_not_final_is_refused_and_may_be_retried() {
    let node = Node {
        answer: None,
        ..final_node()
    };
    let o = on(&fixture("proof"), &proof_asked(0), node);
    assert_eq!(o.code(), 1);
    let r = o.report();
    assert_eq!(r["code"], "not_final_on_chain");
    assert_eq!(r["retryLater"], true);
}

/// An unreadable node decides nothing: exit 3, never a pass.
#[test]
fn an_unreadable_node_is_undecided() {
    let node = Node {
        down: true,
        ..final_node()
    };
    let o = on(&fixture("proof"), &proof_asked(0), node);
    assert_eq!(o.code(), 3);
    assert_eq!(o.report()["decided"], false);
}

/// AGT-2: the opt-out says so in the report.
#[test]
fn unchecked_says_not_verified_on_chain() {
    let r = check_unchecked(&fixture("proof"), &proof_asked(0)).report();
    assert_eq!(r["ok"], true);
    assert_eq!(r["chainChecked"], false);
    assert!(r["warning"]
        .as_str()
        .unwrap()
        .contains("NOT VERIFIED ON CHAIN"));
}

#[test]
fn a_changed_row_or_another_rows_proof_is_refused() {
    let mut bad = fixture("proof");
    let row = bad["row"].as_str().unwrap().to_string();
    let flipped = if row.ends_with('0') { '1' } else { '0' };
    bad["row"] = json!(format!("{}{flipped}", &row[..row.len() - 1]));
    assert_eq!(
        on(&bad, &proof_asked(0), final_node()).report()["code"],
        "proof_invalid"
    );
    // A valid proof, but of row 0 when row 1 was asked for.
    assert_eq!(
        on(&fixture("proof"), &proof_asked(1), final_node()).report()["code"],
        "not_requested"
    );
}

#[test]
fn a_settlement_for_another_subject_is_refused() {
    let asked = parse_asked(
        &json!({"kind": "settlement", "market": "okx-net-flow-921080-921082",
        "round": 1, "shard": SHARD, "subject": "entity:binance"}),
    )
    .unwrap();
    assert_eq!(on(&fixture("settle"), &asked, final_node()).code(), 1);
}

/// AGT-3: a typo in the question is an error, never a dropped condition.
#[test]
fn questions_are_parsed_strictly() {
    for bad in [
        json!({"kind": "proof", "round": 1, "shard": SHARD, "index": 0, "subject": "x"}),
        json!({"kind": "proof", "round": 1, "shard": "0x12", "index": 0}),
        json!({"kind": "proof", "round": -1, "shard": SHARD, "index": 0}),
        json!({"kind": "rows", "round": 1, "shard": SHARD}),
        json!({"kind": "metric", "metric": "m", "subject": "s", "def": {}}),
    ] {
        assert!(parse_asked(&bad).is_err(), "{bad}");
    }
}

fn cli(args: &[&str]) -> (i32, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_onyx-check"))
        .args(args)
        .output()
        .unwrap();
    let report = serde_json::from_slice(&out.stdout).unwrap();
    (out.status.code().unwrap(), report)
}

#[test]
fn the_command_needs_a_node_or_an_explicit_opt_out() {
    let answer = format!("{}/tests/fixtures/proof.json", env!("CARGO_MANIFEST_DIR"));
    let asked = json!({"kind": "proof", "round": 1, "shard": SHARD, "index": 0}).to_string();
    let (code, r) = cli(&["--answer", &answer, "--asked", &asked]);
    assert_eq!((code, r["code"].clone()), (2, json!("usage")));
    let c = format!("0x{}", "11".repeat(20));
    let (code, _) = cli(&[
        "--answer",
        &answer,
        "--asked",
        &asked,
        "--unchecked",
        "--rpc",
        "https://x",
        "--chain-id",
        "1",
        "--contract",
        &c,
    ]);
    assert_eq!(code, 2);
    let (code, r) = cli(&["--answer", &answer, "--asked", &asked, "--unchecked"]);
    assert_eq!((code, r["chainChecked"].clone()), (0, json!(false)));
}

/// AGT-4: plain http to a remote node is never sent (the transport's rule);
/// the command refuses it as a usage error, not a node that may come back.
#[test]
fn plain_http_to_a_remote_node_is_not_used() {
    let answer = format!("{}/tests/fixtures/proof.json", env!("CARGO_MANIFEST_DIR"));
    let asked = json!({"kind": "proof", "round": 1, "shard": SHARD, "index": 0}).to_string();
    let c = format!("0x{}", "11".repeat(20));
    let (code, r) = cli(&[
        "--answer",
        &answer,
        "--asked",
        &asked,
        "--rpc",
        "http://192.0.2.1:8545",
        "--chain-id",
        "84532",
        "--contract",
        &c,
    ]);
    assert_eq!((code, r["code"].clone()), (2, json!("usage")));
}

/// A node whose contract gives no `getFinalizedRoot` answer (`0x`, an
/// account with no code): the caller's setting, not a lying server.
struct NoContract;

impl JsonRpc for NoContract {
    fn call<'a>(&'a self, method: &'a str, _p: Value) -> BoxFuture<'a, Result<Value, String>> {
        let r = match method {
            "eth_chainId" => Ok(json!(format!("0x{CHAIN:x}"))),
            _ => Ok(json!("0x")),
        };
        Box::pin(async move { r })
    }
}

/// Review NS8-R1 finding 5: the caller's wrong settings are undecided, with no retry.
#[test]
fn wrong_node_settings_are_named_as_settings() {
    let node = Node {
        chain: 1,
        ..final_node()
    };
    let o = on(&fixture("proof"), &proof_asked(0), node);
    assert_eq!(o.code(), 3);
    let r = o.report();
    assert_eq!(
        (
            r["code"].clone(),
            r["decided"].clone(),
            r["retryLater"].clone()
        ),
        (json!("node_config"), json!(false), json!(false))
    );
    let o = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(check_on(
            &fixture("proof"),
            &proof_asked(0),
            NoContract,
            CHAIN,
            CONTRACT,
        ));
    assert_eq!(
        (o.code(), o.report()["code"].clone()),
        (3, json!("node_config"))
    );
}

// ---------------------------------------------------------------- offers (AGT-9)

const REVENUE: &str = "0x5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e";
const USDC: &str = "0x036cbd53842c5426634e7929541ec2318f3dcf7e";
const CODE: &[u8] = b"\x60\x80\x60\x40";

/// The caller's node for an offer check: its chain and the code at every address.
struct CodeNode {
    chain: u64,
    code: Vec<u8>,
}

impl JsonRpc for CodeNode {
    fn call<'a>(&'a self, method: &'a str, _p: Value) -> BoxFuture<'a, Result<Value, String>> {
        let r = match method {
            "eth_chainId" => Ok(json!(format!("0x{:x}", self.chain))),
            "eth_getCode" => Ok(json!(format!("0x{}", hex::encode(&self.code)))),
            m => Err(m.to_string()),
        };
        Box::pin(async move { r })
    }
}

fn manifest(chain: u64) -> Value {
    json!({"chainId": chain, "revenue": REVENUE, "usdc": USDC,
           "revenueCodeHash": onyx_core::keccak256(CODE).to_string()})
}

fn offer(pay_to: &str, network: &str, amount: &str) -> Value {
    json!({"x402Version": 2, "resource": {"url": "http://127.0.0.1:8402/v1/proofs/1/x/0"},
           "accepts": [{"scheme": "exact", "network": network, "amount": amount, "asset": USDC,
                        "payTo": pay_to, "maxTimeoutSeconds": 60}]})
}

fn offer_check(offer: &Value, manifest: &Value, max: u128, node: CodeNode) -> Value {
    let o = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(check_offer(offer, manifest, max, &node));
    let mut r = o.report();
    r["exit"] = json!(o.code());
    r
}

fn sepolia() -> CodeNode {
    CodeNode {
        chain: CHAIN,
        code: CODE.to_vec(),
    }
}

/// Review NS8-R1 finding 7: a server cannot steer a payment.
#[test]
fn an_offer_is_payable_only_to_the_pinned_contract_within_the_limit() {
    let r = offer_check(
        &offer(REVENUE, "eip155:84532", "2500"),
        &manifest(CHAIN),
        10_000,
        sepolia(),
    );
    assert_eq!(
        (r["ok"].clone(), r["exit"].clone()),
        (json!(true), json!(0))
    );
    assert_eq!(r["pay"]["amount"], "2500");
    // The operator's own wallet as payTo.
    let r = offer_check(
        &offer(&format!("0x{}", "99".repeat(20)), "eip155:84532", "2500"),
        &manifest(CHAIN),
        10_000,
        sepolia(),
    );
    assert_eq!(
        (r["code"].clone(), r["exit"].clone()),
        (json!("not_pinned"), json!(1))
    );
    // Another network.
    let r = offer_check(
        &offer(REVENUE, "eip155:8453", "2500"),
        &manifest(CHAIN),
        10_000,
        sepolia(),
    );
    assert_eq!(r["code"], "not_pinned");
    // More than the caller allows.
    let r = offer_check(
        &offer(REVENUE, "eip155:84532", "10001"),
        &manifest(CHAIN),
        10_000,
        sepolia(),
    );
    assert_eq!(r["code"], "over_limit");
    // Not an offer.
    let r = offer_check(&json!({"accepts": 1}), &manifest(CHAIN), 10_000, sepolia());
    assert_eq!(r["code"], "malformed_offer");
}

#[test]
fn an_offer_is_not_checked_on_a_mainnet_or_a_node_with_other_code() {
    // A manifest for Base mainnet: this project pays on testnets only.
    let r = offer_check(
        &offer(REVENUE, "eip155:8453", "2500"),
        &manifest(8453),
        10_000,
        CodeNode {
            chain: 8453,
            code: CODE.to_vec(),
        },
    );
    assert_eq!(
        (r["code"].clone(), r["exit"].clone()),
        (json!("manifest"), json!(3))
    );
    // Other code at the pinned address.
    let r = offer_check(
        &offer(REVENUE, "eip155:84532", "2500"),
        &manifest(CHAIN),
        10_000,
        CodeNode {
            chain: CHAIN,
            code: vec![0xfe],
        },
    );
    assert_eq!(
        (r["code"].clone(), r["decided"].clone()),
        (json!("node_config"), json!(false))
    );
}

#[test]
fn the_offer_mode_needs_a_limit_a_manifest_and_a_node() {
    let (code, r) = cli(&["--offer", "-", "--manifest", "m.json", "--rpc", "https://x"]);
    assert_eq!((code, r["code"].clone()), (2, json!("usage")));
    let (code, _) = cli(&["--offer", "-", "--manifest", "m.json", "--max-amount", "1"]);
    assert_eq!(code, 2);
    let (code, _) = cli(&[
        "--offer",
        "-",
        "--manifest",
        "m.json",
        "--max-amount",
        "1",
        "--rpc",
        "https://x",
        "--unchecked",
    ]);
    assert_eq!(code, 2);
}

/// THREATS NS5-6, NS5-14 (ROADMAP C64): the caller's node also answers
/// `levelOf(round, shard)` on its registry.
struct LevelNode {
    root: Node,
    level: u8,
}

fn level_selector() -> Vec<u8> {
    onyx_core::keccak256(b"levelOf(uint64,bytes32)").0[..4].to_vec()
}

impl JsonRpc for LevelNode {
    fn call<'a>(&'a self, method: &'a str, p: Value) -> BoxFuture<'a, Result<Value, String>> {
        let data = p[0]["data"].as_str().unwrap_or_default().to_string();
        if method == "eth_call" && data.starts_with(&format!("0x{}", hex::encode(level_selector())))
        {
            let mut w = [0u8; 32];
            w[31] = self.level;
            let r = Ok(json!(format!("0x{}", hex::encode(w))));
            return Box::pin(async move { r });
        }
        self.root.call(method, p)
    }
}

const REGISTRY: [u8; 20] = [0x22; 20];

fn bound(answer: &Value, asked: &Asked, level: u8, state: Option<std::path::PathBuf>) -> Outcome {
    let node = LevelNode {
        root: final_node(),
        level,
    };
    let bind = onyx_check::Bind {
        registry: Some(REGISTRY),
        ladder_state: state,
    };
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(onyx_check::check_on_bound(
            answer, asked, node, CHAIN, CONTRACT, &bind,
        ))
}

fn h32(s: &str) -> onyx_core::Hash32 {
    onyx_core::Hash32(hex::decode(&s[2..]).unwrap().try_into().unwrap())
}

/// The proof fixture as a node that states `level` serves it: `proofLevel`
/// and the row's one-step ladder on the root.
fn leveled_proof(level: u8) -> (Value, onyx_core::Ladder) {
    let mut a = fixture("proof");
    let row = hex::decode(&a["row"].as_str().unwrap()[2..]).unwrap();
    let l = onyx_core::Ladder::new(
        onyx_core::ladder::value_key(
            &onyx_core::ladder::row_key(1, &h32(SHARD), 0),
            &onyx_core::keccak256(&row),
        ),
        onyx_core::Step {
            level: onyx_core::ProofLevel::from_number(level).unwrap(),
            evidence: h32(ROOT),
            at: 400,
        },
    )
    .unwrap();
    a["proofLevel"] = json!(level);
    a["ladder"] = onyx_api::ladder::to_json(&l, onyx_api::ladder::ROW_RECORD);
    (a, l)
}

#[test]
fn a_lifted_export_level_is_refused() {
    let asked = parse_asked(&json!({"kind": "export", "round": 1, "shard": SHARD})).unwrap();
    let mut a = fixture("export");
    a["proofLevel"] = json!(1);
    let o = bound(&a, &asked, 4, None);
    let r = o.report();
    assert_eq!((o.code(), r["code"].clone()), (1, json!("ladder")), "{r}");
    assert!(r["error"]
        .as_str()
        .unwrap()
        .contains("your registry attests level 4"));
    // The level the registry attests passes, and the report says so.
    a["proofLevel"] = json!(4);
    let o = bound(&a, &asked, 4, None);
    assert_eq!(o.code(), 0, "{}", o.report());
    assert_eq!(o.report()["level"]["registry"], 4);
    // An answer with no level field (an older node): nothing to bind.
    let o = bound(&fixture("export"), &asked, 4, None);
    assert_eq!(o.code(), 0);
    assert!(o.report().get("level").is_none());
}

#[test]
fn a_row_level_above_verify_row_leveled_is_refused() {
    let (lifted, _) = leveled_proof(1);
    let o = bound(&lifted, &proof_asked(0), 4, None);
    assert_eq!(o.code(), 1, "{}", o.report());
    // The same answer at the registry's level passes, ladder and all.
    let (fair, _) = leveled_proof(4);
    assert_eq!(bound(&fair, &proof_asked(0), 4, None).code(), 0);
    // A ladder lifted while proofLevel stays: refused by the ladder check.
    let mut a = fair.clone();
    a["ladder"]["level"] = json!(3);
    a["ladder"]["steps"][0]["level"] = json!(3);
    a["ladder"]["steps"][0]["name"] = json!("source-proven");
    assert_eq!(bound(&a, &proof_asked(0), 4, None).code(), 1);
}

#[test]
fn a_ladder_without_a_step_seen_before_is_refused() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("onyx-check-ladders-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (a, l) = leveled_proof(4);
    // First run: accepted and kept; the same answer again: accepted.
    assert_eq!(bound(&a, &proof_asked(0), 4, Some(dir.clone())).code(), 0);
    let kept = dir.join(format!("{}.json", l.value));
    assert!(kept.exists());
    assert_eq!(bound(&a, &proof_asked(0), 4, Some(dir.clone())).code(), 0);
    // The caller had seen a later rise of this value; an answer without it
    // is a fall.
    let mut seen = l.clone();
    seen.raise(onyx_core::Step {
        level: onyx_core::ProofLevel::Recomputed,
        evidence: h32(ROOT),
        at: 500,
    })
    .unwrap();
    std::fs::write(
        &kept,
        onyx_api::ladder::to_json(&seen, onyx_api::ladder::ROW_RECORD).to_string(),
    )
    .unwrap();
    let o = bound(&a, &proof_asked(0), 4, Some(dir.clone()));
    let r = o.report();
    assert_eq!(o.code(), 1, "{r}");
    assert!(
        r["error"].as_str().unwrap().contains("accepted before"),
        "{r}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
