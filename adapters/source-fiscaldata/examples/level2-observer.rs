//! One independent level-2 observer (ROADMAP B2): a process of its own,
//! with its own key, that never trusts the coordinator for anything it can
//! read itself.
//!
//! `cargo run -p onyx-source-fiscaldata --example level2-observer --
//!   --key-file FILE --pool FILE --spool DIR --state DIR
//!   --record-date YYYY-MM-DD|latest --window-start UNIX
//!   --rpc URL --registry ADDR [--cast PATH] [--body FILE]
//!   [--sealed-garbage | --sealed-garbage-raw] [--clock anvil]`
//!
//! Or, for an outside observer (north-star loop NL16, docs/join.md):
//! `--key-file FILE --state DIR --announcement FILE [--rpc URL] [--cast PATH]`.
//! The announcement gives the pool, record date, window, registry and a
//! suggested RPC; the observer refuses one for another template than its
//! own, one with spool reveals, a round flag given twice, and an RPC on
//! another chain (`onyx_level2::live::apply_announcement`).
//!
//! 1. Reads its key (`--key-file`, a 0600 file with a 32-byte hex key; its
//!    address must be in the pool snapshot `--pool`, the same file the
//!    coordinator draws from).
//! 2. Watches `OnyxRegistry` for the fact's latest draw and checks it
//!    commits to ITS pool, the feed's canonical spec and config, with the
//!    beacon fixed (`drawn_round`, ATT-R1-F4); recomputes the draw and the
//!    round id. Not drawn: says so and exits 0.
//! 3. At its own fetch slot in the window (`onyx_core::fetch::fetch_time`, by
//!    its own clock; partner tracker M2) fetches the pinned record itself
//!    over HTTPS (`onyx_fetch`, CL3: public addresses only, no redirects,
//!    size caps, 20 s; `--body FILE` replays a saved
//!    response instead, for an offline rehearsal), observes, draws a fresh
//!    salt, signs commit and reveal with its key, and saves them in
//!    `--state` (0600) BEFORE anything is sent, so a restarted observer
//!    resends the same reveal.
//! 4. Posts its signed commit on chain itself (`postCommit`, from its own
//!    account: an unlocked anvil account in the rehearsal).
//! 5. Once the CHAIN's latest block is past commit close and its own commit
//!    is on chain inside the phase, reveals:
//!    - when the draw has a reveal key (`drawRevealKey`, ROADMAP C11, ADR-0027
//!      amendment 10): seals its signed reveal to that key with a FRESH
//!      ephemeral key (`sealed::seal_random`), saves the post (0600) before
//!      sending, and posts it on chain itself (`postSealedReveal`); the post
//!      is the receipt. It checks its slot holds exactly its post with a block
//!      time inside the reveal phase: "sealed post on chain", exit 0; outside
//!      the phase, exit 3. A dropped post makes every client refuse the round
//!      (`revealLogComplete`, THREATS REVLOG-1 closed for it).
//!    - otherwise: writes its signed reveal to the coordinator's spool
//!      (`onyx_coordinator::observer_link`), then
//! 6. waits for the coordinator's receipt on chain (`receiptLog`) until 10
//!    minutes past the reveal window: printed and exit 0 with it, exit 3
//!    without (the coordinator withheld or never finished: THREATS
//!    REVLOG-1; the posted commit is the observer's evidence).
//!
//! Time-locked draws (ROADMAP C51, ADR-0027 amendment 14): when the draw was
//! announced with time-locks required (`drawTimeLocked`), the observer seals
//! with `timelock::seal_timelocked_random` to drand quicknet's lock round of
//! the window's `reveal_close` (from the config the on-chain draw commits to),
//! refusing a lock round that is out or due within 30 s by its own clock
//! (`RoundTooSoon`). Nobody, the coordinator included, opens the post before
//! that round's signature exists. ROADMAP C52: the post also carries a far
//! copy of its body key, time-locked to the far round of the record date's
//! last observable day (from the feed's template), so anyone opens it once
//! that round is out, without the coordinator.
//!
//! `--sealed-garbage` (rehearsal only, a local anvil): post junk instead of
//! the sealed reveal, as a griefing observer would (THREATS C4-4): the
//! coordinator dismisses it with a proof every client checks. In a
//! time-locked draw the junk sits behind a valid time-lock (dismissed with
//! its opening); `--sealed-garbage-raw` posts junk with no valid time-lock
//! (dismissed without one; checked with the lock round's signature).
//!
//! `--clock anvil` (rehearsal only, a local anvil): the observer's clock is
//! the system clock shifted to the anvil's block time, for a rehearsal whose
//! anvil was started in the past (`anvil --timestamp`), so its lock rounds are
//! published drand rounds served from fixtures.
//!
//! Rehearsal only (local anvil): true independence needs separate hosts,
//! networks and operators (THREATS L2-SRC-1). EDGAR observers stay
//! rehearsal-only until an operator role contact exists (SEC-4).
//!
//! The code is the shared live observer, [`onyx_level2::live::observer`]
//! (ROADMAP B6): every dated source runs the same observer. On a live chain
//! it signs its own commit with its `--key-file` key; `--receipt-wait SECS`
//! sets how long it waits for the receipt (default 600) and `--address`
//! prints its address.

use onyx_source_fiscaldata::round::from_env;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match from_env() {
        Ok(src) => std::process::exit(onyx_level2::live::observer(src.as_ref(), &args)),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    }
}
