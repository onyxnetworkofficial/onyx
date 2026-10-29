//! Review R7: two ways a draw on chain could still be lost by the
//! coordinator, each leaving a beacon that is fixed (or burned) without a
//! published round:
//!
//! - R7-1: after an `announceDraw` whose send reported an error but landed,
//!   `confirm_announced` read the latest `drawOf` once and then polled only
//!   the final block. On Base Sepolia the final block lags the head by about
//!   500 blocks, more than the 256-block beacon window, so a landed draw was
//!   confirmed too late to fix its beacon and was given up (a strike burned).
//! - R7-2: a chain draw the book could not adopt (a crash between the send
//!   and the first book save, then a restart with other inputs; or a lost or
//!   older book) was silently drawn past: draw 0 stayed fixable by anyone,
//!   and a fixed beacon without a published round voids the fact.
//!
//! Each reviewer PoC is kept with its scenario unchanged and its assertions
//! now stating the fixed behaviour; siblings pin the edges.

use onyx_coordinator::observe::{
    draw_and_open, ChainView, ChainWait, DrawChain, DrawRecord, LogBlock, ObservationResults,
    ObserverSig, OnchainDraw, OnchainFact, Posted, PostedCommit, SimulatedRegistry,
    TranscriptLogQuery, BLOCKHASH_WINDOW,
};
use onyx_core::{Hash32, MinerAddress};
use onyx_source_fiscaldata::round::{config, rehearsal_pool, store};
use onyx_source_fiscaldata::{recorded, spec, RECORDED_WINDOW_START};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

/// A `SimulatedRegistry` like Base Sepolia: the final block lags the head by
/// `lag` blocks (measured 2026-10-12: about 498) and every final read lets
/// `per_poll` blocks pass. `announceDraw` may land and report an error, and
/// latest `drawOf` reads may be stale (a lagging node answers no draw).
struct Lag {
    inner: RefCell<SimulatedRegistry>,
    lag: u64,
    per_poll: u64,
    stale_latest: Cell<u32>,
    /// The next `announceDraw` lands, then reports a receipt timeout.
    announce_err: Cell<bool>,
    /// The next `announceDraw` reports a timeout WITHOUT landing.
    announce_lost: Cell<bool>,
    /// The process dies right after the next `announceDraw` lands: the send
    /// reports an error and every later chain call fails.
    die_after_announce: Cell<bool>,
    dead: Cell<bool>,
    /// Whether the book on disk held the draw when `announceDraw` was sent.
    book: Option<PathBuf>,
    book_held_draw: Cell<Option<bool>>,
    final_polls: Cell<u32>,
    announces: Cell<u32>,
}

impl Lag {
    fn new(lag: u64, per_poll: u64) -> Self {
        Lag {
            inner: RefCell::new(SimulatedRegistry::new()),
            lag,
            per_poll,
            stale_latest: Cell::new(0),
            announce_err: Cell::new(false),
            announce_lost: Cell::new(false),
            die_after_announce: Cell::new(false),
            dead: Cell::new(false),
            book: None,
            book_held_draw: Cell::new(None),
            final_polls: Cell::new(0),
            announces: Cell::new(0),
        }
    }
    fn alive(&self) -> Result<(), String> {
        if self.dead.get() {
            return Err("the process is gone".into());
        }
        Ok(())
    }
}

impl ChainView for Lag {
    fn height(&self) -> Result<u64, String> {
        self.alive()?;
        self.inner.borrow().height()
    }
    fn block_hash(&self, h: u64) -> Result<Option<Hash32>, String> {
        self.alive()?;
        self.inner.borrow().block_hash(h)
    }
}

impl DrawChain for Lag {
    fn draw_count(&self, f: &OnchainFact) -> Result<u32, String> {
        self.alive()?;
        self.inner.borrow().draw_count(f)
    }
    fn draw_budget(&self, f: &OnchainFact) -> Result<(u32, u32), String> {
        self.alive()?;
        self.inner.borrow().draw_budget(f)
    }
    fn announce(&mut self, d: &OnchainDraw) -> Result<(), String> {
        self.alive()?;
        if let Some(b) = &self.book {
            let mut s = store();
            let held = s.load_book(b).is_ok() && s.latest_draw(&spec_now()).is_ok_and(|x| x == *d);
            self.book_held_draw.set(Some(held));
        }
        if self.announce_lost.replace(false) {
            return Err("cast send: timed out (never sent)".into());
        }
        self.announces.set(self.announces.get() + 1);
        self.inner.borrow_mut().announce(d)?;
        if self.die_after_announce.replace(false) {
            self.dead.set(true);
            return Err("killed".into());
        }
        if self.announce_err.replace(false) {
            self.stale_latest.set(1);
            return Err("cast send: timed out waiting for the receipt".into());
        }
        Ok(())
    }
    fn fix_beacon(&mut self, d: &OnchainDraw) -> Result<DrawRecord, String> {
        self.alive()?;
        let h = self.inner.borrow().height;
        if h + 1 > d.beacon_height + BLOCKHASH_WINDOW {
            return Err("fixBeacon: BeaconUnavailable".into());
        }
        self.inner.borrow_mut().fix_beacon(d)
    }
    fn draw_of(&self, f: &OnchainFact, a: u32) -> Result<Option<DrawRecord>, String> {
        self.alive()?;
        if self.stale_latest.get() > 0 {
            self.stale_latest.set(self.stale_latest.get() - 1);
            return Ok(None);
        }
        self.inner.borrow().draw_of(f, a)
    }
    fn draw_of_final(&self, f: &OnchainFact, a: u32) -> Result<(u64, Option<DrawRecord>), String> {
        self.alive()?;
        self.final_polls.set(self.final_polls.get() + 1);
        self.inner.borrow_mut().height += self.per_poll;
        let fin = self.inner.borrow().height.saturating_sub(self.lag);
        let r = self.inner.borrow().draw_of(f, a)?;
        // As of the final block: a draw announced later is absent, and a
        // beacon fixed later is zero (the simulated registry fixes at the
        // head; tests that need it read the beacon as of the head).
        Ok((fin, r.filter(|r| r.announced_at <= fin)))
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
        self.inner.borrow_mut().post_commit(f, a, o, r, c, s)
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
        self.inner.borrow_mut().post_receipt(f, a, o, r, c, s)
    }
    fn log_block(&self) -> Result<Option<LogBlock>, String> {
        self.inner.borrow().log_block()
    }
    fn posted_commit(
        &self,
        b: u64,
        f: &OnchainFact,
        a: u32,
        o: &MinerAddress,
        r: &Hash32,
    ) -> Result<Option<PostedCommit>, String> {
        self.inner.borrow().posted_commit(b, f, a, o, r)
    }
    fn transcript_complete(&self, q: &TranscriptLogQuery) -> Result<bool, String> {
        self.inner.borrow().transcript_complete(q)
    }
}

fn spec_now() -> onyx_core::ObservationSpec {
    spec(&recorded().record_date, RECORDED_WINDOW_START).expect("spec")
}

fn book_path(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("onyx-r7-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("dir");
    d.join("book.json")
}

fn draw(
    s: &mut ObservationResults,
    chain: &mut dyn DrawChain,
    pool: &[onyx_coordinator::observe::Observer],
    wait: &ChainWait,
    book: Option<&Path>,
) -> Result<DrawRecord, String> {
    let sp = spec_now();
    let cfg = config(&sp);
    draw_and_open(s, sp, cfg, pool, chain, wait, &mut |st| match book {
        Some(b) => st.save_book(b),
        None => Ok(()),
    })
    .map(|(_, rec)| rec)
}

/// R7-1 (the reviewer's PoC): one receipt timeout on `announceDraw` (it
/// landed) and one stale latest `drawOf`, on a chain whose final block lags
/// the head by 450 blocks. The draw was confirmed only at the final block,
/// after its 256-block beacon window: never fixed, given up, a strike burned
/// (observed: 91 final polls, head 456, beacon height 3). Now the latest
/// `drawOf` is read again on the next poll, shows the draw, and its beacon is
/// fixed in time: draw 0 opens.
#[test]
fn r7_1_final_only_polling_burns_a_landed_draw() {
    let mut s = store();
    let mut chain = Lag::new(450, 5);
    chain.announce_err.set(true);
    let wait = ChainWait {
        tries: 3,
        polls: 360,
        pause: std::time::Duration::ZERO,
    };
    let rec = draw(&mut s, &mut chain, &rehearsal_pool(), &wait, None)
        .expect("the landed draw is confirmed from the latest block and opened");
    assert_eq!((rec.attempt, rec.draw_count), (0, 1));
    assert_ne!(rec.beacon, Hash32::ZERO, "beacon fixed");
    assert!(
        chain.final_polls.get() <= 1,
        "confirmed on the next poll, not at the final block ({} final polls)",
        chain.final_polls.get()
    );
    assert!(chain.inner.borrow().height <= rec.beacon_height + BLOCKHASH_WINDOW);
    assert_eq!(chain.announces.get(), 1, "never announced again");
}

/// R7-1: positive evidence is acted on at once, but forgetting still needs
/// the final block. An announcement that never landed is forgotten only once
/// a final block at or past its beacon height shows no draw, however many
/// latest reads show none.
#[test]
fn r7_1_a_lost_announce_is_forgotten_only_at_a_final_block() {
    let mut s = store();
    let mut chain = Lag::new(450, 5);
    chain.announce_lost.set(true);
    let wait = ChainWait {
        tries: 3,
        polls: 360,
        pause: std::time::Duration::ZERO,
    };
    let e = draw(&mut s, &mut chain, &rehearsal_pool(), &wait, None).expect_err("forgotten");
    assert!(e.contains("never landed: forgotten"), "{e}");
    // The final block passed the beacon height (3 + DRAW_LEAD) only after
    // the head passed it by the lag: 90 polls of 5 blocks.
    assert!(chain.final_polls.get() >= 90, "{}", chain.final_polls.get());
    let fact = s.onchain_fact(&spec_now()).expect("fact");
    assert_eq!(chain.inner.borrow().draw_count(&fact), Ok(0));
    assert_eq!(s.pending_draw(&spec_now()), None, "the book dropped it");
}

/// R7-2 (the reviewer's PoC): the chain holds draw 0, which the book lacks
/// (the process died after the send and before the first save), and the
/// runner's inputs drifted (one pool member left), so the draw cannot be
/// adopted. It was silently drawn past (attempt 1), leaving draw 0 for
/// anyone to fix. Now the run stops, announces nothing, and still stops
/// after a stranger fixes draw 0.
#[test]
fn r7_2_unadoptable_chain_draw_is_drawn_past() {
    let sp = spec_now();
    let cfg = config(&sp);
    let mut chain = SimulatedRegistry::new();
    let mut s1 = store();
    s1.announce_draw(&sp, &cfg, &rehearsal_pool(), 3, &chain)
        .expect("announce in book");
    let d0 = s1.latest_draw(&sp).expect("draw");
    chain.announce(&d0).expect("lands");
    drop(s1);
    let mut pool = rehearsal_pool();
    pool.remove(0);
    let mut s2 = store();
    let e = draw(&mut s2, &mut chain, &pool, &ChainWait::NOW, None).expect_err("stops");
    assert!(
        e.contains("cannot resume or adopt") && e.contains("still fixable"),
        "{e}"
    );
    let fact = s2.onchain_fact(&sp).expect("fact");
    assert_eq!(chain.draw_count(&fact), Ok(1), "no draw 1");
    assert_eq!(s2.next_attempt(&sp), Ok(0), "the book is unchanged");
    chain.fix_beacon(&d0).expect("a stranger fixes draw 0");
    let e = draw(&mut s2, &mut chain, &pool, &ChainWait::NOW, None).expect_err("still stops");
    assert!(e.contains("its beacon is fixed"), "{e}");
    assert_eq!(chain.draw_count(&fact), Ok(1), "never drawn past");
}

/// R7-2: an unadoptable chain draw whose beacon can never be fixed (zero at
/// a final block past its beacon height + 256) is drawn past: no committee
/// exists for it, and clients accept a draw without a beacon.
#[test]
fn r7_2_an_unfixable_chain_draw_is_drawn_past() {
    let sp = spec_now();
    let cfg = config(&sp);
    let mut chain = SimulatedRegistry::new();
    let mut s1 = store();
    s1.announce_draw(&sp, &cfg, &rehearsal_pool(), 3, &chain)
        .expect("announce in book");
    let d0 = s1.latest_draw(&sp).expect("draw");
    chain.announce(&d0).expect("lands");
    let mut pool = rehearsal_pool();
    pool.remove(0);
    let mut s2 = store();
    chain.height = d0.beacon_height + BLOCKHASH_WINDOW;
    let e = draw(&mut s2, &mut chain, &pool, &ChainWait::NOW, None).expect_err("last block");
    assert!(e.contains("still fixable"), "{e}");
    chain.height = d0.beacon_height + BLOCKHASH_WINDOW + 1;
    let rec = draw(&mut s2, &mut chain, &pool, &ChainWait::NOW, None).expect("drawn past");
    assert_eq!((rec.attempt, rec.draw_count), (1, 2));
    let fact = s2.onchain_fact(&sp).expect("fact");
    assert_eq!(
        chain
            .draw_of(&fact, 0)
            .expect("read")
            .expect("draw 0")
            .beacon,
        Hash32::ZERO
    );
}

/// R7-2 (a), THREATS REVLOG-R5-2: the book on disk holds the draw when
/// `announceDraw` is sent, so a crash right after it lands leaves nothing on
/// chain that the book lacks. A stranger fixes draw 0 meanwhile; the restart
/// resumes draw 0 from the book (with its saved pool, though this runner's
/// pool has drifted) instead of adopting or drawing past it.
#[test]
fn r7_2_the_book_holds_the_draw_before_it_is_sent() {
    let path = book_path("save-first");
    let mut chain = Lag::new(0, 0);
    chain.book = Some(path.clone());
    chain.die_after_announce.set(true);
    let mut s = store();
    let e = draw(
        &mut s,
        &mut chain,
        &rehearsal_pool(),
        &ChainWait::NOW,
        Some(&path),
    )
    .expect_err("killed");
    assert!(e.contains("stays in the book"), "{e}");
    assert_eq!(
        chain.book_held_draw.get(),
        Some(true),
        "saved before the send"
    );
    let d0 = {
        let mut b = store();
        b.load_book(&path).expect("book");
        b.latest_draw(&spec_now()).expect("draw 0 in the book")
    };
    chain
        .inner
        .borrow_mut()
        .fix_beacon(&d0)
        .expect("a stranger fixes draw 0");
    chain.dead.set(false);
    let mut pool = rehearsal_pool();
    pool.remove(0);
    let mut b = store();
    b.load_book(&path).expect("book");
    let rec = draw(&mut b, &mut chain, &pool, &ChainWait::NOW, Some(&path)).expect("resumed");
    assert_eq!((rec.attempt, rec.draw_count), (0, 1));
    assert_eq!(chain.announces.get(), 1, "never announced again");
    let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
}

/// R7-2 (a): a book that cannot be saved sends nothing.
#[test]
fn r7_2_an_unsaved_book_sends_nothing() {
    let mut chain = Lag::new(0, 0);
    let mut s = store();
    let sp = spec_now();
    let cfg = config(&sp);
    let e = draw_and_open(
        &mut s,
        sp.clone(),
        cfg,
        &rehearsal_pool(),
        &mut chain,
        &ChainWait::NOW,
        &mut |_| Err("disk full".into()),
    )
    .expect_err("not saved");
    assert!(e.contains("nothing was sent"), "{e}");
    assert_eq!(chain.announces.get(), 0);
    assert_eq!(s.next_attempt(&sp), Ok(0), "the attempt is not used");
}
