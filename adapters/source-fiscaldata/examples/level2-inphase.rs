//! Level-2 in-phase rehearsal on a real EVM (review REVLOG-R3-4): the
//! recorded Treasury round on a local anvil whose clock starts inside the
//! round's window (`anvil --timestamp`), so every commit post lands INSIDE
//! the commit phase and the coordinator's log read, snapshot and
//! `transcriptComplete` are exercised against `OnyxRegistry` itself, not a
//! simulated chain.
//!
//! One drawn observer griefs: before the coordinator relays its commit, it
//! posts ANOTHER commit for itself, in phase, signed with its own key. The
//! round must absorb that posted commit from the one pinned log block,
//! count the griefer as a non-reveal, settle the value from the others and
//! pass the contract's `transcriptComplete`.
//!
//! Rehearsal only: anvil's clock is moved past `commit_close` with
//! `evm_increaseTime` once the coordinator waits for the commit phase to
//! end (a testnet waits for its finalized block instead). Writes nothing.
//!
//! `cargo run -p onyx-source-fiscaldata --example level2-inphase -- --rpc URL
//!   --registry ADDR --coordinator ADDR --owner ADDR [--cast PATH]`
//! (`scripts/level2-rehearsal.sh` sets it up).

use onyx_coordinator::cast_chain::{parse_addr, CastChain, Finality};
use onyx_coordinator::observe::{
    sign_commit, ChainView, ChainWait, DrawChain, DrawRecord, LogBlock, ObservationOutcome,
    ObserverSig, ObserverStatus, OnchainDraw, OnchainFact, Posted, PostedCommit,
    TranscriptLogQuery,
};
use onyx_core::{keccak256, Hash32, MinerAddress};
use onyx_source_fiscaldata::round::{
    config, observer_address, observer_key, run_waiting, store, transcript_query, Page, POOL_SIZE,
};
use onyx_source_fiscaldata::{recorded, spec, RECORDED_WINDOW_START};
use std::cell::Cell;

/// `CastChain` with a griefing observer and anvil's clock under the
/// rehearsal's control.
struct InPhase {
    inner: CastChain,
    /// The round's `commit_close`.
    close: u64,
    /// The griefer and the commit it posted.
    victim: Option<(MinerAddress, Hash32)>,
    /// Times the clock was moved.
    warps: Cell<u32>,
}

impl InPhase {
    fn latest_time(&self) -> Result<u64, String> {
        let t = self.inner.run(&[
            "block".into(),
            "latest".into(),
            "--field".into(),
            "timestamp".into(),
        ])?;
        t.split_whitespace()
            .next()
            .and_then(|v| v.parse().ok())
            .ok_or(format!("timestamp {t}"))
    }
}

impl ChainView for InPhase {
    fn height(&self) -> Result<u64, String> {
        self.inner.height()
    }
    fn block_hash(&self, h: u64) -> Result<Option<Hash32>, String> {
        self.inner.block_hash(h)
    }
}

impl DrawChain for InPhase {
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
        fact: &OnchainFact,
        attempt: u32,
        observer: &MinerAddress,
        round_id: &Hash32,
        commit: &Hash32,
        sig: &ObserverSig,
    ) -> Result<Posted, String> {
        if self.victim.is_none() {
            let key = (1..=POOL_SIZE)
                .find(|i| observer_address(*i) == *observer)
                .map(observer_key)
                .ok_or("not a pool member")?;
            let c = keccak256(b"rehearsal griefer: posted in phase, never revealed");
            let s = sign_commit(&key, round_id, attempt, &c).ok_or("sign")?;
            let now = self.latest_time()?;
            if now > self.close {
                return Err(format!(
                    "anvil time {now} is past commit close {}: not an in-phase rehearsal",
                    self.close
                ));
            }
            self.inner
                .post_commit(fact, attempt, observer, round_id, &c, &s)?;
            self.victim = Some((*observer, c));
        }
        self.inner
            .post_commit(fact, attempt, observer, round_id, commit, sig)
    }
    fn post_receipt(
        &mut self,
        fact: &OnchainFact,
        attempt: u32,
        observer: &MinerAddress,
        round_id: &Hash32,
        commit: &Hash32,
        sig: &ObserverSig,
    ) -> Result<Posted, String> {
        self.inner
            .post_receipt(fact, attempt, observer, round_id, commit, sig)
    }
    fn log_block(&self) -> Result<Option<LogBlock>, String> {
        let b = self.inner.log_block()?;
        if let Some(b) = b {
            if b.time <= self.close {
                // Rehearsal clock: move anvil past commit close and mine
                // enough blocks that the pinned log block is past it too.
                let step = self.close + 1 - self.latest_time()?.min(self.close);
                self.inner
                    .run(&["rpc".into(), "evm_increaseTime".into(), step.to_string()])?;
                let Finality::Depth(d) = self.inner.finality else {
                    return Err("the in-phase rehearsal runs on a local anvil".into());
                };
                self.inner
                    .run(&["rpc".into(), "anvil_mine".into(), format!("0x{:x}", d + 1)])?;
                self.warps.set(self.warps.get() + 1);
            }
        }
        Ok(b)
    }
    fn posted_commit(
        &self,
        block: u64,
        fact: &OnchainFact,
        attempt: u32,
        observer: &MinerAddress,
        round_id: &Hash32,
    ) -> Result<Option<PostedCommit>, String> {
        self.inner
            .posted_commit(block, fact, attempt, observer, round_id)
    }
    fn transcript_complete(&self, q: &TranscriptLogQuery) -> Result<bool, String> {
        self.inner.transcript_complete(q)
    }
    fn draw_receipt_signer(&self, f: &OnchainFact, a: u32) -> Result<Option<[u8; 20]>, String> {
        self.inner.draw_receipt_signer(f, a)
    }
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let cast = CastChain::from_args(&args)?.ok_or("--rpc URL is required")?;
    if !matches!(cast.finality, Finality::Depth(_)) {
        return Err("the in-phase rehearsal runs on a local anvil only".into());
    }
    let owner = args
        .iter()
        .position(|a| a == "--owner")
        .and_then(|i| args.get(i + 1))
        .ok_or("--owner ADDR is required")?;
    cast.register_metric(&parse_addr(owner)?, &onyx_source_fiscaldata::template())?;
    let rec = recorded();
    let pages: Vec<Page> = rec
        .fetches
        .iter()
        .map(|f| Page {
            fetched_unix: f.fetched_unix,
            body: Some(f.body.clone()),
        })
        .collect();
    let sp = spec(&rec.record_date, RECORDED_WINDOW_START)?;
    let close = config(&sp).commit_close;
    let mut chain = InPhase {
        inner: cast,
        close,
        victim: None,
        warps: Cell::new(0),
    };
    let mut s = store();
    let wait = ChainWait {
        tries: 3,
        polls: 5,
        pause: std::time::Duration::from_millis(200),
    };
    let r = run_waiting(
        &rec.record_date,
        RECORDED_WINDOW_START,
        &pages,
        &mut s,
        &mut chain,
        &wait,
    )
    .map_err(|e| format!("in-phase round: {e}"))?;
    let (griefer, posted) = chain.victim.ok_or("no griefer posted")?;
    let fact = s.onchain_fact(&r.spec).map_err(|e| e.to_string())?;
    // The log block the round was read at, and the griefer's post there.
    let served = r
        .commits
        .iter()
        .find(|(a, _, _)| *a == griefer)
        .map(|(_, c, _)| *c);
    if served != Some(posted) {
        return Err("the griefer's in-phase post was not absorbed".into());
    }
    let status = r
        .settlement
        .statuses()
        .iter()
        .find(|(a, _)| *a == griefer)
        .map(|(_, st)| *st);
    if status != Some(ObserverStatus::Unrevealed) {
        return Err(format!("griefer status {status:?}, not Unrevealed"));
    }
    if !matches!(r.settlement.outcome(), ObservationOutcome::Value(_)) {
        return Err(format!("no value: {:?}", r.settlement.outcome()));
    }
    let head = chain.inner.height()?;
    let p = chain
        .inner
        .posted_commit(head, &fact, r.draw.attempt, &griefer, &r.round_id)?
        .ok_or("the griefer's slot is empty on anvil")?;
    if p.posted_at < r.spec.window_start || p.posted_at > close {
        return Err(format!(
            "the griefer's post landed at {} outside the commit phase [{}, {close}]",
            p.posted_at, r.spec.window_start
        ));
    }
    if !chain
        .inner
        .transcript_complete(&transcript_query(&r, fact.registry_key()))?
    {
        return Err("OnyxRegistry.transcriptComplete refused the recorded round".into());
    }
    println!(
        "in-phase round on anvil: griefer 0x{} posted at {} (commit phase {}..={close}), absorbed from the pinned log block; {} of {} observers revealed; value settled; transcriptComplete true; clock moved {} time(s)",
        griefer.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        p.posted_at,
        r.spec.window_start,
        r.reveals.len(),
        r.observers.len(),
        chain.warps.get()
    );
    println!("IN-PHASE ABSORB ON ANVIL PASSED");
    Ok(())
}
