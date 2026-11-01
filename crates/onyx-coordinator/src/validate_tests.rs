//! Validation tests (ADR-0019): the bonded validator set, the seed, assignments, verdicts and
//! the review findings (L22 and later) as exploits.

use super::*;

/// Review L2-R2: the bond covers one fee per member, exactly at the bound;
/// the PoC's fee 400 x 9 members against bond 1,000 is refused; overflow
/// is refused.
#[test]
fn fees_covered_by_bond_bounds() {
    assert!(fees_covered_by_bond(1_000, 111, 9));
    assert!(!fees_covered_by_bond(1_000, 112, 9));
    assert!(!fees_covered_by_bond(1_000, 400, 9));
    assert!(
        fees_covered_by_bond(4_000_000, 50_000, 16),
        "testnet and demo config"
    );
    assert!(fees_covered_by_bond(0, 0, 128));
    assert!(!fees_covered_by_bond(u128::MAX, u128::MAX, 2));
}

/// A rule set for ledger unit tests.
struct TestRules;
impl RuleSet for TestRules {
    fn proof_type(_: &str) -> Option<TypeId> {
        None
    }
    fn canon_type(_: &str) -> Option<TypeId> {
        None
    }
}

fn settlement_slashing(
    ledger: &mut Ledger<TestRules>,
    round: u64,
    v: MinerAddress,
    bond: u128,
    amount: u128,
) -> Settlement {
    let tag = keccak256(&round.to_be_bytes());
    let seed = keccak256(&[round.to_be_bytes().as_slice(), b"seed"].concat());
    ledger.rounds.insert(
        round,
        RoundRecord {
            tag,
            seed: Some(seed),
            known_at_open: BTreeSet::new(),
            known_at_validation: BTreeSet::new(),
        },
    );
    Settlement {
        round,
        tag,
        seed,
        statuses: BTreeMap::new(),
        entries: vec![LedgerEntry::Slash {
            round,
            actor: v,
            amount,
            reason: "wrong verdict",
        }],
        finalized: BTreeMap::new(),
        references: BTreeMap::new(),
        pending: BTreeSet::new(),
        shards: BTreeSet::new(),
        round_level: true,
        work_of: BTreeMap::new(),
        miners: BTreeSet::new(),
        rules: TypeId::of::<TestRules>(),
        canaries: BTreeSet::new(),
        burned: BTreeSet::new(),
        records: BTreeMap::new(),
        bonds: [(v, bond)].into_iter().collect(),
        effect_ranges: BTreeMap::new(),
    }
}

/// L30-F2: two validations settled from the same snapshot (each thinks
/// the validator still has its whole bond) cannot together take more
/// than the bond: `apply` caps against the live ledger total.
#[test]
fn concurrent_settlements_never_slash_past_the_bond() {
    let mut ledger = Ledger::<TestRules>::new();
    let v = [7u8; 20];
    let a = settlement_slashing(&mut ledger, 1, v, 1_000, 600);
    let b = settlement_slashing(&mut ledger, 2, v, 1_000, 600);
    assert!(ledger.apply(&a));
    assert!(ledger.apply(&b));
    assert_eq!(ledger.validator_slashed[&v], 1_000);
    assert_eq!(ledger.balance(&v), -1_000);
    // A third, fully capped slash adds no entry at all.
    let c = settlement_slashing(&mut ledger, 3, v, 1_000, 600);
    let before = ledger.entries().len();
    assert!(ledger.apply(&c));
    assert_eq!(ledger.entries().len(), before);
}

/// L30-F4: no-valid-root work is held (taken) until its challenge window
/// closes, and longer while a dispute on it is open.
#[test]
fn no_valid_root_work_is_held_until_no_dispute_can_change_it() {
    let mut ledger = Ledger::<TestRules>::new();
    let work = keccak256(b"work");
    let shard = keccak256(b"shard");
    ledger.held.insert(work, (500, 1, shard));
    assert!(ledger.taken().contains(&work));
    ledger.release_held(499);
    assert!(ledger.taken().contains(&work));
    ledger
        .open_disputes
        .insert((1, shard), keccak256(b"dispute"));
    ledger.release_held(600);
    assert!(
        ledger.taken().contains(&work),
        "held while a dispute is open"
    );
    ledger.open_disputes.clear();
    // A pending rerun keeps it too (M5-L46-F1).
    ledger.rerun_required.insert((1, shard));
    ledger.release_held(600);
    assert!(
        ledger.taken().contains(&work),
        "held while a rerun is pending"
    );
    ledger.rerun_required.clear();
    ledger.release_held(600);
    assert!(!ledger.taken().contains(&work));
}

#[test]
fn sampling_rate_is_respected() {
    let seed = keccak256(b"seed");
    let shard = keccak256(b"shard");
    let n = 20_000u32;
    let hits = (0..n)
        .filter(|i| {
            let mut v = [0u8; 20];
            v[..4].copy_from_slice(&i.to_be_bytes());
            assigned(&seed, &v, &shard, &[1; 20], 250_000)
        })
        .count();
    // 25% of 20,000 = 5,000; a 4-sigma band is about +/- 245.
    assert!((4_755..=5_245).contains(&hits), "{hits}");
    assert!(assigned(&seed, &[0; 20], &shard, &[1; 20], PPM));
    assert!(!assigned(&seed, &[0; 20], &shard, &[1; 20], 0));
    // Never one's own reveal.
    assert!(!assigned(&seed, &[1; 20], &shard, &[1; 20], PPM));
}

#[test]
fn deterrence_condition() {
    assert!(deterrence_holds(1_000, 100, 100_000));
    assert!(!deterrence_holds(999, 100, 100_000));
    // L22-F12: overflow is not "deters".
    assert!(!deterrence_holds(u128::MAX, u128::MAX / 2, 2));
}

#[test]
fn canary_list_must_be_strictly_sorted() {
    let a = Canary {
        shard: keccak256(b"s"),
        miner: [1; 20],
        root: keccak256(b"r1"),
    };
    let mut b = a;
    b.miner = [2; 20];
    let salt = keccak256(b"salt");
    assert!(canary_commit(&[a, b], &salt).is_some());
    assert_eq!(canary_commit(&[b, a], &salt), None);
    // L22-F3: one (shard, miner) twice is refused.
    let mut dup = a;
    dup.root = keccak256(b"r2");
    assert_eq!(canary_commit(&[a, dup], &salt), None);
}
/// Mutation L4: validator deterrence needs every one of its conditions;
/// each fails here on its own, and each bound holds at its edge.
#[test]
fn validators_deterred_needs_every_condition() {
    let ok = ValidationConfig {
        verdict_commit_close: 0,
        verdict_reveal_close: 0,
        canary_open_close: 0,
        pending_close: 0,
        rate_ppm: 0,
        miner_reward: 0,
        validator_fee: 10,
        validator_slash: 1_000,
        missed_slash: 1_000,
        // 1,000 x 1% = 10: a blind Agree loses exactly what it earns.
        min_canary_ppm: 10_000,
    };
    assert!(ok.validators_deterred());
    let with = |f: &dyn Fn(&mut ValidationConfig)| {
        let mut c = ok;
        f(&mut c);
        c.validators_deterred()
    };
    // Exactly one condition fails in each.
    assert!(!with(&|c| c.missed_slash = 0));
    assert!(!with(&|c| c.missed_slash = 1_001));
    assert!(!with(&|c| {
        c.validator_fee = 0;
        c.min_canary_ppm = 0;
    }));
    assert!(!with(&|c| c.min_canary_ppm = PPM + 1));
    assert!(!with(&|c| c.validator_fee = 11));
    // Edges that hold.
    assert!(with(&|c| c.missed_slash = 1));
    assert!(with(&|c| {
        c.validator_fee = 0;
        c.min_canary_ppm = 1;
    }));
    assert!(with(&|c| c.min_canary_ppm = PPM));
}
/// Mutation L4: the bond cap is for validator slashes only; a miner's
/// slash (no validator bond) is applied in full.
#[test]
fn a_miner_slash_is_not_capped_by_validator_bonds() {
    let mut ledger = Ledger::<TestRules>::new();
    let m = [8u8; 20];
    let mut s = settlement_slashing(&mut ledger, 1, m, 0, 700);
    s.entries = vec![LedgerEntry::Slash {
        round: 1,
        actor: m,
        amount: 700,
        reason: "revealed a wrong root",
    }];
    s.bonds.clear();
    assert!(ledger.apply(&s));
    assert_eq!(ledger.balance(&m), -700);
}

/// Mutation L4: held work is released at the end of its window (the
/// height itself), not one block later.
#[test]
fn held_work_is_released_at_its_height() {
    let mut ledger = Ledger::<TestRules>::new();
    let work = keccak256(b"work");
    ledger.held.insert(work, (500, 1, keccak256(b"shard")));
    ledger.release_held(500);
    assert!(!ledger.taken().contains(&work));
}

/// Mutation L4: a contract mirror's ledger id is the contract's
/// (`evm_ledger_id`), not the default.
#[test]
fn a_contract_mirror_has_the_contracts_ledger_id() {
    let c = [3u8; 20];
    let mirror = Ledger::<TestRules>::mirror_of_contract(84_532, &c);
    assert_eq!(mirror.id(), onyx_core::evm_ledger_id(84_532, &c));
    assert_ne!(mirror.id(), Ledger::<TestRules>::new().id());
}

// ---- ROADMAP E8 (THREATS MUT-1): settlement outcomes on real rounds ----
//
// The fixture (`crate::test_fixture`) opens, commits, reveals and closes a
// real round of one shard in milliseconds; its recipe's rows do not
// depend on the input, so the rerun reference is the honest root.

use crate::round::RoundConfig;
use crate::test_fixture::{
    actors, canary_salt, signed_verdict, verdict_salt, Actor, Closed, Plays, Rules, World, CFG,
    ROUND,
};
use Plays::{Honest, Silent, Wrong};

/// Source bytes available to reruns.
fn source(_: &ShardSpec) -> Option<Vec<u8>> {
    Some(crate::test_fixture::SOURCE.to_vec())
}

/// Source bytes withheld: reruns are undecided.
fn withheld(_: &ShardSpec) -> Option<Vec<u8>> {
    None
}

/// Settle with the empty canary list the fixture committed to.
fn settle(
    c: &Closed,
    val: &mut Validation<Rules>,
    now: u64,
    src: &dyn Fn(&ShardSpec) -> Option<Vec<u8>>,
) -> Result<Settlement, ValidationError> {
    val.settle(now, Some((&[], &canary_salt())), &c.w.env(src))
}

/// Entries as sorted `(reward?, actor, amount, reason)` tuples.
fn tally(entries: &[LedgerEntry]) -> Vec<(bool, MinerAddress, u128, &'static str)> {
    let mut t: Vec<_> = entries
        .iter()
        .map(|e| match e {
            LedgerEntry::Reward {
                actor,
                amount,
                reason,
                ..
            } => (true, *actor, *amount, *reason),
            LedgerEntry::Slash {
                actor,
                amount,
                reason,
                ..
            } => (false, *actor, *amount, *reason),
        })
        .collect();
    t.sort_unstable();
    t
}

fn reward(
    a: &Actor,
    amount: u128,
    reason: &'static str,
) -> (bool, MinerAddress, u128, &'static str) {
    (true, a.addr, amount, reason)
}

fn slash(
    a: &Actor,
    amount: u128,
    reason: &'static str,
) -> (bool, MinerAddress, u128, &'static str) {
    (false, a.addr, amount, reason)
}

fn sorted<T: Ord>(mut v: Vec<T>) -> Vec<T> {
    v.sort_unstable();
    v
}

#[test]
fn seed_draw_voids_at_each_edge() {
    // Verdict window: at + 20 <= verdict_commit_close.
    assert!(!seed_draw_voids(201, 300, 280));
    assert!(seed_draw_voids(201, 300, 281));
    // Beacon window: at - beacon <= 256.
    assert!(!seed_draw_voids(201, 1_000, 457));
    assert!(seed_draw_voids(201, 1_000, 458));
    // Before the beacon nothing has lapsed.
    assert!(!seed_draw_voids(201, 1_000, 100));
}

#[test]
fn validator_set_rules() {
    let mut set = ValidatorSet::new();
    assert_eq!(
        set.add([1; 20], 99, 100),
        Err(ValidationError::BadValidator)
    );
    assert_eq!(set.add([1; 20], 100, 100), Ok(()));
    assert_eq!(
        set.add([1; 20], 500, 100),
        Err(ValidationError::BadValidator)
    );
    assert!(set.contains(&[1; 20]) && !set.contains(&[2; 20]));
    assert_eq!(set.bond(&[1; 20]), 100);
    assert_eq!(set.bond(&[2; 20]), 0);
    let empty_root = ValidatorSet::new().root();
    assert_ne!(set.root(), empty_root);
    let mut other = ValidatorSet::new();
    other.add([1; 20], 101, 0).expect("add");
    assert_ne!(set.root(), other.root(), "the root binds the bond");
    assert_eq!(
        sample_seed(&Hash32([1; 32]), &Hash32([2; 32])),
        keccak256(&[b"onyx-seed-evm-v1".as_slice(), &[1; 32], &[2; 32]].concat())
    );
}

/// The verdict rule shared with the contract (`OnyxDisputes.verdictRight`).
#[test]
fn verdict_right_table() {
    let (mine, other, re) = (Hash32([1; 32]), Hash32([2; 32]), Hash32([1; 32]));
    let dis = |root| Verdict::Disagree { root };
    assert_eq!(verdict_right(&Verdict::Abstain, &mine, Some(&re)), None);
    assert_eq!(verdict_right(&Verdict::Abstain, &mine, None), None);
    assert_eq!(
        verdict_right(&Verdict::NoValidRoot, &mine, None),
        Some(true)
    );
    assert_eq!(
        verdict_right(&Verdict::NoValidRoot, &mine, Some(&re)),
        Some(false)
    );
    assert_eq!(verdict_right(&Verdict::Agree, &mine, None), Some(false));
    assert_eq!(verdict_right(&dis(other), &mine, None), Some(false));
    assert_eq!(verdict_right(&Verdict::Agree, &mine, Some(&re)), Some(true));
    assert_eq!(
        verdict_right(&Verdict::Agree, &other, Some(&re)),
        Some(false)
    );
    // Disagree is right only on a wrong reveal and only naming the reference.
    assert_eq!(verdict_right(&dis(re), &other, Some(&re)), Some(true));
    assert_eq!(
        verdict_right(&dis(Hash32([3; 32])), &other, Some(&re)),
        Some(false)
    );
    assert_eq!(verdict_right(&dis(re), &mine, Some(&re)), Some(false));
    assert!(is_validator_slash("verdict not revealed"));
    assert!(is_validator_slash("panel: no reveal or off the majority"));
    assert!(!is_validator_slash("commit not revealed"));
    assert_eq!(
        entry_amount(&LedgerEntry::Reward {
            round: 1,
            actor: [1; 20],
            amount: 7,
            reason: "x"
        }),
        7
    );
}

#[test]
fn verdict_texts_are_fixed() {
    let (seed, shard, commit) = (Hash32([1; 32]), Hash32([2; 32]), Hash32([3; 32]));
    assert_eq!(
        verdict_message(&seed, &shard, &[0xab; 20], &commit),
        format!(
            "Onyx verdict v2\nseed: {seed}\nshard: {shard}\nminer: 0x{}\ncommit: {commit}",
            "ab".repeat(20)
        )
    );
    let h = |v: &Verdict| verdict_commit_hash(&seed, &[4; 20], &shard, &[5; 20], v, &commit);
    assert_ne!(h(&Verdict::Agree), h(&Verdict::Abstain));
    assert_ne!(
        h(&Verdict::Disagree { root: seed }),
        h(&Verdict::Disagree { root: shard })
    );
}

/// Validation starts once, from the ledger that opened the round, in its
/// window, with the round's validator set and fees the bond covers.
#[test]
fn validation_starts_once_in_its_window() {
    let mut c = Closed::new(&[Honest]);
    // The round's set, no other.
    let mut wrong = c.set.clone();
    wrong.add([0xee; 20], 10_000, 0).expect("add");
    assert_eq!(
        Validation::new(&c.round, wrong, keccak256(b"b"), 210, &mut c.ledger).err(),
        Some(ValidationError::BadValidator)
    );
    assert_eq!(
        c.validation_at(200).err(),
        Some(ValidationError::WrongPhase)
    );
    assert_eq!(
        c.validation_at(281).err(),
        Some(ValidationError::SeedTooLate)
    );
    let mut other = Ledger::<Rules>::new();
    assert_eq!(
        Validation::new(&c.round, c.set.clone(), keccak256(b"b"), 210, &mut other).err(),
        Some(ValidationError::NotOpenedHere)
    );
    let val = c
        .validation_at(280)
        .expect("the last height that leaves a verdict window");
    assert_eq!(
        val.seed(),
        sample_seed(&c.round.tag(), &keccak256(b"beacon"))
    );
    assert_eq!(
        c.validation_at(280).err(),
        Some(ValidationError::AlreadyValidating)
    );
    // Committers and validators now have history (L25-F6).
    assert!(c.ledger.known_miners().contains(&c.miners[0].addr));
    assert!(c.ledger.known_miners().contains(&c.validators[0].addr));
    // Rate 100%: every validator checks the one reveal, a miner never its own.
    let task = (c.shard, c.miners[0].addr, c.good);
    assert_eq!(val.tasks_for(&c.validators[3].addr), vec![task]);
    assert!(val.tasks_for(&c.miners[0].addr).is_empty(), "not a member");

    // A round that is not closed.
    let w = World::new();
    let mut l = Ledger::<Rules>::new();
    let open = w.open(1, vec![w.spec(1)], CFG, 10, &mut l).expect("open");
    assert_eq!(
        Validation::new(&open, ValidatorSet::new(), keccak256(b"b"), 210, &mut l).err(),
        Some(ValidationError::WrongPhase)
    );
}

/// `min_bond >= validator_fee x members` (review L2-R2), at the bound.
#[test]
fn validation_needs_fees_the_bond_covers() {
    let mut cfg = CFG;
    cfg.validation.validator_fee = 100;
    let canary = canary_commit(&[], &canary_salt()).expect("empty");
    let mut ten = Closed::with(&[Honest], actors("v", 10), &[10_000; 10], canary, cfg);
    assert!(ten.validation_at(210).is_ok());
    let mut eleven = Closed::with(&[Honest], actors("v", 11), &[10_000; 11], canary, cfg);
    assert_eq!(
        eleven.validation_at(210).err(),
        Some(ValidationError::FeesExceedBond)
    );
}

/// Verdict commits and reveals at each edge of their windows and rules.
#[test]
fn verdict_rules_at_each_edge() {
    let mut c = Closed::new(&[Honest, Wrong]);
    let mut val = c.validation();
    let seed = val.seed();
    let (v0, v1) = (&c.validators[0], &c.validators[1]);
    let (m0, m1) = (c.miners[0].addr, c.miners[1].addr);
    let sv = signed_verdict(&seed, v0, &c.shard, &m0, &Verdict::Agree);
    assert_eq!(
        val.commit_verdict(300, &sv),
        Err(ValidationError::WrongPhase)
    );
    let mut forged = sv.clone();
    forged.validator = v1.addr;
    assert_eq!(
        val.commit_verdict(250, &forged),
        Err(ValidationError::BadSignature)
    );
    let outsider = Actor::new("outsider");
    let so = signed_verdict(&seed, &outsider, &c.shard, &m0, &Verdict::Agree);
    assert_eq!(
        val.commit_verdict(250, &so),
        Err(ValidationError::BadValidator)
    );
    let nobody = signed_verdict(&seed, v0, &c.shard, &[0x77; 20], &Verdict::Agree);
    assert_eq!(
        val.commit_verdict(250, &nobody),
        Err(ValidationError::UnknownReveal)
    );
    let stray = signed_verdict(&seed, v0, &Hash32([4; 32]), &m0, &Verdict::Agree);
    assert_eq!(
        val.commit_verdict(250, &stray),
        Err(ValidationError::UnknownReveal)
    );
    assert_eq!(val.commit_verdict(250, &sv), Ok(()));
    assert_eq!(
        val.commit_verdict(299, &sv),
        Err(ValidationError::Duplicate)
    );
    let s1 = signed_verdict(
        &seed,
        v1,
        &c.shard,
        &m1,
        &Verdict::Disagree { root: c.root_of(1) },
    );
    assert_eq!(
        val.commit_verdict(249, &s1),
        Err(ValidationError::TimeWentBack)
    );
    assert_eq!(val.commit_verdict(299, &s1), Ok(()));

    let salt0 = verdict_salt(v0, &c.shard, &m0);
    let reveal = |val: &mut Validation<Rules>, now, v: &Actor, m, verdict, salt| {
        val.reveal_verdict(now, v.addr, c.shard, m, verdict, salt)
    };
    assert_eq!(
        reveal(&mut val, 299, v0, m0, Verdict::Agree, salt0),
        Err(ValidationError::WrongPhase)
    );
    assert_eq!(
        reveal(&mut val, 400, v0, m0, Verdict::Agree, salt0),
        Err(ValidationError::WrongPhase)
    );
    assert_eq!(
        reveal(&mut val, 300, v0, m0, Verdict::Abstain, salt0),
        Err(ValidationError::CommitMismatch)
    );
    assert_eq!(
        reveal(&mut val, 300, v0, m0, Verdict::Agree, Hash32([1; 32])),
        Err(ValidationError::CommitMismatch)
    );
    assert_eq!(
        reveal(&mut val, 300, v0, m1, Verdict::Agree, salt0),
        Err(ValidationError::CommitMismatch),
        "no commit for this task"
    );
    // A Disagree naming the revealed root is not a disagreement.
    let salt1 = verdict_salt(v1, &c.shard, &m1);
    assert_eq!(
        reveal(
            &mut val,
            300,
            v1,
            m1,
            Verdict::Disagree { root: c.root_of(1) },
            salt1
        ),
        Err(ValidationError::CommitMismatch)
    );
    assert_eq!(reveal(&mut val, 300, v0, m0, Verdict::Agree, salt0), Ok(()));
    assert_eq!(
        reveal(&mut val, 399, v0, m0, Verdict::Agree, salt0),
        Err(ValidationError::Duplicate)
    );
    assert_eq!(
        reveal(&mut val, 299, v0, m0, Verdict::Agree, salt0),
        Err(ValidationError::TimeWentBack)
    );
    // Settled: nothing more.
    settle(&c, &mut val, 400, &source).expect("settle");
    assert_eq!(val.commit_verdict(250, &s1), Err(ValidationError::Settled));
    assert_eq!(
        reveal(&mut val, 399, v1, m1, Verdict::Agree, salt1),
        Err(ValidationError::Settled)
    );
}

/// Below 100% some tasks are not assigned, and committing to one is refused.
#[test]
fn a_verdict_on_an_unassigned_task_is_refused() {
    let mut cfg = CFG;
    cfg.validation.rate_ppm = 500_000;
    let canary = canary_commit(&[], &canary_salt()).expect("empty");
    let mut c = Closed::with(
        &[Honest, Honest],
        actors("validator", 5),
        &[10_000; 5],
        canary,
        cfg,
    );
    let mut val = c.validation();
    let seed = val.seed();
    let (v, m) = c
        .validators
        .iter()
        .flat_map(|v| c.miners.iter().map(move |m| (v, m)))
        .find(|(v, m)| !assigned(&seed, &v.addr, &c.shard, &m.addr, 500_000))
        .expect("an unassigned task at 50%");
    let sv = signed_verdict(&seed, v, &c.shard, &m.addr, &Verdict::Agree);
    assert_eq!(
        val.commit_verdict(250, &sv),
        Err(ValidationError::NotAssigned)
    );
    assert!(!val.tasks_for(&v.addr).iter().any(|(_, a, _)| *a == m.addr));
}

/// Unanimous, undisputed, with a full dispute pool: the first correct
/// reveal by commit order earns the reward once; a silent committer
/// forfeits its bond; silent assigned validators are slashed the missed
/// amount once each.
#[test]
fn unanimous_shard_pays_the_first_correct_reveal_once() {
    let mut c = Closed::new(&[Honest, Honest, Silent]);
    let mut val = c.validation();
    assert_eq!(
        settle(&c, &mut val, 399, &withheld).err(),
        Some(ValidationError::WrongPhase)
    );
    // No rerun needed: the source is withheld and the shard still settles.
    let s = settle(&c, &mut val, 400, &withheld).expect("settle");
    assert_eq!(s.round(), ROUND);
    assert!(s.round_level());
    assert_eq!(s.references()[&c.shard], (c.good, SettlePath::Unanimous));
    assert_eq!(s.finalized()[&c.shard], c.good);
    assert!(s.pending().is_empty());
    assert_eq!(
        s.shards().iter().copied().collect::<Vec<_>>(),
        vec![c.shard]
    );
    assert_eq!(
        s.statuses()[&(c.shard, c.miners[0].addr)],
        RevealStatus::Correct
    );
    assert_eq!(
        s.statuses()[&(c.shard, c.miners[1].addr)],
        RevealStatus::Correct
    );
    assert_eq!(s.statuses().len(), 2);
    let mut want = vec![
        reward(&c.miners[0], 500, "first correct reveal"),
        slash(&c.miners[2], 1_002, "commit not revealed"),
    ];
    want.extend(
        c.validators
            .iter()
            .map(|v| slash(v, 100, "assigned task not answered")),
    );
    assert_eq!(tally(s.entries()), sorted(want.clone()));
    assert!(c.ledger.apply(&s));
    assert_eq!(tally(c.ledger.entries()), sorted(want));
    assert!(!c.ledger.apply(&s), "once");
    assert_eq!(c.ledger.balance(&c.miners[0].addr), 500);
    assert_eq!(c.ledger.balance(&c.miners[1].addr), 0);
    assert_eq!(c.ledger.balance(&c.miners[2].addr), -1_002);
    assert_eq!(c.ledger.balance(&c.validators[0].addr), -100);
    let work = c.spec.work_id();
    assert_eq!(c.ledger.finalized()[&work], (ROUND, c.good));
    assert!(!c.ledger.open_work().contains(&work));
    assert!(c.ledger.taken().contains(&work));
    // A later call settles nothing new.
    let again = settle(&c, &mut val, 400, &withheld).expect("again");
    assert!(again.entries().is_empty() && again.shards().is_empty());
    assert!(!again.round_level());
    assert_eq!(
        settle(&c, &mut val, 399, &withheld).err(),
        Some(ValidationError::TimeWentBack)
    );
}

/// Reveals that differ are rerun: the wrong one loses its bond, the first
/// correct one (not the first to commit) is paid; each right verdict earns
/// one fee per (validator, shard, root), each wrong one is slashed, an
/// Abstain is a missed task, a verdict committed and never revealed is
/// slashed once at round level and not also as missed.
#[test]
fn a_split_shard_is_rerun_and_everyone_is_judged_by_the_rerun() {
    let mut c = Closed::new(&[Wrong, Honest, Honest]);
    let mut val = c.validation();
    let (good, bad) = (c.good, c.root_of(0));
    c.verdicts(
        &mut val,
        &[
            (0, 0, Verdict::Disagree { root: good }),
            (0, 1, Verdict::Agree),
            (0, 2, Verdict::Agree),
            (1, 0, Verdict::Agree),
            (1, 2, Verdict::NoValidRoot),
            (2, 0, Verdict::Abstain),
        ],
        &[(3, 1, Verdict::Agree)],
    );
    let s = settle(&c, &mut val, 400, &source).expect("settle");
    assert_eq!(s.references()[&c.shard], (good, SettlePath::Recomputed));
    assert_eq!(s.finalized()[&c.shard], good);
    assert_ne!(bad, good);
    assert_eq!(
        s.statuses()[&(c.shard, c.miners[0].addr)],
        RevealStatus::Wrong
    );
    assert_eq!(
        s.statuses()[&(c.shard, c.miners[1].addr)],
        RevealStatus::Correct
    );
    assert_eq!(
        s.statuses()[&(c.shard, c.miners[2].addr)],
        RevealStatus::Correct
    );
    let v = &c.validators;
    let want = vec![
        slash(&c.miners[0], 1_000, "revealed a wrong root"),
        reward(&c.miners[1], 500, "first correct reveal"),
        // v0: right on the wrong root and on the good root, once each.
        reward(&v[0], 10, "correct verdict"),
        reward(&v[0], 10, "correct verdict"),
        // v1: Agree on a wrong root, "no valid root" on a correct one.
        slash(&v[1], 2_000, "wrong verdict"),
        slash(&v[1], 2_000, "wrong verdict"),
        // v2 abstained: unanswered.
        slash(&v[2], 100, "assigned task not answered"),
        // v3 committed, never revealed: round level only.
        slash(&v[3], 2_000, "verdict not revealed"),
        slash(&v[4], 100, "assigned task not answered"),
    ];
    assert_eq!(tally(s.entries()), sorted(want));
    assert!(c.ledger.apply(&s));
    assert_eq!(c.ledger.balance(&v[0].addr), 20);
    assert_eq!(c.ledger.balance(&v[1].addr), -4_000);
    assert_eq!(c.ledger.validator_slashed[&v[1].addr], 4_000);
    assert_eq!(c.ledger.validator_slashed[&v[3].addr], 2_000);
    // The record a dispute reverses holds only the reference-dependent entries.
    let rec = &c.ledger.settled[&(ROUND, c.shard)];
    assert_eq!(rec.path, SettlePath::Recomputed);
    assert_eq!(rec.reference, good);
    assert_eq!(rec.work, c.spec.work_id());
    assert_eq!(rec.settled_at, 400);
    assert_eq!(
        tally(&rec.effects),
        sorted(vec![
            slash(&c.miners[0], 1_000, "revealed a wrong root"),
            reward(&c.miners[1], 500, "first correct reveal"),
            reward(&v[0], 10, "correct verdict"),
            reward(&v[0], 10, "correct verdict"),
            slash(&v[1], 2_000, "wrong verdict"),
            slash(&v[1], 2_000, "wrong verdict"),
        ])
    );
    assert_eq!(rec.verdict_givers.len(), 4);
    assert_eq!(rec.verdicts.len(), 6);
}

/// One root, but a validator disputes it: rerun (and the disputer is
/// wrong). "No valid root" disputes too.
#[test]
fn a_disputed_unanimous_shard_is_rerun() {
    for dispute in [
        Verdict::Disagree {
            root: Hash32([9; 32]),
        },
        Verdict::NoValidRoot,
    ] {
        let mut c = Closed::new(&[Honest, Honest]);
        let mut val = c.validation();
        c.verdicts(&mut val, &[(0, 1, dispute)], &[]);
        let s = settle(&c, &mut val, 400, &source).expect("settle");
        assert_eq!(s.references()[&c.shard], (c.good, SettlePath::Recomputed));
        assert!(tally(s.entries()).contains(&slash(&c.validators[0], 2_000, "wrong verdict")));
        assert!(tally(s.entries()).contains(&reward(&c.miners[0], 500, "first correct reveal")));
    }
    // An Agree is no dispute.
    let mut c = Closed::new(&[Honest, Honest]);
    let mut val = c.validation();
    c.verdicts(&mut val, &[(0, 1, Verdict::Agree)], &[]);
    let s = settle(&c, &mut val, 400, &withheld).expect("settle");
    assert_eq!(s.references()[&c.shard].1, SettlePath::Unanimous);
}

/// A unanimous shard is taken as is only if a panel could still be drawn
/// to dispute it: more than `panel_size` bonded validators outside its
/// miners and verdict-givers, each able to stake a panel slash on top of
/// what was taken, staked and can still be slashed (L29-F4, L30-F3, L31-F2, F3).
#[test]
fn unanimity_needs_a_panel_that_could_dispute_it() {
    let canary = canary_commit(&[], &canary_salt()).expect("empty");
    let path = |validators: Vec<Actor>, bonds: &[u128], prep: &dyn Fn(&mut Closed)| {
        let mut c = Closed::with(&[Honest, Honest], validators, bonds, canary, CFG);
        prep(&mut c);
        let mut val = c.validation();
        settle(&c, &mut val, 400, &source)
            .expect("settle")
            .references()[&c.shard]
            .1
    };
    let none = |_: &mut Closed| {};
    // panel_size 3: four free validators are a pool, three are not.
    assert_eq!(
        path(actors("v", 4), &[10_000; 4], &none),
        SettlePath::Unanimous
    );
    assert_eq!(
        path(actors("v", 3), &[10_000; 3], &none),
        SettlePath::Recomputed
    );
    // A miner in the set is not in the pool.
    let mut with_miner = actors("v", 4);
    with_miner[3] = Actor::new("miner0");
    assert_eq!(
        path(with_miner, &[10_000; 4], &none),
        SettlePath::Recomputed
    );
    // A validator that can still lose 100 (silent at 100%) needs 600 to sit.
    assert_eq!(
        path(actors("v", 4), &[10_000, 10_000, 10_000, 600], &none),
        SettlePath::Unanimous
    );
    assert_eq!(
        path(actors("v", 4), &[10_000, 10_000, 10_000, 599], &none),
        SettlePath::Recomputed
    );
    // What the ledger already took, or open panels stake, counts too.
    let low = Actor::new("v3").addr;
    assert_eq!(
        path(actors("v", 4), &[10_000, 10_000, 10_000, 601], &|c| {
            c.ledger.validator_slashed.insert(low, 1);
        }),
        SettlePath::Unanimous
    );
    assert_eq!(
        path(actors("v", 4), &[10_000, 10_000, 10_000, 601], &|c| {
            c.ledger.validator_slashed.insert(low, 2);
        }),
        SettlePath::Recomputed
    );
    assert_eq!(
        path(actors("v", 4), &[10_000, 10_000, 10_000, 601], &|c| {
            c.ledger.panel_exposure.insert(low, 2);
        }),
        SettlePath::Recomputed
    );
}

/// A verdict-giver is not in the dispute pool, and a verdict committed
/// and never revealed counts against the giver's free bond.
#[test]
fn verdict_givers_leave_the_pool() {
    // Five validators, one gives a verdict: four free, a pool.
    let mut c = Closed::new(&[Honest, Honest]);
    let mut val = c.validation();
    c.verdicts(&mut val, &[(0, 0, Verdict::Agree)], &[]);
    let s = settle(&c, &mut val, 400, &withheld).expect("settle");
    assert_eq!(s.references()[&c.shard].1, SettlePath::Unanimous);
    // Two give verdicts: three free, no pool.
    let mut c = Closed::new(&[Honest, Honest]);
    let mut val = c.validation();
    c.verdicts(
        &mut val,
        &[(0, 0, Verdict::Agree)],
        &[(1, 1, Verdict::Agree)],
    );
    let s = settle(&c, &mut val, 400, &source).expect("settle");
    assert_eq!(s.references()[&c.shard].1, SettlePath::Recomputed);
}

/// A shard whose rerun cannot be decided (source withheld) waits; at
/// `pending_close` it is voided: no rewards, no slashes, and its work is
/// burned because its reveals are public.
#[test]
fn undecided_shards_wait_then_are_voided() {
    let mut c = Closed::new(&[Honest, Wrong, Silent]);
    let mut val = c.validation();
    c.verdicts(&mut val, &[], &[(0, 0, Verdict::Agree)]);
    let first = settle(&c, &mut val, 400, &withheld).expect("settle");
    assert_eq!(
        first.pending().iter().copied().collect::<Vec<_>>(),
        vec![c.shard]
    );
    assert!(first.shards().is_empty() && first.references().is_empty());
    assert_eq!(
        first.statuses()[&(c.shard, c.miners[0].addr)],
        RevealStatus::Pending
    );
    assert_eq!(
        first.statuses()[&(c.shard, c.miners[1].addr)],
        RevealStatus::Pending
    );
    // Round-level entries come with the first call.
    assert_eq!(
        tally(first.entries()),
        sorted(vec![
            slash(&c.miners[2], 1_002, "commit not revealed"),
            slash(&c.validators[0], 2_000, "verdict not revealed"),
        ])
    );
    assert!(c.ledger.apply(&first));
    let work = c.spec.work_id();
    assert!(c.ledger.open_work().contains(&work), "still open");
    let still = settle(&c, &mut val, 599, &withheld).expect("settle");
    assert_eq!(still.pending().len(), 1);
    assert!(still.entries().is_empty() && !still.round_level());
    let void = settle(&c, &mut val, 600, &withheld).expect("settle");
    assert_eq!(
        void.references()[&c.shard],
        (Hash32::ZERO, SettlePath::Voided)
    );
    assert_eq!(
        void.statuses()[&(c.shard, c.miners[0].addr)],
        RevealStatus::Voided
    );
    assert_eq!(
        void.statuses()[&(c.shard, c.miners[1].addr)],
        RevealStatus::Voided
    );
    assert!(void.entries().is_empty() && void.finalized().is_empty());
    assert!(void.pending().is_empty());
    assert_eq!(void.shards().len(), 1);
    assert!(c.ledger.apply(&void));
    assert!(!c.ledger.open_work().contains(&work));
    assert!(!c.ledger.finalized().contains_key(&work));
    assert!(c.ledger.taken().contains(&work), "burned");
    assert_eq!(c.ledger.balance(&c.miners[1].addr), 0);
}

/// Inputs that arrive before `pending_close` decide a pending shard.
#[test]
fn a_pending_shard_settles_when_its_inputs_arrive() {
    let mut c = Closed::new(&[Honest, Wrong]);
    let mut val = c.validation();
    let first = settle(&c, &mut val, 400, &withheld).expect("settle");
    assert_eq!(first.pending().len(), 1);
    assert!(c.ledger.apply(&first));
    let later = settle(&c, &mut val, 599, &source).expect("settle");
    assert_eq!(
        later.references()[&c.shard],
        (c.good, SettlePath::Recomputed)
    );
    assert!(!later.round_level());
    assert!(c.ledger.apply(&later));
    assert_eq!(c.ledger.balance(&c.miners[0].addr), 500);
    assert_eq!(c.ledger.balance(&c.miners[1].addr), -1_001);
    assert_eq!(c.ledger.finalized()[&c.spec.work_id()], (ROUND, c.good));
}

/// The canary list: opened on the first call before `canary_open_close`
/// (from then on settlement may go without it), it must match its
/// commitment and the reveals, and name fresh addresses only.
#[test]
fn canaries_open_in_time_and_match_fresh_reveals() {
    let salt = keccak256(b"canaries");
    let build = |plays: &[Plays], list: &dyn Fn(&Closed) -> Vec<Canary>| {
        // The list depends on the shard and roots: build once to learn them.
        let probe = Closed::new(plays);
        let l = list(&probe);
        let commit = canary_commit(&l, &salt).expect("sorted");
        let c = Closed::with(plays, actors("validator", 5), &[10_000; 5], commit, CFG);
        (c, l)
    };
    let one = |c: &Closed| {
        vec![Canary {
            shard: c.shard,
            miner: c.miners[0].addr,
            root: c.root_of(0),
        }]
    };
    let (mut c, list) = build(&[Honest, Honest], &one);
    let mut val = c.validation();
    let env_src = |val: &mut Validation<Rules>, now, arg| val.settle(now, arg, &c.w.env(&source));
    assert_eq!(
        env_src(&mut val, 400, None).err(),
        Some(ValidationError::WrongPhase),
        "before canary_open_close the list must be opened"
    );
    assert_eq!(
        env_src(&mut val, 400, Some((&list, &Hash32([1; 32])))).err(),
        Some(ValidationError::CanaryMismatch)
    );
    let mut off = list.clone();
    off[0].root = Hash32([3; 32]);
    let off_commit = canary_commit(&off, &salt).expect("sorted");
    let mut c2 = Closed::with(
        &[Honest, Honest],
        actors("validator", 5),
        &[10_000; 5],
        off_commit,
        CFG,
    );
    let mut val2 = c2.validation();
    assert_eq!(
        val2.settle(400, Some((&off, &salt)), &c2.w.env(&source))
            .err(),
        Some(ValidationError::CanaryMismatch),
        "a canary must match a reveal"
    );
    let s = env_src(&mut val, 400, Some((&list, &salt))).expect("settle");
    // A canary makes the shard a rerun; it earns nothing; the first
    // correct non-canary reveal is paid.
    assert_eq!(s.references()[&c.shard], (c.good, SettlePath::Recomputed));
    assert_eq!(
        s.statuses()[&(c.shard, c.miners[0].addr)],
        RevealStatus::Canary
    );
    assert_eq!(
        s.statuses()[&(c.shard, c.miners[1].addr)],
        RevealStatus::Correct
    );
    assert!(tally(s.entries()).contains(&reward(&c.miners[1], 500, "first correct reveal")));
    assert!(!tally(s.entries()).iter().any(|e| e.1 == c.miners[0].addr));
    assert!(c.ledger.apply(&s));
    assert_eq!(c.ledger.balance(&c.miners[0].addr), 0);
}

/// Validators are judged on canaries like any reveal; a right verdict on
/// a canary earns one fee per (validator, shard), apart from the fee on
/// the real root; a canary with a wrong root is not slashed, an Agree on
/// it is.
#[test]
fn validators_are_judged_on_canaries() {
    let salt = keccak256(b"canaries");
    let probe = Closed::new(&[Wrong, Honest]);
    let list = vec![Canary {
        shard: probe.shard,
        miner: probe.miners[0].addr,
        root: probe.root_of(0),
    }];
    let commit = canary_commit(&list, &salt).expect("sorted");
    let mut c = Closed::with(
        &[Wrong, Honest],
        actors("validator", 5),
        &[10_000; 5],
        commit,
        CFG,
    );
    let mut val = c.validation();
    c.verdicts(
        &mut val,
        &[
            (0, 0, Verdict::Disagree { root: c.good }),
            (0, 1, Verdict::Agree),
            (1, 0, Verdict::Agree),
        ],
        &[],
    );
    let s = val
        .settle(400, Some((&list, &salt)), &c.w.env(&source))
        .expect("settle");
    let t = tally(s.entries());
    assert_eq!(
        s.statuses()[&(c.shard, c.miners[0].addr)],
        RevealStatus::Canary
    );
    assert!(
        !t.iter().any(|e| e.1 == c.miners[0].addr),
        "a canary is never slashed"
    );
    assert_eq!(
        t.iter().filter(|e| e.1 == c.validators[0].addr).count(),
        2,
        "a canary fee and a real fee"
    );
    assert!(t.contains(&slash(&c.validators[1], 2_000, "wrong verdict")));
    assert!(t.contains(&reward(&c.miners[1], 500, "first correct reveal")));
    let rec = &s.records[&c.shard];
    assert_eq!(rec.canaries, [c.miners[0].addr].into_iter().collect());
}

/// From `canary_open_close` on, settlement may go without the list; a
/// canary address with history is refused.
#[test]
fn canaries_are_optional_late_and_must_be_fresh() {
    let mut c = Closed::new(&[Honest, Honest]);
    let mut val = c.validation();
    assert_eq!(
        val.settle(449, None, &c.w.env(&withheld)).err(),
        Some(ValidationError::WrongPhase)
    );
    let s = val.settle(450, None, &c.w.env(&withheld)).expect("settle");
    assert_eq!(s.references()[&c.shard].1, SettlePath::Unanimous);
    assert!(c.ledger.apply(&s));

    let salt = keccak256(b"canaries");
    let probe = Closed::new(&[Honest, Honest]);
    let list = vec![Canary {
        shard: probe.shard,
        miner: probe.miners[0].addr,
        root: probe.root_of(0),
    }];
    let commit = canary_commit(&list, &salt).expect("sorted");
    let mut c = Closed::with(
        &[Honest, Honest],
        actors("validator", 5),
        &[10_000; 5],
        commit,
        CFG,
    );
    c.ledger.record_known([c.miners[0].addr]);
    let mut val = c.validation();
    assert_eq!(
        val.settle(400, Some((&list, &salt)), &c.w.env(&source))
            .err(),
        Some(ValidationError::CanaryMismatch)
    );
}

/// Voided work is burned only if a real reveal made a root public; a
/// shard whose only reveals are canaries is released (L27-F3).
#[test]
fn a_canary_only_voided_shard_is_released() {
    let salt = keccak256(b"canaries");
    let probe = Closed::new(&[Honest]);
    let list = vec![Canary {
        shard: probe.shard,
        miner: probe.miners[0].addr,
        root: probe.root_of(0),
    }];
    let commit = canary_commit(&list, &salt).expect("sorted");
    let mut c = Closed::with(&[Honest], actors("validator", 5), &[10_000; 5], commit, CFG);
    let mut val = c.validation();
    let s = val
        .settle(600, Some((&list, &salt)), &c.w.env(&withheld))
        .expect("settle");
    assert_eq!(s.references()[&c.shard].1, SettlePath::Voided);
    assert!(s.burned.is_empty());
    assert!(c.ledger.apply(&s));
    assert!(!c.ledger.taken().contains(&c.spec.work_id()));
}

/// A validator's slashes in one validation stop at its bond, counting
/// what the ledger took before; a fully capped slash adds no entry.
#[test]
fn validator_slashes_stop_at_the_bond() {
    let canary = canary_commit(&[], &canary_salt()).expect("empty");
    let mut c = Closed::with(
        &[Wrong, Wrong, Honest],
        actors("validator", 5),
        &[10_000, 10_000, 10_000, 10_000, 5_000],
        canary,
        CFG,
    );
    let poor = c.validators[4].addr;
    c.ledger.validator_slashed.insert(poor, 500);
    let mut val = c.validation();
    // Two wrong verdicts (2,000 each) and an unrevealed one (2,000):
    // 4,500 of bond left, so 2,000 + 2,000 + 500.
    c.verdicts(
        &mut val,
        &[(4, 0, Verdict::Agree), (4, 1, Verdict::Agree)],
        &[(4, 2, Verdict::Agree)],
    );
    let s = settle(&c, &mut val, 400, &source).expect("settle");
    let mine: Vec<u128> = s
        .entries()
        .iter()
        .filter_map(|e| match e {
            LedgerEntry::Slash { actor, amount, .. } if *actor == poor => Some(*amount),
            _ => None,
        })
        .collect();
    assert_eq!(sorted(mine), vec![500, 2_000, 2_000]);
    assert!(s.entries().iter().all(|e| entry_amount(e) > 0));
    assert!(c.ledger.apply(&s));
    assert_eq!(c.ledger.validator_slashed[&poor], 5_000);
    assert_eq!(c.ledger.balance(&poor), -4_500);

    // Fully slashed before: no entry at all.
    let mut c = Closed::with(
        &[Wrong, Honest],
        actors("validator", 5),
        &[10_000, 10_000, 10_000, 10_000, 5_000],
        canary,
        CFG,
    );
    c.ledger.validator_slashed.insert(poor, 5_000);
    let mut val = c.validation();
    c.verdicts(&mut val, &[(4, 0, Verdict::Agree)], &[]);
    let s = settle(&c, &mut val, 400, &source).expect("settle");
    assert!(!s
        .entries()
        .iter()
        .any(|e| matches!(e, LedgerEntry::Slash { actor, .. } if *actor == poor)));
}

/// `apply` takes only this ledger's settlements, once, and holds
/// no-valid-root work for its challenge window.
#[test]
fn apply_takes_its_own_settlements_once() {
    let mut c = Closed::new(&[Wrong, Honest]);
    let mut val = c.validation();
    let s = settle(&c, &mut val, 400, &source).expect("settle");
    let mut stranger = Ledger::<Rules>::new();
    assert!(!stranger.apply(&s));
    assert!(stranger.entries().is_empty());
    // Another rule set's settlement.
    let mut foreign = s.clone();
    foreign.rules = TypeId::of::<TestRules>();
    assert!(!c.ledger.apply(&foreign));
    // Another seed or tag.
    let mut reseeded = s.clone();
    reseeded.seed = Hash32([1; 32]);
    assert!(!c.ledger.apply(&reseeded));
    let mut retagged = s.clone();
    retagged.tag = Hash32([1; 32]);
    assert!(!c.ledger.apply(&retagged));
    // A canary that had history when validation started.
    let mut stale = s.clone();
    stale.canaries.insert(c.validators[0].addr);
    assert!(!c.ledger.apply(&stale));
    // No valid root: held until settled_at + challenge_window.
    let mut nvr = s.clone();
    nvr.records.get_mut(&c.shard).expect("record").path = SettlePath::NoValidRoot;
    assert!(c.ledger.apply(&nvr));
    let work = c.spec.work_id();
    assert_eq!(c.ledger.held[&work], (600, ROUND, c.shard));
    assert!(!c.ledger.apply(&s), "already applied");
    // Round level: the round's committers are known.
    assert!(c.ledger.known_miners().contains(&c.miners[1].addr));
}

/// `apply` keeps the first finalized root of a work, and burns voided
/// work only from the settlement's own list.
#[test]
fn apply_records_work_from_the_settlement() {
    let mut c = Closed::new(&[Honest]);
    let mut val = c.validation();
    let s = settle(&c, &mut val, 400, &withheld).expect("settle");
    let work = c.spec.work_id();
    c.ledger.finalized.insert(work, (0, Hash32([5; 32])));
    assert!(c.ledger.apply(&s));
    assert_eq!(
        c.ledger.finalized()[&work],
        (0, Hash32([5; 32])),
        "first root kept"
    );
    assert!(!c.ledger.burned.contains(&work));
    assert!(c.ledger.held.is_empty());
}

/// A validator slash `apply` caps to zero (the ledger took the bond
/// meanwhile) leaves no entry, and no effect in the shard's record.
#[test]
fn a_slash_capped_to_zero_leaves_no_entry_and_no_effect() {
    let mut c = Closed::new(&[Wrong, Honest]);
    let mut val = c.validation();
    c.verdicts(
        &mut val,
        &[(1, 0, Verdict::Agree), (0, 1, Verdict::Agree)],
        &[],
    );
    let s = settle(&c, &mut val, 400, &source).expect("settle");
    let v1 = c.validators[1].addr;
    assert!(tally(s.entries()).contains(&slash(&c.validators[1], 2_000, "wrong verdict")));
    // Another validation took v1's whole bond before this one is applied.
    c.ledger.validator_slashed.insert(v1, 10_000);
    assert!(c.ledger.apply(&s));
    assert!(c.ledger.entries().iter().all(|e| entry_amount(e) > 0));
    assert_eq!(c.ledger.balance(&v1), 0);
    let rec = &c.ledger.settled[&(ROUND, c.shard)];
    assert!(!rec
        .effects
        .iter()
        .any(|e| matches!(e, LedgerEntry::Slash { actor, .. } if *actor == v1)));
    assert_eq!(
        rec.effects.len(),
        3,
        "the miner slash, the reward and v0's fee"
    );
}

/// A registry whose address rules are not this binary's built-in ones
/// is never used for a rerun: a split shard stays pending (L25-F2).
#[test]
fn reruns_need_the_built_in_rules() {
    let canary = canary_commit(&[], &canary_salt()).expect("empty");
    let mut c = Closed::in_world(
        World::with_foreign_canon(),
        &[Wrong, Honest],
        actors("validator", 5),
        &[10_000; 5],
        canary,
        CFG,
    );
    let mut val = c.validation();
    let s = settle(&c, &mut val, 400, &source).expect("settle");
    assert_eq!(
        s.pending().iter().copied().collect::<Vec<_>>(),
        vec![c.shard]
    );
    // A unanimous shard needs no rerun, so no rules.
    let mut c = Closed::in_world(
        World::with_foreign_canon(),
        &[Honest, Honest],
        actors("validator", 5),
        &[10_000; 5],
        canary,
        CFG,
    );
    let mut val = c.validation();
    let s = settle(&c, &mut val, 400, &source).expect("settle");
    assert_eq!(s.references()[&c.shard].1, SettlePath::Unanimous);
}

/// Two shards in one round: a dispute on one is no dispute on the other,
/// each record holds its own verdicts only, and a verdict committed and
/// never revealed counts against its giver's free bond in every shard's
/// dispute pool.
#[test]
fn shards_of_one_round_are_judged_apart() {
    // Shard A: two honest reveals. Shard B: an honest and a wrong one.
    // v0 disputes B; v1 commits a verdict on B it never reveals (2,000 at
    // risk) and is silent on A (100 at risk). Four validators, panel 3:
    // A is unanimous only if all four can sit, so v1 needs 2,100 + 500.
    let run = |v1_bond: u128| {
        let w = World::new();
        let mut ledger = Ledger::<Rules>::new();
        let (a, b) = (w.spec(1), w.spec(2));
        let (sa, sb) = (a.shard_id(), b.shard_id());
        let m = actors("miner", 4);
        let v = actors("validator", 4);
        let mut set = ValidatorSet::new();
        for (i, x) in v.iter().enumerate() {
            set.add(x.addr, if i == 1 { v1_bond } else { 10_000 }, 0)
                .expect("validator");
        }
        let cfg = RoundConfig {
            validator_set_root: set.root(),
            canary_commit: canary_commit(&[], &canary_salt()).expect("empty"),
            ..CFG
        };
        let mut round = w
            .open(ROUND, vec![a.clone(), b.clone()], cfg, 10, &mut ledger)
            .expect("open");
        let works = [
            (sa, w.honest(&a, ROUND, &m[0])),
            (sa, w.honest(&a, ROUND, &m[1])),
            (sb, w.honest(&b, ROUND, &m[2])),
            (sb, w.wrong(&b, ROUND, &m[3], "wrong b")),
        ];
        for (i, (sh, wk)) in works.iter().enumerate() {
            let c = crate::test_fixture::signed_commit(&round, sh, &m[i], wk, 1_000);
            round.commit(50, &c, &mut ledger).expect("commit");
        }
        for (i, (sh, wk)) in works.iter().enumerate() {
            round
                .reveal(150, m[i].addr, *sh, wk.commitment.clone(), wk.salt, &w.cas)
                .expect("reveal");
        }
        round.close(200).expect("close");
        let mut val = Validation::<Rules>::new(&round, set, keccak256(b"beacon"), 210, &mut ledger)
            .expect("validation");
        let seed = val.seed();
        let good_b = w.honest_root(&b);
        let d = signed_verdict(
            &seed,
            &v[0],
            &sb,
            &m[3].addr,
            &Verdict::Disagree { root: good_b },
        );
        val.commit_verdict(250, &d).expect("commit");
        let u = signed_verdict(&seed, &v[1], &sb, &m[2].addr, &Verdict::Agree);
        val.commit_verdict(250, &u).expect("commit");
        val.reveal_verdict(
            350,
            v[0].addr,
            sb,
            m[3].addr,
            Verdict::Disagree { root: good_b },
            verdict_salt(&v[0], &sb, &m[3].addr),
        )
        .expect("reveal");
        let first = val
            .settle(400, Some((&[], &canary_salt())), &w.env(&withheld))
            .expect("settle");
        (w, val, first, sa, sb)
    };
    // v1 can sit: A is unanimous; B is disputed and waits for its inputs.
    let (_, _, first, sa, sb) = run(2_600);
    assert_eq!(first.references()[&sa].1, SettlePath::Unanimous);
    assert!(first.records[&sa].verdicts.is_empty());
    assert!(first.records[&sa].verdict_givers.is_empty());
    assert_eq!(
        first.pending().iter().copied().collect::<Vec<_>>(),
        vec![sb]
    );
    // One less: no pool for A, which must be rerun too.
    let (w, mut val, first, sa, sb) = run(2_599);
    assert_eq!(first.pending().len(), 2);
    let later = val.settle(401, None, &w.env(&source)).expect("settle");
    assert_eq!(later.references()[&sa].1, SettlePath::Recomputed);
    assert_eq!(later.references()[&sb].1, SettlePath::Recomputed);
    assert!(later.records[&sa].verdicts.is_empty());
    assert_eq!(later.records[&sb].verdicts.len(), 1);
}
