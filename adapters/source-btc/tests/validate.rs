//! M4 end to end on real blocks (ADR-0019, loops 25-27). Truth is the reference
//! root: a unanimous undisputed root, or a rerun `settle` does itself from
//! the round's pinned specs. Canaries are reveals the coordinator made;
//! validators are a bonded registry; the ledger that opened a round is the
//! only one its settlement applies to.

mod common;

use onyx_coordinator::cas::{Cas, CasExt, Kind, MemCas};
use onyx_coordinator::dispute::DisputeConfig;
use onyx_coordinator::recompute::RecomputeEnv;
use onyx_coordinator::round::RoundError;
use onyx_coordinator::round::{Round, RoundConfig};
use onyx_coordinator::validate::{
    canary_commit, Canary, Ledger, LedgerEntry, RevealStatus, SettlePath, Validation,
    ValidationConfig, ValidationError, ValidatorSet, Verdict, MIN_VERDICT_WINDOW,
};
use onyx_core::{
    keccak256, AddressProof, DataRights, EntityKind, Hash32, LabelSetRef, MetricInfo, MinerAddress,
    ProofOutcome, ProofVector, Registry, RuleSet, RuntimeProfile, ShardSpec, SourceAdapter,
    SourceId, SourceRef, StaticRegistry, ValueKind,
};
use onyx_labels::attest::AttestCache;
use onyx_miner::{Miner, Work};
use onyx_recipe_host::{Host, HOST_ID};
use onyx_source_btc::{
    fixture_registry, fixture_registry_series, load_label_set, Bip322Proof, BtcAddressCanon,
    BtcFixtureAdapter, BtcRules, CHAIN_ID, DEVNET_DOMAIN, METRIC_ENTITY_FLOW,
    SCHEMA_ENTITY_FLOW_SERIES, SOURCE_ID,
};
use onyx_validator::Validator;
use std::any::TypeId;
use std::path::PathBuf;

/// Golden root of blocks 921,080-921,082 with the full test label set
/// (`fixtures/golden/btc-entity-balance-921080-921082.json`).
const GOLDEN_ROOT: &str = "0xf912872a71710d45d000e4c2c3771e7f31e51e0fe3d0bad0557d8fedca3d86b2";

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

struct World {
    reg: StaticRegistry,
    cas: MemCas,
    spec: ShardSpec,
    /// Same blocks, a smaller label set: other work, for a second round.
    spec_small: ShardSpec,
    source: Vec<u8>,
    host: Host,
}

fn world() -> World {
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let reg = fixture_registry();
    let fixtures = root.join("fixtures");
    let adapter = BtcFixtureAdapter::new(&fixtures, "btc/blocks-921080-921082.json");
    let snapshot = adapter.range_snapshot().expect("snapshot");
    let source = adapter.load(&snapshot).expect("load");
    let labels =
        load_label_set(&fixtures, "labels/test.onyx.entities.btc.v1.json").expect("labels");
    let mut small = labels.clone();
    small.name = "test.onyx.entities.btc.small".into();
    small.labels.truncate(10);
    let wasm = std::fs::read(root.join("recipes/out/btc_entity_balance.wasm")).expect("wasm");
    let cas = MemCas::new();
    cas.put(Kind::Recipe, &wasm).expect("recipe");
    cas.put_label_set(&labels).expect("labels");
    cas.put_label_set(&small).expect("small labels");
    cas.put(Kind::Blob, &[]).expect("params");
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
    let mut spec_small = spec.clone();
    spec_small.label_set = Some(LabelSetRef {
        name: small.name.clone(),
        version: small.version,
        content_hash: small.content_hash(),
    });
    World {
        reg,
        cas,
        spec,
        spec_small,
        source,
        host: Host::new().expect("host"),
    }
}

/// Validation with the binary's built-in Bitcoin address rules (L25-F2).
type Val = Validation<BtcRules>;

fn key(s: &str) -> [u8; 32] {
    keccak256(s.as_bytes()).0
}

/// Inputs for settlement reruns: the source bytes come from `source`.
fn env<'a>(w: &'a World, source: &'a dyn Fn(&ShardSpec) -> Option<Vec<u8>>) -> RecomputeEnv<'a> {
    RecomputeEnv {
        reg: &w.reg,
        cas: &w.cas,
        host: &w.host,
        source,
    }
}

/// A reveal with an invented root, as a lazy miner or a wrong canary makes.
fn invent(base: &Work, m: &Miner, round: u64, text: &str) -> Work {
    let shard = base.shard;
    let mut x = base.clone();
    x.rows = vec![text.as_bytes().to_vec()];
    x.commitment.root = onyx_merkle::MerkleTree::from_rows(&x.rows).root();
    x.commitment.leaf_count = 1;
    x.salt = m.salt(round, &shard);
    x.commit_hash = onyx_core::commit_hash(
        round,
        &shard,
        &x.commitment.binding_hash(),
        &m.address,
        &x.salt,
    );
    x
}

/// Run a round of `spec` in `ledger` with the given (miner, work) list, in
/// commit order. The validator set and canary commitment are fixed in the
/// round config.
fn run_round<R: RuleSet>(
    w: &World,
    spec: &ShardSpec,
    round: u64,
    works: &[(&Miner, &Work)],
    ledger: &mut Ledger<R>,
    set: &ValidatorSet,
    canary_committed: Hash32,
) -> Round {
    let cfg = RoundConfig {
        validator_set_root: set.root(),
        canary_commit: canary_committed,
        ..CFG
    };
    let mut r = common::open_round(round, vec![spec.clone()], &w.reg, &w.cas, cfg, 10, ledger)
        .expect("open");
    let tag = r.tag();
    for (n, (m, wk)) in works.iter().enumerate() {
        r.commit(50 + n as u64, &m.sign_commit(wk, 1_000, &tag), ledger)
            .expect("commit");
    }
    for (m, wk) in works {
        m.publish_rows(wk, &w.cas).expect("rows");
        r.reveal(
            150,
            m.address,
            spec.shard_id(),
            wk.commitment.clone(),
            wk.salt,
            &w.cas,
        )
        .expect("reveal");
    }
    r.close(200).expect("close");
    r
}

/// How a validator decides a verdict for (shard, miner, revealed root).
type Decide<'a> = dyn Fn(Hash32, MinerAddress, Hash32) -> Verdict + 'a;

/// Every assigned validator commits and reveals its verdict.
fn vote(val: &mut Val, voters: &[(&Validator, &Decide<'_>)]) {
    let seed = val.seed();
    let mut pending = Vec::new();
    for (v, decide) in voters {
        for (s, m, r) in val.tasks_for(&v.address) {
            let verdict = decide(s, m, r);
            val.commit_verdict(250, &v.commit(&seed, &s, &m, &verdict))
                .expect("commit verdict");
            pending.push((v.address, s, m, verdict, v.salt(&seed, &s, &m)));
        }
    }
    for (a, s, m, verdict, salt) in pending {
        val.reveal_verdict(350, a, s, m, verdict, salt)
            .expect("reveal verdict");
    }
}

/// Bonded validators who give no verdict. A shard is only taken on
/// unanimity when enough of them exist to draw a dispute panel (L29-F4).
fn spare_validators(n: usize) -> Vec<Validator> {
    (0..n)
        .map(|i| Validator::new(key(&format!("spare validator {i}"))).expect("spare"))
        .collect()
}

fn validators(vs: &[&Validator]) -> ValidatorSet {
    let mut set = ValidatorSet::new();
    for v in vs {
        set.add(v.address, 5_000, 1_000).expect("member");
    }
    set
}

/// Deterministic recompute of a shard, as an honest validator does it.
fn truth(w: &World, spec: &ShardSpec) -> Hash32 {
    onyx_merkle::MerkleTree::from_rows(
        &onyx_miner::compute(
            spec,
            w.source.clone(),
            &w.reg,
            &w.cas,
            &w.host,
            &AttestCache::new(),
        )
        .expect("recompute"),
    )
    .root()
}

fn sorted_canaries(list: &[(&Miner, &Work)], shard: Hash32) -> Vec<Canary> {
    let mut c: Vec<Canary> = list
        .iter()
        .map(|(m, wk)| Canary {
            shard,
            miner: m.address,
            root: wk.commitment.root,
        })
        .collect();
    c.sort_by_key(|c| (c.shard, c.miner));
    c
}

fn slashed(s: &[LedgerEntry], who: &MinerAddress) -> bool {
    s.iter()
        .any(|e| matches!(e, LedgerEntry::Slash { actor, .. } if actor == who))
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn settle_reruns_disputes_and_lazy_actors_lose() {
    let w = world();
    let shard = w.spec.shard_id();
    let truth = truth(&w, &w.spec);
    assert_eq!(truth.to_string(), GOLDEN_ROOT);
    let a = Miner::new(key("miner a")).expect("a");
    let b = Miner::new(key("miner b")).expect("b");
    let c1 = Miner::new(key("lazy c1")).expect("c1");
    let c2 = Miner::new(key("lazy c2")).expect("c2");
    let x1 = Miner::new(key("canary x1")).expect("x1");
    let x2 = Miner::new(key("canary x2")).expect("x2");
    let x3 = Miner::new(key("canary x3")).expect("x3");
    let wa = a
        .work(9, &w.spec, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("A");
    let wb = b
        .work(9, &w.spec, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("B");
    // Two lazy identities share one invented root (L22-F5); three canaries share
    // another wrong root, making a wrong root the plurality (L22-F6).
    let wc1 = invent(&wa, &c1, 9, "lazy root, 8+ bytes");
    let wc2 = invent(&wa, &c2, 9, "lazy root, 8+ bytes");
    let wx1 = invent(&wa, &x1, 9, "canary wrong root");
    let wx2 = invent(&wa, &x2, 9, "canary wrong root");
    let wx3 = invent(&wa, &x3, 9, "canary wrong root");
    let canaries = sorted_canaries(&[(&x1, &wx1), (&x2, &wx2), (&x3, &wx3)], shard);
    let canary_salt = keccak256(b"canary salt");
    let committed = canary_commit(&canaries, &canary_salt).expect("sorted list");

    let honest = Validator::new(key("validator honest")).expect("v1");
    let plurality = Validator::new(key("validator lazy plurality")).expect("v2");
    let griefer = Validator::new(key("validator griefer")).expect("v3");
    let set = validators(&[&honest, &plurality, &griefer]);

    let mut ledger = Ledger::<BtcRules>::default();
    let round = run_round(
        &w,
        &w.spec,
        9,
        &[
            (&a, &wa),
            (&c1, &wc1),
            (&x1, &wx1),
            (&b, &wb),
            (&c2, &wc2),
            (&x2, &wx2),
            (&x3, &wx3),
        ],
        &mut ledger,
        &set,
        committed,
    );
    // L24-F5: opening the round marked its work open.
    assert!(ledger.open_work().contains(&w.spec.work_id()));
    let outcome = round.outcome().expect("closed").clone();
    // L23-F2: a validator set other than the one fixed in the round is refused.
    let other = validators(&[&honest]);
    assert!(matches!(
        Val::new(&round, other, keccak256(b"beacon"), 201, &mut ledger),
        Err(ValidationError::BadValidator)
    ));
    // L24-F3: a ledger that did not open the round cannot host its validation.
    assert!(matches!(
        Val::new(
            &round,
            set.clone(),
            keccak256(b"beacon"),
            201,
            &mut Ledger::<BtcRules>::default()
        ),
        Err(ValidationError::NotOpenedHere)
    ));
    let mut val =
        Val::new(&round, set.clone(), keccak256(b"beacon"), 201, &mut ledger).expect("validation");
    // L24-F3: one validation per round; a second beacon cannot be tried.
    assert!(matches!(
        Val::new(&round, set, keccak256(b"beacon 2"), 201, &mut ledger),
        Err(ValidationError::AlreadyValidating)
    ));

    // L22-F1: a key outside the registry gets no tasks and cannot commit.
    let sybil = Validator::new(key("sybil")).expect("sybil");
    assert!(val.tasks_for(&sybil.address).is_empty());
    let (s0, m0, _) = val.tasks_for(&honest.address)[0];
    assert_eq!(
        val.commit_verdict(250, &sybil.commit(&val.seed(), &s0, &m0, &Verdict::Agree)),
        Err(ValidationError::BadValidator)
    );

    let plural_root = *outcome[&shard]
        .roots
        .iter()
        .max_by_key(|(_, ms)| ms.len())
        .map(|(r, _)| r)
        .expect("roots");
    let judge = |_s: Hash32, _m: MinerAddress, r: Hash32| {
        if r == truth {
            Verdict::Agree
        } else {
            Verdict::Disagree { root: truth }
        }
    };
    let lazy = |_s: Hash32, _m: MinerAddress, r: Hash32| {
        if r == plural_root {
            Verdict::Agree
        } else {
            Verdict::Disagree { root: plural_root }
        }
    };
    let grief = |_s: Hash32, _m: MinerAddress, r: Hash32| Verdict::Disagree {
        root: keccak256(&[r.0.as_slice(), b"grief"].concat()),
    };
    vote(
        &mut val,
        &[(&honest, &judge), (&plurality, &lazy), (&griefer, &grief)],
    );

    // L24-F1: the disputed shard is rerun by settle itself; nobody supplies a root.
    let src = |_: &ShardSpec| Some(w.source.clone());
    let st = val
        .settle(400, Some((&canaries, &canary_salt)), &env(&w, &src))
        .expect("settle");
    assert_eq!(st.references()[&shard], (truth, SettlePath::Recomputed));
    let ss = st.statuses();
    assert_eq!(ss[&(shard, a.address)], RevealStatus::Correct);
    assert_eq!(ss[&(shard, b.address)], RevealStatus::Correct);
    assert_eq!(ss[&(shard, c1.address)], RevealStatus::Wrong);
    assert_eq!(ss[&(shard, c2.address)], RevealStatus::Wrong);
    assert_eq!(ss[&(shard, x1.address)], RevealStatus::Canary);
    assert_eq!(st.finalized().get(&shard), Some(&truth));
    // L24-F7: a settlement only applies to the ledger that opened its round.
    assert!(!Ledger::<BtcRules>::default().apply(&st));
    assert!(ledger.apply(&st));
    // L22-F9: applying the same settlement again changes nothing.
    assert!(!ledger.apply(&st));

    assert_eq!(ledger.balance(&a.address), 500);
    assert_eq!(ledger.balance(&b.address), 0);
    assert_eq!(ledger.balance(&c1.address), -1_000);
    assert_eq!(ledger.balance(&c2.address), -1_000);
    assert_eq!(ledger.balance(&honest.address), 3 * 10);
    assert!(ledger.balance(&plurality.address) < 0);
    assert!(ledger.balance(&griefer.address) < 0);
    // L23-F8: total slashes never exceed the bond (5,000).
    assert!(ledger.balance(&griefer.address) >= -5_000);
    assert_eq!(ledger.finalized().get(&w.spec.work_id()), Some(&(9, truth)));
    assert!(ledger.taken().contains(&w.spec.work_id()));
    assert!(!ledger.open_work().contains(&w.spec.work_id()));
}

/// L24-F1: a source that lies (bytes that are not the pinned snapshot) or is
/// missing leaves the shard pending; nobody is slashed on it. A later call
/// with the real bytes settles it. L23-F3: canaries are checked once.
/// L24-F2: a miner known when a round opened can never be its canary.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn lying_source_leaves_shard_pending_and_known_miners_are_never_canaries() {
    let w = world();
    let shard = w.spec.shard_id();
    let truth = truth(&w, &w.spec);
    let a = Miner::new(key("miner a")).expect("a");
    let x = Miner::new(key("canary x")).expect("x");
    let wa = a
        .work(5, &w.spec, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("A");
    let wx = invent(&wa, &x, 5, "canary wrong root");
    let canaries = sorted_canaries(&[(&x, &wx)], shard);
    let salt = keccak256(b"s");
    let v = Validator::new(key("validator honest")).expect("v");
    let set = validators(&[&v]);
    let mut ledger = Ledger::<BtcRules>::default();
    let round = run_round(
        &w,
        &w.spec,
        5,
        &[(&x, &wx), (&a, &wa)],
        &mut ledger,
        &set,
        canary_commit(&canaries, &salt).expect("list"),
    );
    let mut val =
        Val::new(&round, set, keccak256(b"beacon"), 201, &mut ledger).expect("validation");
    // L23-F7: blind Agree on the canary's wrong root is judged by the rerun.
    let blind = |_: Hash32, _: MinerAddress, _: Hash32| Verdict::Agree;
    vote(&mut val, &[(&v, &blind)]);
    // A lying source: one byte changed. The hash check refuses it.
    let lie = |_: &ShardSpec| {
        let mut b = w.source.clone();
        b[100] ^= 1;
        Some(b)
    };
    let first = val
        .settle(400, Some((&canaries, &salt)), &env(&w, &lie))
        .expect("first settle");
    assert!(first.pending().contains(&shard));
    assert_eq!(first.statuses()[&(shard, a.address)], RevealStatus::Pending);
    assert!(!slashed(first.entries(), &a.address));
    assert!(ledger.apply(&first));
    // A missing source: still pending.
    let none = |_: &ShardSpec| None;
    let still = val.settle(410, None, &env(&w, &none)).expect("still");
    assert!(still.pending().contains(&shard));
    // L23-F3: with the canary now a known miner, a later settle still works.
    let real = |_: &ShardSpec| Some(w.source.clone());
    let later = val.settle(500, None, &env(&w, &real)).expect("later");
    assert_eq!(later.statuses()[&(shard, a.address)], RevealStatus::Correct);
    assert_eq!(later.references()[&shard], (truth, SettlePath::Recomputed));
    assert!(ledger.apply(&later));
    assert_eq!(ledger.balance(&a.address), 500);
    // The blind validator agreed with the wrong canary root: slashed.
    assert!(ledger.balance(&v.address) < 0);

    // L22-F2 stealing, L24-F2: `a` has history now. A round opened in this
    // ledger copies that, so listing `a` as a canary is refused, whatever
    // happens later; settle takes no ledger to swap.
    let wa2 = a
        .work(6, &w.spec_small, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("A2");
    let steal = sorted_canaries(&[(&a, &wa2)], w.spec_small.shard_id());
    let set2 = validators(&[&v]);
    let round2 = run_round(
        &w,
        &w.spec_small,
        6,
        &[(&a, &wa2)],
        &mut ledger,
        &set2,
        canary_commit(&steal, &salt).expect("list"),
    );
    assert!(ledger.known_miners().contains(&a.address));
    let mut val2 = Val::new(&round2, set2, keccak256(b"b2"), 201, &mut ledger).expect("v2");
    let never = |_: &ShardSpec| -> Option<Vec<u8>> { panic!("not reached") };
    assert_eq!(
        val2.settle(400, Some((&steal, &salt)), &env(&w, &never))
            .err(),
        Some(ValidationError::CanaryMismatch)
    );
}

/// L24-F4: a canary list the coordinator never opens cannot block settlement:
/// from `canary_open_close` on, settle proceeds without canaries, and the
/// coordinator's wrong reveals lose their bonds like anyone's.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn unopened_canaries_do_not_block_settlement() {
    let w = world();
    let shard = w.spec.shard_id();
    let a = Miner::new(key("miner a")).expect("a");
    let x = Miner::new(key("canary x")).expect("x");
    let wa = a
        .work(7, &w.spec, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("A");
    let wx = invent(&wa, &x, 7, "canary wrong root");
    let canaries = sorted_canaries(&[(&x, &wx)], shard);
    let v = Validator::new(key("validator honest")).expect("v");
    let set = validators(&[&v]);
    let mut ledger = Ledger::<BtcRules>::default();
    let round = run_round(
        &w,
        &w.spec,
        7,
        &[(&a, &wa), (&x, &wx)],
        &mut ledger,
        &set,
        canary_commit(&canaries, &keccak256(b"s")).expect("list"),
    );
    let mut val =
        Val::new(&round, set, keccak256(b"beacon"), 201, &mut ledger).expect("validation");
    let real = |_: &ShardSpec| Some(w.source.clone());
    // Before the deadline the list must be opened.
    assert_eq!(
        val.settle(420, None, &env(&w, &real)).err(),
        Some(ValidationError::WrongPhase)
    );
    let st = val
        .settle(CFG.validation.canary_open_close, None, &env(&w, &real))
        .expect("settle after the deadline");
    assert_eq!(st.statuses()[&(shard, a.address)], RevealStatus::Correct);
    assert_eq!(st.statuses()[&(shard, x.address)], RevealStatus::Wrong);
    assert!(ledger.apply(&st));
    assert_eq!(ledger.balance(&x.address), -1_000);
    assert_eq!(ledger.balance(&a.address), 500);
}

/// Review L2-R2 (ADR-0022 Amendment 1): `min_bond >= validator_fee x members`,
/// as the contract's `openRound`. At bond 1,000 and fee 10, a round validated
/// by 101 members is refused (a wrong root's bond would not cover one extra
/// rerun fee per member); 100 members is the largest set allowed.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn fees_above_the_bond_per_member_are_refused() {
    let w = world();
    let a = Miner::new(key("miner a")).expect("a");
    let wa = a
        .work(4, &w.spec, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("A");
    let salt = keccak256(b"s");
    for (n, ok) in [(101, false), (100, true)] {
        let spares = spare_validators(n);
        let set = validators(&spares.iter().collect::<Vec<_>>());
        let mut ledger = Ledger::<BtcRules>::default();
        let round = run_round(
            &w,
            &w.spec,
            4,
            &[(&a, &wa)],
            &mut ledger,
            &set,
            canary_commit(&[], &salt).expect("empty"),
        );
        let got = Val::new(&round, set, keccak256(b"beacon"), 201, &mut ledger);
        if ok {
            assert!(got.is_ok(), "{n} members x fee 10 = bond 1,000 is allowed");
        } else {
            assert!(matches!(got, Err(ValidationError::FeesExceedBond)));
        }
    }
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn honest_unanimous_shard_settles_without_recompute() {
    let w = world();
    let a = Miner::new(key("miner a")).expect("a");
    let b = Miner::new(key("miner b")).expect("b");
    let wa = a
        .work(4, &w.spec, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("A");
    let wb = b
        .work(4, &w.spec, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("B");
    let v = Validator::new(key("validator honest")).expect("v");
    let spares = spare_validators(4);
    let mut all = vec![&v];
    all.extend(spares.iter());
    let set = validators(&all);
    let salt = keccak256(b"s");
    let mut ledger = Ledger::<BtcRules>::default();
    let round = run_round(
        &w,
        &w.spec,
        4,
        &[(&a, &wa), (&b, &wb)],
        &mut ledger,
        &set,
        canary_commit(&[], &salt).expect("empty"),
    );
    let mut val =
        Val::new(&round, set, keccak256(b"beacon"), 201, &mut ledger).expect("validation");
    let agree = |_: Hash32, _: MinerAddress, _: Hash32| Verdict::Agree;
    vote(&mut val, &[(&v, &agree)]);
    // A unanimous, undisputed shard is never rerun.
    let never = |_: &ShardSpec| -> Option<Vec<u8>> { panic!("not consulted") };
    let s = val
        .settle(400, Some((&[], &salt)), &env(&w, &never))
        .expect("settle");
    let shard = w.spec.shard_id();
    assert_eq!(s.statuses()[&(shard, a.address)], RevealStatus::Correct);
    assert_eq!(s.references()[&shard].1, SettlePath::Unanimous);
    assert!(ledger.apply(&s));
    assert_eq!(ledger.balance(&a.address), 500);
    assert_eq!(ledger.balance(&v.address), 10);
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn self_assignment_is_refused() {
    let w = world();
    let a = Miner::new(key("miner and validator")).expect("a");
    let va = Validator::new(key("miner and validator")).expect("same key");
    assert_eq!(a.address, va.address);
    let wa = a
        .work(3, &w.spec, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("A");
    let set = validators(&[&va]);
    let mut ledger = Ledger::<BtcRules>::default();
    let round = run_round(
        &w,
        &w.spec,
        3,
        &[(&a, &wa)],
        &mut ledger,
        &set,
        canary_commit(&[], &keccak256(b"s")).expect("empty"),
    );
    // Too late to start: the verdict commit window has closed, or fewer than
    // MIN_VERDICT_WINDOW heights remain; the round is void, as on the
    // contract (M6-L37-F2, M6-L44-F3). A refused start records nothing.
    for late in [
        CFG.validation.verdict_commit_close,
        CFG.validation.verdict_commit_close - MIN_VERDICT_WINDOW + 1,
    ] {
        assert!(matches!(
            Val::new(&round, set.clone(), keccak256(b"b"), late, &mut ledger),
            Err(ValidationError::SeedTooLate)
        ));
    }
    // L22-F1: a validator is never assigned its own reveal, even at 100%.
    let val = Val::new(&round, set, keccak256(b"b"), 201, &mut ledger).expect("validation");
    assert!(val.tasks_for(&va.address).is_empty());
}

/// Every assigned validator in `commit_only` commits a verdict and never
/// reveals it; every one in `full` commits and reveals.
fn vote_partly(
    val: &mut Val,
    commit_only: &[(&Validator, &Decide<'_>)],
    full: &[(&Validator, &Decide<'_>)],
) {
    let seed = val.seed();
    for (v, decide) in commit_only {
        for (s, m, r) in val.tasks_for(&v.address) {
            val.commit_verdict(250, &v.commit(&seed, &s, &m, &decide(s, m, r)))
                .expect("commit verdict");
        }
    }
    vote(val, full);
}

/// A disputed shard: an honest reveal by `a` and an invented one by `c`.
fn disputed_round(
    w: &World,
    id: u64,
    a: &Miner,
    c: &Miner,
    v: &Validator,
    ledger: &mut Ledger<BtcRules>,
) -> Round {
    let wa = a
        .work(id, &w.spec, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("A");
    let wc = invent(&wa, c, id, "lazy root, 8+ bytes");
    run_round(
        w,
        &w.spec,
        id,
        &[(a, &wa), (c, &wc)],
        ledger,
        &validators(&[v]),
        canary_commit(&[], &keccak256(b"s")).expect("empty"),
    )
}

/// L25-F1: "no valid root" is a dispute. A validator who says so on a shard
/// that has a root forces the rerun and is judged wrong by it.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn no_valid_root_verdict_is_a_dispute() {
    let w = world();
    let shard = w.spec.shard_id();
    let a = Miner::new(key("miner a")).expect("a");
    let wa = a
        .work(11, &w.spec, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("A");
    let v = Validator::new(key("validator says no root")).expect("v");
    let set = validators(&[&v]);
    let mut ledger = Ledger::<BtcRules>::default();
    let round = run_round(
        &w,
        &w.spec,
        11,
        &[(&a, &wa)],
        &mut ledger,
        &set,
        canary_commit(&[], &keccak256(b"s")).expect("empty"),
    );
    let mut val = Val::new(&round, set, keccak256(b"beacon"), 201, &mut ledger).expect("val");
    let no_root = |_: Hash32, _: MinerAddress, _: Hash32| Verdict::NoValidRoot;
    vote(&mut val, &[(&v, &no_root)]);
    let real = |_: &ShardSpec| Some(w.source.clone());
    let st = val
        .settle(400, Some((&[], &keccak256(b"s"))), &env(&w, &real))
        .expect("settle");
    // Not unanimous any more: rerun, and the rerun has a root.
    assert_eq!(st.references()[&shard].1, SettlePath::Recomputed);
    assert_eq!(st.statuses()[&(shard, a.address)], RevealStatus::Correct);
    assert!(ledger.apply(&st));
    assert_eq!(ledger.balance(&a.address), 500);
    assert_eq!(ledger.balance(&v.address), -2_000);
}

/// Same id, chain, window and vector results as BIP-322, so the same registry
/// root, but it refuses every real attestation (L25-F2).
struct LyingProof;

impl AddressProof for LyingProof {
    fn namespace(&self) -> &str {
        Bip322Proof.namespace()
    }
    fn chain(&self) -> &str {
        Bip322Proof.chain()
    }
    fn proof_id(&self) -> &str {
        Bip322Proof.proof_id()
    }
    fn max_window(&self) -> u64 {
        Bip322Proof.max_window()
    }
    fn vectors(&self) -> &'static [ProofVector] {
        Bip322Proof.vectors()
    }
    fn verify(&self, body: &str, message: &[u8], signature: &str) -> ProofOutcome {
        let vector = Bip322Proof
            .vectors()
            .iter()
            .any(|v| v.body == body && v.message.as_bytes() == message && v.signature == signature);
        if vector {
            Bip322Proof.verify(body, message, signature)
        } else {
            ProofOutcome::Invalid
        }
    }
}

/// `fixture_registry` with the lying proof in place of BIP-322.
fn lying_registry() -> StaticRegistry {
    let mut r = StaticRegistry::new();
    r.set_domain(DEVNET_DOMAIN)
        .and_then(|r| r.add_entity("entity:okx", EntityKind::Exchange))
        .and_then(|r| r.add_entity("entity:bitwise-bitb", EntityKind::Etf))
        .and_then(|r| r.add_source(DataRights::public_chain(SourceId(SOURCE_ID.into()))))
        .and_then(|r| r.add_canon(Box::new(BtcAddressCanon)))
        .and_then(|r| r.add_proof(Box::new(LyingProof)))
        .and_then(|r| r.add_aggregate(CHAIN_ID))
        .and_then(|r| {
            r.add_metric(
                METRIC_ENTITY_FLOW,
                MetricInfo {
                    value_kind: ValueKind::Bytes,
                    needs_label_set: true,
                },
            )
        })
        .expect("registry");
    r
}

/// L25-F2: a registry with the same root but other proof code is refused for
/// reruns: the disputed shard stays pending and nobody is slashed. The
/// built-in rules then settle it.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn registry_with_foreign_rule_code_is_refused() {
    let w = world();
    let shard = w.spec.shard_id();
    let lying = lying_registry();
    assert_eq!(lying.root(), w.reg.root(), "same root, different code");
    assert!(w.reg.rules_built_in::<BtcRules>());
    assert!(!lying.rules_built_in::<BtcRules>());
    let a = Miner::new(key("miner a")).expect("a");
    let c = Miner::new(key("lazy c")).expect("c");
    let v = Validator::new(key("validator honest")).expect("v");
    let mut ledger = Ledger::<BtcRules>::default();
    let round = disputed_round(&w, 12, &a, &c, &v, &mut ledger);
    let mut val = Val::new(
        &round,
        validators(&[&v]),
        keccak256(b"beacon"),
        201,
        &mut ledger,
    )
    .expect("val");
    let real = |_: &ShardSpec| Some(w.source.clone());
    let lying_env = RecomputeEnv {
        reg: &lying,
        cas: &w.cas,
        host: &w.host,
        source: &real,
    };
    let first = val
        .settle(400, Some((&[], &keccak256(b"s"))), &lying_env)
        .expect("settle");
    assert!(first.pending().contains(&shard));
    assert!(!slashed(first.entries(), &a.address));
    assert!(ledger.apply(&first));
    let later = val.settle(410, None, &env(&w, &real)).expect("later");
    assert_eq!(later.references()[&shard].1, SettlePath::Recomputed);
    assert_eq!(later.statuses()[&(shard, c.address)], RevealStatus::Wrong);
}

/// L25-F4, L27-F1: an identical round opened in an empty ledger gets another
/// tag and cannot be validated against the real ledger; the real round uses
/// the real ledger's record, so a known miner is still refused as a canary.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn canary_snapshot_comes_from_the_ledger_record() {
    let w = world();
    let a = Miner::new(key("miner a")).expect("a");
    let v = Validator::new(key("validator honest")).expect("v");
    let mut real = Ledger::<BtcRules>::default();
    // Round 1 makes `a` known in the real ledger.
    let wa1 = a
        .work(1, &w.spec_small, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("A1");
    let r1 = run_round(
        &w,
        &w.spec_small,
        1,
        &[(&a, &wa1)],
        &mut real,
        &validators(&[&v]),
        canary_commit(&[], &keccak256(b"s")).expect("empty"),
    );
    Val::new(&r1, validators(&[&v]), keccak256(b"b1"), 201, &mut real).expect("v1");
    assert!(real.known_miners().contains(&a.address));
    // Round 2 lists `a` as a canary, opened in the real ledger and, as a copy
    // with the same tag, in an empty one.
    let wa2 = a
        .work(2, &w.spec, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("A2");
    let salt = keccak256(b"s");
    let steal = sorted_canaries(&[(&a, &wa2)], w.spec.shard_id());
    let committed = canary_commit(&steal, &salt).expect("list");
    let r2 = run_round(
        &w,
        &w.spec,
        2,
        &[(&a, &wa2)],
        &mut real,
        &validators(&[&v]),
        committed,
    );
    let mut empty = Ledger::<BtcRules>::default();
    let copy = run_round(
        &w,
        &w.spec,
        2,
        &[(&a, &wa2)],
        &mut empty,
        &validators(&[&v]),
        committed,
    );
    // L27-F1: the copy has another tag (the ledger id is in it), so it cannot
    // be validated against the real ledger.
    assert_ne!(copy.tag(), r2.tag());
    assert!(matches!(
        Val::new(&copy, validators(&[&v]), keccak256(b"b2"), 201, &mut real),
        Err(ValidationError::NotOpenedHere)
    ));
    let mut val = Val::new(&r2, validators(&[&v]), keccak256(b"b2"), 201, &mut real).expect("v2");
    let never = |_: &ShardSpec| -> Option<Vec<u8>> { panic!("not reached") };
    assert_eq!(
        val.settle(400, Some((&steal, &salt)), &env(&w, &never))
            .err(),
        Some(ValidationError::CanaryMismatch)
    );
}

/// L25-F5: inputs withheld past `pending_close` void the shard: nobody is
/// paid or slashed on it. L26-F2: its reveals are public, so its work is
/// never offered again (a copier would take the honest root).
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn withheld_inputs_void_the_shard_at_the_deadline() {
    let w = world();
    let shard = w.spec.shard_id();
    let a = Miner::new(key("miner a")).expect("a");
    let c = Miner::new(key("lazy c")).expect("c");
    let v = Validator::new(key("validator honest")).expect("v");
    let mut ledger = Ledger::<BtcRules>::default();
    let round = disputed_round(&w, 13, &a, &c, &v, &mut ledger);
    let mut val = Val::new(
        &round,
        validators(&[&v]),
        keccak256(b"beacon"),
        201,
        &mut ledger,
    )
    .expect("val");
    let none = |_: &ShardSpec| None;
    let early = val
        .settle(400, Some((&[], &keccak256(b"s"))), &env(&w, &none))
        .expect("early");
    assert!(early.pending().contains(&shard));
    assert!(ledger.apply(&early));
    let st = val
        .settle(CFG.validation.pending_close, None, &env(&w, &none))
        .expect("void");
    assert_eq!(st.references()[&shard].1, SettlePath::Voided);
    assert_eq!(st.statuses()[&(shard, c.address)], RevealStatus::Voided);
    assert!(st.entries().is_empty());
    assert!(ledger.apply(&st));
    assert!(ledger.taken().contains(&w.spec.work_id()));
    assert!(!ledger.open_work().contains(&w.spec.work_id()));
    assert_eq!(ledger.balance(&a.address), 0);
    assert_eq!(ledger.balance(&c.address), 0);
    // L26-F2: the copier's round never opens.
    assert_eq!(
        common::open_round(
            14,
            vec![w.spec.clone()],
            &w.reg,
            &w.cas,
            RoundConfig {
                validator_set_root: validators(&[&v]).root(),
                canary_commit: canary_commit(&[], &keccak256(b"s")).expect("empty"),
                ..CFG
            },
            10,
            &mut ledger
        )
        .err(),
        Some(RoundError::AlreadySettled)
    );
}

/// L26-F1: a miner's commit makes it known at once, whatever order rounds
/// are validated in. Round A (with `m`) is validated after round B, which
/// lists `m` as a canary: B's validation still refuses the list.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn committers_are_known_whatever_the_validation_order() {
    let w = world();
    let m = Miner::new(key("miner m")).expect("m");
    let v = Validator::new(key("validator honest")).expect("v");
    let salt = keccak256(b"s");
    let empty = canary_commit(&[], &salt).expect("empty");
    let mut ledger = Ledger::<BtcRules>::default();
    // Round A: `m` commits honestly.
    let wa = m
        .work(31, &w.spec, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("A");
    let ra = run_round(
        &w,
        &w.spec,
        31,
        &[(&m, &wa)],
        &mut ledger,
        &validators(&[&v]),
        empty,
    );
    // Round B lists `m` as a canary and is validated first.
    let wb = m
        .work(32, &w.spec_small, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("B");
    let steal = sorted_canaries(&[(&m, &wb)], w.spec_small.shard_id());
    let rb = run_round(
        &w,
        &w.spec_small,
        32,
        &[(&m, &wb)],
        &mut ledger,
        &validators(&[&v]),
        canary_commit(&steal, &salt).expect("list"),
    );
    let mut vb = Val::new(&rb, validators(&[&v]), keccak256(b"b"), 201, &mut ledger).expect("vb");
    let real = |_: &ShardSpec| Some(w.source.clone());
    assert_eq!(
        vb.settle(400, Some((&steal, &salt)), &env(&w, &real)).err(),
        Some(ValidationError::CanaryMismatch)
    );
    Val::new(&ra, validators(&[&v]), keccak256(b"a"), 201, &mut ledger).expect("va");
}

/// Rules that name the lying proof for BIP-322: what a coordinator would
/// write to steer reruns (L26-F3).
struct EvilRules;

impl RuleSet for EvilRules {
    fn proof_type(proof_id: &str) -> Option<TypeId> {
        (proof_id == Bip322Proof.proof_id()).then(TypeId::of::<LyingProof>)
    }
    fn canon_type(canon_id: &str) -> Option<TypeId> {
        BtcRules::canon_type(canon_id)
    }
}

/// L26-F3: a settlement made under another rule set cannot be applied to a
/// `Ledger<BtcRules>`.
/// (`Validation<EvilRules>` cannot take a `Ledger<BtcRules>` at all: that
/// is a compile error.)
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn settlement_under_other_rules_is_refused() {
    let w = world();
    let lying = lying_registry();
    let a = Miner::new(key("miner a")).expect("a");
    let c = Miner::new(key("lazy c")).expect("c");
    let v = Validator::new(key("validator honest")).expect("v");
    let mut real = Ledger::<BtcRules>::default();
    let mut evil = Ledger::<EvilRules>::default();
    let wa = a
        .work(41, &w.spec, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("A");
    let wc = invent(&wa, &c, 41, "lazy root, 8+ bytes");
    let works = [(&a, &wa), (&c, &wc)];
    let empty = canary_commit(&[], &keccak256(b"s")).expect("empty");
    let r_real = run_round(
        &w,
        &w.spec,
        41,
        &works,
        &mut real,
        &validators(&[&v]),
        empty,
    );
    let r_evil = run_round(
        &w,
        &w.spec,
        41,
        &works,
        &mut evil,
        &validators(&[&v]),
        empty,
    );
    // Since L27-F1 the tags also differ (each ledger's id is in its tags), so
    // the evil settlement is refused twice over: other rules, other round.
    assert_ne!(r_real.tag(), r_evil.tag());
    let beacon = keccak256(b"beacon");
    Validation::<BtcRules>::new(&r_real, validators(&[&v]), beacon, 201, &mut real).expect("v");
    let mut ve = Validation::<EvilRules>::new(&r_evil, validators(&[&v]), beacon, 201, &mut evil)
        .expect("ve");
    let real_src = |_: &ShardSpec| Some(w.source.clone());
    let lying_env = RecomputeEnv {
        reg: &lying,
        cas: &w.cas,
        host: &w.host,
        source: &real_src,
    };
    let st = ve
        .settle(400, Some((&[], &keccak256(b"s"))), &lying_env)
        .expect("settle under evil rules");
    assert!(!real.apply(&st));
}

/// L25-F6: a miner who committed in a round whose validation has started is
/// known to every later validation, even of a round opened before that.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn committers_of_open_rounds_are_never_canaries() {
    let w = world();
    let m = Miner::new(key("miner m")).expect("m");
    let v = Validator::new(key("validator honest")).expect("v");
    let mut ledger = Ledger::<BtcRules>::default();
    let wm1 = m
        .work(21, &w.spec, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("M1");
    let r1 = run_round(
        &w,
        &w.spec,
        21,
        &[(&m, &wm1)],
        &mut ledger,
        &validators(&[&v]),
        canary_commit(&[], &keccak256(b"s")).expect("empty"),
    );
    // Round 22 opens before round 21's validation, naming `m` as a canary.
    let wm2 = m
        .work(22, &w.spec_small, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("M2");
    let salt = keccak256(b"s");
    let steal = sorted_canaries(&[(&m, &wm2)], w.spec_small.shard_id());
    let r2 = run_round(
        &w,
        &w.spec_small,
        22,
        &[(&m, &wm2)],
        &mut ledger,
        &validators(&[&v]),
        canary_commit(&steal, &salt).expect("list"),
    );
    Val::new(&r1, validators(&[&v]), keccak256(b"b1"), 201, &mut ledger).expect("v1");
    let mut val2 =
        Val::new(&r2, validators(&[&v]), keccak256(b"b2"), 201, &mut ledger).expect("v2");
    let never = |_: &ShardSpec| -> Option<Vec<u8>> { panic!("not reached") };
    assert_eq!(
        val2.settle(400, Some((&steal, &salt)), &env(&w, &never))
            .err(),
        Some(ValidationError::CanaryMismatch)
    );
}

/// L25-F9: a verdict committed and never revealed is slashed once, at round
/// level, not also as a missed task.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn unrevealed_verdict_is_slashed_once() {
    let w = world();
    let a = Miner::new(key("miner a")).expect("a");
    let wa = a
        .work(15, &w.spec, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("A");
    let silent = Validator::new(key("validator silent")).expect("s");
    let honest = Validator::new(key("validator honest")).expect("h");
    let spares = spare_validators(4);
    let mut all = vec![&silent, &honest];
    all.extend(spares.iter());
    let set = validators(&all);
    let mut ledger = Ledger::<BtcRules>::default();
    let round = run_round(
        &w,
        &w.spec,
        15,
        &[(&a, &wa)],
        &mut ledger,
        &set,
        canary_commit(&[], &keccak256(b"s")).expect("empty"),
    );
    let mut val = Val::new(&round, set, keccak256(b"beacon"), 201, &mut ledger).expect("val");
    let agree = |_: Hash32, _: MinerAddress, _: Hash32| Verdict::Agree;
    vote_partly(&mut val, &[(&silent, &agree)], &[(&honest, &agree)]);
    let never = |_: &ShardSpec| -> Option<Vec<u8>> { panic!("unanimous") };
    let st = val
        .settle(400, Some((&[], &keccak256(b"s"))), &env(&w, &never))
        .expect("settle");
    assert!(ledger.apply(&st));
    assert_eq!(ledger.balance(&silent.address), -2_000);
    assert_eq!(ledger.balance(&honest.address), 10);
}

/// L27-F2: canary freshness is the snapshot taken when validation starts.
/// The canary key committing in another round afterwards (after verdicts)
/// cannot veto the round's settlement.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn activity_after_validation_starts_cannot_veto_settlement() {
    let w = world();
    let v = Validator::new(key("validator honest")).expect("v");
    let salt = keccak256(b"s");
    let empty = canary_commit(&[], &salt).expect("empty");
    let real = |_: &ShardSpec| Some(w.source.clone());
    let mut l2 = Ledger::<BtcRules>::default();
    let n = Miner::new(key("miner n")).expect("n");
    let wc = n
        .work(33, &w.spec_small, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("C");
    let steal_c = sorted_canaries(&[(&n, &wc)], w.spec_small.shard_id());
    let rc = run_round(
        &w,
        &w.spec_small,
        33,
        &[(&n, &wc)],
        &mut l2,
        &validators(&[&v]),
        canary_commit(&steal_c, &salt).expect("list"),
    );
    let mut vc = Val::new(&rc, validators(&[&v]), keccak256(b"c"), 201, &mut l2).expect("vc");
    let agree = |_: Hash32, _: MinerAddress, _: Hash32| Verdict::Agree;
    vote(&mut vc, &[(&v, &agree)]);
    // After the verdicts, the canary key commits in another round.
    let wd = n
        .work(34, &w.spec, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("D");
    run_round(
        &w,
        &w.spec,
        34,
        &[(&n, &wd)],
        &mut l2,
        &validators(&[&v]),
        empty,
    );
    let st = vc
        .settle(400, Some((&steal_c, &salt)), &env(&w, &real))
        .expect("settle C");
    assert!(l2.apply(&st), "later activity is no veto");
}

/// L27-F1: a round is bound to the ledger that opened it. The same round in
/// another ledger has another tag, so miners' signed commits do not replay
/// into it, and neither copy can be validated against the other ledger.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_second_ledger_cannot_replay_commits_or_validate() {
    let w = world();
    let m = Miner::new(key("miner m")).expect("m");
    let v = Validator::new(key("validator honest")).expect("v");
    let salt = keccak256(b"s");
    let empty = canary_commit(&[], &salt).expect("empty");
    let cfg = RoundConfig {
        validator_set_root: validators(&[&v]).root(),
        canary_commit: empty,
        ..CFG
    };
    let mut l1 = Ledger::<BtcRules>::default();
    let mut l2 = Ledger::<BtcRules>::default();
    assert_ne!(l1.id(), l2.id());
    let wm = m
        .work(41, &w.spec, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("M");
    let mut r1 =
        common::open_round(41, vec![w.spec.clone()], &w.reg, &w.cas, cfg, 10, &mut l1).expect("r1");
    let signed = m.sign_commit(&wm, 1_000, &r1.tag());
    r1.commit(50, &signed, &mut l1).expect("commit in l1");
    let mut r2 =
        common::open_round(41, vec![w.spec.clone()], &w.reg, &w.cas, cfg, 10, &mut l2).expect("r2");
    assert_ne!(r1.tag(), r2.tag());
    // The signature covers l1's tag: it does not verify in l2's round.
    assert_eq!(
        r2.commit(50, &signed, &mut l2).err(),
        Some(RoundError::BadSignature)
    );
    // A round object cannot take commits through another ledger either.
    assert_eq!(
        r1.commit(51, &signed, &mut l2).err(),
        Some(RoundError::NotOpenedHere)
    );
    r1.close(200).expect("close r1");
    r2.close(200).expect("close r2");
    assert!(matches!(
        Val::new(&r2, validators(&[&v]), keccak256(b"b"), 201, &mut l1),
        Err(ValidationError::NotOpenedHere)
    ));
    assert!(matches!(
        Val::new(&r1, validators(&[&v]), keccak256(b"b"), 201, &mut l2),
        Err(ValidationError::NotOpenedHere)
    ));
    // `m` stays known in l1: a later round there cannot list it as a canary.
    let wb = m
        .work(42, &w.spec_small, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("B");
    let steal = sorted_canaries(&[(&m, &wb)], w.spec_small.shard_id());
    let rb = run_round(
        &w,
        &w.spec_small,
        42,
        &[(&m, &wb)],
        &mut l1,
        &validators(&[&v]),
        canary_commit(&steal, &salt).expect("list"),
    );
    let mut vb = Val::new(&rb, validators(&[&v]), keccak256(b"b2"), 201, &mut l1).expect("vb");
    let real = |_: &ShardSpec| Some(w.source.clone());
    assert_eq!(
        vb.settle(400, Some((&steal, &salt)), &env(&w, &real)).err(),
        Some(ValidationError::CanaryMismatch)
    );
}

/// L27-F3: a shard whose only reveals are the coordinator's canaries is
/// voided without burning its work: no real root became public, so the work
/// is released and can be offered again.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn canary_only_void_releases_the_work() {
    let w = world();
    let shard = w.spec.shard_id();
    let x = Miner::new(key("canary x")).expect("x");
    let v = Validator::new(key("validator honest")).expect("v");
    let salt = keccak256(b"s");
    let wx = x
        .work(51, &w.spec, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("X");
    let list = sorted_canaries(&[(&x, &wx)], shard);
    let mut ledger = Ledger::<BtcRules>::default();
    let round = run_round(
        &w,
        &w.spec,
        51,
        &[(&x, &wx)],
        &mut ledger,
        &validators(&[&v]),
        canary_commit(&list, &salt).expect("list"),
    );
    let mut val =
        Val::new(&round, validators(&[&v]), keccak256(b"b"), 201, &mut ledger).expect("val");
    let none = |_: &ShardSpec| None;
    let early = val
        .settle(400, Some((&list, &salt)), &env(&w, &none))
        .expect("early");
    assert!(early.pending().contains(&shard));
    assert!(ledger.apply(&early));
    let st = val
        .settle(CFG.validation.pending_close, None, &env(&w, &none))
        .expect("void");
    assert_eq!(st.references()[&shard].1, SettlePath::Voided);
    assert!(ledger.apply(&st));
    assert!(!ledger.taken().contains(&w.spec.work_id()));
    common::open_round(
        52,
        vec![w.spec.clone()],
        &w.reg,
        &w.cas,
        RoundConfig {
            validator_set_root: validators(&[&v]).root(),
            canary_commit: canary_commit(&[], &salt).expect("empty"),
            ..CFG
        },
        10,
        &mut ledger,
    )
    .expect("released work can be offered again");
}

/// L27-F4: a shard rerun with no correct reveal publishes the reference, so
/// it is recorded as finalized with no reward and its work is never offered
/// again (a copier would take the published root).
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn rerun_root_without_a_correct_reveal_is_finalized_unpaid() {
    let w = world();
    let shard = w.spec.shard_id();
    let truth = truth(&w, &w.spec);
    let a = Miner::new(key("miner a")).expect("a");
    let c = Miner::new(key("lazy c")).expect("c");
    let v = Validator::new(key("validator honest")).expect("v");
    let salt = keccak256(b"s");
    let wa = a
        .work(61, &w.spec, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("A");
    let wc = invent(&wa, &c, 61, "lazy root, 8+ bytes");
    let mut ledger = Ledger::<BtcRules>::default();
    let round = run_round(
        &w,
        &w.spec,
        61,
        &[(&c, &wc)],
        &mut ledger,
        &validators(&[&v]),
        canary_commit(&[], &salt).expect("empty"),
    );
    let mut val =
        Val::new(&round, validators(&[&v]), keccak256(b"b"), 201, &mut ledger).expect("val");
    let judge = |_: Hash32, _: MinerAddress, r: Hash32| {
        if r == truth {
            Verdict::Agree
        } else {
            Verdict::Disagree { root: truth }
        }
    };
    vote(&mut val, &[(&v, &judge)]);
    let real = |_: &ShardSpec| Some(w.source.clone());
    let st = val
        .settle(400, Some((&[], &salt)), &env(&w, &real))
        .expect("settle");
    assert_eq!(st.references()[&shard], (truth, SettlePath::Recomputed));
    assert_eq!(st.statuses()[&(shard, c.address)], RevealStatus::Wrong);
    assert_eq!(st.finalized().get(&shard), Some(&truth));
    assert!(!st.entries().iter().any(
        |e| matches!(e, LedgerEntry::Reward { reason, .. } if *reason == "first correct reveal")
    ));
    assert!(ledger.apply(&st));
    assert_eq!(
        ledger.finalized().get(&w.spec.work_id()),
        Some(&(61, truth))
    );
    assert_eq!(
        common::open_round(
            62,
            vec![w.spec.clone()],
            &w.reg,
            &w.cas,
            RoundConfig {
                validator_set_root: validators(&[&v]).root(),
                canary_commit: canary_commit(&[], &salt).expect("empty"),
                ..CFG
            },
            10,
            &mut ledger,
        )
        .err(),
        Some(RoundError::AlreadySettled)
    );
}

/// L33-N2: the dispute-panel pool counts only slashes this settlement will
/// certainly take. Spare validators who gave verdicts on the other shard of
/// the round could lose `validator_slash` there only if those verdicts are
/// wrong; counting that worst case dropped them from this shard's pool and
/// sent an honest unanimous shard to a rerun. Each spare has bond 1,000: a
/// missed-task slash (100) still leaves a panel slash (500) covered.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn verdicts_on_another_shard_do_not_drop_validators_from_the_pool() {
    let w = world();
    let a = Miner::new(key("miner a")).expect("a");
    let b = Miner::new(key("miner b")).expect("b");
    let specs = vec![w.spec.clone(), w.spec_small.clone()];
    let big = w.spec.shard_id();
    let small = w.spec_small.shard_id();
    let v = Validator::new(key("validator honest")).expect("v");
    let spares = spare_validators(4);
    let mut set = ValidatorSet::new();
    set.add(v.address, 5_000, 1_000).expect("member");
    for sp in &spares {
        set.add(sp.address, 1_000, 1_000).expect("member");
    }
    let salt = keccak256(b"s");
    let mut ledger = Ledger::<BtcRules>::default();
    let cfg = RoundConfig {
        validator_set_root: set.root(),
        canary_commit: canary_commit(&[], &salt).expect("empty"),
        ..CFG
    };
    let mut r =
        common::open_round(5, specs.clone(), &w.reg, &w.cas, cfg, 10, &mut ledger).expect("open");
    let tag = r.tag();
    let mut works = Vec::new();
    for spec in &specs {
        for m in [&a, &b] {
            let wk = m
                .work(5, spec, w.source.clone(), &w.reg, &w.cas, &w.host)
                .expect("work");
            works.push((m, wk, spec.shard_id()));
        }
    }
    for (n, (m, wk, _)) in works.iter().enumerate() {
        r.commit(50 + n as u64, &m.sign_commit(wk, 1_000, &tag), &mut ledger)
            .expect("commit");
    }
    for (m, wk, shard) in &works {
        m.publish_rows(wk, &w.cas).expect("rows");
        r.reveal(
            150,
            m.address,
            *shard,
            wk.commitment.clone(),
            wk.salt,
            &w.cas,
        )
        .expect("reveal");
    }
    r.close(200).expect("close");
    let mut val = Val::new(&r, set, keccak256(b"beacon"), 201, &mut ledger).expect("validation");
    let seed = val.seed();
    // `v` answers the big shard only; the spares answer the small one only.
    let mut pending = Vec::new();
    for (who, only) in std::iter::once((&v, big)).chain(spares.iter().map(|sp| (sp, small))) {
        for (s, m, _) in val.tasks_for(&who.address) {
            if s != only {
                continue;
            }
            val.commit_verdict(250, &who.commit(&seed, &s, &m, &Verdict::Agree))
                .expect("commit verdict");
            pending.push((who.address, s, m, who.salt(&seed, &s, &m)));
        }
    }
    for (who, s, m, salt) in pending {
        val.reveal_verdict(350, who, s, m, Verdict::Agree, salt)
            .expect("reveal verdict");
    }
    let src = |_: &ShardSpec| Some(w.source.clone());
    let st = val
        .settle(400, Some((&[], &salt)), &env(&w, &src))
        .expect("settle");
    // The spares stay panel candidates for the big shard: taken on unanimity.
    assert_eq!(st.references()[&big].1, SettlePath::Unanimous);
    // Only `v` is left for the small shard: too few for a panel, so rerun.
    assert_eq!(st.references()[&small].1, SettlePath::Recomputed);
}

/// SER-R1-F1: a series shard over blocks where no labelled entity moved ends
/// with `SERIES_NO_ACTIVITY`. Honest miners commit its canonical failure leaf,
/// validators rerun and agree, and the shard settles unanimously: nobody is
/// slashed, and the first committer is paid as for any honest shard. The round
/// opens through the coordinator's preflight (SER-R1-F2).
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn unanimous_no_activity_settles_cleanly() {
    use onyx_coordinator::recompute::{classify, compute, Reference};
    use onyx_recipe_host::{failure_leaf, Outcome};
    let base = world();
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    // Bitwise BITB has no labelled activity in blocks 921,080-921,082.
    let mut quiet = load_label_set(
        &root.join("fixtures"),
        "labels/test.onyx.entities.btc.v1.json",
    )
    .expect("labels");
    quiet.name = "test.onyx.entities.btc.bitb-only".into();
    quiet.labels.retain(|l| l.entity == "entity:bitwise-bitb");
    assert!(!quiet.labels.is_empty());
    let reg = fixture_registry_series();
    let wasm =
        std::fs::read(root.join("recipes/out/btc_entity_flow_series.wasm")).expect("series wasm");
    let cas = MemCas::new();
    cas.put(Kind::Recipe, &wasm).expect("recipe");
    cas.put_label_set(&quiet).expect("labels");
    cas.put(Kind::Blob, &[]).expect("params");
    let mut spec = base.spec.clone();
    spec.recipe = Some(keccak256(&wasm));
    spec.label_set = Some(LabelSetRef {
        name: quiet.name.clone(),
        version: quiet.version,
        content_hash: quiet.content_hash(),
    });
    spec.schema = keccak256(SCHEMA_ENTITY_FLOW_SERIES);
    spec.rights_hash = reg
        .source_rights(&SourceId(SOURCE_ID.into()))
        .expect("rights")
        .hash();
    spec.registry_root = reg.root();
    assert_eq!(spec.validate(&reg), Ok(()));
    let w = World {
        reg,
        cas,
        spec: spec.clone(),
        spec_small: spec.clone(),
        source: base.source.clone(),
        host: Host::new().expect("host"),
    };
    let leaf = failure_leaf(Outcome::RecipeError(onyx_btc_schema::SERIES_NO_ACTIVITY));
    let expected = onyx_merkle::MerkleTree::from_rows(std::slice::from_ref(&leaf)).root();
    // The reference every honest machine reaches: the NO_ACTIVITY leaf's root.
    assert_eq!(
        classify(compute(
            &spec,
            w.source.clone(),
            &w.reg,
            &w.cas,
            &w.host,
            &AttestCache::new()
        )),
        Reference::Root(expected)
    );
    // A miner without the adapter's allow-list abstains (SER-R2-F2) ...
    let plain = Miner::new(key("miner plain")).expect("plain");
    assert!(matches!(
        plain.work(4, &spec, w.source.clone(), &w.reg, &w.cas, &w.host),
        Err(onyx_miner::MinerError::Failed(Outcome::RecipeError(
            onyx_btc_schema::SERIES_NO_ACTIVITY
        )))
    ));
    // ... one with it commits the canonical no-activity leaf.
    let a = Miner::new(key("miner a"))
        .expect("a")
        .with_allowed_failures(common::allowed());
    let b = Miner::new(key("miner b"))
        .expect("b")
        .with_allowed_failures(common::allowed());
    let wa = a
        .work(4, &spec, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("A commits the failure leaf");
    let wb = b
        .work(4, &spec, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("B");
    assert_eq!(wa.rows, vec![leaf]);
    assert_eq!(wa.commitment.root, expected);
    let v = Validator::new(key("validator honest")).expect("v");
    let spares = spare_validators(4);
    let mut all = vec![&v];
    all.extend(spares.iter());
    let set = validators(&all);
    let salt = keccak256(b"s");
    let mut ledger = Ledger::<BtcRules>::default();
    // Open through the preflight: a quiet range is cheap and within budget.
    let never = |_: &ShardSpec| -> Option<Vec<u8>> { None };
    let src = |_: &ShardSpec| -> Option<Vec<u8>> { Some(base.source.clone()) };
    let cfg = || RoundConfig {
        validator_set_root: set.root(),
        canary_commit: canary_commit(&[], &salt).expect("empty"),
        ..CFG
    };
    assert!(matches!(
        Round::open_preflighted(
            4,
            vec![spec.clone()],
            &env(&w, &never),
            cfg(),
            10,
            &mut ledger,
            &common::allowed()
        ),
        Err(RoundError::Preflight { .. })
    ));
    let mut round = Round::open_preflighted(
        4,
        vec![spec.clone()],
        &env(&w, &src),
        cfg(),
        10,
        &mut ledger,
        &common::allowed(),
    )
    .expect("preflight passes");
    let tag = round.tag();
    for (n, (m, wk)) in [(&a, &wa), (&b, &wb)].into_iter().enumerate() {
        round
            .commit(50 + n as u64, &m.sign_commit(wk, 1_000, &tag), &mut ledger)
            .expect("commit");
    }
    for (m, wk) in [(&a, &wa), (&b, &wb)] {
        m.publish_rows(wk, &w.cas).expect("rows");
        round
            .reveal(
                150,
                m.address,
                spec.shard_id(),
                wk.commitment.clone(),
                wk.salt,
                &w.cas,
            )
            .expect("reveal");
    }
    round.close(200).expect("close");
    let mut val =
        Val::new(&round, set, keccak256(b"beacon"), 201, &mut ledger).expect("validation");
    let rerun = |_: Hash32, _: MinerAddress, r: Hash32| -> Verdict {
        v.judge(&spec, w.source.clone(), &r, &w.reg, &w.cas, &w.host)
    };
    vote(&mut val, &[(&v, &rerun)]);
    let s = val
        .settle(400, Some((&[], &salt)), &env(&w, &src))
        .expect("settle");
    let shard = spec.shard_id();
    assert_eq!(s.statuses()[&(shard, a.address)], RevealStatus::Correct);
    assert_eq!(s.statuses()[&(shard, b.address)], RevealStatus::Correct);
    assert_eq!(s.references()[&shard], (expected, SettlePath::Unanimous));
    assert!(!slashed(s.entries(), &a.address) && !slashed(s.entries(), &b.address));
    assert!(ledger.apply(&s));
    assert_eq!(ledger.balance(&a.address), 500);
}
