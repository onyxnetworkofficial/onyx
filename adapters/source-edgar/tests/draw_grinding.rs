//! L2RND-R2: the coordinator cannot grind a dated fact's draw. Each test is
//! an attack that worked at review 2 and must now fail: the registry bounds
//! the draws a (metric, subject) holds without a recorded value, and the
//! runner counts them from the chain, never from its own memory.

use onyx_coordinator::observe::NoInbox;
use onyx_coordinator::observe::{
    draw_and_open, fact_key_dated, ChainView, ChainWait, DrawChain, DrawRecord, OnchainDraw,
    OnchainFact, ReplayClock, SimulatedRegistry, MAX_NON_VALUE_ROUNDS, MAX_SUBJECT_DRAWS,
};
use onyx_core::Hash32;
use onyx_source_edgar::round::{config, load_book, rehearsal_pool, run_and_save, save_book, store};
use onyx_source_edgar::{recorded, spec};

fn book(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("onyx-l2rnd-r2-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let p = dir.join("book.json");
    let _ = std::fs::remove_file(&p);
    p.to_string_lossy().into_owned()
}

/// F1/F3: a coordinator opens a draw (it has now seen the observers),
/// dislikes it, and restarts. Every seen-and-dropped draw counts toward the
/// cap, read from the chain: the fourth try is refused on chain and the fact
/// is closed locally at the cap.
#[test]
fn dropping_a_seen_draw_and_restarting_hits_the_bound_at_the_fourth_try() {
    let path = book("crash");
    let mut chain = SimulatedRegistry::new();
    let rec = &recorded()[0];
    let sp = spec(&rec.company, &rec.period_end, rec.window_start).expect("spec");
    let fact = fact_key_dated(&sp);
    let mut tries = 0;
    let err = loop {
        let mut s = store();
        load_book(&mut s, &path).expect("load");
        let r = draw_and_open(
            &mut s,
            sp.clone(),
            config(&sp),
            &rehearsal_pool(),
            &mut chain,
            &ChainWait::NOW,
            &mut |s| save_book(s, &path),
        );
        tries += 1;
        match r {
            // "crash": drop the store with the round open. The book keeps
            // an opened round (THREATS REVLOG-R4-5), so a grinder must drop
            // it from its book itself; the chain still counts the draw.
            Ok(_) => {
                // The grinder edits its book file: the kept round is cut
                // out (no runner API drops it: review R8).
                save_book(&s, &path).expect("save");
                let mut b: serde_json::Value =
                    serde_json::from_str(&std::fs::read_to_string(&path).expect("read"))
                        .expect("json");
                b["unfinished"] = serde_json::json!([]);
                std::fs::write(&path, b.to_string()).expect("write");
            }
            Err(e) => {
                assert_eq!(s.fact_state(&fact), (true, MAX_NON_VALUE_ROUNDS));
                break e;
            }
        }
        assert!(tries < 20, "grinding was not bounded");
    };
    assert_eq!(tries, MAX_SUBJECT_DRAWS + 1);
    assert!(err.contains("DrawsExhausted"), "{err}");
    let f = store().onchain_fact(&sp).expect("fact");
    assert_eq!(
        chain.draw_budget(&f),
        Ok((MAX_SUBJECT_DRAWS, MAX_SUBJECT_DRAWS))
    );
}

/// The registry where a third party calls the public `fixBeacon` first.
struct FrontRun(SimulatedRegistry);
impl ChainView for FrontRun {
    fn height(&self) -> Result<u64, String> {
        self.0.height()
    }
    fn block_hash(&self, h: u64) -> Result<Option<Hash32>, String> {
        self.0.block_hash(h)
    }
}
impl DrawChain for FrontRun {
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
        self.0.fix_beacon(d)?; // anyone may call it: a griefer goes first
        self.0.fix_beacon(d) // the coordinator's own call
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

/// F4: a stranger fixing the beacon first changes nothing: the beacon is the
/// same block hash, the runner opens the draw, and no attempt is burnt.
#[test]
fn a_front_run_fix_beacon_is_success() {
    let mut chain = FrontRun(SimulatedRegistry::new());
    let rec = &recorded()[0];
    let sp = spec(&rec.company, &rec.period_end, rec.window_start).expect("spec");
    let mut s = store();
    let (round, record) = draw_and_open(
        &mut s,
        sp.clone(),
        config(&sp),
        &rehearsal_pool(),
        &mut chain,
        &ChainWait::NOW,
        &mut |_| Ok(()),
    )
    .expect("a fixed beacon is success");
    assert_eq!(round.draw().attempt, 0);
    assert_eq!(
        record.beacon,
        SimulatedRegistry::hash_of(record.beacon_height)
    );
    assert_eq!(s.fact_state(&fact_key_dated(&sp)), (false, 0));
}

/// The registry whose first `announceDraw` does not land (a revert such as
/// BeaconNotFuture, or an RPC error).
struct FailFirstAnnounce(SimulatedRegistry, bool);
impl ChainView for FailFirstAnnounce {
    fn height(&self) -> Result<u64, String> {
        self.0.height()
    }
    fn block_hash(&self, h: u64) -> Result<Option<Hash32>, String> {
        self.0.block_hash(h)
    }
}
impl DrawChain for FailFirstAnnounce {
    fn draw_count(&self, f: &OnchainFact) -> Result<u32, String> {
        self.0.draw_count(f)
    }
    fn draw_budget(&self, f: &OnchainFact) -> Result<(u32, u32), String> {
        self.0.draw_budget(f)
    }
    fn announce(&mut self, d: &OnchainDraw) -> Result<(), String> {
        if !self.1 {
            self.1 = true;
            return Err("announceDraw reverted".into());
        }
        self.0.announce(d)
    }
    fn fix_beacon(&mut self, d: &OnchainDraw) -> Result<DrawRecord, String> {
        self.0.fix_beacon(d)
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

/// F5: a failed announcement does not wedge the fact. Since review R6-A the
/// draw stays in the saved book until the chain settles it (an error can
/// hide a landed send): a restart while the chain is still below its beacon
/// height stops ("behind the book") and announces nothing; once a final
/// block at or past the beacon height shows no draw (`announceDraw` reverts
/// `BeaconNotFuture` from there on, so it can never land), the restart
/// forgets it and announces attempt 0.
#[test]
fn a_failed_announce_does_not_wedge_the_fact() {
    let path = book("wedge");
    let mut chain = FailFirstAnnounce(SimulatedRegistry::new(), false);
    let rec = &recorded()[0];
    let mut s = store();
    let e = run_and_save(
        &rec.company,
        &rec.period_end,
        rec.window_start,
        &[],
        &mut s,
        &mut chain,
        &path,
        None,
        &ChainWait::NOW,
        &ReplayClock::default(),
        &mut NoInbox,
        &mut |_| Ok(()),
        &mut |_| Ok(()),
    )
    .expect_err("reverted");
    assert!(
        e.error.contains("reverted") && e.error.contains("stays in the book"),
        "{e}"
    );
    assert!(
        e.attempt.is_none(),
        "no beacon was fixed: nothing to publish"
    );
    let sp = spec(&rec.company, &rec.period_end, rec.window_start).expect("spec");
    let mut s = store();
    load_book(&mut s, &path).expect("load");
    let e = draw_and_open(
        &mut s,
        sp.clone(),
        config(&sp),
        &rehearsal_pool(),
        &mut chain,
        &ChainWait::NOW,
        &mut |s| save_book(s, &path),
    )
    .expect_err("not settled yet");
    assert!(e.contains("behind the book"), "{e}");
    assert_eq!(
        chain.0.draw_count(&s.onchain_fact(&sp).expect("fact")),
        Ok(0)
    );
    chain.0.height += 10; // past the forgotten draw's beacon height
    let mut s = store();
    load_book(&mut s, &path).expect("load");
    let (round, _) = draw_and_open(
        &mut s,
        sp.clone(),
        config(&sp),
        &rehearsal_pool(),
        &mut chain,
        &ChainWait::NOW,
        &mut |s| save_book(s, &path),
    )
    .expect("not wedged");
    assert_eq!(round.draw().attempt, 0);
}
