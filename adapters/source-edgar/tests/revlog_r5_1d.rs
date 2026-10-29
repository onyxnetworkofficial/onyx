//! THREATS REVLOG-R5-1d (ROADMAP B5) for the SEC EDGAR runner: the same
//! intake as the Treasury runner (`revlog_r5_1d.rs` in
//! adapters/source-fiscaldata/tests). A run stopped before the log snapshot
//! with one of its three reveals saved, restarted inside the reveal window,
//! takes the two reveals the observers send again.

use onyx_coordinator::observe::{
    ChainView, ChainWait, DrawChain, DrawRecord, LogBlock, ObserverSig, OnchainDraw, OnchainFact,
    Posted, PostedCommit, ReplayClock, SignedReveal, SimulatedRegistry, TranscriptLogQuery,
};
use onyx_core::{Hash32, MinerAddress};
use onyx_source_edgar::round::{
    load_book, run_and_save, save_book, store, Page, RoundRun, RunError,
};
use onyx_source_edgar::{recorded, spec};

/// A simulated registry whose finalized-block read can be cut (the node
/// down, or the process stopping right after the intake).
struct Chain {
    inner: SimulatedRegistry,
    down: bool,
}

impl ChainView for Chain {
    fn height(&self) -> Result<u64, String> {
        self.inner.height()
    }
    fn block_hash(&self, h: u64) -> Result<Option<Hash32>, String> {
        self.inner.block_hash(h)
    }
}

impl DrawChain for Chain {
    fn draw_count(&self, f: &OnchainFact) -> Result<u32, String> {
        self.inner.draw_count(f)
    }
    fn draw_budget(&self, f: &OnchainFact) -> Result<(u32, u32), String> {
        self.inner.draw_budget(f)
    }
    fn announce(&mut self, d: &OnchainDraw) -> Result<(), String> {
        self.inner.announce(d)
    }
    fn fix_beacon(&mut self, d: &OnchainDraw) -> Result<DrawRecord, String> {
        self.inner.fix_beacon(d)
    }
    fn draw_of(&self, f: &OnchainFact, attempt: u32) -> Result<Option<DrawRecord>, String> {
        self.inner.draw_of(f, attempt)
    }
    fn draw_of_final(
        &self,
        f: &OnchainFact,
        attempt: u32,
    ) -> Result<(u64, Option<DrawRecord>), String> {
        self.inner.draw_of_final(f, attempt)
    }
    fn post_commit(
        &mut self,
        f: &OnchainFact,
        attempt: u32,
        o: &MinerAddress,
        round: &Hash32,
        commit: &Hash32,
        sig: &ObserverSig,
    ) -> Result<Posted, String> {
        self.inner.post_commit(f, attempt, o, round, commit, sig)
    }
    fn post_receipt(
        &mut self,
        f: &OnchainFact,
        attempt: u32,
        o: &MinerAddress,
        round: &Hash32,
        commit: &Hash32,
        sig: &ObserverSig,
    ) -> Result<Posted, String> {
        self.inner.post_receipt(f, attempt, o, round, commit, sig)
    }
    fn log_block(&self) -> Result<Option<LogBlock>, String> {
        if self.down {
            return Err("connection refused".into());
        }
        self.inner.log_block()
    }
    fn posted_commit(
        &self,
        block: u64,
        f: &OnchainFact,
        attempt: u32,
        o: &MinerAddress,
        round: &Hash32,
    ) -> Result<Option<PostedCommit>, String> {
        self.inner.posted_commit(block, f, attempt, o, round)
    }
    fn transcript_complete(&self, q: &TranscriptLogQuery) -> Result<bool, String> {
        self.inner.transcript_complete(q)
    }
}

fn pages() -> Vec<Page> {
    let rec = &recorded()[0];
    rec.fetches
        .iter()
        .map(|f| Page {
            fetched_unix: f.fetched_unix,
            body: Some(f.body.clone()),
        })
        .collect()
}

fn run_once(
    chain: &mut Chain,
    book: &str,
    clock: &ReplayClock,
    inbox: &mut Vec<SignedReveal>,
) -> Result<RoundRun, RunError> {
    let rec = &recorded()[0];
    let mut s = store();
    load_book(&mut s, book).expect("the book loads");
    run_and_save(
        &rec.company,
        &rec.period_end,
        rec.window_start,
        &pages(),
        &mut s,
        chain,
        book,
        None,
        &ChainWait::NOW,
        clock,
        inbox,
        &mut |_| Ok(()),
        &mut |_| panic!("no round fails here"),
    )
}

#[test]
fn r5_1d_edgar_a_restart_inside_the_reveal_window_keeps_taking_reveals() {
    let dir = std::env::temp_dir().join(format!("onyx-r5-1d-edgar-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let book = dir.join("book.json").to_string_lossy().into_owned();
    let mut chain = Chain {
        inner: SimulatedRegistry::new(),
        down: true,
    };
    run_once(&mut chain, &book, &ReplayClock::default(), &mut vec![])
        .expect_err("stopped before the log snapshot");
    let rec = &recorded()[0];
    let sp = spec(&rec.company, &rec.period_end, rec.window_start).expect("spec");
    let mut s = store();
    load_book(&mut s, &book).expect("loads");
    let mut u = s.kept_round(&sp).expect("kept");
    assert_eq!(u.reveals.len(), pages().len());
    let mut lost: Vec<SignedReveal> = u
        .reveals
        .split_off(1)
        .into_iter()
        .map(|a| a.reveal)
        .collect();
    s.keep_unfinished(u.clone()).expect("kept");
    save_book(&s, &book).expect("saved");
    chain.down = false;
    let r = run_once(
        &mut chain,
        &book,
        &ReplayClock::at(u.round.config().commit_close + 10),
        &mut lost,
    )
    .expect("settles");
    assert_eq!(
        r.reveals.len(),
        pages().len(),
        "no reveal lost to the restart"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
