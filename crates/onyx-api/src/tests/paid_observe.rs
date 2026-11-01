//! ADR-0049 (NORTH-STAR NS10, north-star loop NL8): a level-2 subject no round settled yet is
//! observed only after a prepaid hold; the hold is charged after the round is served and
//! released in full otherwise. A synthetic daily feed, settled by the coordinator's own round
//! code with the shared test keys.
use super::*;
use crate::observations::{ObservationStore, TemplatePins, Transcript};
use crate::observe::{
    step, ticket_hash, ObserveOffer, ObserveState, RoundRunner, TicketState, BECOMES_PUBLIC,
    REFUND_RULE,
};
use crate::prepaid::{domain_separator, Account, PrepaidError, Voucher};
use crate::prepaid_gate::{voucher_header, ChargeError, Held, PrepaidState, PREPAID_VOUCHER};
use onyx_coordinator::observe::{
    sign_commit, sign_reveal, test_keys, ObservationOutcome, ObservationResults, ObserveConfig,
    Observer,
};
use onyx_core::claim::MetricId;
use onyx_core::{
    day_number, observation_commit, Answer, FeedTemplate, Observation, ObservationKind,
    Redistribution, Subject, SubjectRule,
};
use std::collections::BTreeMap;
use std::sync::Mutex;

const METRIC: &str = "test.daily";
const PRICE: u64 = 500;
const DEADLINE: u64 = 1_800;
/// The day the tests run on: 2026-10-21, one hour in.
fn today() -> u64 {
    u64::try_from(day_number("2026-10-21").expect("day")).expect("day") * 86_400 + 3_600
}
fn subj(date: &str) -> String {
    format!("{METRIC}:{date}")
}
fn uri(date: &str) -> String {
    format!("/v1/observations/{METRIC}?subject={}", subj(date))
}

fn template() -> FeedTemplate {
    FeedTemplate {
        feed: "test.daily.v1".into(),
        metric: MetricId(METRIC.into()),
        fetcher: keccak256(b"test daily fetcher"),
        url_template: "https://example.test/daily/{date}.json".into(),
        selector: "/value".into(),
        kind: ObservationKind::Numeric {
            scale: 2,
            tolerance: 0,
        },
        window_secs: 300,
        min_observers: 3,
        rights: DataRights {
            license: "CC0-1.0".into(),
            redistribution: Redistribution::Allowed,
            source: SourceId("test.page".into()),
            attribution: Some("Source: test page".into()),
            terms: String::new(),
        },
        subject: SubjectRule::Daily {
            prefix: METRIC.into(),
            min_days: 1,
            max_days: 30,
        },
    }
}

/// One settled round of `t` for `subject` from `start`, observers answering `answers`.
fn settle(t: &FeedTemplate, subject: &Subject, start: u64, answers: &[i128]) -> Transcript {
    let (mut store, admin) = ObservationResults::new();
    store
        .register_feed(&admin, t.clone(), "test", 0)
        .expect("registers");
    let spec = t.instantiate(subject, start).expect("spec");
    let cfg = ObserveConfig {
        commit_close: spec.window_end + 60,
        reveal_close: spec.window_end + 180,
        reward: 10,
        outlier_slash: 100,
        miss_slash: 20,
    };
    let observers: Vec<Observer> = (1..=answers.len() as u8)
        .map(|i| Observer {
            address: test_keys::ordered(i).1,
            bond: 1_000,
        })
        .collect();
    let snapshot = store
        .announce_draw(&spec, &cfg, &observers, 10, &SimulatedChain::at(0))
        .expect("announces");
    let mut round = store
        .open_round(
            spec.clone(),
            cfg,
            snapshot,
            &SimulatedChain::with_beacon(Hash32([3; 32])),
        )
        .expect("opens");
    let observers = round.observers().to_vec();
    let rid = round.round_id();
    let attempt = round.draw().attempt;
    let mut reveals = Vec::new();
    for (i, n) in (1..=answers.len() as u8).zip(answers) {
        let (sk, a) = test_keys::ordered(i);
        let o = Observation {
            answer: Answer::Number(*n),
            observed_at: start + 10,
            page_hash: keccak256(&n.to_be_bytes()),
        };
        let salt = keccak256(&[i]);
        let c = observation_commit(&rid, &a, &o, &salt);
        let sig = sign_commit(&sk, &rid, attempt, &c).expect("sign");
        round.commit(start + 10, a, c, sig).expect("commit");
        reveals.push((sk, a, o, salt));
    }
    for (sk, a, o, s) in &reveals {
        let sig = sign_reveal(sk, &rid, attempt, o, s).expect("sign");
        round
            .reveal(cfg.commit_close + 1, *a, o.clone(), *s, sig)
            .expect("reveal");
    }
    let commits = round.signed_commits();
    let reveals = round.signed_reveals();
    let settled = store.settle(&round, cfg.reveal_close + 1).expect("settles");
    let ObservationOutcome::Value(answer) = settled.outcome().clone() else {
        panic!("no value")
    };
    Transcript {
        subject: subject.clone(),
        window_start: start,
        cfg,
        pool: round.pool().to_vec(),
        draw: round.draw(),
        observers,
        commits,
        reveals,
        sealed: Default::default(),
        answer,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Honest,
    Fails,
    OtherDay,
    Forged,
    /// Finishes after the hold's deadline.
    Late,
}

/// The operator's coordinator, faked: settles a round now, then moves the clock past it.
struct Runner {
    calls: AtomicU64,
    mode: Mutex<Mode>,
    now: Arc<AtomicU64>,
}

impl RoundRunner for Runner {
    fn run<'a>(
        &'a self,
        template: &'a FeedTemplate,
        subject: &'a Subject,
        now: u64,
        deadline: u64,
    ) -> BoxFuture<'a, Result<Transcript, String>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mode = *self.mode.lock().expect("mode");
        Box::pin(async move {
            let start = now - now % template.window_secs;
            let t = match mode {
                Mode::Fails => return Err("the source did not answer".into()),
                Mode::Honest => settle(template, subject, start, &[7, 7, 7]),
                Mode::OtherDay => settle(
                    template,
                    &Subject::Aggregate(subj("2026-10-18")),
                    start,
                    &[7, 7, 7],
                ),
                Mode::Forged => {
                    let mut t = settle(template, subject, start, &[7, 7, 7]);
                    t.answer = Answer::Number(8);
                    t
                }
                Mode::Late => {
                    let t = settle(template, subject, start, &[7, 7, 7]);
                    self.now.store(deadline + 1, Ordering::SeqCst);
                    return Ok(t);
                }
            };
            self.now.store(t.cfg.reveal_close + 60, Ordering::SeqCst);
            Ok(t)
        })
    }
}

struct Setup {
    f: Fixture,
    pp: Arc<PrepaidState>,
    runner: Arc<Runner>,
    journal: std::path::PathBuf,
    store: std::path::PathBuf,
    domain: [u8; 32],
}

fn account(deposited: u128, exiting: bool) -> Arc<FixedAccount> {
    Arc::new(FixedAccount(Account {
        deposited,
        redeemed: 0,
        exiting,
    }))
}

fn setup(exiting: bool) -> Setup {
    let f = fixture(true, mock());
    f.now.store(today(), Ordering::SeqCst);
    let t = template();
    let dir = crate::journal::test_dir("observe");
    let store = dir.join("daily.json");
    let text = ObservationStore::to_json(&t, &[], None);
    std::fs::write(&store, text.to_string()).expect("store");
    let pins = TemplatePins::from([(METRIC.to_owned(), t.id())]);
    let feed = ObservationStore::from_json(&text.to_string(), today(), &pins).expect("loads");
    f.state
        .catalog
        .write()
        .expect("lock")
        .add_observations(feed)
        .expect("adds");
    let domain = domain_separator(CHAIN, &CONTRACT);
    let journal = dir.join("prepaid.journal");
    let pp = Arc::new(
        PrepaidState::open(&journal, domain, account(10_000, exiting), 60)
            .expect("open")
            .with_hold_limit(DEADLINE),
    );
    let runner = Arc::new(Runner {
        calls: AtomicU64::new(0),
        mode: Mutex::new(Mode::Honest),
        now: f.now.clone(),
    });
    let offers = BTreeMap::from([(
        METRIC.to_owned(),
        ObserveOffer {
            price: PRICE,
            deadline_secs: DEADLINE,
            store: store.clone(),
        },
    )]);
    let obs = Arc::new(ObserveState::new(offers, runner.clone(), &pp).expect("observe"));
    let state = Arc::try_unwrap(f.state)
        .ok()
        .expect("sole owner")
        .with_prepaid(pp.clone())
        .with_observe(obs)
        .expect("observe on");
    Setup {
        f: Fixture {
            state: Arc::new(state),
            ..f
        },
        pp,
        runner,
        journal,
        store,
        domain,
    }
}

impl Setup {
    fn voucher(&self, who: &str, total: u128) -> String {
        voucher_header(&Voucher::sign(&secret(who), &self.domain, total).expect("sign"))
    }
    fn lines(&self) -> usize {
        std::fs::read_to_string(&self.journal)
            .map(|t| t.lines().count())
            .unwrap_or(0)
    }
    fn mode(&self, m: Mode) {
        *self.runner.mode.lock().expect("mode") = m;
    }
    fn calls(&self) -> u64 {
        self.runner.calls.load(Ordering::SeqCst)
    }
    async fn hold(&self, date: &str, who: &str, total: u128) -> String {
        let (s, _, b) = get(
            &self.f,
            &uri(date),
            &[(PREPAID_VOUCHER, self.voucher(who, total))],
        )
        .await;
        assert_eq!(s, StatusCode::ACCEPTED, "{b}");
        b["ticket"].as_str().expect("ticket").to_owned()
    }
    async fn ticket(&self, t: &str) -> Value {
        let (s, h, b) = get(&self.f, &format!("/v1/tickets/{t}"), &[]).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(cc(&h), "no-store");
        b
    }
}

/// R-OBS-1, R-OBS-2, R-OBS-3: an unpaid request starts no round; a paid one holds the
/// voucher (never redeemable while held), runs one round, serves the value through the
/// ticket, and only then charges it; a second buyer of the queued fact pays nothing.
#[tokio::test(start_paused = true)]
async fn an_unpaid_subject_runs_no_round_and_a_paid_one_is_served_then_charged() {
    let s = setup(false);
    let (st, _, b) = get(&s.f, &uri("2026-10-20"), &[]).await;
    assert_eq!(
        (st, code(&b)),
        (
            StatusCode::PAYMENT_REQUIRED,
            Some("observe_payment_required")
        )
    );
    assert_eq!(b["observe"]["price"], "500");
    assert_eq!(b["observe"]["pays"], PREPAID_VOUCHER);
    assert_eq!(step(&s.f.state).await, Default::default());
    assert_eq!((s.calls(), s.lines()), (0, 0), "no money, no round");
    let ticket = s.hold("2026-10-20", "buyer a", 500).await;
    assert_eq!(s.lines(), 1);
    assert!(
        s.pp.to_redeem().is_empty(),
        "a held voucher is never redeemed"
    );
    // Another buyer of the same fact: told it is queued, nothing taken, no ticket.
    let (st, h, b) = get(
        &s.f,
        &uri("2026-10-20"),
        &[(PREPAID_VOUCHER, s.voucher("buyer b", 500))],
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED);
    assert_eq!(b["state"], "queued");
    assert!(b.get("ticket").is_none());
    assert_eq!(h[header::RETRY_AFTER], "30");
    assert_eq!(s.lines(), 1);
    assert_eq!(s.ticket(&ticket).await["state"], "queued");
    let (_, _, health) = get(&s.f, "/v1/health", &[]).await;
    assert_eq!(health["observe"]["queued"], 1);
    // The worker runs the round: served, then charged.
    let done = step(&s.f.state).await;
    assert_eq!(s.calls(), 1);
    assert!(done.observed.is_some() && done.released.is_empty());
    assert_eq!(s.lines(), 2);
    assert_eq!(
        s.pp.to_redeem()
            .iter()
            .map(|v| v.cumulative)
            .collect::<Vec<_>>(),
        vec![500]
    );
    let b = s.ticket(&ticket).await;
    assert_eq!(b["state"], "observed");
    assert_eq!(b["observation"]["value"], "0.07");
    // The round is in the results store and loads with every check.
    let pins = TemplatePins::from([(METRIC.to_owned(), template().id())]);
    let stored = std::fs::read_to_string(&s.store).expect("store");
    let feed =
        ObservationStore::from_json(&stored, s.f.now.load(Ordering::SeqCst), &pins).expect("loads");
    assert!(feed.record(&subj("2026-10-20"), None, None).is_some());
    // Later buyers of the fact pay the ordinary way; no second round.
    let (st, _, b) = get(&s.f, &uri("2026-10-20"), &[]).await;
    assert!(
        st == StatusCode::OK || code(&b) == Some("payment_required"),
        "{st} {b}"
    );
    assert_eq!(step(&s.f.state).await, Default::default());
    assert_eq!(s.calls(), 1);
}

/// R-OBS-2, R-OBS-6: a failed round and a passed deadline each release the hold in full; the
/// buyer's next voucher starts from the earlier total.
#[tokio::test(start_paused = true)]
async fn a_failed_round_or_a_passed_deadline_releases_the_hold_in_full() {
    let s = setup(false);
    s.mode(Mode::Fails);
    let first = s.hold("2026-10-20", "buyer a", 500).await;
    let done = step(&s.f.state).await;
    assert_eq!(done.released, vec![ticket_hash(&hex32(&first))]);
    let b = s.ticket(&first).await;
    assert_eq!(
        (b["state"].as_str(), b["refund"].as_str()),
        (Some("released"), Some(REFUND_RULE))
    );
    assert!(s.pp.to_redeem().is_empty());
    // The same total again: accepted, so nothing was charged.
    let second = s.hold("2026-10-20", "buyer a", 500).await;
    s.mode(Mode::Honest);
    s.f.now.fetch_add(DEADLINE, Ordering::SeqCst);
    let done = step(&s.f.state).await;
    assert_eq!(done.released, vec![ticket_hash(&hex32(&second))]);
    assert_eq!(s.calls(), 1, "a hold past its deadline runs no round");
    assert_eq!(s.ticket(&second).await["state"], "released");
    assert!(s.pp.to_redeem().is_empty());
    let (_, _, health) = get(&s.f, "/v1/health", &[]).await;
    assert_eq!(health["observe"]["released"], 2);
}

/// R-OBS-4: a transcript for another day, one that does not check against the pinned
/// template, or one that arrives after the deadline is never served and never charged.
#[tokio::test(start_paused = true)]
async fn a_round_for_another_day_a_forged_or_a_late_one_is_never_served() {
    for mode in [Mode::OtherDay, Mode::Forged, Mode::Late] {
        let s = setup(false);
        s.mode(mode);
        let t = s.hold("2026-10-20", "buyer a", 500).await;
        let done = step(&s.f.state).await;
        assert_eq!(done.released.len(), 1);
        assert_eq!(s.ticket(&t).await["state"], "released");
        let c = s.f.state.catalog.read().expect("lock");
        let feed = c.observations(METRIC).expect("feed");
        assert!(feed.records.is_empty(), "nothing served");
        drop(c);
        assert!(s.pp.to_redeem().is_empty());
        let stored: Value =
            serde_json::from_str(&std::fs::read_to_string(&s.store).expect("store")).expect("json");
        assert_eq!(stored["rounds"], json!([]));
    }
}

/// NL8-1, NL8-3 (R-OBS-7, R-OBS-9): one hold per buyer, and no other voucher while it is
/// pending; buyers' facts run oldest first.
#[tokio::test(start_paused = true)]
async fn one_hold_per_buyer_and_facts_run_in_turn() {
    let s = setup(false);
    let a = s.hold("2026-10-20", "buyer a", 500).await;
    let (st, _, b) = get(
        &s.f,
        &uri("2026-10-19"),
        &[(PREPAID_VOUCHER, s.voucher("buyer a", 1000))],
    )
    .await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::PAYMENT_REQUIRED, Some("prepaid_refused"))
    );
    assert!(b["error"]["message"].as_str().expect("m").contains("hold"));
    // Its ordinary answers wait too (the voucher API, as the paid routes call it).
    let now = today();
    let paid = |total| Voucher::sign(&secret("buyer a"), &s.domain, total).expect("sign");
    assert_eq!(
        s.pp.charge(paid(1000), 1000, (1, Hash32([1; 32])), Hash32([2; 32]), now),
        Err(ChargeError::Refused(PrepaidError::HoldPending))
    );
    assert_eq!(s.lines(), 1);
    let b2 = s.hold("2026-10-19", "buyer b", 500).await;
    assert_eq!(
        step(&s.f.state).await.observed,
        Some(ticket_hash(&hex32(&a)))
    );
    assert_eq!(
        step(&s.f.state).await.observed,
        Some(ticket_hash(&hex32(&b2)))
    );
    // Charged: buyer a pays on as usual, from the held total.
    assert!(s
        .pp
        .charge(paid(1500), 1000, (1, Hash32([1; 32])), Hash32([3; 32]), now)
        .is_ok());
}

/// NL8-2 (R-OBS-8): no hold during an exit, none longer than the hold limit, and an offer
/// whose deadline outlasts it is refused at start.
#[tokio::test(start_paused = true)]
async fn holds_respect_the_exit_and_the_hold_limit() {
    let s = setup(true);
    let (st, _, b) = get(
        &s.f,
        &uri("2026-10-20"),
        &[(PREPAID_VOUCHER, s.voucher("buyer a", 500))],
    )
    .await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::PAYMENT_REQUIRED, Some("prepaid_refused"))
    );
    assert_eq!(s.lines(), 0);
    let dir = crate::journal::test_dir("observe-limit");
    let pp = PrepaidState::open(dir.join("j"), s.domain, account(10_000, false), 60)
        .expect("open")
        .with_hold_limit(DEADLINE);
    let long = BTreeMap::from([(
        METRIC.to_owned(),
        ObserveOffer {
            price: PRICE,
            deadline_secs: DEADLINE + 1,
            store: s.store.clone(),
        },
    )]);
    assert!(ObserveState::new(long, s.runner.clone(), &pp).is_err());
    let v = Voucher::sign(&secret("buyer c"), &s.domain, 500).expect("sign");
    pp.refresh(v.buyer, today()).await.expect("read");
    let held = |deadline| Held {
        ticket: Hash32([9; 32]),
        metric: METRIC.into(),
        subject: subj("2026-10-20"),
        deadline,
        created: today(),
    };
    assert_eq!(
        pp.hold(v.clone(), 500, held(today() + DEADLINE + 1), today()),
        Err(ChargeError::Refused(PrepaidError::HoldTooLong {
            limit: DEADLINE
        }))
    );
    assert_eq!(pp.hold(v, 500, held(today() + DEADLINE), today()), Ok(()));
}

/// NL8-4, NL8-5 (R-OBS-10, R-OBS-11): the offer says the value becomes public and is the same
/// for every subject the template allows now; a day it does not allow, or a metric with no
/// offer, is the usual 404 and nothing is held.
#[tokio::test(start_paused = true)]
async fn the_offer_is_the_same_for_every_allowed_day_and_says_the_value_becomes_public() {
    let s = setup(false);
    let (_, _, one) = get(&s.f, &uri("2026-10-20"), &[]).await;
    let (_, _, two) = get(&s.f, &uri("2026-09-25"), &[]).await;
    assert_eq!(one["observe"], two["observe"]);
    assert_eq!(one["observe"]["becomesPublic"], BECOMES_PUBLIC);
    assert_eq!(one["observe"]["refund"], REFUND_RULE);
    for date in ["2026-10-21", "2026-08-01", "2026-10-99"] {
        let (st, _, b) = get(
            &s.f,
            &uri(date),
            &[(PREPAID_VOUCHER, s.voucher("buyer a", 500))],
        )
        .await;
        assert_eq!(
            (st, code(&b)),
            (StatusCode::NOT_FOUND, Some("unknown_subject")),
            "{date}"
        );
    }
    // A query naming a window is not an observe request either.
    let (st, _, _) = get(&s.f, &format!("{}&window_end=1", uri("2026-10-20")), &[]).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert_eq!(s.lines(), 0);
    let (st, _, b) = get(&s.f, "/v1/tickets/00", &[]).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::NOT_FOUND, Some("unknown_ticket"))
    );
}

/// A restart: a pending hold comes back queued and runs; an ended one still answers its
/// ticket.
#[tokio::test(start_paused = true)]
async fn a_restart_resumes_pending_holds_and_keeps_ended_tickets() {
    let s = setup(false);
    let t = s.hold("2026-10-20", "buyer a", 500).await;
    let h = ticket_hash(&hex32(&t));
    let reopen = || {
        let pp = PrepaidState::open(&s.journal, s.domain, account(10_000, false), 60)
            .expect("reopen")
            .with_hold_limit(DEADLINE);
        let offers = BTreeMap::from([(
            METRIC.to_owned(),
            ObserveOffer {
                price: PRICE,
                deadline_secs: DEADLINE,
                store: s.store.clone(),
            },
        )]);
        let o = ObserveState::new(offers, s.runner.clone(), &pp).expect("observe");
        (pp, o)
    };
    let (pp, o) = reopen();
    assert_eq!(pp.holds().len(), 1);
    assert_eq!(o.ticket(&h).map(|t| t.state), Some(TicketState::Queued));
    assert!(pp.to_redeem().is_empty());
    step(&s.f.state).await;
    let (pp, o) = reopen();
    assert!(pp.holds().is_empty());
    assert_eq!(o.ticket(&h).map(|t| t.state), Some(TicketState::Observed));
    assert_eq!(pp.to_redeem().len(), 1);
}

fn hex32(s: &str) -> [u8; 32] {
    hex::decode(s).expect("hex").try_into().expect("32 bytes")
}
