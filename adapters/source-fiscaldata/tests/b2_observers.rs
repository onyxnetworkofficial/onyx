//! ROADMAP B2: independent observers with their own keys.
//!
//! The coordinator runs the round with `Observers::Independent`: it holds
//! no observer key. Each observer reads the registry's draw itself
//! (`drawn_round`), observes, posts its own signed commit on chain and
//! sends its signed reveal to the coordinator's inbox. These tests drive
//! the observers from the inbox's first read (the coordinator's clock is
//! past the commit phase then), posting their commits at an in-phase block
//! time of the simulated registry first.

use onyx_coordinator::keyfile::{key_address, ReceiptKey};
use onyx_coordinator::observe::{
    test_keys, ChainView, ChainWait, DrawChain, DrawRecord, LogBlock, ObservationOutcome,
    ObservationResults, Observer, ObserverSig, ObserverStatus, OnchainDraw, OnchainFact, Posted,
    PostedCommit, RegistryDeployment, ReplayClock, RevealInbox, SignedReveal, SimulatedRegistry,
    TranscriptLogQuery,
};
use onyx_core::{Answer, Hash32, MinerAddress, ObservationSpec};
use onyx_source_fiscaldata::round::{
    drawn_round, observer_turn, parse_pool, run_and_save_with, store, Observers, Page,
};
use onyx_source_fiscaldata::{recorded, spec, RECORDED_BODY, RECORDED_WINDOW_START};
use std::cell::RefCell;
use std::rc::Rc;

/// Total public debt outstanding on 2026-10-08, in cents, as recorded.
const RECORDED_CENTS: i128 = 4_030_531_621_082_972;

/// One registry both the coordinator and the observers use.
#[derive(Clone)]
struct Shared(Rc<RefCell<SimulatedRegistry>>);

impl ChainView for Shared {
    fn height(&self) -> Result<u64, String> {
        self.0.borrow().height()
    }
    fn block_hash(&self, h: u64) -> Result<Option<Hash32>, String> {
        self.0.borrow().block_hash(h)
    }
}

impl DrawChain for Shared {
    fn deployment(&self) -> Result<Option<RegistryDeployment>, String> {
        self.0.borrow().deployment()
    }
    fn draw_count(&self, f: &OnchainFact) -> Result<u32, String> {
        self.0.borrow().draw_count(f)
    }
    fn draw_budget(&self, f: &OnchainFact) -> Result<(u32, u32), String> {
        self.0.borrow().draw_budget(f)
    }
    fn announce(&mut self, d: &OnchainDraw) -> Result<(), String> {
        self.0.borrow_mut().announce(d)
    }
    fn fix_beacon(&mut self, d: &OnchainDraw) -> Result<DrawRecord, String> {
        self.0.borrow_mut().fix_beacon(d)
    }
    fn draw_of(&self, f: &OnchainFact, a: u32) -> Result<Option<DrawRecord>, String> {
        self.0.borrow().draw_of(f, a)
    }
    fn draw_of_final(&self, f: &OnchainFact, a: u32) -> Result<(u64, Option<DrawRecord>), String> {
        self.0.borrow().draw_of_final(f, a)
    }
    fn post_commit(
        &mut self,
        f: &OnchainFact,
        a: u32,
        o: &MinerAddress,
        r: &Hash32,
        c: &Hash32,
        s: &ObserverSig,
    ) -> Result<Posted, String> {
        self.0.borrow_mut().post_commit(f, a, o, r, c, s)
    }
    fn post_receipt(
        &mut self,
        f: &OnchainFact,
        a: u32,
        o: &MinerAddress,
        r: &Hash32,
        c: &Hash32,
        s: &ObserverSig,
    ) -> Result<Posted, String> {
        self.0.borrow_mut().post_receipt(f, a, o, r, c, s)
    }
    fn log_block(&self) -> Result<Option<LogBlock>, String> {
        self.0.borrow().log_block()
    }
    fn posted_commit(
        &self,
        b: u64,
        f: &OnchainFact,
        a: u32,
        o: &MinerAddress,
        r: &Hash32,
    ) -> Result<Option<PostedCommit>, String> {
        self.0.borrow().posted_commit(b, f, a, o, r)
    }
    fn transcript_complete(&self, q: &TranscriptLogQuery) -> Result<bool, String> {
        self.0.borrow().transcript_complete(q)
    }
    fn draw_receipt_signer(&self, f: &OnchainFact, a: u32) -> Result<Option<[u8; 20]>, String> {
        self.0.borrow().draw_receipt_signer(f, a)
    }
}

/// Seven pool members, each with its own key (none of them the
/// coordinator's or the receipt key).
fn keys() -> Vec<[u8; 32]> {
    (0..7)
        .map(|i| test_keys::secret("b2 observer", i))
        .collect()
}

fn pool() -> Vec<Observer> {
    let text = serde_json::to_string(
        &keys()
            .iter()
            .map(|k| {
                serde_json::json!({
                    "address": format!("0x{}", hx(&key_address(k).expect("key"))),
                    "bond": 1_000,
                })
            })
            .collect::<Vec<_>>(),
    )
    .expect("json");
    parse_pool(&text).expect("pool")
}

fn hx(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn feed() -> ObservationSpec {
    spec(&recorded().record_date, RECORDED_WINDOW_START).expect("spec")
}

/// What one drawn observer process does (and what the coordinator does
/// with its reveal).
#[derive(Clone, Copy, PartialEq)]
enum Act {
    Honest,
    /// Commits and reveals another value (colluding with the other liars).
    Lie,
    /// Commits on chain, then never reveals.
    Withhold,
    /// Honest, but the coordinator drops its reveal (THREATS REVLOG-1).
    Dropped,
}

/// The observer processes, run at the inbox's first read: each reads the
/// registry's latest draw, and if drawn, observes, posts its commit on
/// chain at an in-phase block time and hands back its reveal.
struct Processes {
    reg: Rc<RefCell<SimulatedRegistry>>,
    /// One per DRAWN observer, in pool order.
    acts: Vec<Act>,
    ran: bool,
    drawn: Vec<MinerAddress>,
}

impl RevealInbox for Processes {
    fn take(&mut self, round: &Hash32, observers: &[MinerAddress], now: u64) -> Vec<SignedReveal> {
        if self.ran {
            return vec![];
        }
        self.ran = true;
        let sp = feed();
        let fact = store().onchain_fact(&sp).expect("fact");
        let mut reg = self.reg.borrow_mut();
        let n = reg.draw_count(&fact).expect("count");
        let rec = reg.draw_of(&fact, n - 1).expect("read").expect("a draw");
        let body = |cents: &str| -> Vec<u8> {
            std::str::from_utf8(RECORDED_BODY)
                .expect("utf8")
                .replace("40305316210829.72", cents)
                .into_bytes()
        };
        let mut out = vec![];
        for k in keys() {
            let me = key_address(&k).expect("key");
            let Some((seed, drawn, id)) = drawn_round(&sp, &pool(), &rec, &me).expect("the draw")
            else {
                continue;
            };
            // Every observer computes the coordinator's round on its own.
            assert_eq!(id, *round);
            assert_eq!(
                drawn.iter().map(|o| o.address).collect::<Vec<_>>(),
                observers
            );
            let act = self.acts[self.drawn.len()];
            self.drawn.push(me);
            let page = Page {
                fetched_unix: sp.window_start + 10,
                body: Some(match act {
                    Act::Lie => body("30305316210829.72"),
                    _ => RECORDED_BODY.to_vec(),
                }),
            };
            let (commit, sig, reveal) =
                observer_turn(&sp, &id, seed.attempt, &k, &page).expect("turn");
            reg.time = sp.window_start + 20;
            reg.post_commit(&fact, seed.attempt, &me, &id, &commit, &sig)
                .expect("postCommit in phase");
            if !matches!(act, Act::Withhold | Act::Dropped) {
                out.push(reveal);
            }
        }
        // The chain moves on: the log is read past the commit phase.
        reg.time = now;
        out
    }
}

/// Run one live-shaped round with `acts` per drawn observer.
fn round(
    acts: Vec<Act>,
) -> (
    onyx_source_fiscaldata::round::RoundRun,
    Rc<RefCell<SimulatedRegistry>>,
    Vec<MinerAddress>,
) {
    let reg = Rc::new(RefCell::new(SimulatedRegistry::new()));
    let receipt = test_keys::secret("b2 receipt", 0);
    reg.borrow_mut().receipt_signer = key_address(&receipt).expect("key");
    let mut inbox = Processes {
        reg: reg.clone(),
        acts,
        ran: false,
        drawn: vec![],
    };
    let dir = std::env::temp_dir().join(format!(
        "onyx-b2-observers-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    std::fs::create_dir_all(&dir).expect("dir");
    let book = dir.join("book.json");
    let mut s: ObservationResults = store();
    let key = receipt_key(&dir, &receipt);
    let mut chain = Shared(reg.clone());
    let r = run_and_save_with(
        &recorded().record_date,
        RECORDED_WINDOW_START,
        Observers::Independent(&pool()),
        &mut s,
        &mut chain,
        &book,
        None,
        &ChainWait::NOW,
        &ReplayClock::default(),
        &mut inbox,
        &key,
        None,
        &mut |_| Ok(()),
        &mut |_| Ok(()),
    )
    .expect("the round runs");
    let _ = std::fs::remove_dir_all(&dir);
    (r, reg, inbox.drawn)
}

fn rand_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

/// The deployment's receipt key, from a 0600 key file.
fn receipt_key(dir: &std::path::Path, secret: &[u8; 32]) -> ReceiptKey {
    let p = dir.join("receipt.key");
    std::fs::write(&p, hx(secret)).expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }
    ReceiptKey::from_file(&p).expect("receipt key")
}

/// ROADMAP B2: five drawn observers out of seven, each with its own key,
/// commit on chain and reveal through the inbox; the coordinator, holding
/// none of their keys, absorbs the posted commits, receipts each reveal with
/// the deployment's receipt key, and the round settles the recorded value.
#[test]
fn b2_independent_observers_settle_the_round_without_the_coordinator_holding_their_keys() {
    let (r, reg, drawn) = round(vec![Act::Honest; 5]);
    assert_eq!(drawn.len(), 5, "five of the seven are drawn");
    assert_eq!(
        *r.settlement.outcome(),
        ObservationOutcome::Value(Answer::Number(RECORDED_CENTS))
    );
    assert!(r
        .settlement
        .statuses()
        .iter()
        .all(|(_, s)| *s == ObserverStatus::Agreed));
    assert_eq!(r.commits.len(), 5, "every commit came from the chain");
    let fact = store().onchain_fact(&feed()).expect("fact");
    for a in &drawn {
        assert!(
            reg.borrow()
                .log
                .receipt(&fact.registry_key(), 0, a, &r.round_id)
                .is_some(),
            "each reveal is receipted on chain"
        );
    }
    assert!(r.claim.is_some(), "the claim is accepted at level 2");
}

/// ROADMAP B2 adversarial: two drawn observers colluding on a false value
/// (a minority of the five) are outvoted and slashed as outliers; one that
/// commits and withholds its reveal is a non-reveal (never slashed,
/// ADR-0027 item 6). Their keys are their own; the coordinator cannot sign
/// for them. A colluding MAJORITY of a draw still settles its value: the
/// limit THREATS L2-F2 states, which only independent operators in a large
/// pool make unlikely.
#[test]
fn b2_colluding_observers_win_only_as_a_majority_of_the_draw() {
    let lie = |r: &onyx_source_fiscaldata::round::RoundRun| {
        r.settlement
            .statuses()
            .iter()
            .filter(|(_, s)| *s == ObserverStatus::Outlier)
            .count()
    };
    let (r, _, _) = round(vec![
        Act::Lie,
        Act::Lie,
        Act::Honest,
        Act::Honest,
        Act::Honest,
    ]);
    assert_eq!(
        *r.settlement.outcome(),
        ObservationOutcome::Value(Answer::Number(RECORDED_CENTS))
    );
    assert_eq!(lie(&r), 2, "the colluding minority is slashed");
    let (r, _, drawn) = round(vec![
        Act::Withhold,
        Act::Honest,
        Act::Honest,
        Act::Honest,
        Act::Honest,
    ]);
    assert_eq!(
        *r.settlement.outcome(),
        ObservationOutcome::Value(Answer::Number(RECORDED_CENTS))
    );
    assert!(r
        .settlement
        .statuses()
        .iter()
        .any(|(a, s)| *a == drawn[0] && *s == ObserverStatus::Unrevealed));
    let (r, _, _) = round(vec![Act::Lie, Act::Lie, Act::Lie, Act::Honest, Act::Honest]);
    assert_eq!(
        *r.settlement.outcome(),
        ObservationOutcome::Value(Answer::Number(3_030_531_621_082_972)),
        "a colluding majority of the draw wins (THREATS L2-F2)"
    );
}

/// ROADMAP B2 adversarial (THREATS REVLOG-1, residual): a coordinator that
/// drops one honest reveal is not caught by the chain (it never signed a
/// receipt), so the round settles without it and the observer shows as
/// unrevealed; what the observer holds is its posted commit and the
/// missing receipt, which it sees on chain (`level2-observer` exits 3).
#[test]
fn b2_a_coordinator_that_drops_a_reveal_leaves_the_observer_without_a_receipt() {
    let (r, reg, drawn) = round(vec![
        Act::Honest,
        Act::Dropped,
        Act::Honest,
        Act::Honest,
        Act::Honest,
    ]);
    let fact = store().onchain_fact(&feed()).expect("fact");
    let dropped: Vec<_> = drawn
        .iter()
        .filter(|a| {
            reg.borrow()
                .log
                .receipt(&fact.registry_key(), 0, a, &r.round_id)
                .is_none()
        })
        .collect();
    assert_eq!(
        dropped,
        vec![&drawn[1]],
        "only the dropped observer lacks a receipt"
    );
    assert_eq!(
        *r.settlement.outcome(),
        ObservationOutcome::Value(Answer::Number(RECORDED_CENTS)),
        "the chain cannot tell: the round settles without it"
    );
    for a in &dropped {
        assert!(r
            .settlement
            .statuses()
            .iter()
            .any(|(x, s)| x == *a && *s == ObserverStatus::Unrevealed));
    }
}

/// ROADMAP B2 adversarial: an observer refuses to commit to a draw that does
/// not commit to ITS pool snapshot (a coordinator drawing from another
/// pool) or whose beacon is not fixed (ATT-R1-F4); a pool member not drawn
/// is told so and does nothing.
#[test]
fn b2_an_observer_commits_only_to_a_draw_of_its_own_pool() {
    let reg = Rc::new(RefCell::new(SimulatedRegistry::new()));
    let sp = feed();
    let receipt = test_keys::secret("b2 receipt", 0);
    reg.borrow_mut().receipt_signer = key_address(&receipt).expect("key");
    let mut s = store();
    let mut chain = Shared(reg.clone());
    let (round, rec) = onyx_coordinator::observe::draw_and_open(
        &mut s,
        sp.clone(),
        onyx_source_fiscaldata::round::config(&sp),
        &pool(),
        &mut chain,
        &ChainWait::NOW,
        &mut |_| Ok(()),
    )
    .expect("open");
    let drawn: Vec<_> = round.observers().iter().map(|o| o.address).collect();
    let me = drawn[0];
    let (_, _, id) = drawn_round(&sp, &pool(), &rec, &me)
        .expect("ok")
        .expect("drawn");
    assert_eq!(id, round.round_id());
    let outsider = pool()
        .iter()
        .map(|o| o.address)
        .find(|a| !drawn.contains(a))
        .expect("seven members, five drawn");
    assert_eq!(drawn_round(&sp, &pool(), &rec, &outsider), Ok(None));
    // Another pool (one member's bond changed): the commitment differs.
    let mut other = pool();
    other[0].bond += 1;
    assert!(drawn_round(&sp, &other, &rec, &me).is_err());
    // An unfixed beacon.
    let mut unfixed = rec;
    unfixed.beacon = Hash32::ZERO;
    assert!(drawn_round(&sp, &pool(), &unfixed, &me).is_err());
}

/// ROADMAP B2 adversarial (key reuse): one key cannot hold two seats in the
/// pool snapshot (a member listed twice, in any case or order, is refused),
/// and a malformed snapshot is refused rather than read as a smaller pool.
#[test]
fn b2_a_pool_snapshot_with_a_member_twice_is_refused() {
    let a = format!("0x{}", hx(&key_address(&keys()[0]).expect("key")));
    let b = format!("0x{}", hx(&key_address(&keys()[1]).expect("key")));
    let twice = format!(
        r#"[{{"address":"{a}","bond":1000}},{{"address":"{b}","bond":1000}},{{"address":"{}","bond":5}}]"#,
        a.to_uppercase().replace("0X", "0x")
    );
    assert!(parse_pool(&twice).is_err(), "a member listed twice");
    for bad in [
        "[]".to_string(),
        "{}".into(),
        format!(r#"[{{"address":"{a}"}}]"#),
        format!(r#"[{{"address":"{a}","bond":-1}}]"#),
        r#"[{"address":"0x1234","bond":1}]"#.into(),
    ] {
        assert!(parse_pool(&bad).is_err(), "{bad}");
    }
    let ok = format!(r#"[{{"address":"{b}","bond":1000}},{{"address":"{a}","bond":1000}}]"#);
    let p = parse_pool(&ok).expect("two members, any order");
    assert!(p[0].address < p[1].address, "sorted ascending");
}
