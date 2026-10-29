//! M5 disputes end to end on real blocks (ADR-0021): a bonded challenge
//! against a settled shard, a drawn panel of bonded validators that reruns
//! the shard, appeals to a larger panel, and the ledger entries that follow.

mod common;

use onyx_coordinator::cas::{Cas, CasExt, Kind, MemCas};
use onyx_coordinator::dispute::{
    challenge_message, honest_panel_root, DisputeConfig, DisputeError, DisputeOutcome, Finality,
    ProofLevel, SignedChallenge,
};
use onyx_coordinator::recompute::{no_valid_root_marker, RecomputeEnv};
use onyx_coordinator::round::RoundConfig;
use onyx_coordinator::validate::{
    canary_commit, Ledger, LedgerEntry, SettlePath, Validation, ValidationConfig, ValidatorSet,
    Verdict,
};
use onyx_core::{
    keccak256, Hash32, LabelSetRef, MinerAddress, Registry, RuntimeProfile, ShardSpec,
    SourceAdapter, SourceId, SourceRef, StaticRegistry,
};
use onyx_labels::attest::AttestCache;
use onyx_labels::LabelSet;
use onyx_miner::{Miner, Work};
use onyx_recipe_host::{Host, HOST_ID};
use onyx_source_btc::{fixture_registry, load_label_set, BtcFixtureAdapter, BtcRules, SOURCE_ID};
use onyx_validator::Validator;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// Golden root of blocks 921,080-921,082 with the full test label set.
const GOLDEN_ROOT: &str = "0xf912872a71710d45d000e4c2c3771e7f31e51e0fe3d0bad0557d8fedca3d86b2";

const CFG: RoundConfig = RoundConfig {
    commit_close: 100,
    reveal_close: 200,
    min_bond: 1_000,
    validator_set_root: Hash32([0; 32]),
    canary_commit: Hash32([0; 32]),
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

const ROUND: u64 = 7;
/// Settlement height; challenges are open in [400, 600).
const SETTLED_AT: u64 = 400;

struct World {
    reg: StaticRegistry,
    cas: MemCas,
    spec: ShardSpec,
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
    let wasm = std::fs::read(root.join("recipes/out/btc_entity_balance.wasm")).expect("wasm");
    let cas = MemCas::new();
    cas.put(Kind::Recipe, &wasm).expect("recipe");
    cas.put_label_set(&labels).expect("labels");
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
    World {
        reg,
        cas,
        spec,
        source,
        host: Host::new().expect("host"),
    }
}

fn key(s: &str) -> [u8; 32] {
    keccak256(s.as_bytes()).0
}

/// A reveal with an invented root, as a lazy miner makes.
fn invent(base: &Work, m: &Miner, text: &str) -> Work {
    invent_in(base, m, text, ROUND)
}

fn invent_in(base: &Work, m: &Miner, text: &str, round: u64) -> Work {
    let mut x = base.clone();
    x.rows = vec![text.as_bytes().to_vec()];
    x.commitment.root = onyx_merkle::MerkleTree::from_rows(&x.rows).root();
    x.commitment.leaf_count = 1;
    x.salt = m.salt(round, &base.shard);
    x.commit_hash = onyx_core::commit_hash(
        round,
        &base.shard,
        &x.commitment.binding_hash(),
        &m.address,
        &x.salt,
    );
    x
}

/// A shard settled in a ledger, ready to be disputed.
struct Setup {
    w: World,
    ledger: Ledger<BtcRules>,
    tag: Hash32,
    shard: Hash32,
    /// The rerun result (the golden root), from a real panel-style rerun.
    truth: Hash32,
    /// The reference settlement chose.
    reference: Hash32,
    miners: Vec<MinerAddress>,
    lazy_validator: Validator,
    /// Bonded validators who gave no verdict: panel candidates.
    panelists: Vec<Validator>,
    challenger: Validator,
    appellant: Validator,
    /// How settlement decided the shard.
    path: SettlePath,
    /// The bonded validator set (reused by a second round).
    set: ValidatorSet,
}

/// Knobs for [`settled_with`].
struct Opts {
    wrong: bool,
    /// Bonded validators who give no verdict (panel candidates).
    panelists: usize,
    panelist_bond: u128,
    dispute: DisputeConfig,
}

/// Settle the fixture shard. With `wrong`, two lazy miners reveal the same
/// invented root and the only verdict-giving validator blindly agrees, so a
/// wrong root is settled unanimously; otherwise two honest miners reveal the
/// golden root.
fn settled(wrong: bool, extra_panelists: usize) -> Setup {
    let s = settled_with(Opts {
        wrong,
        panelists: 10 + extra_panelists,
        panelist_bond: 5_000,
        dispute: CFG.dispute,
    });
    assert_eq!(s.path, SettlePath::Unanimous);
    assert_eq!(s.reference == s.truth, !wrong);
    s
}

fn settled_with(o: Opts) -> Setup {
    let wrong = o.wrong;
    let w = world();
    let shard = w.spec.shard_id();
    let a = Miner::new(key("miner a")).expect("a");
    let b = Miner::new(key("miner b")).expect("b");
    let wa = a
        .work(ROUND, &w.spec, w.source.clone(), &w.reg, &w.cas, &w.host)
        .expect("A");
    let wb = if wrong {
        invent(&wa, &b, "lazy root, 8+ bytes")
    } else {
        b.work(ROUND, &w.spec, w.source.clone(), &w.reg, &w.cas, &w.host)
            .expect("B")
    };
    let wa = if wrong {
        invent(&wa, &a, "lazy root, 8+ bytes")
    } else {
        wa
    };
    let lazy_validator = Validator::new(key("validator blind")).expect("lazy");
    let panelists: Vec<Validator> = (0..o.panelists)
        .map(|i| Validator::new(key(&format!("panelist {i}"))).expect("panelist"))
        .collect();
    let mut set = ValidatorSet::new();
    set.add(lazy_validator.address, 5_000, 1_000)
        .expect("member");
    for p in &panelists {
        set.add(p.address, o.panelist_bond, o.panelist_bond.min(1_000))
            .expect("member");
    }
    let salt = keccak256(b"no canaries");
    let mut ledger = Ledger::<BtcRules>::default();
    let cfg = RoundConfig {
        validator_set_root: set.root(),
        canary_commit: canary_commit(&[], &salt).expect("empty"),
        dispute: o.dispute,
        ..CFG
    };
    let mut r = common::open_round(
        ROUND,
        vec![w.spec.clone()],
        &w.reg,
        &w.cas,
        cfg,
        10,
        &mut ledger,
    )
    .expect("open");
    let tag = r.tag();
    for (n, (m, wk)) in [(&a, &wa), (&b, &wb)].iter().enumerate() {
        r.commit(50 + n as u64, &m.sign_commit(wk, 1_000, &tag), &mut ledger)
            .expect("commit");
    }
    for (m, wk) in [(&a, &wa), (&b, &wb)] {
        m.publish_rows(wk, &w.cas).expect("rows");
        r.reveal(
            150,
            m.address,
            shard,
            wk.commitment.clone(),
            wk.salt,
            &w.cas,
        )
        .expect("reveal");
    }
    r.close(200).expect("close");
    let set_for_setup = set.clone();
    let mut val =
        Validation::<BtcRules>::new(&r, set, keccak256(b"beacon"), 201, &mut ledger).expect("val");
    let seed = val.seed();
    let mut pending = Vec::new();
    for (s, m, _) in val.tasks_for(&lazy_validator.address) {
        val.commit_verdict(250, &lazy_validator.commit(&seed, &s, &m, &Verdict::Agree))
            .expect("commit verdict");
        pending.push((s, m, lazy_validator.salt(&seed, &s, &m)));
    }
    for (s, m, salt) in pending {
        val.reveal_verdict(350, lazy_validator.address, s, m, Verdict::Agree, salt)
            .expect("reveal verdict");
    }
    let src = |_: &ShardSpec| Some(w.source.clone());
    let env = RecomputeEnv {
        reg: &w.reg,
        cas: &w.cas,
        host: &w.host,
        source: &src,
    };
    let st = val
        .settle(SETTLED_AT, Some((&[], &salt)), &env)
        .expect("settle");
    assert!(ledger.apply(&st));
    let (reference, path) = st.references()[&shard];
    // A real rerun, as an honest panel member does it.
    let rec = ledger.shard_record(ROUND, &shard).expect("record");
    let truth = honest_panel_root::<BtcRules>(&env, rec, &AttestCache::new()).expect("rerun");
    assert_eq!(truth.to_string(), GOLDEN_ROOT);
    Setup {
        w,
        ledger,
        tag,
        shard,
        truth,
        reference,
        miners: vec![a.address, b.address],
        lazy_validator,
        panelists,
        challenger: Validator::new(key("challenger")).expect("challenger"),
        appellant: Validator::new(key("appellant")).expect("appellant"),
        path,
        set: set_for_setup,
    }
}

/// Nothing a dispute pays out is minted (L29-F3).
fn conserves(s: &Setup, id: &Hash32) {
    let d = s.ledger.dispute(id).expect("dispute");
    assert!(
        d.paid_out() <= d.forfeited(),
        "paid {} > forfeited {}",
        d.paid_out(),
        d.forfeited()
    );
}

/// The current panel's members (as validators we hold keys for).
fn panel<'a>(s: &'a Setup, id: &Hash32) -> Vec<&'a Validator> {
    let d = s.ledger.dispute(id).expect("dispute");
    let members = d.panels().last().expect("panel").members().to_vec();
    members
        .iter()
        .map(|m| {
            s.panelists
                .iter()
                .find(|p| p.address == *m)
                .expect("drawn from our panelists")
        })
        .collect()
}

/// Every current panel member votes `choose(index)` (None = stays silent).
fn panel_votes(
    s: &mut Setup,
    id: &Hash32,
    commit_at: u64,
    reveal_at: u64,
    choose: &dyn Fn(usize) -> Option<Hash32>,
) {
    let level = s.ledger.dispute(id).expect("d").current_level();
    let voters: Vec<(
        MinerAddress,
        Hash32,
        Hash32,
        onyx_coordinator::dispute::SignedPanelVote,
    )> = panel(s, id)
        .into_iter()
        .enumerate()
        .filter_map(|(i, v)| {
            let root = choose(i)?;
            Some((
                v.address,
                root,
                v.panel_salt(id, level),
                v.panel_vote(id, level, &root),
            ))
        })
        .collect();
    for (_, _, _, vote) in &voters {
        s.ledger
            .commit_panel_vote(commit_at, vote)
            .expect("commit vote");
    }
    for (who, root, salt, _) in voters {
        s.ledger
            .reveal_panel_vote(reveal_at, id, who, root, salt)
            .expect("reveal vote");
    }
}

fn open(s: &mut Setup, claimed: Hash32, at: u64) -> Result<Hash32, DisputeError> {
    let c = s
        .challenger
        .challenge(ROUND, &s.tag, &s.shard, &claimed, 1_000);
    s.ledger.open_dispute(at, &c, keccak256(b"dispute beacon"))
}

fn has(entries: &[LedgerEntry], who: &MinerAddress, reason: &str) -> bool {
    entries.iter().any(|e| match e {
        LedgerEntry::Reward {
            actor, reason: r, ..
        }
        | LedgerEntry::Slash {
            actor, reason: r, ..
        } => actor == who && *r == reason,
    })
}

fn lie() -> Hash32 {
    keccak256(b"a root nobody computes")
}

/// The settler's rerun of a shard marked "rerun required".
fn rerun(s: &mut Setup, round: u64, shard: Hash32) -> Result<DisputeOutcome, DisputeError> {
    let source = s.w.source.clone();
    let src = move |_: &ShardSpec| Some(source.clone());
    let env = RecomputeEnv {
        reg: &s.w.reg,
        cas: &s.w.cas,
        host: &s.w.host,
        source: &src,
    };
    s.ledger
        .rerun_shard(round, &shard, &env, &AttestCache::new())
}

/// Nobody on any of the dispute's panels was slashed.
fn no_panel_slashes(s: &Setup, id: &Hash32) {
    let e = s.ledger.entries();
    for p in s.ledger.dispute(id).expect("d").panels() {
        for m in p.members() {
            assert!(!has(e, m, "panel: no reveal or off the majority"));
        }
    }
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn wrong_finalized_root_is_overturned_by_an_honest_panel() {
    let mut s = settled(true, 0);
    let work = s
        .ledger
        .shard_record(ROUND, &s.shard)
        .expect("rec")
        .spec()
        .expect("spec")
        .work_id();
    assert_eq!(s.ledger.finalized()[&work].1, s.reference);
    let truth = s.truth;
    let id = open(&mut s, truth, 410).expect("open");
    panel_votes(&mut s, &id, 420, 470, &|_| Some(truth));
    // Unanimous panel: no appeal possible, resolve once reveals close.
    assert_eq!(
        s.ledger.resolve_dispute(509, &id),
        Err(DisputeError::WrongPhase)
    );
    let out = s.ledger.resolve_dispute(510, &id).expect("resolve");
    assert_eq!(out, DisputeOutcome::Overturned { reference: truth });
    assert_eq!(s.ledger.finalized()[&work].1, truth);
    let e = s.ledger.entries().to_vec();
    // Both lazy miners lose their bonds; the first one's reward is reversed.
    assert!(has(&e, &s.miners[0], "dispute: reward reversed"));
    assert_eq!(s.ledger.balance(&s.miners[0]), 500 - 500 - 1_000);
    assert_eq!(s.ledger.balance(&s.miners[1]), -1_000);
    // The challenger earns half of the 2,000 newly slashed and pays its
    // panel's fees (3 x 10) out of its bond (L29-F3).
    assert_eq!(s.ledger.balance(&s.challenger.address), 1_000 - 30);
    conserves(&s, &id);
    // The blind validator's fee is reversed and its Agrees are now wrong.
    assert!(has(&e, &s.lazy_validator.address, "wrong verdict"));
    assert!(s.ledger.balance(&s.lazy_validator.address) < 0);
    for p in panel(&s, &id) {
        assert!(has(&e, &p.address, "panel: on the majority"));
    }
}

/// L29-F1 (AUD-1-F8): a signed challenge cannot be used again once its
/// dispute resolved: the same signed challenge, inside the challenge window,
/// is refused, so the victim is never slashed twice.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_signed_challenge_cannot_be_replayed_after_it_resolves() {
    let mut s = settled(true, 0);
    let truth = s.truth;
    let id = open(&mut s, truth, 410).expect("open");
    panel_votes(&mut s, &id, 420, 470, &|_| Some(truth));
    s.ledger.resolve_dispute(510, &id).expect("resolve");
    let before = s.ledger.entries().len();
    // Same challenger, same signed message, still inside the window (400 + 200):
    // refused (the overturned reference now equals the claim, and the shard is
    // closed; the consumed key backs both).
    let again = open(&mut s, truth, 520);
    assert!(
        matches!(
            again,
            Err(DisputeError::SameRoot | DisputeError::Closed | DisputeError::Replayed)
        ),
        "{again:?}"
    );
    assert_eq!(s.ledger.entries().len(), before, "nothing slashed again");
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn frivolous_challenge_against_the_right_root_loses_its_bond() {
    let mut s = settled(false, 0);
    let truth = s.truth;
    let id = open(&mut s, lie(), 410).expect("open");
    panel_votes(&mut s, &id, 420, 470, &|_| Some(truth));
    assert_eq!(
        s.ledger.resolve_dispute(510, &id),
        Ok(DisputeOutcome::Upheld)
    );
    assert_eq!(s.ledger.balance(&s.challenger.address), -1_000);
    // The panel's fees (30) come out of the bond; half of the rest (970) goes
    // to the first correct miner, on top of its 500 reward.
    assert_eq!(s.ledger.balance(&s.miners[0]), 500 + 485);
    conserves(&s, &id);
    let work = s.w.spec.work_id();
    assert_eq!(s.ledger.finalized()[&work].1, truth);
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_lying_minority_and_a_silent_member_are_slashed() {
    let mut s = settled(false, 0);
    let truth = s.truth;
    let id = open(&mut s, lie(), 410).expect("open");
    // Two honest members and one liar: a majority, but not unanimous.
    panel_votes(&mut s, &id, 420, 470, &|i| {
        Some(if i == 2 { lie() } else { truth })
    });
    // Not unanimous: appealable until 560, so not resolvable before.
    assert_eq!(
        s.ledger.resolve_dispute(510, &id),
        Err(DisputeError::WrongPhase)
    );
    assert_eq!(
        s.ledger.resolve_dispute(560, &id),
        Ok(DisputeOutcome::Upheld)
    );
    let members = panel(&s, &id).iter().map(|v| v.address).collect::<Vec<_>>();
    let e = s.ledger.entries().to_vec();
    assert!(has(&e, &members[2], "panel: no reveal or off the majority"));
    assert!(has(&e, &members[0], "panel: on the majority"));

    // A silent member is slashed too.
    let mut s = settled(false, 0);
    let id = open(&mut s, lie(), 410).expect("open");
    panel_votes(&mut s, &id, 420, 470, &|i| (i != 1).then_some(truth));
    assert_eq!(
        s.ledger.resolve_dispute(560, &id),
        Ok(DisputeOutcome::Upheld)
    );
    let members = panel(&s, &id).iter().map(|v| v.address).collect::<Vec<_>>();
    assert!(has(
        s.ledger.entries(),
        &members[1],
        "panel: no reveal or off the majority"
    ));
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_split_panel_is_appealed_and_the_larger_panel_decides() {
    let mut s = settled(true, 0);
    let truth = s.truth;
    let reference = s.reference;
    let id = open(&mut s, truth, 410).expect("open");
    // 1 for the truth, 1 for the old reference, 1 silent: no majority.
    panel_votes(&mut s, &id, 420, 470, &|i| match i {
        0 => Some(truth),
        1 => Some(reference),
        _ => None,
    });
    let first: Vec<MinerAddress> = panel(&s, &id).iter().map(|v| v.address).collect();
    // Appeal bond must be at least twice the challenge bond.
    let low = s.appellant.appeal(&id, 0, &truth, 1_999);
    assert_eq!(
        s.ledger.appeal(515, &low, keccak256(b"b2")),
        Err(DisputeError::BondTooLow)
    );
    let ok = s.appellant.appeal(&id, 0, &truth, 2_000);
    s.ledger.appeal(515, &ok, keccak256(b"b2")).expect("appeal");
    let second: Vec<MinerAddress> = panel(&s, &id).iter().map(|v| v.address).collect();
    assert_eq!(second.len(), 7);
    assert!(second.iter().all(|m| !first.contains(m)));
    panel_votes(&mut s, &id, 520, 570, &|_| Some(truth));
    let out = s.ledger.resolve_dispute(615, &id).expect("resolve");
    assert_eq!(out, DisputeOutcome::Overturned { reference: truth });
    let e = s.ledger.entries().to_vec();
    assert!(has(&e, &first[0], "panel: on the majority"));
    assert!(has(&e, &first[1], "panel: no reveal or off the majority"));
    assert!(has(&e, &first[2], "panel: no reveal or off the majority"));
    // The appellant backed the winning root: only its panel's fees are taken.
    assert!(!has(&e, &s.appellant.address, "appeal lost"));
    assert!(has(&e, &s.appellant.address, "panel fees paid"));
    conserves(&s, &id);
}

/// q13 (a): a split is not a final answer. After the last appeal a split
/// panel burns the funders' fees, slashes nobody and sends the shard to the
/// settler's rerun, which then decides the dispute like a majority would.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn after_the_last_appeal_a_split_panel_goes_to_the_rerun_which_decides() {
    let mut s = settled(true, 0);
    let truth = s.truth;
    let reference = s.reference;
    let id = open(&mut s, truth, 410).expect("open");
    panel_votes(&mut s, &id, 420, 470, &|i| match i {
        0 => Some(truth),
        1 => Some(reference),
        _ => None,
    });
    let ok = s.appellant.appeal(&id, 0, &truth, 2_000);
    s.ledger.appeal(515, &ok, keccak256(b"b2")).expect("appeal");
    // 3 truth, 3 old reference, 1 silent: no strict majority of 7.
    panel_votes(&mut s, &id, 520, 570, &|i| match i {
        0..=2 => Some(truth),
        3..=5 => Some(reference),
        _ => None,
    });
    // max_appeals = 1: no further appeal.
    let again = s.challenger.appeal(&id, 1, &truth, 4_000);
    assert_eq!(
        s.ledger.appeal(615, &again, keccak256(b"b3")),
        Err(DisputeError::NoAppeal)
    );
    assert_eq!(
        s.ledger.resolve_dispute(615, &id),
        Ok(DisputeOutcome::RerunRequired)
    );
    let e = s.ledger.entries().to_vec();
    assert!(!has(&e, &s.challenger.address, "challenge lost"));
    assert!(!has(&e, &s.appellant.address, "appeal lost"));
    // L30-F1: a split is not free. Each funder pays its panel's fees, burned:
    // 3 x 10 for the challenger, 7 x 10 for the appellant.
    assert_eq!(s.ledger.balance(&s.challenger.address), -30);
    assert_eq!(s.ledger.balance(&s.appellant.address), -70);
    // L31-F1: nobody on any panel is slashed for a split.
    no_panel_slashes(&s, &id);
    // Waiting for the rerun: the root is unchanged, the dispute takes no more
    // votes, and every new challenge is pointed to the rerun.
    let work = s.w.spec.work_id();
    assert_eq!(s.ledger.finalized()[&work].1, reference);
    assert!(s.ledger.rerun_required().contains(&(ROUND, s.shard)));
    assert_eq!(
        s.ledger.dispute(&id).expect("d").outcome(),
        Some(DisputeOutcome::RerunRequired)
    );
    assert_eq!(
        s.ledger.resolve_dispute(700, &id),
        Err(DisputeError::Resolved)
    );
    let fresh = s
        .lazy_validator
        .challenge(ROUND, &s.tag, &s.shard, &truth, 1_000);
    assert_eq!(
        s.ledger.open_dispute(580, &fresh, keccak256(b"b")),
        Err(DisputeError::RerunRequired)
    );
    conserves(&s, &id);

    // The rerun finds the truth: the challenger's root, so Overturned. The
    // appellant backed it too and keeps the rest of its bond; the challenger
    // earns half of the 2,000 newly slashed from the lazy miners.
    let shard = s.shard;
    assert_eq!(
        rerun(&mut s, ROUND, shard),
        Ok(DisputeOutcome::Overturned { reference: truth })
    );
    assert_eq!(
        s.ledger.dispute(&id).expect("d").outcome(),
        Some(DisputeOutcome::Overturned { reference: truth })
    );
    assert_eq!(s.ledger.finalized()[&work].1, truth);
    let e = s.ledger.entries().to_vec();
    assert!(!has(&e, &s.appellant.address, "appeal lost"));
    assert_eq!(s.ledger.balance(&s.appellant.address), -70);
    assert_eq!(s.ledger.balance(&s.challenger.address), -30 + 1_000);
    assert!(has(&e, &s.miners[1], "revealed a wrong root"));
    no_panel_slashes(&s, &id);
    conserves(&s, &id);
    // Decided: the shard is closed, whatever the next challenge claims.
    let other = s
        .lazy_validator
        .challenge(ROUND, &s.tag, &s.shard, &lie(), 1_000);
    assert_eq!(
        s.ledger.open_dispute(580, &other, keccak256(b"b")),
        Err(DisputeError::Closed)
    );
}

/// L30-F1: a panel where nobody reveals could not rerun the shard: nobody on
/// it is slashed, the challenger pays its panel's fees (burned) and the
/// settler's rerun decides (q13 (a)).
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn an_all_silent_panel_slashes_nobody_and_costs_the_challenger_its_fees() {
    let mut s = settled(true, 0);
    let truth = s.truth;
    let id = open(&mut s, truth, 410).expect("open");
    panel_votes(&mut s, &id, 420, 470, &|_| None);
    assert_eq!(
        s.ledger.resolve_dispute(560, &id),
        Ok(DisputeOutcome::RerunRequired)
    );
    no_panel_slashes(&s, &id);
    assert!(has(
        s.ledger.entries(),
        &s.challenger.address,
        "panel fees burned"
    ));
    assert_eq!(s.ledger.balance(&s.challenger.address), -30);
    conserves(&s, &id);
    let shard = s.shard;
    assert_eq!(
        rerun(&mut s, ROUND, shard),
        Ok(DisputeOutcome::Overturned { reference: truth })
    );
    no_panel_slashes(&s, &id);
    conserves(&s, &id);
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_challenge_opens_only_in_its_ledger_once_and_in_its_window() {
    let mut s = settled(true, 0);
    let truth = s.truth;
    // Too late.
    assert_eq!(open(&mut s, truth, 600), Err(DisputeError::WrongPhase));
    // Claiming the reference itself is no dispute.
    let same = s.reference;
    assert_eq!(open(&mut s, same, 410), Err(DisputeError::SameRoot));
    // Bond below the minimum.
    let low = s.challenger.challenge(ROUND, &s.tag, &s.shard, &truth, 999);
    assert_eq!(
        s.ledger.open_dispute(410, &low, keccak256(b"b")),
        Err(DisputeError::BondTooLow)
    );
    let id = open(&mut s, truth, 410).expect("open");
    // One open dispute per shard.
    assert_eq!(open(&mut s, truth, 411), Err(DisputeError::AlreadyOpen));
    assert!(s.ledger.dispute(&id).is_some());

    // Another ledger settled the same round: other tag, so the challenge
    // signed for the first ledger does not open there.
    let mut other = settled(true, 0);
    assert_ne!(other.tag, s.tag);
    let c = s
        .challenger
        .challenge(ROUND, &s.tag, &s.shard, &truth, 1_000);
    assert_eq!(
        other.ledger.open_dispute(410, &c, keccak256(b"b")),
        Err(DisputeError::WrongTag)
    );
    // A ledger that never settled the shard knows nothing about it.
    let mut empty = Ledger::<BtcRules>::default();
    assert_eq!(
        empty.open_dispute(410, &c, keccak256(b"b")),
        Err(DisputeError::NotSettledHere)
    );
    // A signature by someone else is refused.
    let mut forged = other
        .challenger
        .challenge(ROUND, &other.tag, &other.shard, &truth, 1_000);
    forged.challenger = s.appellant.address;
    assert_eq!(
        other.ledger.open_dispute(410, &forged, keccak256(b"b")),
        Err(DisputeError::BadSignature)
    );
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn only_recomputed_claims_can_be_disputed_so_far() {
    let mut s = settled(true, 0);
    let truth = s.truth;
    let level = ProofLevel::BondedClaim;
    let c = SignedChallenge {
        round: ROUND,
        tag: s.tag,
        shard: s.shard,
        challenger: s.challenger.address,
        claimed_root: truth,
        bond: 1_000,
        level,
        sig: s
            .challenger
            .sign(&challenge_message(&s.tag, &s.shard, &truth, 1_000, level)),
    };
    assert_eq!(
        s.ledger.open_dispute(410, &c, keccak256(b"b")),
        Err(DisputeError::Unsupported)
    );
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn miners_challenger_appellants_and_verdict_givers_are_never_drawn() {
    let mut s = settled(true, 0);
    let truth = s.truth;
    let reference = s.reference;
    let id = open(&mut s, truth, 410).expect("open");
    let mut excluded: BTreeSet<MinerAddress> = s.miners.iter().copied().collect();
    excluded.insert(s.challenger.address);
    excluded.insert(s.lazy_validator.address);
    let first: Vec<MinerAddress> = panel(&s, &id).iter().map(|v| v.address).collect();
    assert!(first.iter().all(|m| !excluded.contains(m)));
    // Only our 10 non-voting panelists remain eligible.
    assert!(first
        .iter()
        .all(|m| s.panelists.iter().any(|p| p.address == *m)));
    panel_votes(&mut s, &id, 420, 470, &|i| match i {
        0 => Some(truth),
        1 => Some(reference),
        _ => None,
    });
    let ok = s.appellant.appeal(&id, 0, &truth, 2_000);
    s.ledger.appeal(515, &ok, keccak256(b"b2")).expect("appeal");
    let second: Vec<MinerAddress> = panel(&s, &id).iter().map(|v| v.address).collect();
    assert!(second
        .iter()
        .all(|m| !excluded.contains(m) && *m != s.appellant.address && !first.contains(m)));
    // A non-member cannot vote.
    let intruder = s.challenger.panel_vote(&id, 1, &truth);
    assert_eq!(
        s.ledger.commit_panel_vote(520, &intruder),
        Err(DisputeError::NotOnPanel)
    );
}

fn short_window() -> DisputeConfig {
    DisputeConfig {
        challenge_window: 300,
        max_appeals: 0,
        ..CFG.dispute
    }
}

/// L29-F1, q13 (a): after a split no challenge opens, the same signature or
/// a fresh one: the shard waits for the rerun, then it is closed.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn after_a_split_no_challenge_opens_until_the_rerun_and_then_the_shard_is_closed() {
    let mut s = settled_with(Opts {
        wrong: true,
        panelists: 10,
        panelist_bond: 5_000,
        dispute: short_window(),
    });
    let truth = s.truth;
    let reference = s.reference;
    let c = s
        .challenger
        .challenge(ROUND, &s.tag, &s.shard, &truth, 1_000);
    let id = s
        .ledger
        .open_dispute(410, &c, keccak256(b"b"))
        .expect("open");
    // A split panel with no appeals left: the shard goes to the rerun.
    panel_votes(&mut s, &id, 420, 470, &|i| match i {
        0 => Some(truth),
        1 => Some(reference),
        _ => None,
    });
    assert_eq!(
        s.ledger.resolve_dispute(510, &id),
        Ok(DisputeOutcome::RerunRequired)
    );
    let fresh = s
        .challenger
        .challenge(ROUND, &s.tag, &s.shard, &truth, 1_001);
    // Same signature or a new one, inside the 300-height window: refused.
    for ch in [&c, &fresh] {
        assert_eq!(
            s.ledger.open_dispute(520, ch, keccak256(b"b")),
            Err(DisputeError::RerunRequired)
        );
    }
    let shard = s.shard;
    assert_eq!(
        rerun(&mut s, ROUND, shard),
        Ok(DisputeOutcome::Overturned { reference: truth })
    );
    // The truth is now the reference; any other claim finds the shard closed.
    let other = s
        .challenger
        .challenge(ROUND, &s.tag, &s.shard, &lie(), 1_000);
    assert_eq!(
        s.ledger.open_dispute(530, &other, keccak256(b"b")),
        Err(DisputeError::Closed)
    );
}

/// L29-F6: after Upheld, Overturned or Corrected the shard is closed.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_decided_shard_is_closed_to_further_disputes() {
    let mut s = settled_with(Opts {
        wrong: false,
        panelists: 10,
        panelist_bond: 5_000,
        dispute: short_window(),
    });
    let truth = s.truth;
    let id = open(&mut s, lie(), 410).expect("open");
    panel_votes(&mut s, &id, 420, 470, &|_| Some(truth));
    assert_eq!(
        s.ledger.resolve_dispute(510, &id),
        Ok(DisputeOutcome::Upheld)
    );
    let again = s
        .appellant
        .challenge(ROUND, &s.tag, &s.shard, &lie(), 1_000);
    assert_eq!(
        s.ledger.open_dispute(520, &again, keccak256(b"b")),
        Err(DisputeError::Closed)
    );
}

/// L29-F2, M5-L46-F2: an appeal draws exactly 2k+1 members or none. With
/// none, a split panel never decides by its bare majority: the dispute goes
/// to the settler's rerun, which decides it (q13 (a)).
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn an_appeal_never_shrinks_the_deciding_panel() {
    // 8 candidates: the first panel takes 3, leaving 5 < 7.
    let mut s = settled_with(Opts {
        wrong: false,
        panelists: 8,
        panelist_bond: 5_000,
        dispute: CFG.dispute,
    });
    assert_eq!(s.path, SettlePath::Unanimous);
    let truth = s.truth;
    let id = open(&mut s, lie(), 410).expect("open");
    panel_votes(&mut s, &id, 420, 470, &|i| (i != 2).then_some(truth));
    let a = s.appellant.appeal(&id, 0, &lie(), 2_000);
    assert_eq!(
        s.ledger.appeal(515, &a, keccak256(b"b2")),
        Err(DisputeError::RerunRequired)
    );
    assert_eq!(s.ledger.dispute(&id).expect("d").panels().len(), 1);
    // No second appeal; the dispute resolves at once, to the rerun.
    assert_eq!(
        s.ledger.appeal(516, &a, keccak256(b"b3")),
        Err(DisputeError::NoAppeal)
    );
    assert_eq!(
        s.ledger.resolve_dispute(516, &id),
        Ok(DisputeOutcome::RerunRequired)
    );
    no_panel_slashes(&s, &id);
    let shard = s.shard;
    assert_eq!(rerun(&mut s, ROUND, shard), Ok(DisputeOutcome::Upheld));
    let e = s.ledger.entries().to_vec();
    assert!(has(&e, &s.challenger.address, "challenge lost"));
    // The appeal was never taken: its bond is untouched.
    assert_eq!(s.ledger.balance(&s.appellant.address), 0);
    conserves(&s, &id);
}

/// M5-L46-F2: a bribed 2-of-3 first panel backs a lie, the appeal finds no
/// larger panel (too small a pool), so the bribed majority decides nothing:
/// the rerun keeps the correct root and the challenger loses its bond.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_bribed_two_of_three_panel_with_no_appeal_panel_is_overruled_by_the_rerun() {
    let mut s = settled_with(Opts {
        wrong: false,
        panelists: 8,
        panelist_bond: 5_000,
        dispute: CFG.dispute,
    });
    let truth = s.truth;
    let work = s.w.spec.work_id();
    let id = open(&mut s, lie(), 410).expect("open");
    panel_votes(&mut s, &id, 420, 470, &|i| {
        Some(if i < 2 { lie() } else { truth })
    });
    let a = s.appellant.appeal(&id, 0, &truth, 2_000);
    assert_eq!(
        s.ledger.appeal(515, &a, keccak256(b"b2")),
        Err(DisputeError::RerunRequired)
    );
    assert_eq!(
        s.ledger.resolve_dispute(520, &id),
        Ok(DisputeOutcome::RerunRequired)
    );
    // The lie never became the reference, and the root is not final.
    assert_eq!(s.ledger.finalized()[&work].1, truth);
    let shard = s.shard;
    assert!(matches!(
        s.ledger.finality(700, ROUND, &shard),
        Finality::RerunPending { reference, .. } if reference == truth
    ));
    assert_eq!(s.ledger.final_root(700, &work), None);
    no_panel_slashes(&s, &id);
    assert_eq!(rerun(&mut s, ROUND, shard), Ok(DisputeOutcome::Upheld));
    assert_eq!(s.ledger.finalized()[&work].1, truth);
    assert_eq!(s.ledger.final_root(700, &work), Some(truth));
    let e = s.ledger.entries().to_vec();
    assert!(has(&e, &s.challenger.address, "challenge lost"));
    assert!(!has(&e, &s.appellant.address, "appeal lost"));
    conserves(&s, &id);
}

/// L29-F3: the reviewer's attack. The attacker challenges a correct root
/// with the minimum bond, controls every panelist, keeps one silent per
/// panel so it is never unanimous, and appeals backing the reference. Fees
/// come only out of its own bonds, so it loses money; nothing is minted.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_frivolous_challenge_with_silent_sybils_and_appeals_loses_money() {
    let mut s = settled(false, 0);
    let truth = s.truth;
    let id = open(&mut s, lie(), 410).expect("open");
    panel_votes(&mut s, &id, 420, 470, &|i| (i != 2).then_some(truth));
    let a = s.appellant.appeal(&id, 0, &truth, 2_000);
    s.ledger.appeal(515, &a, keccak256(b"b2")).expect("appeal");
    panel_votes(&mut s, &id, 520, 570, &|i| (i != 0).then_some(truth));
    assert_eq!(
        s.ledger.resolve_dispute(615, &id),
        Ok(DisputeOutcome::Upheld)
    );
    conserves(&s, &id);
    let attacker: i128 = s
        .panelists
        .iter()
        .map(|p| p.address)
        .chain([s.challenger.address, s.appellant.address])
        .map(|a| s.ledger.balance(&a))
        .sum::<i128>();
    // Settlement already charged each silent assignee its missed slash
    // (10 x 100); the dispute alone must also lose money.
    assert!(attacker + 10 * 100 < 0, "attacker net {attacker}");
}

/// L29-F4: when every bonded validator gave a verdict there is nobody left
/// to draw a panel from, so settlement does not trust unanimity: it reruns
/// the shard and the wrong root is caught at once.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_shard_nobody_could_dispute_is_rerun_at_settlement() {
    let s = settled_with(Opts {
        wrong: true,
        panelists: 0,
        panelist_bond: 5_000,
        dispute: CFG.dispute,
    });
    assert_eq!(s.path, SettlePath::Recomputed);
    assert_eq!(s.reference, s.truth);
}

/// L30-F3: a validator whose bond left cannot cover a panel slash is never
/// drawn and does not count toward the pool. Each panelist (bond 550) is
/// assigned and silent, so settlement takes its 100 missed-task slash,
/// leaving 450 < 500: nobody could sit on a panel, so the shard is rerun at
/// settlement (L29-F4). A dispute finds no panel, so the settler reruns the
/// shard (q13 (a)); the rerun confirms the root and closes the shard.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn validators_without_bond_for_a_panel_slash_are_never_drawn() {
    let mut s = settled_with(Opts {
        wrong: true,
        panelists: 10,
        panelist_bond: 550,
        dispute: CFG.dispute,
    });
    assert_eq!(s.path, SettlePath::Recomputed);
    assert_eq!(s.reference, s.truth);
    assert_eq!(open(&mut s, lie(), 410), Err(DisputeError::RerunRequired));
    let shard = s.shard;
    assert_eq!(rerun(&mut s, ROUND, shard), Ok(DisputeOutcome::Upheld));
    assert_eq!(open(&mut s, lie(), 420), Err(DisputeError::Closed));
}

/// A rule set with no built-in rules: nothing in the fixture registry is its own.
struct NoRules;

impl onyx_core::RuleSet for NoRules {
    fn proof_type(_: &str) -> Option<std::any::TypeId> {
        None
    }
    fn canon_type(_: &str) -> Option<std::any::TypeId> {
        None
    }
}

/// L29-F8: an honest panel member never reruns with rules that are not the
/// built-in ones, exactly like settlement.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn panel_reruns_refuse_registries_with_foreign_rules() {
    let s = settled(false, 0);
    let src = |_: &ShardSpec| Some(s.w.source.clone());
    let env = RecomputeEnv {
        reg: &s.w.reg,
        cas: &s.w.cas,
        host: &s.w.host,
        source: &src,
    };
    let rec = s.ledger.shard_record(ROUND, &s.shard).expect("record");
    assert_eq!(
        honest_panel_root::<NoRules>(&env, rec, &AttestCache::new()),
        None
    );
    assert_eq!(
        honest_panel_root::<BtcRules>(&env, rec, &AttestCache::new()),
        Some(s.truth)
    );
}

/// L31-F1: honest members who cannot rerun stay silent; one sybil revealing
/// junk must not turn that silence into slashes. The split goes to the
/// rerun, nobody on the panel is slashed, and the challenger pays its
/// panel's fees.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn one_sybil_reveal_in_a_split_slashes_no_honest_silent_member() {
    let mut s = settled(true, 0);
    let truth = s.truth;
    let id = open(&mut s, truth, 410).expect("open");
    panel_votes(&mut s, &id, 420, 470, &|i| (i == 0).then(lie));
    // Not unanimous, so appealable until 560; nobody appeals.
    assert_eq!(
        s.ledger.resolve_dispute(559, &id),
        Err(DisputeError::WrongPhase)
    );
    assert_eq!(
        s.ledger.resolve_dispute(560, &id),
        Ok(DisputeOutcome::RerunRequired)
    );
    no_panel_slashes(&s, &id);
    assert_eq!(s.ledger.balance(&s.challenger.address), -30);
    conserves(&s, &id);
}

/// L33-N4: a sybil's junk reveal on an earlier panel does not make that
/// panel's honest silence slashable when a later panel has a majority:
/// nobody on the first panel revealed the majority root.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn junk_on_an_earlier_panel_does_not_slash_its_silent_members() {
    let mut s = settled(true, 0);
    let truth = s.truth;
    let id = open(&mut s, truth, 410).expect("open");
    panel_votes(&mut s, &id, 420, 470, &|i| (i == 0).then(lie));
    let first: Vec<MinerAddress> = panel(&s, &id).iter().map(|v| v.address).collect();
    let a = s.appellant.appeal(&id, 0, &truth, 2_000);
    s.ledger.appeal(515, &a, keccak256(b"b2")).expect("appeal");
    panel_votes(&mut s, &id, 520, 570, &|_| Some(truth));
    assert_eq!(
        s.ledger.resolve_dispute(615, &id),
        Ok(DisputeOutcome::Overturned { reference: truth })
    );
    let e = s.ledger.entries().to_vec();
    // The junk revealer is off the majority; the silent members are not slashed.
    assert!(has(&e, &first[0], "panel: no reveal or off the majority"));
    assert!(!has(&e, &first[1], "panel: no reveal or off the majority"));
    assert!(!has(&e, &first[2], "panel: no reveal or off the majority"));
    conserves(&s, &id);
}

/// L33-N1, q13 (a): a bribed minority cannot protect a wrong unanimous root
/// by forcing a split. One bribed member reveals the wrong reference, one
/// honest member cannot rerun and stays silent: no majority. The rerun
/// restores the correct root, the lazy miners are slashed, and nobody on the
/// panel is slashed for the split.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_bribed_minority_forcing_a_split_is_overruled_by_the_rerun() {
    let mut s = settled_with(Opts {
        wrong: true,
        panelists: 10,
        panelist_bond: 5_000,
        dispute: short_window(),
    });
    assert_eq!(s.path, SettlePath::Unanimous);
    let truth = s.truth;
    let reference = s.reference;
    let work = s.w.spec.work_id();
    let id = open(&mut s, truth, 410).expect("open");
    panel_votes(&mut s, &id, 420, 470, &|i| match i {
        0 => Some(truth),
        1 => Some(reference),
        _ => None,
    });
    // No appeals in this config: the split is final for the panel.
    assert_eq!(
        s.ledger.resolve_dispute(510, &id),
        Ok(DisputeOutcome::RerunRequired)
    );
    assert_eq!(s.ledger.finalized()[&work].1, reference);
    let shard = s.shard;
    assert_eq!(
        rerun(&mut s, ROUND, shard),
        Ok(DisputeOutcome::Overturned { reference: truth })
    );
    assert_eq!(s.ledger.finalized()[&work].1, truth);
    let e = s.ledger.entries().to_vec();
    for m in &s.miners {
        assert!(has(&e, m, "revealed a wrong root"));
    }
    assert!(has(&e, &s.miners[0], "dispute: reward reversed"));
    assert_eq!(s.ledger.balance(&s.miners[0]), 500 - 500 - 1_000);
    assert_eq!(s.ledger.balance(&s.miners[1]), -1_000);
    assert!(has(&e, &s.lazy_validator.address, "wrong verdict"));
    // Nobody is slashed for the split, the bribed member included.
    no_panel_slashes(&s, &id);
    // The challenger paid its panel's fees (burned) and earns half of the
    // 2,000 newly slashed.
    assert_eq!(s.ledger.balance(&s.challenger.address), -30 + 1_000);
    conserves(&s, &id);
}

/// Settle a second round in the same ledger over the same blocks with half
/// the test labels (another shard and another work id). Returns the round
/// tag, the shard and its pinned spec, the reference it settled on, and the
/// honest rerun root.
fn settle_half_round(s: &mut Setup, round: u64, wrong: bool) -> (Hash32, Hash32, Hash32, Hash32) {
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let full = load_label_set(
        &root.join("fixtures"),
        "labels/test.onyx.entities.btc.v1.json",
    )
    .expect("labels");
    let mut okx_seen = 0usize;
    let half = LabelSet {
        name: "test.onyx.entities.btc.half".into(),
        version: 1,
        labels: full
            .labels
            .iter()
            .filter(|l| {
                if l.entity != "entity:okx" {
                    return true;
                }
                okx_seen += 1;
                okx_seen % 2 == 1
            })
            .cloned()
            .collect(),
    };
    s.w.cas.put_label_set(&half).expect("half labels");
    let mut spec = s.w.spec.clone();
    spec.label_set = Some(LabelSetRef {
        name: half.name.clone(),
        version: half.version,
        content_hash: half.content_hash(),
    });
    let shard = spec.shard_id();
    let a = Miner::new(key("miner a")).expect("a");
    let b = Miner::new(key("miner b")).expect("b");
    let wa = a
        .work(
            round,
            &spec,
            s.w.source.clone(),
            &s.w.reg,
            &s.w.cas,
            &s.w.host,
        )
        .expect("A");
    let (wa, wb) = if wrong {
        (
            invent_in(&wa, &a, "lazy half root, 8+ bytes", round),
            invent_in(&wa, &b, "lazy half root, 8+ bytes", round),
        )
    } else {
        let wb = b
            .work(
                round,
                &spec,
                s.w.source.clone(),
                &s.w.reg,
                &s.w.cas,
                &s.w.host,
            )
            .expect("B");
        (wa, wb)
    };
    let salt = keccak256(b"no canaries");
    let cfg = RoundConfig {
        validator_set_root: s.set.root(),
        canary_commit: canary_commit(&[], &salt).expect("empty"),
        dispute: s
            .ledger
            .shard_record(ROUND, &s.shard)
            .map(|r| r.dispute_config())
            .expect("round 1 record"),
        ..CFG
    };
    let mut r = common::open_round(
        round,
        vec![spec.clone()],
        &s.w.reg,
        &s.w.cas,
        cfg,
        10,
        &mut s.ledger,
    )
    .expect("open round 2");
    let tag = r.tag();
    for (n, (m, wk)) in [(&a, &wa), (&b, &wb)].iter().enumerate() {
        r.commit(
            50 + n as u64,
            &m.sign_commit(wk, 1_000, &tag),
            &mut s.ledger,
        )
        .expect("commit");
    }
    for (m, wk) in [(&a, &wa), (&b, &wb)] {
        m.publish_rows(wk, &s.w.cas).expect("rows");
        r.reveal(
            150,
            m.address,
            shard,
            wk.commitment.clone(),
            wk.salt,
            &s.w.cas,
        )
        .expect("reveal");
    }
    r.close(200).expect("close");
    let mut val = Validation::<BtcRules>::new(
        &r,
        s.set.clone(),
        keccak256(b"beacon 2"),
        201,
        &mut s.ledger,
    )
    .expect("val");
    let seed = val.seed();
    let lazy = &s.lazy_validator;
    let mut pending = Vec::new();
    for (sh, m, _) in val.tasks_for(&lazy.address) {
        val.commit_verdict(250, &lazy.commit(&seed, &sh, &m, &Verdict::Agree))
            .expect("commit verdict");
        pending.push((sh, m, lazy.salt(&seed, &sh, &m)));
    }
    for (sh, m, salt) in pending {
        val.reveal_verdict(350, lazy.address, sh, m, Verdict::Agree, salt)
            .expect("reveal verdict");
    }
    let source = s.w.source.clone();
    let src = move |_: &ShardSpec| Some(source.clone());
    let env = RecomputeEnv {
        reg: &s.w.reg,
        cas: &s.w.cas,
        host: &s.w.host,
        source: &src,
    };
    let st = val
        .settle(SETTLED_AT, Some((&[], &salt)), &env)
        .expect("settle round 2");
    assert!(s.ledger.apply(&st));
    let (reference, path) = st.references()[&shard];
    assert_eq!(path, SettlePath::Unanimous);
    let rec = s.ledger.shard_record(round, &shard).expect("record");
    let truth = honest_panel_root::<BtcRules>(&env, rec, &AttestCache::new()).expect("rerun");
    (tag, shard, reference, truth)
}

/// L31-F2 and L31-F3. Four panelists, each with free bond for exactly one
/// panel slash. Both rounds settle by unanimity (round 2 on a wrong root).
/// Then a dispute on round 1 draws three of them; while it is open they
/// stake that slash, so a dispute on round 2's shard finds no panel. The
/// root is not left final: the shard is marked for a rerun and the
/// settler's rerun corrects it. Resolving the first dispute releases the
/// stake. (Had the first dispute been open when round 2 settled, the pool
/// count would have seen the stake and rerun round 2 at settlement.)
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn open_panels_stake_bond_and_an_undisputable_unanimous_shard_is_rerun() {
    let mut s = settled_with(Opts {
        wrong: true,
        panelists: 4,
        panelist_bond: 750,
        dispute: CFG.dispute,
    });
    assert_eq!(s.path, SettlePath::Unanimous);
    let truth = s.truth;
    let round2 = ROUND + 1;
    let (tag2, shard2, reference2, truth2) = settle_half_round(&mut s, round2, true);
    assert_ne!(
        reference2, truth2,
        "round 2 settled a wrong root unanimously"
    );

    let id = open(&mut s, truth, 410).expect("open on round 1");
    let drawn = s.ledger.dispute(&id).expect("d").panels()[0]
        .members()
        .to_vec();
    assert_eq!(drawn.len(), 3);
    for m in &drawn {
        assert_eq!(s.ledger.panel_exposure(m), CFG.dispute.panel_slash);
    }
    let c = s
        .challenger
        .challenge(round2, &tag2, &shard2, &truth2, 1_000);
    assert_eq!(
        s.ledger.open_dispute(415, &c, keccak256(b"beacon round 2")),
        Err(DisputeError::RerunRequired)
    );
    assert!(s.ledger.rerun_required().contains(&(round2, shard2)));
    // Marked: later challenges are pointed to the rerun too.
    let c2 = s
        .appellant
        .challenge(round2, &tag2, &shard2, &truth2, 1_000);
    assert_eq!(
        s.ledger
            .open_dispute(416, &c2, keccak256(b"beacon round 2")),
        Err(DisputeError::RerunRequired)
    );
    let source = s.w.source.clone();
    let src = move |_: &ShardSpec| Some(source.clone());
    let env = RecomputeEnv {
        reg: &s.w.reg,
        cas: &s.w.cas,
        host: &s.w.host,
        source: &src,
    };
    assert_eq!(
        s.ledger
            .rerun_shard(round2, &shard2, &env, &AttestCache::new()),
        Ok(DisputeOutcome::Corrected { reference: truth2 })
    );
    let work2 = s
        .ledger
        .shard_record(round2, &shard2)
        .expect("rec 2")
        .spec()
        .expect("spec")
        .work_id();
    assert_eq!(s.ledger.finalized()[&work2], (round2, truth2));
    assert!(s.ledger.rerun_required().is_empty());

    // Resolving the first dispute releases the members' stake.
    panel_votes(&mut s, &id, 420, 470, &|_| Some(truth));
    assert_eq!(
        s.ledger.resolve_dispute(510, &id),
        Ok(DisputeOutcome::Overturned { reference: truth })
    );
    for m in &drawn {
        assert_eq!(s.ledger.panel_exposure(m), 0);
    }
    conserves(&s, &id);
}

/// L31-F4: a correction to "no valid root" holds the work until the shard's
/// challenge window ends, like settling with no valid root, instead of
/// burning it forever. Once released, a new round may offer it again.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_correction_to_no_valid_root_holds_the_work_and_then_releases_it() {
    let mut s = settled(false, 0);
    let marker = no_valid_root_marker(&s.shard);
    let work = s.w.spec.work_id();
    let id = open(&mut s, marker, 410).expect("open");
    // A bribed panel: unanimous on the marker (L29-R1 residual).
    panel_votes(&mut s, &id, 420, 470, &|_| Some(marker));
    assert_eq!(
        s.ledger.resolve_dispute(510, &id),
        Ok(DisputeOutcome::Overturned { reference: marker })
    );
    assert!(!s.ledger.finalized().contains_key(&work));
    assert!(
        s.ledger.taken().contains(&work),
        "held, not offered at once"
    );
    conserves(&s, &id);
    // The hold ends at settled_at + challenge_window = 600: a later round may
    // offer the work again (a burn would refuse it forever).
    let late = RoundConfig {
        commit_close: 1_100,
        reveal_close: 1_200,
        beacon_height: 1_201,
        validator_set_root: s.set.root(),
        canary_commit: canary_commit(&[], &keccak256(b"later")).expect("empty"),
        validation: ValidationConfig {
            verdict_commit_close: 1_300,
            verdict_reveal_close: 1_400,
            canary_open_close: 1_450,
            pending_close: 1_600,
            ..CFG.validation
        },
        ..CFG
    };
    common::open_round(
        ROUND + 1,
        vec![s.w.spec.clone()],
        &s.w.reg,
        &s.w.cas,
        late,
        1_000,
        &mut s.ledger,
    )
    .expect("released work can be offered again");
}

/// L33-N3: a sham challenge on a correct unanimous shard that finds no panel
/// forces one rerun at most: the rerun confirms the root, records it as
/// recomputed and closes the shard, so the next sham challenge is refused.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_rerun_that_confirms_a_unanimous_root_records_it_and_closes_the_shard() {
    let mut s = settled_with(Opts {
        wrong: false,
        panelists: 4,
        panelist_bond: 750,
        dispute: CFG.dispute,
    });
    let round2 = ROUND + 1;
    let (tag2, shard2, reference2, truth2) = settle_half_round(&mut s, round2, false);
    assert_eq!(reference2, truth2);
    let path = |s: &Setup| s.ledger.shard_record(round2, &shard2).expect("rec").path();
    assert_eq!(path(&s), SettlePath::Unanimous);
    // A dispute on round 1 stakes three of the four panelists.
    open(&mut s, lie(), 410).expect("open on round 1");
    let sham = s
        .challenger
        .challenge(round2, &tag2, &shard2, &lie(), 1_000);
    assert_eq!(
        s.ledger
            .open_dispute(415, &sham, keccak256(b"beacon round 2")),
        Err(DisputeError::RerunRequired)
    );
    assert_eq!(rerun(&mut s, round2, shard2), Ok(DisputeOutcome::Upheld));
    assert_eq!(path(&s), SettlePath::Recomputed);
    assert!(s.ledger.rerun_required().is_empty());
    let again = s.appellant.challenge(round2, &tag2, &shard2, &lie(), 1_000);
    assert_eq!(
        s.ledger
            .open_dispute(416, &again, keccak256(b"beacon round 2")),
        Err(DisputeError::Closed)
    );
}

/// M5-L46-F4: a rerun the settler never runs expires at its deadline. The
/// shard is unresolved, never final; funders lost only the burned panel
/// fees; nothing else moves and the shard is closed.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_rerun_nobody_runs_expires_unresolved_and_is_never_final() {
    let mut s = settled_with(Opts {
        wrong: true,
        panelists: 10,
        panelist_bond: 5_000,
        dispute: short_window(),
    });
    let truth = s.truth;
    let reference = s.reference;
    let work = s.w.spec.work_id();
    let shard = s.shard;
    assert!(matches!(
        s.ledger.finality(450, ROUND, &shard),
        Finality::Disputable { .. }
    ));
    let id = open(&mut s, truth, 410).expect("open");
    panel_votes(&mut s, &id, 420, 470, &|i| match i {
        0 => Some(truth),
        1 => Some(reference),
        _ => None,
    });
    assert_eq!(
        s.ledger.resolve_dispute(510, &id),
        Ok(DisputeOutcome::RerunRequired)
    );
    // Deadline: marked at 510 + challenge_window 300.
    assert_eq!(
        s.ledger.finality(700, ROUND, &shard),
        Finality::RerunPending {
            reference,
            deadline: 810
        }
    );
    assert_eq!(
        s.ledger.expire_rerun(809, ROUND, &shard),
        Err(DisputeError::WrongPhase)
    );
    let before = s.ledger.entries().len();
    assert_eq!(
        s.ledger.expire_rerun(810, ROUND, &shard),
        Ok(DisputeOutcome::Unresolved)
    );
    assert_eq!(s.ledger.entries().len(), before, "nothing else moves");
    assert_eq!(
        s.ledger.dispute(&id).expect("d").outcome(),
        Some(DisputeOutcome::Unresolved)
    );
    assert_eq!(
        s.ledger.finality(2_000, ROUND, &shard),
        Finality::Unresolved { reference }
    );
    assert_eq!(s.ledger.final_root(2_000, &work), None);
    // The challenger paid its panel's fees (burned) and keeps the rest.
    assert_eq!(s.ledger.balance(&s.challenger.address), -30);
    no_panel_slashes(&s, &id);
    assert_eq!(rerun(&mut s, ROUND, shard), Err(DisputeError::Unknown));
    assert_eq!(
        s.ledger.expire_rerun(900, ROUND, &shard),
        Err(DisputeError::Unknown)
    );
    let again = s
        .appellant
        .challenge(ROUND, &s.tag, &s.shard, &truth, 1_000);
    assert_eq!(
        s.ledger.open_dispute(650, &again, keccak256(b"b")),
        Err(DisputeError::Closed)
    );
    conserves(&s, &id);
}
