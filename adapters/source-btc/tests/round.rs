//! M3 end to end on real blocks (ADR-0018): a round opens only with every
//! pinned input in the CAS; honest miners compute and commit; reveals are
//! checked against commit, shard and data availability; copiers, late and
//! wrong-label miners fail in the expected way.

mod common;

use onyx_coordinator::cas::{Cas, CasExt, Kind, MemCas};
use onyx_coordinator::dispute::DisputeConfig;
use onyx_coordinator::round::{Round, RoundConfig, RoundError, RoundPhase};
use onyx_coordinator::validate::{
    Ledger, ValidationConfig, MIN_DRAW_SLACK, MIN_PENDING_GAP, MIN_VERDICT_WINDOW,
};
use onyx_core::{
    keccak256, Commitment, LabelSetRef, Registry, RuntimeProfile, ShardSpec, SourceAdapter,
    SourceId, SourceRef,
};
use onyx_labels::LabelSet;
use onyx_miner::Miner;
use onyx_recipe_host::{Host, HOST_ID};
use onyx_source_btc::{fixture_registry, load_label_set, BtcFixtureAdapter, BtcRules, SOURCE_ID};
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn profile() -> RuntimeProfile {
    RuntimeProfile {
        abi_version: 1,
        host: HOST_ID.into(),
        fuel_limit: 4_000_000_000,
        max_memory_bytes: 256 << 20,
        max_output_bytes: 1 << 20,
        max_call_depth: 512,
    }
}

struct World {
    reg: onyx_core::StaticRegistry,
    cas: MemCas,
    spec: ShardSpec,
    source: Vec<u8>,
    full: LabelSet,
    wasm: Vec<u8>,
}

fn world() -> World {
    let reg = fixture_registry();
    let fixtures = root().join("fixtures");
    let adapter = BtcFixtureAdapter::new(&fixtures, "btc/blocks-921080-921082.json");
    let snapshot = adapter.range_snapshot().expect("snapshot");
    let source = adapter.load(&snapshot).expect("load");
    let full = load_label_set(&fixtures, "labels/test.onyx.entities.btc.v1.json").expect("labels");
    let wasm = std::fs::read(root().join("recipes/out/btc_entity_balance.wasm")).expect("wasm");
    let spec = ShardSpec {
        recipe: Some(keccak256(&wasm)),
        runtime: Some(profile()),
        label_set: Some(LabelSetRef {
            name: full.name.clone(),
            version: full.version,
            content_hash: full.content_hash(),
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
    World {
        reg,
        cas: MemCas::new(),
        spec,
        source,
        full,
        wasm,
    }
}

fn publish_inputs(w: &World) {
    w.cas.put(Kind::Recipe, &w.wasm).expect("recipe");
    w.cas.put_label_set(&w.full).expect("labels");
    w.cas.put(Kind::Blob, &w.spec.params).expect("params");
}

const CFG: RoundConfig = RoundConfig {
    commit_close: 100,
    reveal_close: 200,
    min_bond: 1_000,
    validator_set_root: onyx_core::Hash32([0; 32]),
    canary_commit: onyx_core::Hash32([0; 32]),
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
    dispute: DisputeConfig {
        challenge_window: 200,
        min_challenge_bond: 1_000,
        panel_size: 3,
        panel_commit_window: 50,
        panel_reveal_window: 50,
        appeal_window: 50,
        max_appeals: 1,
        panel_fee: 10,
        panel_slash: 500,
    },
};

/// q10 (a): a shard whose pinned inputs are not all in the CAS never opens.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn round_opens_only_with_inputs_available() {
    let w = world();
    let open = |w: &World| {
        common::open_round(
            1,
            vec![w.spec.clone()],
            &w.reg,
            &w.cas,
            CFG,
            10,
            &mut Ledger::<BtcRules>::default(),
        )
    };
    assert!(matches!(open(&w), Err(RoundError::Unavailable(_))));
    w.cas.put(Kind::Recipe, &w.wasm).expect("recipe");
    w.cas.put(Kind::Blob, &w.spec.params).expect("params");
    assert!(matches!(open(&w), Err(RoundError::Unavailable(s)) if s.starts_with("label set")));
    w.cas.put_label_set(&w.full).expect("labels");
    assert!(open(&w).is_ok());
    // L18-F4: a pin whose version differs from the stored set, or an invalid
    // (unsorted) set, is not usable and does not open.
    let mut wrong_version = w.spec.clone();
    if let Some(p) = wrong_version.label_set.as_mut() {
        p.version += 1;
    }
    assert!(matches!(
        common::open_round(1, vec![wrong_version], &w.reg, &w.cas, CFG, 10, &mut Ledger::<BtcRules>::default()),
        Err(RoundError::Unavailable(s)) if s.contains("version")
    ));
    let mut unsorted = w.full.clone();
    unsorted.name = "test.onyx.entities.btc.unsorted".into();
    unsorted.labels.swap(0, 1);
    w.cas.put_label_set(&unsorted).expect("unsorted");
    let mut uspec = w.spec.clone();
    uspec.label_set = Some(LabelSetRef {
        name: unsorted.name.clone(),
        version: unsorted.version,
        content_hash: unsorted.content_hash(),
    });
    assert!(matches!(
        common::open_round(1, vec![uspec], &w.reg, &w.cas, CFG, 10, &mut Ledger::<BtcRules>::default()),
        Err(RoundError::Unavailable(s)) if s.contains("invalid")
    ));
    // A set citing an attestation that is not stored does not open either.
    let mut cites = w.full.clone();
    cites.name = "test.onyx.entities.btc.cites".into();
    cites.labels[0].evidence = vec![onyx_core::EvidenceRef {
        kind: onyx_labels::ATTESTATION_EVIDENCE.into(),
        content_hash: keccak256(b"attestation nobody stored"),
    }];
    w.cas.put_label_set(&cites).expect("cites");
    let mut spec = w.spec.clone();
    spec.label_set = Some(LabelSetRef {
        name: cites.name.clone(),
        version: cites.version,
        content_hash: cites.content_hash(),
    });
    assert!(matches!(
        common::open_round(1, vec![spec], &w.reg, &w.cas, CFG, 10, &mut Ledger::<BtcRules>::default()),
        Err(RoundError::Unavailable(s)) if s.starts_with("attestation")
    ));
    // Bad times and a spec pinned to another registry are refused.
    let late = RoundConfig {
        commit_close: 5,
        ..CFG
    };
    assert_eq!(
        common::open_round(
            1,
            vec![w.spec.clone()],
            &w.reg,
            &w.cas,
            late,
            10,
            &mut Ledger::<BtcRules>::default()
        )
        .err(),
        Some(RoundError::BadTimes)
    );
    let mut other = w.spec.clone();
    other.registry_root = keccak256(b"other");
    assert!(matches!(
        common::open_round(
            1,
            vec![other],
            &w.reg,
            &w.cas,
            CFG,
            10,
            &mut Ledger::<BtcRules>::default()
        ),
        Err(RoundError::BadSpec(_))
    ));
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn commit_reveal_round_on_real_blocks() {
    let w = world();
    publish_inputs(&w);
    let mut ledger = Ledger::<BtcRules>::default();
    let mut round = common::open_round(
        7,
        vec![w.spec.clone()],
        &w.reg,
        &w.cas,
        CFG,
        10,
        &mut ledger,
    )
    .expect("open");
    let shard = w.spec.shard_id();
    // L20-F4: miners can recompute the tag they sign from the published round data.
    assert_eq!(
        round.tag(),
        onyx_coordinator::round::round_tag("onyx.devnet", &ledger.id(), 7, &CFG, &[shard])
    );
    let host = Host::new().expect("host");
    let miner = |n: u8| Miner::new(keccak256(&[n, 0x5a]).0).expect("key");
    let (a, b, c, d, e, f) = (
        miner(0xa),
        miner(0xb),
        miner(0xc),
        miner(0xd),
        miner(0xe),
        miner(0xf),
    );

    // Honest miners compute from their own copy of the blocks.
    let wa = a
        .work(7, &w.spec, w.source.clone(), &w.reg, &w.cas, &host)
        .expect("A");
    let wb = b
        .work(7, &w.spec, w.source.clone(), &w.reg, &w.cas, &host)
        .expect("B");
    assert_eq!(
        wa.commitment.root, wb.commitment.root,
        "same pinned input, same root"
    );
    assert_ne!(
        wa.commit_hash, wb.commit_hash,
        "commits differ by miner and salt"
    );
    assert_eq!(
        format!("{}", wa.commitment.root),
        "0xf912872a71710d45d000e4c2c3771e7f31e51e0fe3d0bad0557d8fedca3d86b2",
        "golden root"
    );
    // L18-F1: nothing about the answer is in the store during the commit phase.
    assert!(!w.cas.has(Kind::Rows, &wa.commitment.root));

    // Lazy miner: invents a root and does not compute.
    let mut lazy = wa.clone();
    let fake_rows = vec![b"made up!".to_vec()];
    lazy.commitment.root = onyx_merkle::MerkleTree::from_rows(&fake_rows).root();
    lazy.commitment.leaf_count = 1;
    lazy.rows = fake_rows.clone();
    lazy.salt = c.salt(7, &shard);
    lazy.commit_hash = onyx_core::commit_hash(
        7,
        &shard,
        &lazy.commitment.binding_hash(),
        &c.address,
        &lazy.salt,
    );

    // Wrong-label miner: computes with another label set.
    let mut half = w.full.clone();
    half.name = "test.onyx.entities.btc.half".into();
    half.labels.truncate(10);
    w.cas.put_label_set(&half).expect("half");
    let mut wrong_spec = w.spec.clone();
    wrong_spec.label_set = Some(LabelSetRef {
        name: half.name.clone(),
        version: half.version,
        content_hash: half.content_hash(),
    });
    let mut wf = f
        .work(7, &wrong_spec, w.source.clone(), &w.reg, &w.cas, &host)
        .expect("F");
    wf.shard = shard;
    wf.commit_hash = onyx_core::commit_hash(
        7,
        &shard,
        &wf.commitment.binding_hash(),
        &f.address,
        &wf.salt,
    );

    // Commit phase (every commit signed by its miner).
    assert_eq!(round.phase(50), RoundPhase::Commit);
    assert_eq!(
        round.commit(50, &a.sign_commit(&wa, 1_000, &round.tag()), &mut ledger),
        Ok(0)
    );
    assert_eq!(
        round.commit(51, &b.sign_commit(&wb, 5_000, &round.tag()), &mut ledger),
        Ok(1)
    );
    assert_eq!(
        round.commit(52, &c.sign_commit(&lazy, 1_000, &round.tag()), &mut ledger),
        Ok(2)
    );
    // The copier posts A's commit hash, signed with its own key.
    let mut copied = wa.clone();
    copied.salt = d.salt(7, &shard);
    assert_eq!(
        round.commit(
            53,
            &d.sign_commit(&copied, 1_000, &round.tag()),
            &mut ledger
        ),
        Ok(3)
    );
    assert_eq!(
        round.commit(54, &e.sign_commit(&wa, 1_000, &round.tag()), &mut ledger),
        Ok(4)
    );
    assert_eq!(
        round.commit(55, &f.sign_commit(&wf, 1_000, &round.tag()), &mut ledger),
        Ok(5)
    );
    assert_eq!(
        round.commit(56, &a.sign_commit(&wa, 1_000, &round.tag()), &mut ledger),
        Err(RoundError::DuplicateCommit)
    );
    // L18-F2: a griefer cannot commit in another miner's name.
    let victim = miner(0x77);
    let mut forged = victim.sign_commit(&wa, 1_000_000, &round.tag());
    forged.sig = c.sign_commit(&wa, 1_000_000, &round.tag()).sig;
    assert_eq!(
        round.commit(56, &forged, &mut ledger),
        Err(RoundError::BadSignature)
    );
    assert_eq!(
        round.commit(56, &victim.sign_commit(&wa, 999, &round.tag()), &mut ledger),
        Err(RoundError::BondTooLow)
    );
    let mut elsewhere = wa.clone();
    elsewhere.shard = keccak256(b"no such shard");
    assert_eq!(
        round.commit(
            57,
            &victim.sign_commit(&elsewhere, 1_000, &round.tag()),
            &mut ledger
        ),
        Err(RoundError::UnknownShard)
    );
    // No reveal while commits are open, no commit after they close.
    assert_eq!(
        round.reveal(60, a.address, shard, wa.commitment.clone(), wa.salt, &w.cas),
        Err(RoundError::WrongPhase(RoundPhase::Commit))
    );
    assert_eq!(
        round.commit(
            100,
            &victim.sign_commit(&wa, 1_000, &round.tag()),
            &mut ledger
        ),
        Err(RoundError::WrongPhase(RoundPhase::Reveal))
    );

    // L19-F2: a rejected call does not move time forward.
    assert_eq!(
        round.commit(
            250,
            &victim.sign_commit(&wa, 1_000, &round.tag()),
            &mut ledger
        ),
        Err(RoundError::WrongPhase(RoundPhase::Validate))
    );
    // L19-F3: a signed commit cannot be replayed into another round instance.
    let other_cfg = RoundConfig {
        commit_close: 101,
        ..CFG
    };
    let mut other_ledger = Ledger::<BtcRules>::default();
    let mut other = common::open_round(
        7,
        vec![w.spec.clone()],
        &w.reg,
        &w.cas,
        other_cfg,
        10,
        &mut other_ledger,
    )
    .expect("other round");
    assert_eq!(
        other.commit(
            50,
            &a.sign_commit(&wa, 1_000, &round.tag()),
            &mut other_ledger
        ),
        Err(RoundError::BadSignature)
    );

    // Reveal phase: rows are published now, not before.
    for (m, wk) in [(&a, &wa), (&b, &wb)] {
        m.publish_rows(wk, &w.cas).expect("rows");
    }
    let reveal = |r: &mut Round, m: &Miner, c: &Commitment, salt| {
        r.reveal(150, m.address, shard, c.clone(), salt, &w.cas)
    };
    assert_eq!(reveal(&mut round, &a, &wa.commitment, wa.salt), Ok(()));
    assert_eq!(reveal(&mut round, &b, &wb.commitment, wb.salt), Ok(()));
    // L18-F3: after an accepted call at 150, heights never go backwards.
    assert_eq!(
        round.commit(
            149,
            &victim.sign_commit(&wa, 1_000, &round.tag()),
            &mut ledger
        ),
        Err(RoundError::TimeWentBack)
    );
    assert_eq!(
        reveal(&mut round, &a, &wa.commitment, wa.salt),
        Err(RoundError::AlreadyRevealed)
    );
    // Lazy miner's rows are not in the CAS: refused until they are.
    assert_eq!(
        reveal(&mut round, &c, &lazy.commitment, lazy.salt),
        Err(RoundError::RowsUnavailable)
    );
    c.publish_rows(&lazy, &w.cas).expect("fake rows");
    assert_eq!(reveal(&mut round, &c, &lazy.commitment, lazy.salt), Ok(()));
    // Copier replays A's revealed commitment and salt: bound to A's address, so it fails.
    assert_eq!(
        reveal(&mut round, &d, &wa.commitment, wa.salt),
        Err(RoundError::CommitMismatch)
    );
    // Wrong-label miner: its commitment pins another label set.
    f.publish_rows(&wf, &w.cas).expect("rows");
    assert_eq!(
        reveal(&mut round, &f, &wf.commitment, wf.salt),
        Err(RoundError::WrongShard)
    );
    // E never reveals.

    // Close: honest root agreed by A and B, lazy root alone, D/E/F forfeit.
    assert!(matches!(round.close(160), Err(RoundError::WrongPhase(_))));
    let out = round.close(200).expect("close");
    let o = &out[&shard];
    assert_eq!(o.first().map(|x| x.1), Some(a.address));
    let mut honest = vec![a.address, b.address];
    honest.sort();
    let mut got = o.roots[&wa.commitment.root].clone();
    got.sort();
    assert_eq!(got, honest);
    assert_eq!(o.roots[&lazy.commitment.root], vec![c.address]);
    let mut forfeited: Vec<_> = o.forfeited.iter().map(|x| x.0).collect();
    forfeited.sort();
    let mut want = vec![d.address, e.address, f.address];
    want.sort();
    assert_eq!(forfeited, want);
    // L18-F3: a closed round stays closed and its outcome does not change.
    assert_eq!(
        round.reveal(
            201,
            e.address,
            shard,
            wa.commitment.clone(),
            e.salt(7, &shard),
            &w.cas
        ),
        Err(RoundError::Closed)
    );
    assert_eq!(round.close(500).expect("again"), out);
    // Rows behind the honest root are retrievable by anyone who checks.
    assert_eq!(
        w.cas.rows(&wa.commitment.root).map(|r| r.len() as u64),
        Some(wa.commitment.leaf_count)
    );
}

/// L19-F4, L24-F5: work opened in a round is taken in the ledger that opened
/// it, so no other round in that ledger offers it again, under any id.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn settled_shard_not_reoffered() {
    let w = world();
    publish_inputs(&w);
    let mut ledger = Ledger::<BtcRules>::default();
    common::open_round(
        8,
        vec![w.spec.clone()],
        &w.reg,
        &w.cas,
        CFG,
        10,
        &mut ledger,
    )
    .expect("open");
    assert!(ledger.open_work().contains(&w.spec.work_id()));
    assert!(matches!(
        common::open_round(
            9,
            vec![w.spec.clone()],
            &w.reg,
            &w.cas,
            CFG,
            10,
            &mut ledger
        ),
        Err(RoundError::AlreadySettled)
    ));
    // L24-F5: a round id is opened once per ledger.
    assert!(matches!(
        common::open_round(8, vec![], &w.reg, &w.cas, CFG, 10, &mut ledger),
        Err(RoundError::RoundExists)
    ));
    // L20-F2: re-pinning the same work to a changed registry is still the same work.
    let mut reg2 = fixture_registry();
    reg2.add_entity("entity:unrelated", onyx_core::EntityKind::Fund)
        .expect("entity");
    let mut spec2 = w.spec.clone();
    spec2.registry_root = reg2.root();
    assert_ne!(spec2.shard_id(), w.spec.shard_id());
    assert_eq!(spec2.work_id(), w.spec.work_id());
    assert!(matches!(
        common::open_round(10, vec![spec2], &reg2, &w.cas, CFG, 10, &mut ledger),
        Err(RoundError::AlreadySettled)
    ));
}

/// L24-F3: the validation rules are fixed and checked when the round opens.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn validation_rules_are_fixed_at_open() {
    let w = world();
    publish_inputs(&w);
    let with = |v: ValidationConfig| RoundConfig {
        validation: v,
        ..CFG
    };
    let open = |cfg: RoundConfig| {
        common::open_round(
            1,
            vec![w.spec.clone()],
            &w.reg,
            &w.cas,
            cfg,
            10,
            &mut Ledger::<BtcRules>::default(),
        )
        .err()
    };
    let v = CFG.validation;
    // Rate 0: nothing would ever be checked.
    assert_eq!(
        open(with(ValidationConfig { rate_ppm: 0, ..v })),
        Some(RoundError::BadValidationConfig)
    );
    // L26-F2: voiding may not start right after the canary deadline.
    assert_eq!(
        open(with(ValidationConfig {
            pending_close: v.canary_open_close + MIN_PENDING_GAP - 1,
            ..v
        })),
        Some(RoundError::BadValidationConfig)
    );
    // L26-F4: validator deterrence is computed at the canary density.
    assert_eq!(
        open(with(ValidationConfig {
            min_canary_ppm: 0,
            ..v
        })),
        Some(RoundError::BadValidationConfig)
    );
    // 2,000 x 0.4% < 10: a blind Agree would pay.
    assert_eq!(
        open(with(ValidationConfig {
            min_canary_ppm: 4_000,
            ..v
        })),
        Some(RoundError::BadValidationConfig)
    );
    // Rate too low to deter: bond 1,000 x 10% < reward 500.
    assert_eq!(
        open(with(ValidationConfig {
            rate_ppm: 100_000,
            ..v
        })),
        Some(RoundError::BadValidationConfig)
    );
    // L22-F10: no reveal window.
    assert_eq!(
        open(with(ValidationConfig {
            verdict_reveal_close: v.verdict_commit_close,
            ..v
        })),
        Some(RoundError::BadValidationConfig)
    );
    // L25-F8: validators must be deterred too: no free wrong verdict, no
    // free silence, and a wrong verdict costs at least as much as silence.
    for bad in [
        ValidationConfig {
            validator_slash: 0,
            missed_slash: 0,
            ..v
        },
        ValidationConfig {
            missed_slash: 0,
            ..v
        },
        ValidationConfig {
            validator_slash: v.missed_slash - 1,
            ..v
        },
        ValidationConfig {
            validator_fee: v.validator_slash + 1,
            ..v
        },
    ] {
        assert_eq!(
            open(with(bad)),
            Some(RoundError::BadValidationConfig),
            "{bad:?}"
        );
    }
    // L25-F5: the void deadline cannot come before the canary deadline.
    assert_eq!(
        open(with(ValidationConfig {
            pending_close: v.canary_open_close - 1,
            ..v
        })),
        Some(RoundError::BadValidationConfig)
    );
    // M6-L44-F3: the seed must be drawable at MIN_DRAW_SLACK heights, each
    // leaving a full verdict window, exactly as the contract's _checkConfig.
    let tight = CFG.beacon_height + MIN_VERDICT_WINDOW + MIN_DRAW_SLACK;
    assert_eq!(
        open(with(ValidationConfig {
            verdict_commit_close: tight - 1,
            ..v
        })),
        Some(RoundError::BadValidationConfig)
    );
    assert_eq!(
        open(with(ValidationConfig {
            verdict_commit_close: tight,
            ..v
        })),
        None
    );
    // Canary deadline before verdict reveals close.
    assert_eq!(
        open(with(ValidationConfig {
            canary_open_close: v.verdict_reveal_close - 1,
            ..v
        })),
        Some(RoundError::BadValidationConfig)
    );
    // The rules are in the tag miners sign: another rate is another round.
    let lid = keccak256(b"ledger id");
    let a = onyx_coordinator::round::round_tag("onyx.devnet", &lid, 1, &CFG, &[w.spec.shard_id()]);
    let b = onyx_coordinator::round::round_tag(
        "onyx.devnet",
        &lid,
        1,
        &with(ValidationConfig {
            rate_ppm: 900_000,
            ..v
        }),
        &[w.spec.shard_id()],
    );
    assert_ne!(a, b);
}

/// L25-F1: a recipe the canonical engine refuses never opens a round, so no
/// invented root can be finalized for a shard nobody can compute.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn recipe_the_engine_refuses_never_opens() {
    let w = world();
    publish_inputs(&w);
    let float = std::fs::read(root().join("recipes/out/test_float.wasm")).expect("float recipe");
    w.cas.put(Kind::Recipe, &float).expect("put");
    let mut spec = w.spec.clone();
    spec.recipe = Some(keccak256(&float));
    let r = common::open_round(
        1,
        vec![spec],
        &w.reg,
        &w.cas,
        CFG,
        10,
        &mut Ledger::<BtcRules>::default(),
    );
    assert!(
        matches!(r, Err(RoundError::RecipeRejected(_))),
        "{:?}",
        r.err()
    );
}
