//! Shared vectors with contracts/test/OnyxSettlement.t.sol (ADR-0022): the
//! settlement contract must compute the same round tag, commit message, EIP-191
//! digest, validator set root, canary commitment, verdict commitments and
//! sampling assignments as the Rust coordinator. Regenerate with
//! `ONYX_BLESS=1 cargo test -p onyx-source-btc --test evm_vectors`.

mod common;

use onyx_coordinator::dispute::{
    can_sit, claim_commit, dispute_id, draw_panel, panel_commit_hash, DisputeConfig,
};
use onyx_coordinator::recompute::no_valid_root_marker;
use onyx_coordinator::round::{commit_message, round_tag, RoundConfig};
use onyx_coordinator::validate::{
    assigned, canary_commit, sample_seed, seed_draw_voids, verdict_commit_hash, verdict_right,
    Canary, ValidationConfig, ValidatorSet, Verdict, BEACON_WINDOW, MIN_DRAW_SLACK,
    MIN_VERDICT_WINDOW,
};
use onyx_core::{evm_ledger_id, keccak256, Hash32, MinerAddress};
use onyx_labels::attest::{eip191_digest, eip191_recover};
use serde_json::json;

/// Dispute rules in every vector round (round tag v4 fields, M5). Mirrored in
/// contracts/test/OnyxSettlementVectors.t.sol.
const DISPUTE: DisputeConfig = DisputeConfig {
    challenge_window: 200,
    min_challenge_bond: 1_000,
    panel_size: 3,
    panel_commit_window: 50,
    panel_reveal_window: 50,
    appeal_window: 50,
    max_appeals: 1,
    panel_fee: 10,
    panel_slash: 500,
};

fn hx(h: &Hash32) -> String {
    format!("0x{}", h.to_hex())
}

fn addr_hex(a: &MinerAddress) -> String {
    format!("0x{}", hex::encode(a))
}

fn key_addr(sk: &secp256k1::SecretKey) -> MinerAddress {
    let pk = sk
        .public_key(&secp256k1::Secp256k1::new())
        .serialize_uncompressed();
    let mut a = [0u8; 20];
    a.copy_from_slice(&keccak256(&pk[1..]).0[12..]);
    a
}

fn sign(sk: &secp256k1::SecretKey, msg: &[u8]) -> [u8; 65] {
    let digest = secp256k1::Message::from_digest(eip191_digest(msg).0);
    let (recid, rs) = secp256k1::Secp256k1::new()
        .sign_ecdsa_recoverable(&digest, sk)
        .serialize_compact();
    let mut sig = [0u8; 65];
    sig[..64].copy_from_slice(&rs);
    sig[64] = 27 + u8::try_from(recid.to_i32()).expect("recid");
    sig
}

/// The real 3-block fixture shard, with everything a rerun needs.
struct Fixture {
    reg: onyx_core::StaticRegistry,
    cas: onyx_coordinator::cas::MemCas,
    host: onyx_recipe_host::Host,
    spec: onyx_core::ShardSpec,
    source: Vec<u8>,
}

fn fixture() -> Fixture {
    use onyx_coordinator::cas::{Cas, CasExt, Kind, MemCas};
    use onyx_core::{
        LabelSetRef, Registry, RuntimeProfile, ShardSpec, SourceAdapter, SourceId, SourceRef,
    };
    use onyx_recipe_host::HOST_ID;
    use onyx_source_btc::{fixture_registry, load_label_set, BtcFixtureAdapter, SOURCE_ID};

    let root = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let fixtures = root.join("fixtures");
    let reg = fixture_registry();
    let adapter = BtcFixtureAdapter::new(&fixtures, "btc/blocks-921080-921082.json");
    let snapshot = adapter.range_snapshot().expect("snapshot");
    let source = adapter.load(&snapshot).expect("load");
    let labels =
        load_label_set(&fixtures, "labels/test.onyx.entities.btc.v1.json").expect("labels");
    let wasm = std::fs::read(root.join("recipes/out/btc_entity_balance.wasm")).expect("wasm");
    let spec = ShardSpec {
        recipe: Some(keccak256(&wasm)),
        runtime: Some(RuntimeProfile {
            abi_version: 1,
            host: HOST_ID.into(),
            fuel_limit: 4_000_000_000,
            max_memory_bytes: 256 << 20,
            max_output_bytes: 1 << 20,
            max_call_depth: 512,
        }),
        label_set: Some(LabelSetRef {
            name: labels.name.clone(),
            version: labels.version,
            content_hash: labels.content_hash(),
        }),
        source: SourceRef {
            source: SourceId(SOURCE_ID.into()),
            snapshot,
        },
        schema: keccak256(b"onyx.btc.entity-flow-row.v1"),
        rights_hash: reg
            .source_rights(&SourceId(SOURCE_ID.into()))
            .expect("rights")
            .hash(),
        params: Vec::new(),
        registry_root: reg.root(),
    };
    let cas = MemCas::new();
    cas.put(Kind::Recipe, &wasm).expect("recipe");
    cas.put_label_set(&labels).expect("labels");
    cas.put(Kind::Blob, &spec.params).expect("params");
    Fixture {
        reg,
        cas,
        host: onyx_recipe_host::Host::new().expect("host"),
        spec,
        source,
    }
}

/// Three honest miners commit in order 0, 1, 2 on the real 3-block fixture shard and reveal
/// in order 2, 0, 1. Returns each reveal's commit sequence number (reveal order) and the
/// index, in reveal order, of the reveal Rust pays (`ShardOutcome::first` after `close`).
fn reward_order_from_a_real_round() -> (Vec<u64>, usize) {
    use onyx_coordinator::validate::Ledger;
    use onyx_miner::Miner;
    use onyx_source_btc::BtcRules;

    let Fixture {
        reg,
        cas,
        host,
        spec,
        source,
    } = fixture();
    let cfg = RoundConfig {
        commit_close: 100,
        reveal_close: 200,
        min_bond: 1_000,
        validator_set_root: Hash32::ZERO,
        canary_commit: Hash32::ZERO,
        beacon_height: 201,
        validation: ValidationConfig {
            verdict_commit_close: 300,
            verdict_reveal_close: 400,
            canary_open_close: 450,
            pending_close: 600,
            rate_ppm: 1_000_000,
            miner_reward: 500,
            validator_fee: 10,
            validator_slash: 2_000,
            missed_slash: 100,
            min_canary_ppm: 50_000,
        },
        dispute: DISPUTE,
    };
    let mut ledger = Ledger::<BtcRules>::default();
    let mut round =
        common::open_round(9, vec![spec.clone()], &reg, &cas, cfg, 10, &mut ledger).expect("open");
    let shard = spec.shard_id();
    let tag = round.tag();
    let miners: Vec<Miner> = (0u8..3)
        .map(|n| Miner::new(keccak256(&[n, 0x77]).0).expect("key"))
        .collect();
    let works: Vec<_> = miners
        .iter()
        .map(|m| {
            m.work(9, &spec, source.clone(), &reg, &cas, &host)
                .expect("work")
        })
        .collect();
    let seqs: Vec<u64> = miners
        .iter()
        .zip(&works)
        .map(|(m, w)| {
            round
                .commit(50, &m.sign_commit(w, 1_000, &tag), &mut ledger)
                .expect("commit")
        })
        .collect();
    let reveal_order = [2usize, 0, 1];
    for &i in &reveal_order {
        miners[i].publish_rows(&works[i], &cas).expect("rows");
        round
            .reveal(
                150,
                miners[i].address,
                shard,
                works[i].commitment.clone(),
                works[i].salt,
                &cas,
            )
            .expect("reveal");
    }
    let out = round.close(200).expect("close");
    let paid = out[&shard].first().expect("a reveal").1;
    let reveal_seqs = reveal_order.iter().map(|&i| seqs[i]).collect();
    let winner = reveal_order
        .iter()
        .position(|&i| miners[i].address == paid)
        .expect("paid miner revealed");
    (reveal_seqs, winner)
}

/// What a validator does on one reveal, as encoded in the vector.
const SILENT: u8 = 0;
const AGREE: u8 = 1;
const DISAGREE: u8 = 2;
const ABSTAIN: u8 = 3;
const UNREVEALED: u8 = 4;

/// A real round on the fixture shard: two honest miners reveal; validator `i` does
/// `actions[i][j]` on miner `j`'s reveal (rate 100%, bonds 5000), and Rust settles it.
/// Returns, per validator, the fee, missed-task slash and unrevealed-verdict slash in the
/// settlement's ledger entries (M6-F7: unrevealed verdicts are slashed once each at round
/// level, whatever the shard's outcome).
fn unrevealed_slashing_from_a_real_round(f: &Fixture, actions: &[[u8; 2]]) -> serde_json::Value {
    use onyx_coordinator::recompute::RecomputeEnv;
    use onyx_coordinator::validate::{Ledger, LedgerEntry, Validation};
    use onyx_core::ShardSpec;
    use onyx_miner::Miner;
    use onyx_source_btc::BtcRules;
    use onyx_validator::Validator;

    let validators: Vec<Validator> = (0..actions.len())
        .map(|i| Validator::new(keccak256(format!("vector validator {i}").as_bytes()).0))
        .collect::<Result<_, _>>()
        .expect("validators");
    let mut set = ValidatorSet::new();
    for v in &validators {
        set.add(v.address, 5_000, 1_000).expect("member");
    }
    let salt = keccak256(b"vector canary salt");
    let cfg = RoundConfig {
        commit_close: 100,
        reveal_close: 200,
        min_bond: 1_000,
        validator_set_root: set.root(),
        canary_commit: canary_commit(&[], &salt).expect("empty"),
        beacon_height: 201,
        validation: ValidationConfig {
            verdict_commit_close: 300,
            verdict_reveal_close: 400,
            canary_open_close: 450,
            pending_close: 600,
            rate_ppm: 1_000_000,
            miner_reward: 500,
            validator_fee: 10,
            validator_slash: 2_000,
            missed_slash: 100,
            min_canary_ppm: 50_000,
        },
        dispute: DISPUTE,
    };
    let mut ledger = Ledger::<BtcRules>::default();
    let mut round = common::open_round(
        11,
        vec![f.spec.clone()],
        &f.reg,
        &f.cas,
        cfg,
        10,
        &mut ledger,
    )
    .expect("open");
    let shard = f.spec.shard_id();
    let tag = round.tag();
    let miners: Vec<Miner> = (0u8..2)
        .map(|n| Miner::new(keccak256(&[n, 0x55]).0).expect("key"))
        .collect();
    let works: Vec<_> = miners
        .iter()
        .map(|m| {
            m.work(11, &f.spec, f.source.clone(), &f.reg, &f.cas, &f.host)
                .expect("work")
        })
        .collect();
    for (n, (m, w)) in miners.iter().zip(&works).enumerate() {
        round
            .commit(50 + n as u64, &m.sign_commit(w, 1_000, &tag), &mut ledger)
            .expect("commit");
    }
    for (m, w) in miners.iter().zip(&works) {
        m.publish_rows(w, &f.cas).expect("rows");
        round
            .reveal(150, m.address, shard, w.commitment.clone(), w.salt, &f.cas)
            .expect("reveal");
    }
    round.close(200).expect("close");
    let mut val = Validation::<BtcRules>::new(&round, set, keccak256(b"beacon"), 201, &mut ledger)
        .expect("validation");
    let seed = val.seed();
    let mut reveals = Vec::new();
    for (v, acts) in validators.iter().zip(actions) {
        let tasks = val.tasks_for(&v.address);
        for (m, &act) in miners.iter().zip(acts) {
            assert!(
                tasks.iter().any(|t| t.1 == m.address),
                "rate 100%: assigned"
            );
            let verdict = match act {
                SILENT => continue,
                AGREE | UNREVEALED => Verdict::Agree,
                DISAGREE => Verdict::Disagree {
                    root: keccak256(b"a wrong root"),
                },
                ABSTAIN => Verdict::Abstain,
                _ => unreachable!("unknown action"),
            };
            val.commit_verdict(250, &v.commit(&seed, &shard, &m.address, &verdict))
                .expect("commit verdict");
            if act != UNREVEALED {
                let vs = v.salt(&seed, &shard, &m.address);
                reveals.push((v.address, m.address, verdict, vs));
            }
        }
    }
    for (a, m, verdict, vs) in reveals {
        val.reveal_verdict(350, a, shard, m, verdict, vs)
            .expect("reveal verdict");
    }
    let source = f.source.clone();
    let src = move |_: &ShardSpec| Some(source.clone());
    let env = RecomputeEnv {
        reg: &f.reg,
        cas: &f.cas,
        host: &f.host,
        source: &src,
    };
    let s = val.settle(400, Some((&[], &salt)), &env).expect("settle");
    let total = |who: &MinerAddress, why: &str| -> u128 {
        s.entries()
            .iter()
            .map(|e| match e {
                LedgerEntry::Reward {
                    actor,
                    amount,
                    reason,
                    ..
                }
                | LedgerEntry::Slash {
                    actor,
                    amount,
                    reason,
                    ..
                } if actor == who && *reason == why => *amount,
                _ => 0,
            })
            .sum()
    };
    let per = |why: &str| -> Vec<u64> {
        validators
            .iter()
            .map(|v| u64::try_from(total(&v.address, why)).expect("small"))
            .collect()
    };
    json!({
        "actions": actions.iter().flatten().collect::<Vec<_>>(),
        "fee": per("correct verdict"),
        "missed": per("assigned task not answered"),
        "unrevealed": per("verdict not revealed"),
    })
}

/// M6-G4: a real Rust round on a ledger that mirrors the settlement contract
/// (`Ledger::mirror_of_contract`). Its round tag uses the contract's ledger id, its
/// validation seed is the contract's `keccak256("onyx-seed-evm-v1" ‖ tag ‖
/// blockhash)`, and the tasks each validator gets at a 50% rate are exported
/// so the contract can check it judges exactly those (ADR-0022 item 5).
fn evm_seed_from_a_real_round(
    f: &Fixture,
    chain_id: u64,
    contract: &MinerAddress,
) -> serde_json::Value {
    use onyx_coordinator::validate::{Ledger, Validation};
    use onyx_core::Registry;
    use onyx_miner::Miner;
    use onyx_source_btc::BtcRules;
    use onyx_validator::Validator;

    let validators: Vec<Validator> = (0..4)
        .map(|i| Validator::new(keccak256(format!("evm seed validator {i}").as_bytes()).0))
        .collect::<Result<_, _>>()
        .expect("validators");
    let mut set = ValidatorSet::new();
    for v in &validators {
        set.add(v.address, 5_000, 1_000).expect("member");
    }
    let salt = keccak256(b"evm seed canary salt");
    let cfg = RoundConfig {
        commit_close: 100,
        reveal_close: 200,
        min_bond: 1_000,
        validator_set_root: set.root(),
        canary_commit: canary_commit(&[], &salt).expect("empty"),
        beacon_height: 201,
        validation: ValidationConfig {
            verdict_commit_close: 300,
            verdict_reveal_close: 400,
            canary_open_close: 450,
            pending_close: 600,
            rate_ppm: 500_000,
            miner_reward: 500,
            validator_fee: 10,
            validator_slash: 2_000,
            missed_slash: 100,
            min_canary_ppm: 50_000,
        },
        dispute: DISPUTE,
    };
    let mut ledger = Ledger::<BtcRules>::mirror_of_contract(chain_id, contract);
    assert_eq!(
        ledger.id(),
        evm_ledger_id(chain_id, contract),
        "contract ledger id"
    );
    let mut round = common::open_round(
        12,
        vec![f.spec.clone()],
        &f.reg,
        &f.cas,
        cfg,
        10,
        &mut ledger,
    )
    .expect("open");
    let shard = f.spec.shard_id();
    let tag = round.tag();
    assert_eq!(
        tag,
        round_tag(
            f.reg.domain(),
            &evm_ledger_id(chain_id, contract),
            12,
            &cfg,
            &[shard]
        ),
        "the round tag is the contract's"
    );
    let miners: Vec<Miner> = (0u8..3)
        .map(|n| Miner::new(keccak256(&[n, 0x47]).0).expect("key"))
        .collect();
    let works: Vec<_> = miners
        .iter()
        .map(|m| {
            m.work(12, &f.spec, f.source.clone(), &f.reg, &f.cas, &f.host)
                .expect("work")
        })
        .collect();
    for (n, (m, w)) in miners.iter().zip(&works).enumerate() {
        round
            .commit(50 + n as u64, &m.sign_commit(w, 1_000, &tag), &mut ledger)
            .expect("commit");
    }
    for (m, w) in miners.iter().zip(&works) {
        m.publish_rows(w, &f.cas).expect("rows");
        round
            .reveal(150, m.address, shard, w.commitment.clone(), w.salt, &f.cas)
            .expect("reveal");
    }
    round.close(200).expect("close");
    // Stands in for blockhash(201).
    let beacon = keccak256(b"evm seed beacon");
    let val =
        Validation::<BtcRules>::new(&round, set, beacon, 202, &mut ledger).expect("validation");
    assert_eq!(
        val.seed(),
        sample_seed(&tag, &beacon),
        "seed from tag and beacon only"
    );
    let tasks: Vec<Vec<bool>> = validators
        .iter()
        .map(|v| {
            let mine = val.tasks_for(&v.address);
            miners
                .iter()
                .map(|m| mine.iter().any(|t| t.0 == shard && t.1 == m.address))
                .collect()
        })
        .collect();
    let flat: Vec<bool> = tasks.iter().flatten().copied().collect();
    assert!(
        flat.iter().any(|&t| t) && flat.iter().any(|&t| !t),
        "a 50% rate assigns some tasks and skips others"
    );
    let v = &cfg.validation;
    let d = &cfg.dispute;
    json!({
        "round": 12,
        // Everything the contract needs to derive `tag` itself (M6-G4R-F2).
        "domain": f.reg.domain(),
        "ledgerId": hx(&evm_ledger_id(chain_id, contract)),
        "shards": [hx(&shard)],
        "cfg": {
            "commitClose": cfg.commit_close, "revealClose": cfg.reveal_close,
            "minBond": cfg.min_bond.to_string(),
            "validatorSetRoot": hx(&cfg.validator_set_root),
            "canaryCommit": hx(&cfg.canary_commit), "beaconHeight": cfg.beacon_height,
            "verdictCommitClose": v.verdict_commit_close,
            "verdictRevealClose": v.verdict_reveal_close,
            "canaryOpenClose": v.canary_open_close, "pendingClose": v.pending_close,
            "ratePpm": v.rate_ppm, "minerReward": v.miner_reward.to_string(),
            "validatorFee": v.validator_fee.to_string(),
            "validatorSlash": v.validator_slash.to_string(),
            "missedSlash": v.missed_slash.to_string(), "minCanaryPpm": v.min_canary_ppm,
            "challengeWindow": d.challenge_window,
            "minChallengeBond": d.min_challenge_bond.to_string(),
            "panelSize": d.panel_size, "panelCommitWindow": d.panel_commit_window,
            "panelRevealWindow": d.panel_reveal_window, "appealWindow": d.appeal_window,
            "maxAppeals": d.max_appeals, "panelFee": d.panel_fee.to_string(),
            "panelSlash": d.panel_slash.to_string(),
        },
        "tag": hx(&tag),
        "beacon": hx(&beacon),
        "seed": hx(&val.seed()),
        "shard": hx(&shard),
        "ratePpm": 500_000,
        "validators": validators.iter().map(|v| addr_hex(&v.address)).collect::<Vec<_>>(),
        "miners": miners.iter().map(|m| addr_hex(&m.address)).collect::<Vec<_>>(),
        "tasks": tasks,
    })
}

fn addr(seed: &str) -> MinerAddress {
    let mut a = [0u8; 20];
    a.copy_from_slice(&keccak256(seed.as_bytes()).0[12..]);
    a
}

/// Dispute vectors (ADR-0024, M5-L49-F5): config validity at every boundary, the panel
/// draw, the panel vote, claim and case-id hashes, the no-valid-root marker and the verdict
/// rule the contract judges with.
fn dispute_vectors(ledger: &Hash32) -> serde_json::Value {
    // Each case changes one field of DISPUTE (sum of panel windows = 150).
    let mut cases: Vec<(&str, DisputeConfig)> = vec![("base", DISPUTE)];
    let d = DISPUTE;
    for (name, c) in [
        ("even panel", DisputeConfig { panel_size: 4, ..d }),
        ("zero panel", DisputeConfig { panel_size: 0, ..d }),
        (
            "panel 1",
            DisputeConfig {
                panel_size: 1,
                min_challenge_bond: 20,
                ..d
            },
        ),
        (
            "zero commit window",
            DisputeConfig {
                panel_commit_window: 0,
                ..d
            },
        ),
        (
            "zero reveal window",
            DisputeConfig {
                panel_reveal_window: 0,
                ..d
            },
        ),
        (
            "zero appeal window",
            DisputeConfig {
                appeal_window: 0,
                ..d
            },
        ),
        (
            "zero challenge window",
            DisputeConfig {
                challenge_window: 0,
                ..d
            },
        ),
        (
            "window equals panel",
            DisputeConfig {
                challenge_window: 150,
                ..d
            },
        ),
        (
            "window one more",
            DisputeConfig {
                challenge_window: 151,
                ..d
            },
        ),
        (
            "zero bond",
            DisputeConfig {
                min_challenge_bond: 0,
                panel_fee: 0,
                ..d
            },
        ),
        (
            "bond equals fees",
            DisputeConfig {
                min_challenge_bond: 40,
                ..d
            },
        ),
        (
            "bond below fees",
            DisputeConfig {
                min_challenge_bond: 39,
                ..d
            },
        ),
        (
            "zero slash",
            DisputeConfig {
                panel_slash: 0,
                panel_fee: 0,
                ..d
            },
        ),
        (
            "slash equals 9 fees",
            DisputeConfig {
                panel_slash: 90,
                ..d
            },
        ),
        (
            "slash below 9 fees",
            DisputeConfig {
                panel_slash: 89,
                ..d
            },
        ),
        ("zero fee", DisputeConfig { panel_fee: 0, ..d }),
        (
            "max appeals",
            DisputeConfig {
                max_appeals: 4,
                ..d
            },
        ),
        (
            "too many appeals",
            DisputeConfig {
                max_appeals: 5,
                ..d
            },
        ),
        (
            "panel 63 no appeals",
            DisputeConfig {
                panel_size: 63,
                max_appeals: 0,
                min_challenge_bond: 640,
                ..d
            },
        ),
        (
            "panel 65 no appeals",
            DisputeConfig {
                panel_size: 65,
                max_appeals: 0,
                min_challenge_bond: 660,
                ..d
            },
        ),
        (
            "panel 15 two appeals",
            DisputeConfig {
                panel_size: 15,
                max_appeals: 2,
                min_challenge_bond: 160,
                ..d
            },
        ),
        (
            "panel 17 two appeals",
            DisputeConfig {
                panel_size: 17,
                max_appeals: 2,
                min_challenge_bond: 180,
                ..d
            },
        ),
        (
            "windows overflow",
            DisputeConfig {
                panel_commit_window: u64::MAX,
                challenge_window: u64::MAX,
                ..d
            },
        ),
    ] {
        cases.push((name, c));
    }
    let col = |f: &dyn Fn(&DisputeConfig) -> String| -> Vec<String> {
        cases.iter().map(|(_, c)| f(c)).collect()
    };
    let config_valid = json!({
        "names": cases.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        "challengeWindow": col(&|c| c.challenge_window.to_string()),
        "minChallengeBond": col(&|c| c.min_challenge_bond.to_string()),
        "panelSize": col(&|c| c.panel_size.to_string()),
        "panelCommitWindow": col(&|c| c.panel_commit_window.to_string()),
        "panelRevealWindow": col(&|c| c.panel_reveal_window.to_string()),
        "appealWindow": col(&|c| c.appeal_window.to_string()),
        "maxAppeals": col(&|c| c.max_appeals.to_string()),
        "panelFee": col(&|c| c.panel_fee.to_string()),
        "panelSlash": col(&|c| c.panel_slash.to_string()),
        "valid": cases.iter().map(|(_, c)| c.is_valid()).collect::<Vec<_>>(),
    });

    let seed = keccak256(b"vector seed");
    let shard = keccak256(b"shard a");
    let challenger = addr("challenger");
    let claimed = keccak256(b"claimed root");
    let claim_salt = keccak256(b"claim salt");
    let claim = claim_commit(ledger, 7, &shard, &challenger, &claimed, &claim_salt);
    let case_id = dispute_id(ledger, 7, &shard, &challenger, &claim, 0);
    let rerun_id = dispute_id(ledger, 7, &shard, &[0u8; 20], &Hash32::ZERO, 1);
    let beacon = keccak256(b"panel beacon");
    let candidates: std::collections::BTreeSet<MinerAddress> = (0..10)
        .map(|i| addr(&format!("panel candidate {i}")))
        .collect();
    let draw = |level: u32, size: u32| -> Vec<String> {
        draw_panel(&seed, &case_id, level, &beacon, &candidates, size)
            .iter()
            .map(addr_hex)
            .collect()
    };
    let member = addr("panel candidate 3");
    let vote_root = keccak256(b"panel root");
    let vote_salt = keccak256(b"panel salt");

    // Verdict rule: every verdict kind against a matching and a different miner root, with a
    // reference and with no valid root. 0 = not judged (Abstain), 1 = right, 2 = wrong.
    let x = keccak256(b"root x");
    let y = keccak256(b"root y");
    let mut rows = (
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    for (kind, verdict) in [
        (0u8, Verdict::Agree),
        (1, Verdict::Disagree { root: x }),
        (1, Verdict::Disagree { root: y }),
        (2, Verdict::Abstain),
        (3, Verdict::NoValidRoot),
    ] {
        for miner_root in [x, y] {
            for reference in [Some(x), None] {
                let vroot = match verdict {
                    Verdict::Disagree { root } => root,
                    _ => Hash32::ZERO,
                };
                let res = match verdict_right(&verdict, &miner_root, reference.as_ref()) {
                    None => 0u8,
                    Some(true) => 1,
                    Some(false) => 2,
                };
                rows.0.push(kind);
                rows.1.push(hx(&vroot));
                rows.2.push(hx(&miner_root));
                rows.3.push(reference.is_none());
                rows.4.push(hx(
                    &reference.unwrap_or_else(|| no_valid_root_marker(&shard))
                ));
                rows.5.push(res);
            }
        }
    }
    // M6-L2-F3: appeal floors (level, size) for the base config, and for fees above the
    // doubled minimum (not a valid config: `appeal_floor` is pure, both branches are checked).
    let fee_bound = DisputeConfig {
        panel_fee: 400,
        ..d
    };
    let mut floor = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (c, level, size) in [
        (d, 1u32, 7u32),
        (d, 2, 15),
        (d, 4, 63),
        (fee_bound, 1, 7),
        (fee_bound, 1, 3),
    ] {
        floor.0.push(c.min_challenge_bond.to_string());
        floor.1.push(c.panel_fee.to_string());
        floor.2.push(level);
        floor.3.push(size);
        floor.4.push(c.appeal_floor(level, size).to_string());
    }
    // L29-F4 / M6-L2-F1: who can sit on a panel (bond, taken, panel slash).
    let mut sit = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (bond, taken, slash) in [
        (5_000u128, 0u128, 500u128),
        (5_000, 4_500, 500),
        (5_000, 4_501, 500),
        (5_000, 5_000, 500),
        (5_000, 9_000, 500),
        (0, 0, 1),
    ] {
        sit.0.push(bond.to_string());
        sit.1.push(taken.to_string());
        sit.2.push(slash.to_string());
        sit.3.push(can_sit(bond, taken, slash));
    }
    json!({
        "configValid": config_valid,
        "appealFloor": {
            "minChallengeBond": floor.0,
            "panelFee": floor.1,
            "level": floor.2,
            "size": floor.3,
            "floor": floor.4,
        },
        "canSit": {
            "bond": sit.0,
            "taken": sit.1,
            "panelSlash": sit.2,
            "result": sit.3,
        },
        "seed": hx(&seed),
        "shard": hx(&shard),
        "round": 7,
        "challenger": addr_hex(&challenger),
        "claimedRoot": hx(&claimed),
        "claimSalt": hx(&claim_salt),
        "claimCommit": hx(&claim),
        "caseId": hx(&case_id),
        "rerunCaseId": hx(&rerun_id),
        "beacon": hx(&beacon),
        "candidates": candidates.iter().map(addr_hex).collect::<Vec<_>>(),
        "draw3": draw(0, 3),
        "draw7": draw(1, 7),
        "draw11": draw(2, 11),
        "voteMember": addr_hex(&member),
        "voteLevel": 1,
        "voteRoot": hx(&vote_root),
        "voteSalt": hx(&vote_salt),
        "voteHash": hx(&panel_commit_hash(&case_id, 1, &member, &vote_root, &vote_salt)),
        "noValidRootMarker": hx(&no_valid_root_marker(&shard)),
        "verdictRight": {
            "kind": rows.0,
            "verdictRoot": rows.1,
            "minerRoot": rows.2,
            "noValidRoot": rows.3,
            "reference": rows.4,
            "result": rows.5,
        },
    })
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn settlement_contract_vectors_match() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/settlement/vectors.json"
    );
    // The contract's ledger id binds chain id and address (M6-F9); Base Sepolia's chain id.
    let chain_id = 84_532_u64;
    let ledger_addr = addr("onyx test settlement contract");
    let ledger = evm_ledger_id(chain_id, &ledger_addr);

    let mut set = ValidatorSet::new();
    let mut members: Vec<(MinerAddress, u128)> = (1..=3)
        .map(|i| (addr(&format!("validator {i}")), 5_000 + i as u128))
        .collect();
    members.sort();
    for (v, b) in &members {
        set.add(*v, *b, 1).expect("member");
    }

    let cfg = RoundConfig {
        commit_close: 100,
        reveal_close: 200,
        min_bond: 1_000,
        validator_set_root: set.root(),
        canary_commit: keccak256(b"canary commit placeholder"),
        beacon_height: 201,
        validation: ValidationConfig {
            verdict_commit_close: 300,
            verdict_reveal_close: 400,
            canary_open_close: 450,
            pending_close: 600,
            rate_ppm: 1_000_000,
            miner_reward: 500,
            validator_fee: 10,
            validator_slash: 2_000,
            missed_slash: 100,
            min_canary_ppm: 0,
        },
        dispute: DISPUTE,
    };
    let mut shards = vec![keccak256(b"shard b"), keccak256(b"shard a")];
    shards.sort();
    let tag = round_tag("onyx.devnet", &ledger, 7, &cfg, &shards);

    let sk = secp256k1::SecretKey::from_slice(&keccak256(b"onyx evm vector miner").0).expect("key");
    let miner = key_addr(&sk);
    let commit = keccak256(b"some commit hash");
    let bond = 1_234_567_u128;
    let msg = commit_message(&tag, &shards[0], &commit, bond);
    let sig = sign(&sk, msg.as_bytes());
    assert_eq!(eip191_recover(msg.as_bytes(), &sig), Some(miner));

    let canaries = vec![
        Canary {
            shard: shards[0],
            miner: addr("canary 1"),
            root: keccak256(b"canary root 1"),
        },
        Canary {
            shard: shards[1],
            miner: addr("canary 2"),
            root: keccak256(b"canary root 2"),
        },
    ];
    let canary_salt = keccak256(b"canary salt");
    let canary_root = canary_commit(&canaries, &canary_salt).expect("sorted");

    let seed = keccak256(b"vector seed");
    let v0 = members[0].0;
    let vsalt = keccak256(b"verdict salt");
    let verdicts: Vec<serde_json::Value> = [
        (0u8, Verdict::Agree, Hash32::ZERO),
        (
            1,
            Verdict::Disagree {
                root: keccak256(b"other root"),
            },
            keccak256(b"other root"),
        ),
        (2, Verdict::Abstain, Hash32::ZERO),
        (3, Verdict::NoValidRoot, Hash32::ZERO),
    ]
    .iter()
    .map(|(kind, v, root)| {
        json!({
            "kind": kind,
            "root": hx(root),
            "hash": hx(&verdict_commit_hash(&seed, &v0, &shards[0], &miner, v, &vsalt)),
        })
    })
    .collect();
    let assignments: Vec<serde_json::Value> = (0..16)
        .map(|i| {
            let m = addr(&format!("assign miner {i}"));
            json!({
                "miner": addr_hex(&m),
                "rate": 250_000,
                "assigned": assigned(&seed, &v0, &shards[1], &m, 250_000),
            })
        })
        .collect();

    // Reward order (M6-F4, M6-L34-F6): produced by the coordinator's own settle path on a
    // real round, so drift in `Round::close` / `ShardOutcome::first` changes the vector.
    let (reveal_seqs, winner_index) = reward_order_from_a_real_round();
    let fx = fixture();

    // (vcc, at) pairs; parallel arrays so forge can read them with parseJson*Array.
    let seed_cases = [
        (30u64, 1u64),
        (30, 10),
        (30, 11),
        (30, 30),
        (400, 256),
        (400, 257),
    ];
    let seed_vcc: Vec<u64> = seed_cases.iter().map(|c| c.0).collect();
    let seed_at: Vec<u64> = seed_cases.iter().map(|c| c.1).collect();
    let seed_voids: Vec<bool> = seed_cases
        .iter()
        .map(|&(vcc, at)| seed_draw_voids(1_000, 1_000 + vcc, 1_000 + at))
        .collect();
    let disputes = dispute_vectors(&ledger);
    let evm_seed = evm_seed_from_a_real_round(&fx, chain_id, &ledger_addr);
    let doc = json!({
        "domain": "onyx.devnet",
        "chainId": chain_id,
        "ledger": addr_hex(&ledger_addr),
        "ledgerId": hx(&ledger),
        "round": 7,
        "shards": shards.iter().map(hx).collect::<Vec<_>>(),
        "members": members.iter().map(|(a, _)| addr_hex(a)).collect::<Vec<_>>(),
        "bonds": members.iter().map(|(_, b)| b.to_string()).collect::<Vec<_>>(),
        "validatorSetRoot": hx(&set.root()),
        "roundTag": hx(&tag),
        "commit": {
            "shard": hx(&shards[0]),
            "commitHash": hx(&commit),
            "bond": bond.to_string(),
            "message": msg,
            "digest": hx(&eip191_digest(msg.as_bytes())),
            "miner": addr_hex(&miner),
            "sig": format!("0x{}", hex::encode(sig)),
        },
        "canaries": canaries.iter().map(|c| json!({
            "shard": hx(&c.shard), "miner": addr_hex(&c.miner), "root": hx(&c.root)
        })).collect::<Vec<_>>(),
        "canarySalt": hx(&canary_salt),
        "canaryCommit": hx(&canary_root),
        "seed": hx(&seed),
        "validator": addr_hex(&v0),
        "verdictSalt": hx(&vsalt),
        "verdictMiner": addr_hex(&miner),
        "verdicts": verdicts,
        "assignShard": hx(&shards[1]),
        "assignments": assignments,
        "rewardOrder": { "revealSeqs": reveal_seqs, "winnerIndex": winner_index },
        // M6-L44-F3: when drawing the seed voids the round (Rust `seed_draw_voids`, contract
        // `drawSeed`). Heights are offsets from the beacon height; `vcc` is
        // verdict_commit_close. The tightest valid config is vcc = window + slack.
        "seedDraw": {
            "minVerdictWindow": MIN_VERDICT_WINDOW,
            "minDrawSlack": MIN_DRAW_SLACK,
            "beaconWindow": BEACON_WINDOW,
            "vcc": seed_vcc,
            "at": seed_at,
            "voids": seed_voids,
        },
        "unrevealedSlashing": {
            // Unanimous shard: compare fees, missed and unrevealed slashes.
            // M6-L44-F4: also Abstain + unrevealed and silent + unrevealed, which pin the
            // branch order of the per-shard judgement (an unrevealed commit counts as an
            // answer, L29-F9).
            "finalized": unrevealed_slashing_from_a_real_round(&fx, &[
                [AGREE, AGREE],
                [AGREE, UNREVEALED],
                [UNREVEALED, UNREVEALED],
                [ABSTAIN, ABSTAIN],
                [SILENT, SILENT],
                [ABSTAIN, UNREVEALED],
                [SILENT, UNREVEALED],
            ]),
            // Disputed shard (Rust reruns it; the contract leaves it Pending): unrevealed
            // verdicts are still slashed once each.
            "disputed": unrevealed_slashing_from_a_real_round(&fx, &[
                [AGREE, AGREE],
                [DISAGREE, AGREE],
                [UNREVEALED, UNREVEALED],
                [UNREVEALED, SILENT],
                [SILENT, SILENT],
            ]),
        },
        // M6 loop 2 (ADR-0024): the on-chain dispute game's hashes, draws and rules.
        "disputes": disputes,
        // M6-G4: Rust's seed and tasks on a ledger that mirrors this contract.
        "evmSeed": evm_seed,
    });
    let text = serde_json::to_string_pretty(&doc).expect("json") + "\n";
    if std::env::var_os("ONYX_BLESS").is_some() {
        std::fs::create_dir_all(std::path::Path::new(path).parent().expect("parent"))
            .expect("mkdir");
        std::fs::write(path, &text).expect("write vectors");
    }
    let on_disk = std::fs::read_to_string(path)
        .expect("fixtures/settlement/vectors.json missing; run with ONYX_BLESS=1");
    assert_eq!(on_disk, text, "settlement vectors drifted from ADR-0022");
}
