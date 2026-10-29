//! L2RND-R1: the draw's beacon is the chain's, its attempt is counted when
//! announced, and an exposed draw does not close a fixed fact. Each test is an
//! attack that worked before the fix and must now fail.

use onyx_coordinator::observe::{
    check_beacon, check_draw, draw_and_open, draw_observers, ChainView, ChainWait, DrawChain,
    DrawRecord, DrawSeed, ObservationResults, ObserveConfig, ObserveError, Observer, OnchainDraw,
    OnchainFact, SimulatedChain, SimulatedRegistry, BLOCKHASH_WINDOW, MAX_DRAW_ATTEMPTS,
    MAX_SUBJECT_DRAWS,
};
use onyx_core::claim::{MetricId, Subject};
use onyx_core::{
    keccak256, DataRights, FeedTemplate, Hash32, ObservationKind, ObservationSpec, Redistribution,
    SourceId, SubjectRule,
};

const USD: &str = "example.page.usd_per_unit";

/// A chain whose final block shows the book's draw `.0` with its beacon
/// zero, past its `blockhash` window: the proof `abandon_draw` needs to give
/// a draw up (the draw invariant, review R8). Nothing else is read.
struct Unfixable(OnchainDraw);

impl Unfixable {
    fn at(&self) -> u64 {
        self.0.beacon_height + BLOCKHASH_WINDOW + 1
    }
}

impl ChainView for Unfixable {
    fn height(&self) -> Result<u64, String> {
        Ok(self.at())
    }
    fn block_hash(&self, _: u64) -> Result<Option<Hash32>, String> {
        Err("unused".into())
    }
}

impl DrawChain for Unfixable {
    fn draw_count(&self, _: &OnchainFact) -> Result<u32, String> {
        Err("unused".into())
    }
    fn draw_budget(&self, _: &OnchainFact) -> Result<(u32, u32), String> {
        Err("unused".into())
    }
    fn announce(&mut self, _: &OnchainDraw) -> Result<(), String> {
        Err("unused".into())
    }
    fn fix_beacon(&mut self, _: &OnchainDraw) -> Result<DrawRecord, String> {
        Err("unused".into())
    }
    fn draw_of(&self, _: &OnchainFact, _: u32) -> Result<Option<DrawRecord>, String> {
        Err("unused".into())
    }
    fn draw_of_final(
        &self,
        _: &OnchainFact,
        attempt: u32,
    ) -> Result<(u64, Option<DrawRecord>), String> {
        let d = &self.0;
        Ok((
            self.at(),
            (attempt == d.attempt).then_some(DrawRecord {
                chain_id: 31_337,
                registry: [0; 20],
                attempt,
                draw_count: attempt + 1,
                announced_at: d.beacon_height - 1,
                commit: d.commit,
                beacon_height: d.beacon_height,
                beacon: Hash32::ZERO,
            }),
        ))
    }
    fn post_commit(
        &mut self,
        _: &OnchainFact,
        _: u32,
        _: &onyx_core::MinerAddress,
        _: &Hash32,
        _: &Hash32,
        _: &onyx_coordinator::observe::ObserverSig,
    ) -> Result<onyx_coordinator::observe::Posted, String> {
        Err("unused".into())
    }
    fn post_receipt(
        &mut self,
        _: &OnchainFact,
        _: u32,
        _: &onyx_core::MinerAddress,
        _: &Hash32,
        _: &Hash32,
        _: &onyx_coordinator::observe::ObserverSig,
    ) -> Result<onyx_coordinator::observe::Posted, String> {
        Err("unused".into())
    }
    fn log_block(&self) -> Result<Option<onyx_coordinator::observe::LogBlock>, String> {
        Err("unused".into())
    }
    fn posted_commit(
        &self,
        _: u64,
        _: &OnchainFact,
        _: u32,
        _: &onyx_core::MinerAddress,
        _: &Hash32,
    ) -> Result<Option<onyx_coordinator::observe::PostedCommit>, String> {
        Err("unused".into())
    }
    fn transcript_complete(
        &self,
        _: &onyx_coordinator::observe::TranscriptLogQuery,
    ) -> Result<bool, String> {
        Err("unused".into())
    }
}

/// Give up the book's draw of `sp` with the chain's proof that its beacon
/// can never be fixed.
fn give_up(st: &mut ObservationResults, sp: &ObservationSpec) {
    let d = st.held_draw(sp).expect("the book holds a draw");
    st.abandon_draw(sp, &Unfixable(d), &ChainWait::NOW)
        .expect("abandon");
}

fn rights() -> DataRights {
    DataRights {
        license: "CC0-1.0".into(),
        redistribution: Redistribution::Allowed,
        source: SourceId("example.page".into()),
        attribution: None,
        terms: String::new(),
    }
}

fn spec_at(shift: u64) -> ObservationSpec {
    ObservationSpec {
        feed: USD.into(),
        metric: MetricId(USD.into()),
        subject: Subject::Entity("entity:alpha".into()),
        fetcher: keccak256(b"fetcher"),
        url: "https://example.org/page.json".into(),
        selector: "/data/usd_per_unit".into(),
        kind: ObservationKind::Numeric {
            scale: 2,
            tolerance: 50,
        },
        window_start: 1_040 + shift,
        window_end: 1_060 + shift,
        min_observers: 3,
        rights: rights(),
    }
}

fn cfg_at(shift: u64) -> ObserveConfig {
    ObserveConfig {
        commit_close: 1_100 + shift,
        reveal_close: 1_200 + shift,
        reward: 10,
        outlier_slash: 100,
        miss_slash: 20,
    }
}

fn store() -> ObservationResults {
    let s = spec_at(0);
    let (mut st, admin) = ObservationResults::new();
    st.register_feed(
        &admin,
        FeedTemplate {
            feed: s.feed.clone(),
            metric: s.metric.clone(),
            fetcher: s.fetcher,
            url_template: s.url.clone(),
            selector: s.selector.clone(),
            kind: s.kind,
            window_secs: s.window_end - s.window_start,
            min_observers: s.min_observers,
            rights: s.rights.clone(),
            subject: SubjectRule::Fixed(s.subject.clone()),
        },
        "t",
        0,
    )
    .expect("register");
    st
}

fn pool(n: u8, bond: u128) -> Vec<Observer> {
    (1..=n)
        .map(|i| Observer {
            address: [i; 20],
            bond,
        })
        .collect()
}

fn colluders_only(o: &[Observer]) -> bool {
    o.len() == 3 && o.iter().all(|m| m.address[0] <= 3)
}

/// A chain at `height` whose blocks up to `height` hash to `hash`.
fn chain(height: u64, hash: Hash32) -> SimulatedChain {
    SimulatedChain {
        height,
        beacon: hash,
        beacon_upto: height,
    }
}

/// F1: a coordinator grinds beacons offline until one draws only its
/// colluders. The store reads the beacon from the chain instead, and a
/// consumer comparing the beacon with the chain's block hash refuses the
/// ground one.
#[test]
fn a_ground_beacon_is_neither_opened_nor_accepted() {
    let mut st = store();
    let sp = spec_at(0);
    let snap = st
        .announce_draw(
            &sp,
            &cfg_at(0),
            &pool(9, 1_000),
            50,
            &SimulatedChain::at(40),
        )
        .expect("announce");
    let ground = (0u64..)
        .map(|i| keccak256(&i.to_le_bytes()))
        .find(|b| {
            let seed = DrawSeed::for_pool(&snap, 50, *b, 0);
            colluders_only(&draw_observers(&sp.id(), &snap, &seed, 3, 100))
        })
        .expect("found");
    let real = keccak256(b"block 50");
    assert_eq!(
        st.open_round(sp.clone(), cfg_at(0), snap.clone(), &chain(49, real))
            .err(),
        Some(ObserveError::BeaconNotYet),
        "no beacon before its block"
    );
    let r = st
        .open_round(sp.clone(), cfg_at(0), snap.clone(), &chain(60, real))
        .expect("open");
    assert_eq!(
        r.draw().beacon,
        real,
        "the beacon is the chain's block hash"
    );
    assert_eq!(check_beacon(&r.draw(), &chain(60, real)), Ok(()));
    // A transcript drawn with the ground beacon passes the pure draw check,
    // but not the chain check.
    let forged = DrawSeed::for_pool(&snap, 50, ground, 0);
    let picked = draw_observers(&sp.id(), &snap, &forged, 3, 100);
    assert_eq!(check_draw(&sp, &cfg_at(0), &snap, &forged, &picked), Ok(()));
    assert_eq!(
        check_beacon(&forged, &chain(60, real)),
        Err(ObserveError::BeaconMismatch)
    );
}

/// F2: with the beacon fixed, a coordinator tweaks the pool or names a later
/// attempt until the draw suits it. The open must use the announced pool and
/// the latest announced attempt, and attempts stop at 64.
#[test]
fn a_tweaked_pool_or_attempt_is_refused() {
    let mut st = store();
    let sp = spec_at(0);
    let real = Hash32([7; 32]);
    let snap = st
        .announce_draw(
            &sp,
            &cfg_at(0),
            &pool(9, 1_000),
            50,
            &SimulatedChain::at(40),
        )
        .expect("announce");
    let mut tweaked = snap.clone();
    tweaked[8].bond += 1;
    assert_eq!(
        st.open_round(sp.clone(), cfg_at(0), tweaked, &chain(60, real))
            .err(),
        Some(ObserveError::DrawMismatch)
    );
    // Give attempt 0 up and announce attempt 1: only attempt 1 opens.
    give_up(&mut st, &sp);
    let snap = st
        .announce_draw(
            &sp,
            &cfg_at(0),
            &pool(9, 1_000),
            70,
            &SimulatedChain::at(60),
        )
        .expect("announce 1");
    let r = st
        .open_round(sp.clone(), cfg_at(0), snap.clone(), &chain(80, real))
        .expect("open");
    assert_eq!(r.draw().attempt, 1);
    // A consumer refuses any attempt past the cap.
    let late = DrawSeed::for_pool(&snap, 70, real, MAX_DRAW_ATTEMPTS);
    let drawn = draw_observers(&sp.id(), &snap, &late, 3, 100);
    assert_eq!(
        check_draw(&sp, &cfg_at(0), &snap, &late, &drawn),
        Err(ObserveError::BadPool)
    );
}

/// F3: a coordinator that saw the beacon restarts from its book and
/// announces the same attempt again with a new height. The attempt is in the
/// book from its announcement, WITH its inputs (review REVLOG-R5): the
/// restarted store refuses a second announcement, opens attempt 0 only with
/// the announced pool and height (so with the same beacon and committee),
/// and never draws attempt 0 with other inputs.
#[test]
fn a_restart_after_announce_cannot_redraw_the_attempt() {
    let mut a = store();
    let sp = spec_at(0);
    let snap = a
        .announce_draw(
            &sp,
            &cfg_at(0),
            &pool(9, 1_000),
            50,
            &SimulatedChain::at(40),
        )
        .expect("announce");
    let book = a.non_value_book().expect("writable");
    let mut b = store();
    b.absorb_non_value_book(&book).expect("absorb");
    assert_eq!(
        b.announce_draw(
            &sp,
            &cfg_at(0),
            &pool(9, 1_000),
            90,
            &SimulatedChain::at(80),
        )
        .err(),
        Some(ObserveError::DrawAlreadyAnnounced),
        "attempt 0 is not announced again with a new height"
    );
    assert_eq!(
        b.open_round(
            sp.clone(),
            cfg_at(0),
            pool(8, 1_000),
            &chain(60, Hash32([2; 32]))
        )
        .err(),
        Some(ObserveError::DrawMismatch),
        "only the announced pool opens"
    );
    assert_eq!(b.announced_pool(&sp), Ok(snap.clone()));
    let r = b
        .open_round(sp, cfg_at(0), snap, &chain(100, Hash32([2; 32])))
        .expect("open");
    assert_eq!(
        (r.draw().attempt, r.draw().beacon_height),
        (0, 50),
        "attempt 0 at its announced height"
    );
}

/// F4: two fixed-window facts share bonds; the second cannot open while the
/// first holds them. Giving its draw up must not close the fixed fact: it is
/// drawn again later, until the 64-attempt cap.
#[test]
fn an_exposed_draw_does_not_close_a_fixed_fact() {
    let mut st = store();
    let (a, b) = (spec_at(0), spec_at(10_000));
    let p = pool(3, 100);
    let sa = st
        .announce_draw(&a, &cfg_at(0), &p, 50, &SimulatedChain::at(40))
        .expect("a");
    let sb = st
        .announce_draw(&b, &cfg_at(10_000), &p, 50, &SimulatedChain::at(40))
        .expect("b");
    st.open_round(a, cfg_at(0), sa, &chain(60, Hash32([1; 32])))
        .expect("open a");
    assert_eq!(
        st.open_round(b.clone(), cfg_at(10_000), sb, &chain(60, Hash32([1; 32])))
            .err(),
        Some(ObserveError::ObserverExposed)
    );
    give_up(&mut st, &b);
    st.announce_draw(&b, &cfg_at(10_000), &p, 70, &SimulatedChain::at(65))
        .expect("the fixed fact is drawn again");
    for n in 2..MAX_DRAW_ATTEMPTS {
        give_up(&mut st, &b);
        let h = 100 + u64::from(n) * 10;
        st.announce_draw(&b, &cfg_at(10_000), &p, h, &SimulatedChain::at(h - 1))
            .expect("announce");
    }
    give_up(&mut st, &b);
    assert_eq!(
        st.announce_draw(&b, &cfg_at(10_000), &p, 9_999, &SimulatedChain::at(9_000))
            .err(),
        Some(ObserveError::FactClosed),
        "64 attempts at most"
    );
}

/// The registry, with the process "crashing" right after the on-chain
/// announcement (the beacon block then exists and the coordinator saw it),
/// and `.1` more blocks mined before anyone looks again.
struct CrashAfterAnnounce(SimulatedRegistry, u64);

impl ChainView for CrashAfterAnnounce {
    fn height(&self) -> Result<u64, String> {
        self.0.height()
    }
    fn block_hash(&self, h: u64) -> Result<Option<Hash32>, String> {
        self.0.block_hash(h)
    }
}

impl DrawChain for CrashAfterAnnounce {
    fn draw_count(&self, f: &OnchainFact) -> Result<u32, String> {
        self.0.draw_count(f)
    }
    fn draw_budget(&self, f: &OnchainFact) -> Result<(u32, u32), String> {
        self.0.draw_budget(f)
    }
    fn announce(&mut self, d: &OnchainDraw) -> Result<(), String> {
        self.0.announce(d)
    }
    fn fix_beacon(&mut self, d: &OnchainDraw) -> Result<DrawRecord, String> {
        self.0.height = d.beacon_height + 1 + self.1;
        Err("crash".into())
    }
    fn draw_of(&self, f: &OnchainFact, attempt: u32) -> Result<Option<DrawRecord>, String> {
        self.0.draw_of(f, attempt)
    }
    fn draw_of_final(
        &self,
        f: &OnchainFact,
        attempt: u32,
    ) -> Result<(u64, Option<DrawRecord>), String> {
        self.0.draw_of_final(f, attempt)
    }
    fn post_commit(
        &mut self,
        f: &OnchainFact,
        attempt: u32,
        o: &[u8; 20],
        round: &Hash32,
        commit: &Hash32,
        sig: &[u8; 65],
    ) -> Result<onyx_coordinator::observe::Posted, String> {
        self.0.post_commit(f, attempt, o, round, commit, sig)
    }
    fn post_receipt(
        &mut self,
        f: &OnchainFact,
        attempt: u32,
        o: &[u8; 20],
        round: &Hash32,
        commit: &Hash32,
        sig: &[u8; 65],
    ) -> Result<onyx_coordinator::observe::Posted, String> {
        self.0.post_receipt(f, attempt, o, round, commit, sig)
    }
    fn log_block(&self) -> Result<Option<onyx_coordinator::observe::LogBlock>, String> {
        self.0.log_block()
    }
    fn posted_commit(
        &self,
        block: u64,
        f: &OnchainFact,
        attempt: u32,
        o: &[u8; 20],
        round: &Hash32,
    ) -> Result<Option<onyx_coordinator::observe::PostedCommit>, String> {
        self.0.posted_commit(block, f, attempt, o, round)
    }
    fn transcript_complete(
        &self,
        q: &onyx_coordinator::observe::TranscriptLogQuery,
    ) -> Result<bool, String> {
        self.0.transcript_complete(q)
    }
}

fn book_path(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("onyx-l2rnd-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let p = dir.join("book.json");
    let _ = std::fs::remove_file(&p);
    p
}

/// F3: the runner path. The book is saved once the draw is announced, before
/// its beacon, WITH the announcement (review REVLOG-R5): a coordinator that
/// crashes after the announcement (and after the beacon block exists) and
/// restarts from its book resumes attempt 0 with the inputs it announced (the
/// registry's commitment checks them), never announces it again with other
/// inputs and never leaves it for a new draw (a fixed beacon without a
/// published round voids the fact for clients).
#[test]
fn a_crash_after_announce_cannot_redraw() {
    let path = book_path("after");
    let mut chain = CrashAfterAnnounce(SimulatedRegistry::new(), 0);
    let mut a = store();
    let err = draw_and_open(
        &mut a,
        spec_at(0),
        cfg_at(0),
        &pool(9, 1_000),
        &mut chain,
        &ChainWait::NOW,
        &mut |s| s.save_book(&path),
    )
    .expect_err("crashed");
    assert!(
        err.contains("crash") && err.contains("stays announced"),
        "{err}"
    );
    let mut b = store();
    b.load_book(&path).expect("book");
    let (r, rec) = draw_and_open(
        &mut b,
        spec_at(0),
        cfg_at(0),
        &pool(9, 1_000),
        &mut chain.0,
        &ChainWait::NOW,
        &mut |s| s.save_book(&path),
    )
    .expect("restart");
    assert_eq!(
        (r.draw().attempt, rec.draw_count),
        (0, 1),
        "the same draw, resumed"
    );
    assert!(rec.announced_at < rec.beacon_height);
    assert_eq!(check_beacon(&r.draw(), &chain.0), Ok(()));
}

/// Review REVLOG-R5: a draw whose beacon was never fixed and whose beacon
/// block left the `blockhash` window can no longer be fixed: it is given up
/// (counted; no beacon on chain means no committee, which clients accept)
/// rather than kept announced for ever, and the next run draws again.
#[test]
fn an_unfixable_announced_draw_is_given_up() {
    let path = book_path("unfixable");
    let mut chain = CrashAfterAnnounce(SimulatedRegistry::new(), BLOCKHASH_WINDOW);
    let mut a = store();
    let err = draw_and_open(
        &mut a,
        spec_at(0),
        cfg_at(0),
        &pool(9, 1_000),
        &mut chain,
        &ChainWait::NOW,
        &mut |s| s.save_book(&path),
    )
    .expect_err("unfixable");
    assert!(err.contains("given up"), "{err}");
    let mut b = store();
    b.load_book(&path).expect("book");
    let (r, rec) = draw_and_open(
        &mut b,
        spec_at(0),
        cfg_at(0),
        &pool(9, 1_000),
        &mut chain.0,
        &ChainWait::NOW,
        &mut |s| s.save_book(&path),
    )
    .expect("the next draw");
    assert_eq!((r.draw().attempt, rec.draw_count), (1, 2));
}

/// F3, THREATS REVLOG-R5-2: the book is saved with the draw before
/// `announceDraw` is sent (review R7-2), so only a LOST book (never written
/// here) leaves a chain draw the book lacks. The registry is the authority:
/// the restarted store adopts the chain's draw 0, whose commitment is the one
/// it announces (same pool, spec and config), and resumes it rather than
/// leave it for anyone to fix (a fixed beacon without a published round
/// voids the fact).
#[test]
fn a_crash_before_the_book_is_saved_follows_the_chain() {
    let path = book_path("before");
    let mut chain = SimulatedRegistry::new();
    let mut a = store();
    draw_and_open(
        &mut a,
        spec_at(0),
        cfg_at(0),
        &pool(9, 1_000),
        &mut chain,
        &ChainWait::NOW,
        &mut |_| Ok(()),
    )
    .expect("draw 0, its book lost");
    assert!(!path.exists(), "the book was never written");
    let mut b = store();
    b.load_book(&path).expect("first run");
    // Without the results store read, nothing is adopted (review R9-F1).
    let e = draw_and_open(
        &mut b,
        spec_at(0),
        cfg_at(0),
        &pool(9, 1_000),
        &mut chain,
        &ChainWait::NOW,
        &mut |s| s.save_book(&path),
    )
    .expect_err("the results store was not read");
    assert!(e.contains("cannot resume or adopt"), "{e}");
    // The runner read it: it publishes no draw of the fact.
    b.note_published_draws(&spec_at(0), &[]).expect("noted");
    let (r, rec) = draw_and_open(
        &mut b,
        spec_at(0),
        cfg_at(0),
        &pool(9, 1_000),
        &mut chain,
        &ChainWait::NOW,
        &mut |s| s.save_book(&path),
    )
    .expect("restart");
    assert_eq!((r.draw().attempt, rec.draw_count), (0, 1), "draw 0 adopted");
    assert_eq!(check_beacon(&r.draw(), &chain), Ok(()));
}

/// REVLOG-R5-2, review R7-2: a chain draw whose commitment is NOT the one
/// this runner announces (another pool) is never adopted, and never drawn
/// past while its beacon is fixed (here) or still fixable: the run stops
/// and announces nothing.
#[test]
fn a_chain_draw_with_other_inputs_is_not_adopted() {
    let path = book_path("other");
    let mut chain = SimulatedRegistry::new();
    let mut a = store();
    draw_and_open(
        &mut a,
        spec_at(0),
        cfg_at(0),
        &pool(9, 1_000),
        &mut chain,
        &ChainWait::NOW,
        &mut |_| Ok(()),
    )
    .expect("draw 0, its book lost");
    let mut b = store();
    b.load_book(&path).expect("first run");
    let e = draw_and_open(
        &mut b,
        spec_at(0),
        cfg_at(0),
        &pool(9, 2_000),
        &mut chain,
        &ChainWait::NOW,
        &mut |s| s.save_book(&path),
    )
    .expect_err("restart stops");
    assert!(
        e.contains("cannot resume or adopt") && e.contains("its beacon is fixed"),
        "{e}"
    );
    assert_eq!(
        chain.draw_count(&b.onchain_fact(&spec_at(0)).expect("fact")),
        Ok(1)
    );
}

/// L2RND-R2-F1/F2: a fixed feed's coordinator shifts the window to get a
/// fresh fact (and a fresh draw) each time. Every window of the subject
/// shares one on-chain bound: the fourth unrecorded draw is refused, a window
/// off the feed's grid is refused outright, and only an owner reopen (or a
/// recorded value) frees the subject.
#[test]
fn shifted_windows_share_the_subject_bound() {
    let len = spec_at(0).window_end - spec_at(0).window_start;
    let mut chain = SimulatedRegistry::new();
    let mut st = store();
    let draw = |st: &mut ObservationResults, chain: &mut SimulatedRegistry, k: u64| {
        draw_and_open(
            st,
            spec_at(k * len),
            cfg_at(k * len),
            &pool(9, 1_000),
            chain,
            &ChainWait::NOW,
            &mut |_| Ok(()),
        )
        .map(|_| ())
    };
    for k in 0..u64::from(MAX_SUBJECT_DRAWS) {
        draw(&mut st, &mut chain, k).expect("within the bound");
    }
    let e = draw(&mut st, &mut chain, 3).expect_err("fourth window");
    assert!(e.contains("DrawsExhausted"), "{e}");
    let off_grid = draw_and_open(
        &mut st,
        spec_at(1),
        cfg_at(1),
        &pool(9, 1_000),
        &mut chain,
        &ChainWait::NOW,
        &mut |_| Ok(()),
    )
    .expect_err("off the grid");
    assert!(off_grid.contains("grid"), "{off_grid}");
    // A recorded value frees one draw; an owner reopen grants three more.
    let fact = st.onchain_fact(&spec_at(0)).expect("fact");
    chain.record_value(&fact);
    draw(&mut st, &mut chain, 4).expect("a recorded value frees one");
    assert!(draw(&mut st, &mut chain, 5).is_err());
    chain.reopen_draws(&fact);
    for k in 5..8 {
        draw(&mut st, &mut chain, k).expect("reopened");
    }
    assert!(draw(&mut st, &mut chain, 8).is_err());
}

/// ATT-R1-F4: an observer commits only when the registry's `drawOf` shows the
/// round's draw with its beacon fixed. A draw the coordinator announced but
/// never fixed (so a client reads it as a draw without a committee) gets no
/// commit, and neither does a record for another draw.
#[test]
fn observers_commit_only_to_a_draw_whose_beacon_is_fixed_on_chain() {
    use onyx_coordinator::observe::check_commit_allowed;
    let seed = DrawSeed {
        pool: Hash32([1; 32]),
        beacon_height: 20,
        beacon: Hash32([2; 32]),
        attempt: 1,
    };
    let commit = Hash32([3; 32]);
    let fixed = DrawRecord {
        chain_id: 31_337,
        registry: [0; 20],
        attempt: 1,
        draw_count: 2,
        announced_at: 15,
        commit,
        beacon_height: 20,
        beacon: Hash32([2; 32]),
    };
    check_commit_allowed(&seed, &commit, &fixed).expect("fixed: commit");
    let unfixed = DrawRecord {
        beacon: Hash32::ZERO,
        ..fixed
    };
    assert_eq!(
        check_commit_allowed(&seed, &commit, &unfixed),
        Err(ObserveError::BeaconNotFixed)
    );
    let others = [
        DrawRecord {
            attempt: 0,
            ..fixed
        },
        DrawRecord {
            commit: Hash32([4; 32]),
            ..fixed
        },
        DrawRecord {
            beacon_height: 21,
            ..fixed
        },
        DrawRecord {
            beacon: Hash32([5; 32]),
            ..fixed
        },
        DrawRecord {
            announced_at: 20,
            ..fixed
        },
    ];
    for r in others {
        assert_eq!(
            check_commit_allowed(&seed, &commit, &r),
            Err(ObserveError::BeaconMismatch),
            "{r:?}"
        );
    }
}
