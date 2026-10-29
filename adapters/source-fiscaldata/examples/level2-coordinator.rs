//! The live level-2 coordinator for independent observers (ROADMAP B2).
//!
//! `cargo run -p onyx-source-fiscaldata --example level2-coordinator --
//!   --rpc URL --registry ADDR --coordinator ADDR [--owner ADDR] [--cast PATH]
//!   --book PATH --pool FILE --spool DIR --receipt-key FILE [--reveal-key FILE]
//!   [--reporter ADDR] --record-date YYYY-MM-DD|latest --window-start UNIX
//!   [--store PATH [--new-store]] [--clock system|anvil] [--finality finalized|N]
//!   [--beacon drand|fixture:FILE] [--require-time-lock] [--announce FILE]`
//!
//! `--announce FILE` (north-star loop NL16, docs/join.md) writes the round
//! announcement an outside observer joins with: chain, registry, metric,
//! template, series, record date, window, pool and how reveals travel
//! (`onyx_level2::live::announcement`); it holds no secret.
//!
//! It draws the round's observers from the bonded pool snapshot `--pool`
//! (JSON `[{"address", "bond"}]`, the file every observer holds too),
//! announces the draw on `OnyxRegistry` and fixes its beacon, then holds no
//! observer key and simulates no observer: each drawn observer
//! (examples/level2-observer.rs, a process of its own with its own key)
//! reads the draw from the registry, fetches the source itself, posts its
//! signed commit on chain, and sends its signed reveal to the spool `--spool`
//! (`onyx_coordinator::observer_link`, ADR-0027 amendment 9). The
//! coordinator takes the reveals from the spool while the reveal window is
//! open by its SYSTEM clock (a replayed clock is refused), absorbs the
//! posted commits from the transcript log once the commit phase is final,
//! receipts each reveal with the deployment's own receipt key and settles.
//!
//! `--receipt-key FILE` is required (ROADMAP B3): a 0600 file with the
//! deployment's own hot key, never the public test key, and different from
//! `--coordinator`, `--owner`, `--reporter` and every pool member (else the
//! run refuses to start). A different key than the registry's
//! `receiptSigner` is set (rotated) before the draw is announced.
//!
//! `--reveal-key FILE` (ROADMAP C11, ADR-0027 amendment 10): a 0600 file with
//! the coordinator's reveal key, a key of its own (refused when it is the
//! receipt key, the coordinator, the registry owner, the reporter or a pool
//! member; a public key only on a local anvil). Its point is set with
//! `setRevealKey` before the draw is announced (a rotation when the
//! registry's differs), so the draw snapshots it: each drawn observer then
//! posts its reveal on chain encrypted to it (`postSealedReveal`), and the
//! coordinator, once the chain's finalized block is past reveal close, reads
//! every drawn slot from that one block, opens or dismisses each post, and
//! checks `revealLogComplete`. Without it the draw takes no sealed post and
//! the observers reveal through the spool (REVLOG-1 open for them).
//!
//! Time-locked draws (ROADMAP C51, ADR-0027 amendment 14): `--require-time-lock`
//! sends `requireTimeLock()` (ONE WAY: nobody can turn it off) before the draw,
//! so every draw from then on needs the reveal key and is time-locked
//! (`drawTimeLocked`). For such a draw the coordinator, after the finalized
//! block past reveal close, waits for drand quicknet's lock round signature
//! from `--beacon` (drand's public HTTP relays, no account; or
//! `fixture:FILE`, published rounds from a file, local anvil only) and keeps
//! it only when it verifies against the pinned quicknet key; without it the
//! round waits unfinished and nothing is opened (THREATS C12-4, C53). The
//! dismissals then carry their time-lock openings. `--clock anvil` (local anvil
//! only): the system clock shifted to the anvil's block time, for a rehearsal
//! whose anvil was started in the past so its lock rounds are published ones.
//!
//! Every round is recorded on chain before it is written to the results
//! store (ROADMAP C11): a value with `recordObservation`, a round without a
//! value with `recordAttempt` (REVLOG-5), so a client finds a record for every
//! round served sealed.
//!
//! Rehearsal only so far: `scripts/level2-observers.sh` runs it with several
//! observer processes on ONE machine against a local anvil. True
//! independence (separate hosts, networks and operators, THREATS L2-SRC-1)
//! needs real operators. The book and `--store` rules are those of
//! `level2-round` (one runner per book, the results store written before the
//! round leaves the book). Prints the round's outcome as JSON.
//!
//! The code is the shared live runner, [`onyx_level2::live::coordinator`]
//! (ROADMAP B6): every dated source runs the same coordinator. On a live
//! chain the coordinator's key comes from `ONYX_L2_COORDINATOR_KEY` and the
//! owner's from `--owner-key FILE` (`scripts/level2-sepolia.sh`).

use onyx_source_fiscaldata::round::from_env;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    onyx_level2::live::coordinator(from_env()?.as_ref(), &args)
}
